#!/usr/bin/env python3
"""Token 清洗、选词与 Zipf 量化（格式 v1 冻结规则）。"""

import math
import re
import string
from decimal import Decimal, ROUND_HALF_UP

MIN_WORD_LEN = 1
MAX_WORD_LEN = 48
SINGLE_LETTERS = frozenset(("a", "i"))  # 单字母词只保留 a 和 i
ZIPF_CENTI_MIN = 0
ZIPF_CENTI_MAX = 900

ASCII_LETTERS = frozenset(string.ascii_letters)
URL_RE = re.compile(r"://|^www\.|\.(?:com|org|net|edu|gov|io|cn|co)(?:/|$)", re.IGNORECASE)

# 跳过类别，声明顺序即 classify_token 的判定优先级与报告顺序
SKIP_CATEGORIES = (
    "empty", "control", "non-ascii", "whitespace", "url", "too-long",
    "digit", "apostrophe", "hyphen", "symbol", "single-letter",
)


class BuildError(Exception):
    """构建期可预期的失败（参数、来源、覆盖表、产物自检）。"""


def quantize_zipf(value, context=""):
    """zipf_centi = round_half_up(Zipf × 100)，合法 0..900。

    用 Decimal(str(value)) 避免二进制浮点误差；NaN/Infinity 直接拒绝。
    """
    if isinstance(value, float) and not math.isfinite(value):
        raise BuildError("Zipf 值不是有限数：%r%s" % (value, context))
    try:
        dec = Decimal(str(value))
    except Exception:
        raise BuildError("Zipf 值无法量化：%r%s" % (value, context))
    if not dec.is_finite():
        raise BuildError("Zipf 值不是有限数：%r%s" % (value, context))
    centi = int((dec * 100).quantize(Decimal("1"), rounding=ROUND_HALF_UP))
    if not ZIPF_CENTI_MIN <= centi <= ZIPF_CENTI_MAX:
        raise BuildError("zipf_centi 越界（合法 0..900）：%d（源值 %r）%s"
                         % (centi, value, context))
    return centi


def classify_token(word):
    """返回跳过类别；None 表示通过清洗。

    判定顺序即优先级，每个 token 只计入第一个命中的类别。被拒绝的 token 不做
    任何变形（don't 不会变成 dont，C++ 不会变成 c），只统计类别。
    """
    if not word:
        return "empty"
    for ch in word:
        code = ord(ch)
        if code < 0x20 or code == 0x7F:
            return "control"
    for ch in word:
        if ord(ch) > 0x7F:
            return "non-ascii"
    if any(ch.isspace() for ch in word):
        return "whitespace"
    if URL_RE.search(word):
        return "url"
    if len(word) > MAX_WORD_LEN:
        return "too-long"
    if any(ch.isdigit() for ch in word):
        return "digit"
    if "'" in word:
        return "apostrophe"
    if "-" in word:
        return "hyphen"
    if any(ch not in ASCII_LETTERS for ch in word):
        return "symbol"
    if len(word) == 1 and word.lower() not in SINGLE_LETTERS:
        return "single-letter"
    return None


def select_base_words(candidates, limit):
    """清洗 → 合并同 key → 截断，返回 (pairs, stats)。

    candidates 为 (原词面, zipf) 序列。pairs 是 [(key, zipf)]：先按 Zipf 降序、
    并列按 key 字节序升序，再取前 limit 个唯一 key（过滤在截断之前完成）。
    """
    best = {}
    skipped = {}
    merged = 0
    scanned = 0
    for word, zipf in candidates:
        scanned += 1
        if isinstance(zipf, float) and not math.isfinite(zipf):
            raise BuildError("词频不是有限数：%r → %r" % (word, zipf))
        category = classify_token(word)
        if category is not None:
            skipped[category] = skipped.get(category, 0) + 1
            continue
        key = word.lower()
        old = best.get(key)
        if old is None:
            best[key] = zipf
        else:
            merged += 1
            if zipf > old:
                best[key] = zipf
    ordered = sorted(best.items(), key=lambda item: (-item[1], item[0]))
    pairs = ordered[:limit] if limit is not None else ordered
    stats = {
        "scanned": scanned,
        "skipped": skipped,
        "merged": merged,
        "unique_keys": len(best),
        "selected": len(pairs),
    }
    return pairs, stats
