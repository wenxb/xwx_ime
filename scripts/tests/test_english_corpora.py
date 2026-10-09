#!/usr/bin/env python3
"""英文评测语料的模式与隔离校验（M1 验收，见 docs/english-input/05-execution-tests.md 第 15 节）。

这份测试只校验**语料本身**：字段、行数门槛、词汇隔离、以及（在真实词库
存在时）每个 target 的 key 确实在 `data/english.bin` 里。评测质量由
`cargo run --example english_eval` 负责。

`data/english.bin` 不进 git，缺失时相关断言 `skipTest`——但模式与隔离
断言永远跑。
"""

import json
import re
import struct
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CORPORA = ROOT / "tests" / "corpora"
ENGLISH_BIN = ROOT / "data" / "english.bin"

TUNING = CORPORA / "english_tuning.jsonl"
ACCEPTANCE = CORPORA / "english_acceptance.jsonl"

FIELDS = {
    "id",
    "kind",
    "input",
    "target",
    "intent",
    "previous_english",
    "max_rank",
    "expected_first_language",
    "note",
}
KEY_RE = re.compile(r"^[a-z]{1,48}$")
INTENTS = {"english", "chinese", "ambiguous"}
FIRST_LANGS = {None, "english", "chinese"}


def load(path: Path) -> list[dict]:
    rows = []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as e:  # pragma: no cover - 失败信息更直观
            raise AssertionError(f"{path.name}:{n} JSON 解析失败: {e}") from e
    return rows


def read_english_keys(path: Path) -> dict[str, str]:
    """独立最小 reader：key -> display（格式见 docs/english-input/01-data.md）。"""
    b = path.read_bytes()
    assert b[:8] == b"XWXENG1\0", "魔数不匹配"
    version, header_len, word_count, _prefix_count = struct.unpack_from("<IIII", b, 8)
    assert version == 1 and header_len == 64, "版本/头长不匹配"
    words_off, _po, _io, strings_off, file_len = struct.unpack_from("<QQQQQ", b, 24)
    assert file_len == len(b), "file_len 与实际长度不一致"
    out = {}
    prev = None
    for i in range(word_count):
        off = words_off + i * 16
        key_off, disp_off, key_len, disp_len, _q, reserved = struct.unpack_from("<IIHHHH", b, off)
        assert reserved == 0, "reserved 必须为 0"
        key = b[strings_off + key_off : strings_off + key_off + key_len].decode("utf-8")
        disp = b[strings_off + disp_off : strings_off + disp_off + disp_len].decode("utf-8")
        if prev is not None:
            assert prev < key, "word 未按 key 严格升序"
        prev = key
        out[key] = disp
    return out


class CorpusSchemaTest(unittest.TestCase):
    """两个文件的模式断言（不依赖英文词库）。"""

    def setUp(self) -> None:
        for path in (TUNING, ACCEPTANCE):
            if not path.is_file():
                self.fail(f"缺语料文件: {path}")

    def test_line_count_thresholds(self) -> None:
        self.assertGreaterEqual(len(load(TUNING)), 60, "调参集至少 60 行")
        self.assertGreaterEqual(len(load(ACCEPTANCE)), 120, "验收集至少 120 行")

    def test_fields_types_and_enums(self) -> None:
        for path in (TUNING, ACCEPTANCE):
            for row in load(path):
                where = f"{path.name}:{row.get('id')}"
                self.assertEqual(set(row), FIELDS, f"{where} 字段集不对")
                self.assertIsInstance(row["id"], str)
                self.assertIn(row["kind"], {"query"}, f"{where} M1 只支持 query 行")
                self.assertRegex(row["input"], KEY_RE, f"{where} input 必须是小写字母")
                self.assertIn(row["intent"], INTENTS, where)
                self.assertIn(row["expected_first_language"], FIRST_LANGS, where)
                self.assertIsInstance(row["max_rank"], int)
                self.assertGreaterEqual(row["max_rank"], 1, where)
                self.assertIsInstance(row["note"], str)
                self.assertTrue(row["note"], where)
                prev = row["previous_english"]
                self.assertTrue(prev is None or KEY_RE.match(prev), f"{where} previous_english")

    def test_ids_are_globally_unique(self) -> None:
        ids = [r["id"] for r in load(TUNING) + load(ACCEPTANCE)]
        self.assertEqual(len(ids), len(set(ids)), "id 必须全局唯一")

    def test_english_target_keys_are_disjoint_between_the_two_sets(self) -> None:
        def keys(path: Path) -> set[str]:
            return {r["target"].lower() for r in load(path) if r["intent"] != "chinese"}

        tuning, acceptance = keys(TUNING), keys(ACCEPTANCE)
        self.assertGreater(len(tuning), 0)
        self.assertGreater(len(acceptance), 0)
        self.assertEqual(
            tuning & acceptance, set(), "同一个英文词不能同时进调参集和验收集"
        )

    def test_coverage_shape(self) -> None:
        tuning, acceptance = load(TUNING), load(ACCEPTANCE)
        for name, rows in (("tuning", tuning), ("acceptance", acceptance)):
            guard = sum(1 for r in rows if r["intent"] in {"chinese", "ambiguous"})
            self.assertGreaterEqual(guard, 8, f"{name} 的中文保护/同形行太少")
            ctx = sum(1 for r in rows if r["previous_english"])
            self.assertGreaterEqual(ctx, 5, f"{name} 的上下文行太少")
        mixed_case = sum(
            1
            for r in acceptance
            if r["intent"] != "chinese" and r["target"] != r["target"].lower()
        )
        self.assertGreaterEqual(mixed_case, 8, "验收集需要覆盖规范显示大小写")


class CorpusAgainstDictTest(unittest.TestCase):
    """需要在真实 `data/english.bin` 上核对的断言。"""

    def setUp(self) -> None:
        if not ENGLISH_BIN.is_file():
            self.skipTest(f"{ENGLISH_BIN} 不存在（不进 git，先用构建脚本生成）")
        self.keys = read_english_keys(ENGLISH_BIN)

    def test_every_target_key_exists_and_display_matches(self) -> None:
        for path in (TUNING, ACCEPTANCE):
            for row in load(path):
                if row["intent"] == "chinese":
                    continue
                key = row["target"].lower()
                self.assertIn(key, self.keys, f"{row['id']}: {key} 不在英文词表里")
                self.assertEqual(
                    self.keys[key],
                    row["target"],
                    f"{row['id']}: target 必须是词库里的规范显示形式",
                )


if __name__ == "__main__":
    unittest.main()
