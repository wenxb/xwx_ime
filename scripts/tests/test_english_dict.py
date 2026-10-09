#!/usr/bin/env python3
"""build_english_dict.py 的数据层测试（验收矩阵 D01～D08 的构建端部分）。

只用标准库；产物一律写在临时目录。读取器在测试内独立实现（直接按
docs/english-input/01-data.md 的字节定义解析），不复用构建端解析逻辑。
"""

import ast
import contextlib
import hashlib
import io
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS_DIR = os.path.dirname(HERE)
REPO_DIR = os.path.dirname(SCRIPTS_DIR)
if SCRIPTS_DIR not in sys.path:
    sys.path.insert(0, SCRIPTS_DIR)

import build_english_dict as bed  # noqa: E402  （须先加入 scripts/ 到 sys.path）
from english import pack as pack_mod  # noqa: E402
from english import tokens as tokens_mod  # noqa: E402

FIXTURE = os.path.join(REPO_DIR, "tests", "fixtures", "english", "words.tsv")
LETTERS = "abcdefghijklmnopqrstuvwxyz"
HEADER_STRUCT = struct.Struct("<8sIIIIQQQQQ")


# ---------------------------------------------------------------------------
# 独立静态读取器
# ---------------------------------------------------------------------------

class Reader:
    """只按格式定义解释字节：header → words → prefixes → ids → strings。"""

    def __init__(self, blob):
        (self.magic, self.version, self.header_len, self.word_count, self.prefix_count,
         self.words_off, self.prefixes_off, self.ids_off, self.strings_off,
         self.file_len) = HEADER_STRUCT.unpack_from(blob, 0)
        self.blob = blob
        self.pool = blob[self.strings_off:]
        self._words = None
        self._prefixes = None

    def raw_word(self, index):
        return struct.unpack_from("<IIHHHH", self.blob, self.words_off + 16 * index)

    def raw_prefix(self, index):
        return struct.unpack_from("<IHHII", self.blob, self.prefixes_off + 16 * index)

    def words(self):
        """[(key, display, q, reserved)]，按文件顺序。"""
        if self._words is None:
            out = []
            for i in range(self.word_count):
                ko, do, klen, dlen, q, reserved = self.raw_word(i)
                out.append((self.pool[ko:ko + klen].decode("ascii"),
                            self.pool[do:do + dlen].decode("ascii"), q, reserved))
            self._words = out
        return self._words

    def prefixes(self):
        """[(prefix, ids)]，按文件顺序；ids 为 word_id 元组。"""
        if self._prefixes is None:
            out = []
            for i in range(self.prefix_count):
                po, plen, id_count, start, reserved = self.raw_prefix(i)
                ids = struct.unpack_from("<%dI" % id_count, self.blob, self.ids_off + 4 * start)
                out.append((self.pool[po:po + plen].decode("ascii"), tuple(ids), reserved))
            self._prefixes = out
        return self._prefixes

    def id_count(self):
        return (self.strings_off - self.ids_off) // 4

    def word_id(self, key):
        """精确查询：对严格升序的 key 列二分。"""
        words = self.words()
        raw = key.encode("ascii") if isinstance(key, str) else key
        lo, hi = 0, len(words) - 1
        while lo <= hi:
            mid = (lo + hi) // 2
            probe = words[mid][0].encode("ascii")
            if probe == raw:
                return mid
            lo, hi = (mid + 1, hi) if probe < raw else (lo, mid - 1)
        return None

    def prefix_ids(self, prefix):
        """对严格升序的 prefix 列二分。"""
        entries = self.prefixes()
        raw = prefix.encode("ascii") if isinstance(prefix, str) else prefix
        lo, hi = 0, len(entries) - 1
        while lo <= hi:
            mid = (lo + hi) // 2
            probe = entries[mid][0].encode("ascii")
            if probe == raw:
                return entries[mid][1]
            lo, hi = (mid + 1, hi) if probe < raw else (lo, mid - 1)
        return None


def oracle_top32(words, prefix, k=32):
    """独立全扫描 oracle：直接按 static_score_centi 公式算，不用等价变换。

    words 为按文件顺序（已断言为 key 升序）的 (key, q) 列表，下标即 word_id。
    """
    scored = [(key, q - 15 * (len(key) - len(prefix)))
              for key, q in words if key.startswith(prefix)]
    scored.sort(key=lambda item: (-item[1], item[0]))
    index = {key: i for i, (key, _q) in enumerate(words)}
    return [index[key] for key, _score in scored[:k]]


