#!/usr/bin/env python3
"""词级简拼/混合缩写样本生成（测试面 A，见 DESIGN.md）。

从 dict.bin 枚举全部纯汉字词条，系统性生成（零人工挑样本）：
- 纯简拼：首字母串；zh/ch/sh 两种写法都生成（折叠 zgr 与全声母 zhgr）；
- 混合缩写：部分音节缩写+部分全拼。2 字词 2 种模式全枚举（zguo/zhongg），
  3/4 字词全枚举（2^n-2 种），≥5 字词采样 3 种；带 zh/ch/sh 的词额外出
  一个折叠变体（只对首个模式，防爆炸）。

按词频分层抽样控制总量（每层词数上限，CLI 可调）。
输出 JSONL：{"input","word","style","syls","freq"}
style ∈ abbrev / abbrev_unfold / mixed / mixed_unfold。
"""

import json
import random
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from build_eval import read_dict, syllable_set  # noqa: E402

HAN_RE = re.compile(r"^[\u4e00-\u9fff]+$")
DIGRAPHS = ("zh", "ch", "sh")
STRATA = [1_000_000, 100_000, 10_000, 1_000, 0]
DEFAULT_CAPS = [25_000, 25_000, 25_000, 15_000, 10_000]


def folded(syl: str) -> str:
    return syl[0]


def unfolded(syl: str) -> str:
    return syl[:2] if syl[:2] in DIGRAPHS else syl[0]


def best_key(word: str, keys: list[tuple[str, int]], syllables: set[str]):
    """词的全部拼音 key 里取频次最高且音节全合法的那条。"""
    for key, _freq in sorted(keys, key=lambda kf: kf[1], reverse=True):
        syls = key.split(" ")
        if all(s in syllables for s in syls):
            return syls
    return None


def mixed_inputs(syls: list[str], patterns: list[tuple[int, ...]], fold_first: bool):
    """按模式位生成混合输入。位=1 表示该音节取首字母，0 表示全拼。

    fold_first=True 时对首个模式补一个 zh/ch/sh 折叠写法变体（若该模式
    里有缩写的双声母音节）。返回 (输入, style, 模式位串)。
    """
    out = []
    for pi, pat in enumerate(patterns):
        for fold in ([False, True] if pi == 0 and fold_first else [False]):
            parts = []
            for syl, is_init in zip(syls, pat):
                if not is_init:
                    parts.append(syl)
                else:
                    i = folded(syl) if fold else unfolded(syl)
                    parts.append(i)
            key = "".join(parts)
            if not key.isascii() or not key.islower():
                continue
            style = "mixed_unfold" if fold else "mixed"
            out.append((key, style, "".join(str(b) for b in pat)))
    return out


def word_samples(syls: list[str]) -> list[tuple[str, str, str]]:
    """一个词的全部（输入, style, 模式位串）样本；纯简拼模式位为全 1。"""
    n = len(syls)
    has_digraph = any(s[:2] in DIGRAPHS for s in syls)
    ones = "1" * n
    samples = [("".join(folded(s) for s in syls), "abbrev", ones)]
    if has_digraph:
        samples.append(("".join(unfolded(s) for s in syls), "abbrev_unfold", ones))

    if n == 2:
        pats = [(1, 0), (0, 1)]
        fold_first = has_digraph
    elif n == 3:
        pats = [(1, 1, 0), (1, 0, 1), (0, 1, 1), (1, 0, 0), (0, 1, 0), (0, 0, 1)]
        fold_first = has_digraph
    elif n == 4:
        pats = [
            (1, 1, 1, 0), (1, 1, 0, 1), (1, 0, 1, 1), (0, 1, 1, 1),
            (1, 1, 0, 0), (1, 0, 1, 0), (1, 0, 0, 1), (0, 1, 1, 0),
            (0, 1, 0, 1), (0, 0, 1, 1), (1, 0, 0, 0), (0, 1, 0, 0),
            (0, 0, 1, 0), (0, 0, 0, 1),
        ]
        fold_first = has_digraph
    else:
        rng = random.Random(sum(ord(c) for c in "".join(syls)))
        pats = []
        while len(pats) < 3:
            pat = tuple(rng.randint(0, 1) for _ in syls)
            if 0 < sum(pat) < n:
                pats.append(pat)
        fold_first = False
    samples.extend(mixed_inputs(syls, pats, fold_first))
    return samples


def main() -> None:
    dict_path = sys.argv[1] if len(sys.argv) > 1 else "data/dict.bin"
    out_path = sys.argv[2] if len(sys.argv) > 2 else "tools/typing_sim/data/abbrev_samples.jsonl"
    caps = DEFAULT_CAPS
    if len(sys.argv) > 3:
        caps = [int(x) for x in sys.argv[3].split(",")]
    seed = int(sys.argv[4]) if len(sys.argv) > 4 else 42

    entries, _total = read_dict(dict_path)
    seg_rs = Path(__file__).resolve().parents[2] / "crates/xwx_engine/src/segmenter.rs"
    syllables = syllable_set(seg_rs.read_text(encoding="utf-8"))
    print(f"dict.bin: {len(entries)} 条; 音节表 {len(syllables)} 个")

    # 词 -> 最高频合法 key 的音节列表 + 词频
    words: dict[str, tuple[list[str], int]] = {}
    for py, word, freq in entries:
        if freq < 1 or not HAN_RE.match(word):
            continue
        if not (2 <= len(word) <= 8):
            continue
        syls = py.split(" ")
        if not all(s in syllables for s in syls):
            continue
        cur = words.get(word)
        if cur is None or freq > cur[1]:
            words[word] = (syls, freq)

    # 分层抽样
    strata: dict[int, list[tuple[str, list[str], int]]] = {s: [] for s in STRATA}
    for word, (syls, freq) in words.items():
        for s in STRATA:
            if freq >= s:
                strata[s].append((word, syls, freq))
                break

    rng = random.Random(seed)
    chosen: list[tuple[str, list[str], int]] = []
    for s, cap in zip(STRATA, caps):
        bucket = strata[s]
        rng.shuffle(bucket)
        take = bucket[:cap]
        chosen.extend(take)
        print(f"层 freq≥{s}: 词 {len(bucket)} 取 {len(take)}")

    seen: set[tuple[str, str]] = set()
    out = []
    for word, syls, freq in chosen:
        for key, style, pat in word_samples(syls):
            k = (key, word)
            if k in seen:
                continue
            seen.add(k)
            out.append({"input": key, "word": word, "style": style, "pat": pat,
                        "syls": " ".join(syls), "freq": freq})

    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as f:
        for r in out:
            f.write(json.dumps(r, ensure_ascii=False, separators=(",", ":")) + "\n")
    by_style: dict[str, int] = {}
    for r in out:
        by_style[r["style"]] = by_style.get(r["style"], 0) + 1
    print(f"写出 {len(out)} 条 → {out_path}\n按 style: {by_style}")


if __name__ == "__main__":
    main()