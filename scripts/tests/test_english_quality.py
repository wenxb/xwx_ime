"""`english/quality.py` 的测试：把「哪些字符串算英文单词」钉成可复核的判据。

这些用例的取值全部来自真实数据（wordfreq 的 large 表 + SymSpell 一元表 +
hunspell en_US），不是编的：`ve`/`gra`/`lun` 确实是 wordfreq 里的碎片，
`github` 确实只在 hunspell 里（且带 `/SM` 标志）。
"""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from english.quality import (  # noqa: E402
    FREQUENT_CENTI,
    SHORT_WORDS,
    filter_entries,
    keep_word,
    load_references,
    parse_reference_file,
)

REF = {"hello", "world", "color", "center", "github", "math", "dog", "run"}


class KeepWordRules(unittest.TestCase):
    def test_reference_hit_is_kept(self):
        self.assertEqual(keep_word("hello", 100, REF), (True, "reference"))

    def test_short_whitelist_keeps_real_two_letter_words(self):
        for w in ("a", "i", "am", "is", "to", "we", "ok", "oh"):
            self.assertIn(w, SHORT_WORDS)
            self.assertEqual(keep_word(w, 100, REF), (True, "short"), w)

    def test_short_fragments_are_dropped_even_when_frequent(self):
        # `ve`(456) / `ll`(474) 频次很高——靠频率规则根本拦不住，必须靠白名单。
        for w in ("ve", "ll", "im", "gc", "zz", "dr", "aj"):
            self.assertEqual(keep_word(w, 474, REF), (False, "short"), w)

    def test_stem_saves_plural_and_gerund_missing_from_reference(self):
        # hunspell 只存词干（`color/S`），派生形得靠去词尾救回来。
        self.assertEqual(keep_word("colors", 100, REF), (True, "stem"))
        self.assertEqual(keep_word("colorful", 100, REF), (False, "junk"))
        self.assertEqual(keep_word("dogs", 100, REF), (True, "stem"))
        self.assertEqual(keep_word("running", 100, REF), (True, "stem"))
        self.assertEqual(keep_word("centers", 100, REF), (True, "stem"))

    def test_frequent_keeps_words_no_reference_covers(self):
        # 参考词表对品牌名/网络词覆盖很差；频率是最后一道兜底。
        # `math` 用的是「参考表命中」，这里要的是**参考表覆盖不到**的高频词。
        self.assertEqual(keep_word("google", FREQUENT_CENTI, REF), (True, "frequent"))
        self.assertEqual(keep_word("lol", 480, REF), (True, "frequent"))

    def test_rare_and_unknown_is_junk(self):
        # `gra`(272)/`lun`(267) 是真实语料里的碎片：既不精确命中、词干也不在、
        # 频率还低于门槛。
        for w, q in (("gra", 272), ("lun", 267), ("bri", 286), ("holi", 271), ("bre", 253)):
            self.assertEqual(keep_word(w, q, REF), (False, "junk"), w)

    def test_without_reference_the_filter_is_identity(self):
        # 不给参考词表 = 旧行为，产物逐字节不变（向后兼容的硬约束）。
        for w in ("ve", "gra", "lol", "zzz"):
            self.assertEqual(keep_word(w, 500, set()), (True, "no_reference"), w)


class FilterEntries(unittest.TestCase):
    def test_protected_keys_always_survive(self):
        entries = {"ve": ("ve", 456), "gra": ("gra", 272), "kubernetes": ("Kubernetes", 350)}
        kept, stats = filter_entries(entries, REF, protected={"ve", "kubernetes"})
        self.assertEqual(set(kept), {"ve", "kubernetes"})
        self.assertEqual(stats["protected"], 2)
        self.assertEqual(stats["junk"], 1)

    def test_stats_account_for_every_entry(self):
        entries = {"hello": ("hello", 400), "ve": ("ve", 456), "gra": ("gra", 272)}
        kept, stats = filter_entries(entries, REF)
        self.assertEqual(sum(stats.values()), len(entries))
        self.assertEqual(set(kept), {"hello"})


class ParseReference(unittest.TestCase):
    def _write(self, text):
        fd, path = tempfile.mkstemp(suffix=".txt")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        self.addCleanup(os.unlink, path)
        return path

    def test_strips_hunspell_flags(self):
        # 这是踩过的坑：不剥 `/SM` 的话 `github/SM` 过不了「纯字母」检查，
        # 整份 hunspell 里带标志的词（绝大多数）会被丢掉，参考表缩水一大半，
        # 过滤随之变得过激（`github`/`center` 都被误删）。
        path = self._write("github/SM\ncenter/AB\nhello\n")
        self.assertEqual(parse_reference_file(path), {"github", "center", "hello"})

    def test_accepts_count_column_and_lowercases(self):
        path = self._write("Hello 12345\nWorld\t42\n")
        self.assertEqual(parse_reference_file(path), {"hello", "world"})

    def test_skips_junk_lines_but_needs_something_valid(self):
        path = self._write("\n# comment\n12345\nok\n")
        self.assertEqual(parse_reference_file(path), {"ok"})
        bad = self._write("\n12345\n---\n")
        with self.assertRaises(ValueError):
            parse_reference_file(bad)

    def test_load_references_unions_files(self):
        a = self._write("hello\nworld\n")
        b = self._write("world\ncenter\n")
        self.assertEqual(load_references([a, b]), {"hello", "world", "center"})

    def test_load_references_with_no_paths_is_empty(self):
        self.assertEqual(load_references([]), set())


if __name__ == "__main__":
    unittest.main()