def two_letter_suffix(index):
    return LETTERS[index // 26] + LETTERS[index % 26]


def group_rows(prefix, count, q):
    """生成 count 个共享 prefix 的词条行（词面 = prefix + 两字母后缀）。"""
    return [(prefix + two_letter_suffix(i),) * 2 + (q,) for i in range(count)]


def read_rows(path):
    """读取 fixture 的 key/display/zipf_centi 行。"""
    rows = []
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            key, display, centi = line.split("\t")
            rows.append((key, display, int(centi)))
    return rows


# ---------------------------------------------------------------------------
# 测试基类
# ---------------------------------------------------------------------------

class BaseCase(unittest.TestCase):
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
        """调用 CLI，返回 (退出码, stdout, stderr)。"""
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = bed.main(argv)
        return code, out.getvalue(), err.getvalue()

    def build(self, rows, overrides_rows=None, out="english.bin", comment=None):
        """写 entries 表并构建，返回 (Reader, blob, meta, entries 路径, 覆盖表路径)。"""
        entries = self.write_table("entries.tsv", rows, comment)
        argv = ["--entries", entries, "--out", self.path(out)]
        ov_path = None
        if overrides_rows is not None:
            ov_path = self.write_table("overrides.tsv", overrides_rows, "# overrides")
            argv += ["--overrides", ov_path]
        code, stdout, stderr = self.run_main(argv)
        self.assertEqual(code, 0, "构建失败：%s%s" % (stdout, stderr))
        with open(self.path(out), "rb") as handle:
            blob = handle.read()
        meta_file = pack_mod.meta_path_for(self.path(out))
        with open(meta_file, encoding="utf-8") as handle:
            meta = json.load(handle)
        return Reader(blob), blob, meta, entries, ov_path


# ---------------------------------------------------------------------------
# D01 确定性
# ---------------------------------------------------------------------------

class TestD01Determinism(BaseCase):
    def test_same_source_and_params_give_byte_identical_bin_and_meta(self):
        rows = read_rows(FIXTURE)
        overrides = [("python", "Python", ""), ("github", "GitHub", ""),
                     ("zzz", "Zzz", 350)]
        left = self.build(rows, overrides, out=os.path.join("a", "english.bin"))
        right = self.build(rows, overrides, out=os.path.join("b", "english.bin"))
        self.assertEqual(left[1], right[1], "bin 不是逐字节相同")
        left_meta = pack_mod.meta_path_for(self.path(os.path.join("a", "english.bin")))
        right_meta = pack_mod.meta_path_for(self.path(os.path.join("b", "english.bin")))
        with open(left_meta, "rb") as lh, open(right_meta, "rb") as rh:
            self.assertEqual(lh.read(), rh.read(), "meta 不是逐字节相同")

    def test_meta_hash_matches_product_and_has_no_volatile_fields(self):
        _reader, blob, meta, entries, _ov = self.build(read_rows(FIXTURE))
        self.assertEqual(meta["sha256"], hashlib.sha256(blob).hexdigest())
        self.assertEqual(meta["file_len"], len(blob))
        self.assertEqual(meta["format"]["version"], 1)
        self.assertEqual(meta["format"]["header_len"], 64)
        self.assertEqual(meta["parameters"]["prefix_top_k"], 32)
        self.assertEqual(meta["parameters"]["prefix_penalty_centi"], 15)
        self.assertEqual(meta["parameters"]["limit"], None)  # --entries 不截断
        self.assertEqual(meta["source"]["kind"], "entries")
        self.assertEqual(meta["source"]["path"], "entries.tsv")
        text = json.dumps(meta, ensure_ascii=False)
        self.assertNotIn(self.tmp, text, "meta 含机器绝对路径")
        for volatile in ("time", "timestamp", "built_at", "date", "random", "seed", "user"):
            self.assertNotIn(volatile, {key.lower() for key in meta}, volatile)
        self.assertEqual(self.path("english.meta.json"),
                         pack_mod.meta_path_for(self.path("english.bin")))

    def test_rebuild_into_same_path_is_idempotent_and_leaves_no_temp(self):
        rows = read_rows(FIXTURE)
        self.build(rows, out="english.bin")
        with open(self.path("english.bin"), "rb") as handle:
            first = handle.read()
        self.build(rows, out="english.bin")
        with open(self.path("english.bin"), "rb") as handle:
            self.assertEqual(first, handle.read())
        leftovers = [name for name in os.listdir(self.tmp) if name.endswith(".tmp")]
        self.assertEqual(leftovers, [], "残留临时文件")


# ---------------------------------------------------------------------------
# D02 清洗先于截断、去重、覆盖优先、非法输入报错
# ---------------------------------------------------------------------------

class TestD02FilteringAndOverrides(BaseCase):
    def test_filtering_happens_before_truncation(self):
        candidates = [("don't", 9.0), ("C++", 8.5), ("Hello", 7.0), ("hello", 6.5),
                      ("world", 6.0), ("dock", 5.0)]
        pairs, stats = bed.select_base_words(candidates, 3)
        # 若先截断后过滤，只剩 1 个词；正确行为是过滤后取 3 个
        self.assertEqual(sorted(key for key, _ in pairs), ["dock", "hello", "world"])
        self.assertEqual(stats["skipped"], {"apostrophe": 1, "symbol": 1})
        self.assertEqual(stats["merged"], 1)  # Hello/hello 合并
        self.assertEqual(stats["unique_keys"], 3)
        self.assertEqual(pairs[0][1], 7.0)  # 保留最高基础频率

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
        reader, _blob, meta, _entries, _ov = self.build(rows, overrides)
        words = {key: (display, q) for key, display, q, _res in reader.words()}
        self.assertEqual(words["hello"], ("Hello", 700))   # 覆盖优先
        self.assertEqual(words["world"], ("World", 510))   # 空第 3 列沿用基础词频
        self.assertEqual(words["zzz"], ("Zzz", 300))       # 基础中不存在 → 300
        self.assertEqual(words["newbyte"], ("NewByte", 350))
        self.assertEqual(meta["overrides"]["added_words"], 2)
        with open(_ov, "rb") as handle:
            self.assertEqual(meta["overrides"]["sha256"], hashlib.sha256(handle.read()).hexdigest())

    def test_invalid_override_reports_file_and_line(self):
        rows = read_rows(FIXTURE)
        for bad_rows, lineno, needle in (
                ([("hello", "Hello", 400), ("hello", "Hello", 500)], 2, "重复"),
                ([("a", "A", 400), ("python", "PythonX", 400)], 2, "display"),
                ([("a", "A", 400), ("B", "B", 400)], 2, "key"),
                ([("a", "A", 400), ("bb", "Bb", 901)], 2, "越界"),
                ([("a", "A", 400), ("bb", "Bb")], 2, "3 个 TAB")):
            entries = self.write_table("entries.tsv", rows)
            overrides = self.write_table("overrides.tsv", bad_rows)
            code, _stdout, stderr = self.run_main(
                ["--entries", entries, "--overrides", overrides,
                 "--out", self.path("bad.bin")])
            self.assertEqual(code, 1)
            self.assertIn("overrides.tsv:%d" % lineno, stderr)
            self.assertIn(needle, stderr)
            self.assertFalse(os.path.exists(self.path("bad.bin")))

    def test_invalid_entries_report_file_and_line(self):
        for rows, lineno in (([("a", "a", 700), ("b", "b", 901)], 2),
                             ([("a", "a", 700), ("a", "A", 700)], 2),
                             ([("A", "A", 700)], 1),
                             ([("a", "b", 700)], 1),
                             ([("a", "a", 700), ("z" * 49, "z" * 49, 700)], 2)):
            entries = self.write_table("entries.tsv", rows)
            code, _stdout, stderr = self.run_main(
                ["--entries", entries, "--out", self.path("bad.bin")])
            self.assertEqual(code, 1)
            self.assertIn("entries.tsv:%d" % lineno, stderr)


# ---------------------------------------------------------------------------
# D03 离线 --entries：不导入 wordfreq
# ---------------------------------------------------------------------------

class _BlockWordfreq:
    """meta_path 拦截器：--entries 路径一旦导入 wordfreq 就失败。"""

    def find_spec(self, name, path=None, target=None):
        if name == "wordfreq" or name.startswith("wordfreq."):
            raise AssertionError("--entries 路径不得导入 wordfreq")
        return None


class TestD03OfflineEntries(BaseCase):
    def test_entries_build_never_imports_wordfreq(self):
        self.assertNotIn("wordfreq", sys.modules)
        blocker = _BlockWordfreq()
        sys.meta_path.insert(0, blocker)
        try:
            reader, _blob, _meta, _entries, _ov = self.build(read_rows(FIXTURE))
        finally:
            sys.meta_path.remove(blocker)
        self.assertNotIn("wordfreq", sys.modules)
        keys = [key for key, _d, _q, _r in reader.words()]
        self.assertIn("hello", keys)
        self.assertIn("women", keys)
        self.assertNotIn("don't", keys)
        self.assertNotIn("c++", keys)

    def test_wordfreq_import_is_lazy_in_source(self):
        wordfreq_imports = 0
        for name, path in (("build_english_dict", os.path.join(SCRIPTS_DIR, "build_english_dict.py")),
                           ("tokens", tokens_mod.__file__),
                           ("tables", os.path.join(SCRIPTS_DIR, "english", "tables.py")),
                           ("pack", pack_mod.__file__)):
            with open(path, encoding="utf-8") as handle:
                tree = ast.parse(handle.read(), path)
            top_level = {id(node) for node in tree.body}
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom):
                    names = [node.module or ""]
                else:
                    continue
                if any(part.split(".")[0] == "wordfreq" for part in names):
                    self.assertNotIn(id(node), top_level,
                                     "%s 顶层导入了 wordfreq（必须延迟到 --wordfreq 分支）" % name)
                    wordfreq_imports += 1
        self.assertGreater(wordfreq_imports, 0, "找不到 wordfreq 导入（--wordfreq 分支失效？）")

    def test_cli_subprocess_without_wordfreq_and_without_network(self):
        entries = self.write_table("entries.tsv", read_rows(FIXTURE))
        out = self.path("sub.bin")
        code = ("import sys; sys.modules['wordfreq'] = None\n"
                "sys.path.insert(0, %r)\n"
                "import build_english_dict as bed\n"
                "raise SystemExit(bed.main(['--entries', %r, '--out', %r]))\n"
                % (SCRIPTS_DIR, entries, out))
        env = dict(os.environ)
        env.pop("PYTHONPATH", None)  # 子进程里连 wordfreq 都不可见
        result = subprocess.run([sys.executable, "-c", code], cwd=REPO_DIR, env=env,
                                capture_output=True, text=True, timeout=180)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(os.path.exists(out))
        self.assertTrue(os.path.exists(pack_mod.meta_path_for(out)))

    def test_wordfreq_limit_validated_before_import(self):
        for limit in ("0", "-1", "200001"):
            code, _stdout, stderr = self.run_main(
                ["--wordfreq", "--limit", limit, "--out", self.path("wf.bin")])
            self.assertEqual(code, 1)
            self.assertIn("--limit", stderr)
            self.assertNotIn("wordfreq", sys.modules)
        self.assertEqual(bed.WORDFREQ_PINNED_VERSION, "3.1.1")
        self.assertEqual(bed.WORDFREQ_WORDLIST, "large")


