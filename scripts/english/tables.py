#!/usr/bin/env python3
"""--entries / --overrides 表解析：key<TAB>display<TAB>zipf_centi。

非法输入一律报错并给出文件与行号，不静默跳过。
"""

import re
from collections import namedtuple

from english.tokens import BuildError, MAX_WORD_LEN, MIN_WORD_LEN, ZIPF_CENTI_MAX, ZIPF_CENTI_MIN

DEFAULT_OVERRIDE_ZIPF_CENTI = 300  # 覆盖词在无基础词频时的缺省 q

Override = namedtuple("Override", ("key", "display", "q"))

KEY_RE = re.compile(r"[a-z]{%d,%d}" % (MIN_WORD_LEN, MAX_WORD_LEN))
DISPLAY_RE = re.compile(r"[A-Za-z]{%d,%d}" % (MIN_WORD_LEN, MAX_WORD_LEN))
CENTI_RE = re.compile(r"[0-9]+")


def _line_error(path, lineno, message):
    return BuildError("%s:%d: %s" % (path, lineno, message))


def _iter_table_lines(path):
    """yield (行号, 去掉换行的原始行)；跳过空行与 # 注释行。"""
    with open(path, "r", encoding="utf-8") as handle:
        for lineno, raw in enumerate(handle, 1):
            line = raw.rstrip("\n").rstrip("\r")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            yield lineno, line


def _split_three(path, lineno, line):
    fields = line.split("\t")
    if len(fields) != 3:
        raise _line_error(path, lineno, "需要 3 个 TAB 分隔列，实际 %d 列" % len(fields))
    return fields


def _check_key_display(path, lineno, key, display):
    """key 必须是小写完整单词，display 的 ASCII 小写结果必须等于 key。"""
    if not KEY_RE.fullmatch(key):
        raise _line_error(path, lineno, "key 必须是 1..48 个小写 ASCII 字母：%r" % key)
    if not DISPLAY_RE.fullmatch(display):
        raise _line_error(path, lineno, "display 必须是 1..48 个 ASCII 字母：%r" % display)
    if display.lower() != key:
        raise _line_error(path, lineno,
                          "display.lower() 必须等于 key：%r → %r" % (display, key))


def _parse_centi(path, lineno, text, empty_allowed):
    if text == "":
        if empty_allowed:
            return None
        raise _line_error(path, lineno, "第 3 列不能为空（zipf_centi 0..900）")
    if not CENTI_RE.fullmatch(text):
        raise _line_error(path, lineno, "第 3 列必须是 0..900 的整数：%r" % text)
    centi = int(text)
    if not ZIPF_CENTI_MIN <= centi <= ZIPF_CENTI_MAX:
        raise _line_error(path, lineno, "zipf_centi 越界（0..900）：%d" % centi)
    return centi


def parse_overrides_file(path):
    """人工覆盖表：key<TAB>display<TAB>zipf_centi_override。

    第 3 列留空表示沿用基础词频（基础中不存在时用 300）。重复 key、列数不对、
    display 与 key 不匹配都报错，并给出文件与行号。
    """
    overrides = []
    first_line = {}
    for lineno, line in _iter_table_lines(path):
        key, display, centi_text = _split_three(path, lineno, line)
        _check_key_display(path, lineno, key, display)
        centi = _parse_centi(path, lineno, centi_text, empty_allowed=True)
        if key in first_line:
            raise _line_error(path, lineno, "key %r 重复（首次出现在第 %d 行）"
                              % (key, first_line[key]))
        first_line[key] = lineno
        overrides.append(Override(key, display, centi))
    return overrides


def parse_entries_file(path):
    """--entries 清单：key<TAB>display<TAB>zipf_centi（已是量化值）。"""
    entries = {}
    for lineno, line in _iter_table_lines(path):
        key, display, centi_text = _split_three(path, lineno, line)
        _check_key_display(path, lineno, key, display)
        centi = _parse_centi(path, lineno, centi_text, empty_allowed=False)
        if key in entries:
            raise _line_error(path, lineno, "key %r 重复" % key)
        entries[key] = (display, centi)
    return entries


def apply_overrides(entries, overrides):
    """按覆盖表改写 display/q；覆盖词不受基础词 50k 截断影响，可新增 key。"""
    merged = dict(entries)
    added = 0
    for override in overrides:
        base = merged.get(override.key)
        if override.q is not None:
            centi = override.q
        elif base is not None:
            centi = base[1]
        else:
            centi = DEFAULT_OVERRIDE_ZIPF_CENTI
        if base is None:
            added += 1
        merged[override.key] = (override.display, centi)
    return merged, added
