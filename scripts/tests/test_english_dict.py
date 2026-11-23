#!/usr/bin/env python3
"""`scripts/build_english_dict.py` 构建端逻辑测试（离线）。"""

import contextlib
import hashlib
import io
import json
import os
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS_DIR = os.path.dirname(HERE)
REPO_DIR = os.path.dirname(SCRIPTS_DIR)
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

import build_english_dict as bed  # noqa: E402
from english import pack as pack_mod  # noqa: E402
from english import tokens as tokens_mod  # noqa: E402

FIXTURE = os.path.join(REPO_DIR, "tests", "fixtures", "english", "words.tsv")
HEADER_STRUCT = struct.Struct("<8sIIIIQQQQQ")


def read_words(blob):
    """最小静态读取：返回 {key: (display, q)}，校验覆盖合并产物。"""
    _m, _v, _hl, word_count, _pc, words_off, _po, _io, strings_off, _fl = HEADER_STRUCT.unpack_from(blob, 0)
    pool = blob[strings_off:]
    out = {}
    for i in range(word_count):
        ko, do, klen, dlen, q, _res = struct.unpack_from("<IIHHHH", blob, words_off + 16 * i)
        out[pool[ko:ko + klen].decode("ascii")] = (pool[do:do + dlen].decode("ascii"), q)
    return out


def read_rows(path):
    rows = []
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            key, display, centi = line.split("\t")
            rows.append((key, display, int(centi)))
    return rows


class BuilderTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="english-dict-test-")
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name

    def path(self, name):
        return os.path.join(self.tmp, name)

    def write_table(self, name, rows, comment=None):
        target = self.path(name)
        with open(target, "w", encoding="utf-8", newline="") as handle:
            if comment:
                handle.write(comment + "\n")
            for row in rows:
                handle.write("\t".join(str(field) for field in row) + "\n")
        return target

    def run_main(self, argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = bed.main(argv)
        return code, out.getvalue(), err.getvalue()

    def build(self, rows, overrides_rows=None, out="english.bin"):
        entries = self.write_table("entries.tsv", rows)
        argv = ["--entries", entries, "--out", self.path(out)]
        ov_path = None
        if overrides_rows is not None:
            ov_path = self.write_table("overrides.tsv", overrides_rows, "# overrides")
            argv += ["--overrides", ov_path]
        code, stdout, stderr = self.run_main(argv)
        self.assertEqual(code, 0, f"构建失败：{stdout}{stderr}")
        with open(self.path(out), "rb") as handle:
            blob = handle.read()
        with open(pack_mod.meta_path_for(self.path(out)), encoding="utf-8") as handle:
            meta = json.load(handle)
        return blob, meta, ov_path

    def test_determinism_meta_and_atomic_rebuild(self):
        rows = read_rows(FIXTURE)
        overrides = [("python", "Python", ""), ("github", "GitHub", ""), ("zzz", "Zzz", 350)]
        blob_a, meta_a, _ = self.build(rows, overrides, out=os.path.join("a", "english.bin"))
        blob_b, meta_b, _ = self.build(rows, overrides, out=os.path.join("b", "english.bin"))
        self.assertEqual(blob_a, blob_b)
        self.assertEqual(meta_a, meta_b)
        self.assertEqual(meta_a["sha256"], hashlib.sha256(blob_a).hexdigest())
        self.assertEqual(meta_a["file_len"], len(blob_a))
        self.assertNotIn(self.tmp, json.dumps(meta_a, ensure_ascii=False))
        for volatile in ("time", "timestamp", "built_at", "date", "random", "seed", "user"):
            self.assertNotIn(volatile, {k.lower() for k in meta_a})
        self.assertNotIn("wordfreq", sys.modules, "--entries 不得导入 wordfreq")

        # 同路径重构建幂等且无 .tmp 残留
        self.build(rows, out="english.bin")
        self.build(rows, out="english.bin")
        self.assertEqual([n for n in os.listdir(self.tmp) if n.endswith(".tmp")], [])

    def test_filtering_happens_before_truncation(self):
        candidates = [("don't", 9.0), ("C++", 8.5), ("Hello", 7.0), ("hello", 6.5),
                      ("world", 6.0), ("dock", 5.0)]
        pairs, stats = bed.select_base_words(candidates, 3)
        self.assertEqual(sorted(k for k, _ in pairs), ["dock", "hello", "world"])
        self.assertEqual(stats["skipped"], {"apostrophe": 1, "symbol": 1})
        self.assertEqual(stats["merged"], 1)
        self.assertEqual(pairs[0][1], 7.0)

    def test_skip_categories(self):
        cases = {"": "empty", "he\tllo": "control", "naïve": "non-ascii",
                 "he llo": "whitespace", "www.google.com": "url", "x" * 49: "too-long",
                 "42": "digit", "don't": "apostrophe", "well-known": "hyphen",
                 "C++": "symbol", "b": "single-letter"}
        for token, expected in cases.items():
            self.assertEqual(tokens_mod.classify_token(token), expected, repr(token))
        for token in ("a", "i", "A", "I", "an", "he", "you", "can", "women"):
            self.assertIsNone(tokens_mod.classify_token(token), token)

    def test_quantize_half_up_and_rejects_nonfinite(self):
        self.assertEqual(bed.quantize_zipf("3.505"), 351)
        self.assertEqual(bed.quantize_zipf(3.5), 350)
        self.assertEqual(bed.quantize_zipf(0.0), 0)
        self.assertEqual(bed.quantize_zipf(9.0), 900)
        for bad in (float("nan"), float("inf"), float("-inf"), 9.1, -0.1):
            with self.assertRaises(bed.BuildError):
                bed.quantize_zipf(bad)

    def test_overrides_win_default_display_and_are_not_truncated(self):
        rows = [("hello", "hello", 450), ("world", "world", 510)]
        overrides = [("hello", "Hello", 700), ("world", "World", ""),
                     ("zzz", "Zzz", ""), ("newbyte", "NewByte", 350)]
        blob, meta, ov_path = self.build(rows, overrides)
        words = read_words(blob)
        self.assertEqual(words["hello"], ("Hello", 700))
        self.assertEqual(words["world"], ("World", 510))
        self.assertEqual(words["zzz"], ("Zzz", 300))
        self.assertEqual(words["newbyte"], ("NewByte", 350))
        self.assertEqual(meta["overrides"]["added_words"], 2)
        with open(ov_path, "rb") as handle:
            self.assertEqual(meta["overrides"]["sha256"], hashlib.sha256(handle.read()).hexdigest())

    def test_invalid_override_and_entries_report_file_and_line(self):
        rows = read_rows(FIXTURE)
        for bad_rows, lineno, needle in (
            ([("hello", "Hello", 400), ("hello", "Hello", 500)], 2, "重复"),
            ([("a", "A", 400), ("python", "PythonX", 400)], 2, "display"),
            ([("a", "A", 400), ("B", "B", 400)], 2, "key"),
            ([("a", "A", 400), ("bb", "Bb", 901)], 2, "越界"),
            ([("a", "A", 400), ("bb", "Bb")], 2, "3 个 TAB"),
        ):
            entries = self.write_table("entries.tsv", rows)
            overrides = self.write_table("overrides.tsv", bad_rows)
            code, _out, stderr = self.run_main(
                ["--entries", entries, "--overrides", overrides, "--out", self.path("bad.bin")]
            )
            self.assertEqual(code, 1)
            self.assertIn(f"overrides.tsv:{lineno}", stderr)
            self.assertIn(needle, stderr)
            self.assertFalse(os.path.exists(self.path("bad.bin")))

        for bad_entries, lineno in (
            ([("a", "a", 700), ("b", "b", 901)], 2),
            ([("a", "a", 700), ("a", "A", 700)], 2),
            ([("A", "A", 700)], 1),
            ([("a", "b", 700)], 1),
            ([("a", "a", 700), ("z" * 49, "z" * 49, 700)], 2),
        ):
            entries = self.write_table("entries.tsv", bad_entries)
            code, _out, stderr = self.run_main(["--entries", entries, "--out", self.path("bad.bin")])
            self.assertEqual(code, 1)
            self.assertIn(f"entries.tsv:{lineno}", stderr)


if __name__ == "__main__":
    unittest.main()