# ---------------------------------------------------------------------------
# D04 字典序末尾的高频词仍进 Top-32
# ---------------------------------------------------------------------------

class TestD04Top32Ordering(BaseCase):
    def test_high_freq_word_last_in_dict_order_enters_top32(self):
        low = group_rows("ab", 40, 200)          # 4 字母，静态分 200-60=140
        high_key = "abzzzz"
        rows = low + [(high_key, high_key, 900)]  # 6 字母，静态分 900-90=810
        reader, _blob, meta, _entries, _ov = self.build(rows)
        self.assertEqual(meta["word_count"], 41)
        low_keys = sorted(row[0] for row in low)
        self.assertLess(max(low_keys), high_key)  # 高频词确实在字典序末尾
        ids = reader.prefix_ids("ab")
        self.assertIsNotNone(ids)
        self.assertEqual(len(ids), 32)
        high_id = reader.word_id(high_key)
        self.assertEqual(ids[0], high_id, "静态分最高的词必须排在该前缀列表首位")
        self.assertGreater(high_id, reader.word_id(max(low_keys)))
        # 朴素「字典序前 32 个」会漏掉它并多收一个低频词
        self.assertNotIn(reader.word_id("abbn"), ids)
        for wid in ids:
            self.assertTrue(reader.words()[wid][0].startswith("ab"))


