#!/usr/bin/env python3
"""`scripts/build_english_bigram.py` 与 `english.bigram` 布局的测试（M2，离线）。

全部用小型人工表，**不联网**。真实 SymSpell 数据只在报告里引用。
"""

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
    serialize,
    verify_blob,
)
from english.tokens import BuildError  # noqa: E402

SCRIPT = ROOT / "scripts" / "build_english_bigram.py"
FIXTURES = ROOT / "tests" / "fixtures" / "english"
LEXICON = FIXTURES / "words.bin"
SMALL_TSV = FIXTURES / "bigram.tsv"


def make_lexicon(tmp: Path, words) -> Path:
    """用真实构建脚本现造一个临时 english.bin（键必须是小写字母）。"""
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


def build(tsv_text: str, tmp: Path, extra=(), lexicon: Path | None = None):
    tsv = tmp / "in.tsv"
    tsv.write_text(tsv_text, encoding="utf-8")
    out = tmp / "out.bin"
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), "--input", str(tsv),
         "--lexicon", str(lexicon or LEXICON), "--out", str(out), *extra],
        capture_output=True, text=True, check=False,
    )
    return proc, out


def read_rows(blob: bytes):
    """独立最小 reader：返回 {prev: [(next, count), ...]} 与 observed_total。"""
    magic, version, header_len, n_ctx, n_edge = struct.unpack_from("<8sIIII", blob, 0)
    assert magic == BIGRAM_MAGIC and version == 1 and header_len == HEADER_LEN
    ctx_off, edge_off, str_off, file_len = struct.unpack_from("<QQQQ", blob, 24)
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
        if not LEXICON.is_file():
            self.skipTest(f"{LEXICON} 不存在（由 test_english_dict 的夹具生成）")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_repository_fixture_matches_the_documented_shape(self) -> None:
        proc, out = build(SMALL_TSV.read_text(encoding="utf-8"), self.tmp)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        blob = out.read_bytes()
        info = verify_blob(blob)
        self.assertEqual(info["context_count"], 6)
        self.assertEqual(info["edge_count"], 8)
        rows = read_rows(blob)
        # 大小写归一化 + 重复合并：7852480 + 100 + 5
        self.assertEqual(rows["hello"][0][0], ("world", 7852585))
        self.assertEqual(rows["thank"][0][0], ("you", 733376704))
        # 非法键与词表外目标都被过滤
        self.assertNotIn("bad_key", rows)
        self.assertNotIn("zzzznotaword", dict(rows["hello"][0]).keys())

    def test_observed_total_is_summed_before_top32_truncation(self) -> None:
        # 40 个 next 都挂在同一个 prev 下：只留 32 条，但 observed_total 必须是 40 条之和。
        # 23 词的仓库夹具装不下 40 个目标，所以现造一个够大的临时词库。
        lex = make_lexicon(self.tmp, ["pzz"] + [f"n{a}{b}" for a in "abc" for b in "abcdefghijklmnopqrstuvwxyz"][:40])
        lines = [f"pzz n{a}{b} {i + 1}" for i, (a, b) in enumerate(
            [(a, b) for a in "abc" for b in "abcdefghijklmnopqrstuvwxyz"][:40])]
        proc, out = build("\n".join(lines) + "\n", self.tmp, lexicon=lex)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        rows = read_rows(out.read_bytes())
        edges, total = rows["pzz"]
        self.assertEqual(len(edges), TOP_PER_CONTEXT)
        self.assertEqual(total, sum(range(1, 41)), "observed_total 必须在截断前求和")
        # 行内按 count 降序
        counts = [c for _n, c in edges]
        self.assertEqual(counts, sorted(counts, reverse=True))

    def test_rows_are_sorted_by_count_desc_then_next_asc(self) -> None:
        proc, out = build("hello world 5\nhello there 5\nhello them 9\n", self.tmp)
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
            self.assertIn(":1", proc.stderr, f"{text!r} 的报错要带行号")
            self.assertIn(needle, proc.stderr)

    def test_absent_and_invalid_words_are_counted_not_silently_dropped(self) -> None:
        proc, out = build("bad_key world 3\nhello zzzz 7\nhello world 5\n", self.tmp)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        meta = json.loads(out.with_name("out.meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta["stats"]["drop_invalid_key"], 1)
        self.assertEqual(meta["stats"]["drop_absent"], 1)
        self.assertEqual(meta["stats"]["contexts"], 1)

    def test_meta_records_the_lexicon_hash_and_is_deterministic(self) -> None:
        text = SMALL_TSV.read_text(encoding="utf-8")
        proc_a, out_a = build(text, self.tmp)
        self.assertEqual(proc_a.returncode, 0, proc_a.stderr)
        proc_b, out_b = build(text, self.tmp / "second" if False else self.tmp)
        self.assertEqual(proc_b.returncode, 0, proc_b.stderr)
        meta_a = json.loads(out_a.with_name("out.meta.json").read_text(encoding="utf-8"))
        meta_b = json.loads(out_b.with_name("out.meta.json").read_text(encoding="utf-8"))
        self.assertEqual(meta_a, meta_b, "同一输入必须产出逐字节相同的 meta")
        self.assertEqual(meta_a["product"]["sha256"], meta_b["product"]["sha256"])
        self.assertEqual(
            meta_a["lexicon"]["sha256"],
            __import__("hashlib").sha256(LEXICON.read_bytes()).hexdigest(),
            "meta 必须记录构建时 english.bin 的 SHA-256",
        )
        self.assertIsNone(meta_a["source"]["url"])
        self.assertNotIn(str(self.tmp), json.dumps(meta_a), "meta 不得写入绝对路径")

    def test_atomic_replace_leaves_no_tmp_file(self) -> None:
        proc, out = build("hello world 5\n", self.tmp)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(out.is_file())
        self.assertFalse((out.with_name("out.bin.tmp")).exists(), "临时文件必须被 rename 掉")


class LayoutTest(unittest.TestCase):
    """不依赖词库的纯布局测试：直接喂给 `serialize`/`verify_blob`。"""

    def test_serialize_then_verify_roundtrip(self) -> None:
        contexts = [("hello", 2, 0, 30), ("thank", 1, 2, 10)]
        edges = [("world", 20), ("there", 10), ("you", 10)]
        blob = serialize(contexts, edges)
        info = verify_blob(blob)
        self.assertEqual(info["context_count"], 2)
        self.assertEqual(info["edge_count"], 3)
        self.assertEqual(info["file_len"], len(blob))

    def test_bad_magic_version_and_lengths_are_rejected(self) -> None:
        blob = bytearray(serialize([("hello", 1, 0, 5)], [("world", 5)]))
        blob[0] = ord("Y")
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))

        blob = bytearray(serialize([("hello", 1, 0, 5)], [("world", 5)]))
        struct.pack_into("<I", blob, 8, 2)
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))

        blob = bytearray(serialize([("hello", 1, 0, 5)], [("world", 5)]))
        struct.pack_into("<Q", blob, 48, len(blob) + 1)
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))

    def test_nonzero_reserved_is_rejected(self) -> None:
        blob = bytearray(serialize([("hello", 1, 0, 5)], [("world", 5)]))
        struct.pack_into("<Q", blob, 56, 1)
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))

    def test_count_above_observed_total_is_rejected(self) -> None:
        blob = bytearray(serialize([("hello", 1, 0, 5)], [("world", 5)]))
        edge_off = struct.unpack_from("<Q", blob, 32)[0]
        struct.pack_into("<Q", blob, edge_off + 8, 99)
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))

    def test_unsorted_contexts_are_rejected(self) -> None:
        blob = bytearray(serialize([("zoo", 1, 0, 5), ("abc", 1, 1, 5)], [("world", 5), ("there", 5)]))
        with self.assertRaises(BuildError):
            verify_blob(bytes(blob))


if __name__ == "__main__":
    unittest.main()
