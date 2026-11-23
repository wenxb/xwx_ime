#!/usr/bin/env python3
"""`scripts/build_english_bigram.py` 构建端逻辑测试（离线）。"""

import hashlib
import json
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from english.bigram import (  # noqa: E402
    BIGRAM_MAGIC,
    CONTEXT_RECORD_LEN,
    EDGE_RECORD_LEN,
    HEADER_LEN,
    TOP_PER_CONTEXT,
)

SCRIPT = ROOT / "scripts" / "build_english_bigram.py"
LEXICON = ROOT / "tests" / "fixtures" / "english" / "words.bin"


def make_lexicon(tmp: Path, words) -> Path:
    tsv = tmp / "lex.tsv"
    tsv.write_text("\n".join(f"{w}\t{w}\t400" for w in words) + "\n", encoding="utf-8")
    out = tmp / "lex.bin"
    proc = subprocess.run(
        [sys.executable, str(ROOT / "scripts" / "build_english_dict.py"),
         "--entries", str(tsv), "--out", str(out)],
        capture_output=True, text=True, check=False,
    )
    if proc.returncode != 0:
        raise AssertionError(f"造临时词库失败: {proc.stderr}")
    return out


def build(tsv_text: str, tmp: Path, lexicon: Path | None = None):
    tmp.mkdir(parents=True, exist_ok=True)
    tsv = tmp / "in.tsv"
    tsv.write_text(tsv_text, encoding="utf-8")
    out = tmp / "out.bin"
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--input", str(tsv),
         "--lexicon", str(lexicon or LEXICON), "--out", str(out)],
        capture_output=True, text=True, check=False,
    )
    return proc, out


def read_rows(blob: bytes):
    magic, version, header_len, n_ctx, _n_edge = struct.unpack_from("<8sIIII", blob, 0)
    assert magic == BIGRAM_MAGIC and version == 1 and header_len == HEADER_LEN
    ctx_off, edge_off, str_off, _file_len = struct.unpack_from("<QQQQ", blob, 24)
    out = {}
    for i in range(n_ctx):
        prev_off, prev_len, ec, es, _r, total = struct.unpack_from(
            "<IHHIIQ", blob, ctx_off + i * CONTEXT_RECORD_LEN
        )
        prev = blob[str_off + prev_off : str_off + prev_off + prev_len].decode()
        edges = []
        for j in range(ec):
            no, nl, _er, count = struct.unpack_from(
                "<IHHQ", blob, edge_off + (es + j) * EDGE_RECORD_LEN
            )
            edges.append((blob[str_off + no : str_off + no + nl].decode(), count))
        out[prev] = (edges, total)
    return out


class BuilderTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_observed_total_is_summed_before_top32_truncation(self) -> None:
        pairs = [(a, b) for a in "abc" for b in "abcdefghijklmnopqrstuvwxyz"][:40]
        lex = make_lexicon(self.tmp, ["pzz"] + [f"n{a}{b}" for a, b in pairs])
        lines = [f"pzz n{a}{b} {i + 1}" for i, (a, b) in enumerate(pairs)]
        proc, out = build("\n".join(lines) + "\n", self.tmp, lexicon=lex)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        edges, total = read_rows(out.read_bytes())["pzz"]
        self.assertEqual(len(edges), TOP_PER_CONTEXT)
        self.assertEqual(total, sum(range(1, 41)))
        counts = [c for _n, c in edges]
        self.assertEqual(counts, sorted(counts, reverse=True))

    def test_merges_case_duplicates_and_sorts_by_count_desc_then_next_asc(self) -> None:
        text = "HELLO World 2\nhello world 3\nhello there 5\nhello them 9\n"
        proc, out = build(text, self.tmp)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        edges, total = read_rows(out.read_bytes())["hello"]
        self.assertEqual(edges, [("them", 9), ("there", 5), ("world", 5)])
        self.assertEqual(total, 19)

    def test_malformed_lines_fail_with_line_numbers(self) -> None:
        for text, needle in [
            ("hello world\n", "三列"),
            ("hello world abc\n", "count 不是整数"),
            ("hello world 0\n", "必须是正的"),
            ("hello world -5\n", "必须是正的"),
            ("hello\n", "三列"),
        ]:
            proc, _ = build(text, self.tmp)
            self.assertEqual(proc.returncode, 1, f"{text!r} 应当失败")
            self.assertIn(":1", proc.stderr)
            self.assertIn(needle, proc.stderr)

    def test_meta_stats_determinism_and_atomic_no_tmp(self) -> None:
        text = "bad_key world 3\nhello zzzz 7\nhello world 5\n"
        proc_a, out_a = build(text, self.tmp / "a")
        proc_b, out_b = build(text, self.tmp / "b")
        self.assertEqual(proc_a.returncode, 0, proc_a.stderr)
        self.assertEqual(proc_b.returncode, 0, proc_b.stderr)
        self.assertEqual(out_a.read_bytes(), out_b.read_bytes())
        self.assertFalse((out_a.with_name("out.bin.tmp")).exists())

        meta_a = json.loads(out_a.with_name("out.meta.json").read_text(encoding="utf-8"))
        meta_b = json.loads(out_b.with_name("out.meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta_a, meta_b)
        self.assertEqual(meta_a["stats"]["drop_invalid_key"], 1)
        self.assertEqual(meta_a["stats"]["drop_absent"], 1)
        self.assertEqual(meta_a["stats"]["contexts"], 1)
        self.assertEqual(meta_a["lexicon"]["sha256"], hashlib.sha256(LEXICON.read_bytes()).hexdigest())
        self.assertIsNone(meta_a["source"]["url"])
        self.assertNotIn(str(self.tmp), json.dumps(meta_a))


if __name__ == "__main__":
    unittest.main()