# ---------------------------------------------------------------------------
# D05 与独立全扫描 oracle 一致 / 同分并列
# ---------------------------------------------------------------------------

class TestD05OracleEquivalence(BaseCase):
    def build_grouped(self):
        rows = group_rows("qq", 40, 300)                    # 4 字母 → 240
        rows += [(prefix + two_letter_suffix(i) + "z", prefix + two_letter_suffix(i) + "z", 315)
                 for prefix in ("qq",) for i in range(35)]  # 5 字母 → 240（并列）
        rows += [("qqqqqq", "qqqqqq", 400), ("qzzz", "qzzz", 100),
                 ("qz", "qz", 800), ("quz", "quz", 120)]
        reader, _blob, meta, _entries, _ov = self.build(rows)
        return reader, meta

    def test_emitted_cache_matches_full_scan_oracle_for_every_prefix(self):
        reader, meta = self.build_grouped()
        words = [(key, q) for key, _display, q, _res in reader.words()]
        keys = [key for key, _q in words]
        self.assertEqual(keys, sorted(keys), "word 记录必须按 key 严格升序")
        expected_prefixes = set()
        for key in keys:
            for plen in range(1, len(key) + 1):
                expected_prefixes.add(key[:plen])
        emitted = {prefix: ids for prefix, ids, _res in reader.prefixes()}
        self.assertEqual(set(emitted), expected_prefixes)
        self.assertEqual(len(emitted), meta["prefix_count"])
        ties = 0
        for prefix in sorted(expected_prefixes):
            want = oracle_top32(words, prefix)
            self.assertEqual(list(emitted[prefix]), want, "前缀 %r 与 oracle 不一致" % prefix)
            if len(want) == 32:
                scores = [words[wid][1] - 15 * (len(words[wid][0]) - len(prefix))
                          for wid in want]
                ties += len(scores) - len(set(scores))
        self.assertGreater(ties, 0, "测试数据里应有同分并列")

    def test_equal_score_ties_follow_key_order(self):
        reader, _meta = self.build_grouped()
        ids = reader.prefix_ids("qq")
        self.assertEqual(len(ids), 32)
        keys = [reader.words()[wid][0] for wid in ids]
        scores = [reader.words()[wid][2] - 15 * (len(keys[i]) - 2) for i, wid in enumerate(ids)]
        for i in range(1, len(ids)):
            self.assertGreaterEqual(scores[i - 1], scores[i], "未按静态分降序")
            if scores[i - 1] == scores[i]:
                self.assertLess(keys[i - 1], keys[i], "同分未按 key 升序")


# ---------------------------------------------------------------------------
# D06 低频精确词：不在 Top-32 也能精确召回
# ---------------------------------------------------------------------------

class TestD06ExactFallback(BaseCase):
    def test_low_freq_exact_word_reachable_outside_top32(self):
        rows = group_rows("z", 40, 250) + [("zzzzzz", "zzzzzz", 100)]
        reader, _blob, _meta, _entries, _ov = self.build(rows)
        low_id = reader.word_id("zzzzzz")  # 独立二分查找，不依赖前缀缓存
        self.assertIsNotNone(low_id)
        self.assertEqual(reader.words()[low_id][:3], ("zzzzzz", "zzzzzz", 100))
        ids = reader.prefix_ids("z")
        self.assertEqual(len(ids), 32)
        self.assertNotIn(low_id, ids, "该词不应进入前缀 z 的 Top-32")
        self.assertEqual(reader.prefix_ids("zzzzzz"), (low_id,))
        self.assertIsNone(reader.word_id("zzzzz"))


# ---------------------------------------------------------------------------
# D07/D08（构建端部分）静态读取器能否独立解析并重算结构
# ---------------------------------------------------------------------------

class TestD07D08Layout(BaseCase):
    def setUp(self):
        super().setUp()
        self.rows = read_rows(FIXTURE)
        self.reader, self.blob, self.meta, _entries, _ov = self.build(self.rows)

    def test_header_and_sections(self):
        reader, blob = self.reader, self.blob
        self.assertEqual(reader.magic, b"XWXENG1\x00")
        self.assertEqual(reader.version, 1)
        self.assertEqual(reader.header_len, 64)
        self.assertEqual(reader.file_len, len(blob))
        self.assertEqual(reader.file_len, os.path.getsize(self.path("english.bin")))
        self.assertEqual(reader.words_off, 64)
        self.assertEqual(reader.prefixes_off, reader.words_off + 16 * reader.word_count)
        self.assertEqual(reader.ids_off, reader.prefixes_off + 16 * reader.prefix_count)
        self.assertEqual(reader.strings_off, reader.ids_off + 4 * reader.id_count())
        self.assertEqual(len(reader.pool), reader.file_len - reader.strings_off)
        self.assertEqual(len(reader.pool), self.meta["strings_bytes"])

    def test_counts_rederived_from_records_match_header_and_meta(self):
        reader = self.reader
        # 逐条走满 words / prefixes 区段，看是否存在多余或缺失的字节
        for index in range(reader.word_count):
            ko, do, klen, dlen, q, reserved = reader.raw_word(index)
            self.assertEqual(reserved, 0)
            self.assertLessEqual(ko + klen, len(reader.pool))
            self.assertLessEqual(do + dlen, len(reader.pool))
        self.assertEqual(reader.prefixes_off - reader.words_off, 16 * len(reader.words()))
        self.assertEqual(reader.ids_off - reader.prefixes_off, 16 * len(reader.prefixes()))
        self.assertEqual(reader.word_count, len(self.rows))
        self.assertEqual(reader.word_count, self.meta["word_count"])
        self.assertEqual(reader.prefix_count, len(reader.prefixes()))
        self.assertEqual(reader.prefix_count, self.meta["prefix_count"])
        self.assertEqual(self.meta["sha256"], hashlib.sha256(self.blob).hexdigest())

    def test_records_and_prefix_relations_are_valid(self):
        reader = self.reader
        words = reader.words()
        keys = [key for key, _display, _q, _res in words]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(len(set(keys)), len(keys), "key 必须唯一")
        for key, display, q, reserved in words:
            self.assertTrue(key.isalpha() and key.islower() and key.isascii(), key)
            self.assertTrue(display.isalpha() and display.isascii(), display)
            self.assertEqual(display.lower(), key)
            self.assertTrue(1 <= len(key) <= 48 and 1 <= len(display) <= 48)
            self.assertTrue(0 <= q <= 900)
            self.assertEqual(reserved, 0)
        expected_prefixes = set()
        for key in keys:
            for plen in range(1, len(key) + 1):
                expected_prefixes.add(key[:plen])
        emitted = reader.prefixes()
        self.assertEqual({prefix for prefix, _ids, _res in emitted}, expected_prefixes)
        self.assertEqual([prefix for prefix, _ids, _res in emitted], sorted(expected_prefixes))
        for prefix, ids, reserved in emitted:
            self.assertEqual(reserved, 0)
            self.assertTrue(1 <= len(ids) <= 32)
            self.assertEqual(len(set(ids)), len(ids), "前缀引用列表不得有重复 ID")
            for wid in ids:
                self.assertLess(wid, len(words))
                self.assertTrue(words[wid][0].startswith(prefix), "%r / %r" % (prefix, words[wid][0]))
        self.assertEqual(reader.id_count(), sum(len(ids) for _p, ids, _r in emitted))

    def test_fixture_expectations_are_encoded(self):
        words = {key: (display, q) for key, display, q, _res in self.reader.words()}
        self.assertEqual(words["i"], ("I", 690))
        self.assertEqual(words["a"], ("a", 700))
        self.assertEqual(words["python"], ("Python", 410))
        for key in ("an", "he", "you", "can", "women", "hello", "help", "docker"):
            self.assertIn(key, words)
        self.assertEqual(words["women"][1], 500)
        for rejected in ("don't", "dont", "c++", "c", "http2"):
            self.assertNotIn(rejected, words)
