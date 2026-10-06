#!/usr/bin/env python3
"""语料 → 打字计划 JSONL（测试面 B/C/D，见 DESIGN.md）。

输入：人民日报 1998-01 标注语料（自带人工分词），逐行解析：
- 词表项 `词/词类`；`[复合短语]词类` 整体当一个词（机构/专名，现实中
  用户正是对这类长词打简拼）；
- 含非汉字字符的词跳过（全角数字、外文等），计数报告；
- 句末标点映射为键盘键：，→',' 。→'.' ！→'!' ？→'?' ；→';' 、→'\\'。

每词按习惯模型选输入风格（词长→分布，CLI 可调）：
full 全拼 / abbrev 首字母简拼 / mixed 头部或尾部 k 个音节缩写。
简拼写法默认折叠（zh→z），--unfold 概率改为全声母（zh）。

每 --whole-every 句额外出一条整句计划（mode=whole，词长≤12 的句）：
整句全拼连打、零干预空格——测字转换正确率（评测通行口径）。

词→拼音：dict.bin 最高频 key 优先；缺词回落 pypinyin（lve→lue 归一，
音节仍不合法则跳过该词并计数）。
输出：{"id","mode","sent","punct","words":[{"text","style","input"}]}
"""

import json
import random
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
from build_eval import syllable_set  # noqa: E402

HAN_RE = re.compile(r"^[\u4e00-\u9fff]+$")
PUNCT_KEY = {"，": ",", "。": ".", "！": "!", "？": "?", "；": ";", "：": ":", "、": "\\", "（": "(", "）": ")"}
DIGRAPHS = ("zh", "ch", "sh")

# 词长 → [全拼, 简拼, 混合] 概率（DESIGN.md 习惯模型，可调）
STYLE_DIST = {1: (1.0, 0.0, 0.0), 2: (0.85, 0.15, 0.0), 3: (0.55, 0.45, 0.0)}
STYLE_LONG = (0.35, 0.55, 0.10)


class Pinyinizer:
    """词 → 音节列表。dict 优先，pypinyin 兜底，均不合法则 None。"""

    def __init__(self, dict_path: str, syllables: set[str]):
        self.syllables = syllables
        self.word_keys: dict[str, list[tuple[str, int]]] = {}
        import struct
        sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts"))
        from build_eval import read_dict
        entries, _ = read_dict(dict_path)
        for py, word, freq in entries:
            if freq < 1:
                continue
            self.word_keys.setdefault(word, []).append((py, freq))
        from pypinyin import lazy_pinyin
        self._lazy = lazy_pinyin

    def _valid(self, syls: list[str]) -> bool:
        return all(s in self.syllables for s in syls)

    def __call__(self, word: str):
        best = sorted(self.word_keys.get(word, []), key=lambda kf: kf[1], reverse=True)
        for key, _f in best:
            syls = key.split(" ")
            if self._valid(syls):
                return syls
        syls = [self._norm(s) for s in self._lazy(word)]
        return syls if self._valid(syls) else None

    @staticmethod
    def _norm(s: str) -> str:
        if s in ("lve",):
            return "lue"
        if s in ("nve",):
            return "nue"
        return s


def style_input(syls: list[str], n_sy: int, style: str, unfold: bool, rng) -> str:
    """按风格把音节列表变成键盘字母串。"""
    def init(s: str) -> str:
        if unfold and s[:2] in DIGRAPHS:
            return s[:2]
        return s[0]

    if style == "full":
        return "".join(syls)
    if style == "abbrev":
        return "".join(init(s) for s in syls)
    # mixed：头 k 个或尾 k 个音节缩写（k∈1..n-1）
    k = rng.randint(1, n_sy - 1)
    if rng.random() < 0.5:
        return "".join(init(s) if i < k else s for i, s in enumerate(syls))
    return "".join(s if i < n_sy - k else init(s) for i, s in enumerate(syls))


def parse_line(line: str):
    """一行语料 → (句子 id, [词], 句末标点)。"""
    toks = line.split()
    if len(toks) < 2:
        return None
    sid = toks[0].split("/")[0]
    words, punct = [], ""
    i, n = 1, len(toks)
    while i < n:
        tok = toks[i]
        if tok.startswith("["):
            buf = []
            while i < n and not toks[i].endswith("]"):
                buf.append(toks[i].lstrip("[").rsplit("/", 1)[0])
                i += 1
            if i < n:
                buf.append(toks[i].split("]")[0])
                words.append("".join(buf))
            i += 1
            continue
        word = tok.rsplit("/", 1)[0]
        pos = tok.rsplit("/", 1)[-1] if "/" in tok else ""
        if pos == "w" and word in PUNCT_KEY and i == n - 1:
            punct = PUNCT_KEY[word]
        elif HAN_RE.match(word):
            words.append(word)
        i += 1
    return (sid, words, punct) if words else None


def main() -> None:
    corpus = sys.argv[1] if len(sys.argv) > 1 else "tools/typing_sim/data/199801/199801.txt"
    dict_path = sys.argv[2] if len(sys.argv) > 2 else "data/dict.bin"
    out_path = sys.argv[3] if len(sys.argv) > 3 else "tools/typing_sim/data/plans.jsonl"
    limit = int(sys.argv[4]) if len(sys.argv) > 4 else 6000
    unfold_p = float(sys.argv[5]) if len(sys.argv) > 5 else 0.25
    whole_every = int(sys.argv[6]) if len(sys.argv) > 6 else 5
    seed = int(sys.argv[7]) if len(sys.argv) > 7 else 42

    seg_rs = Path(__file__).resolve().parents[2] / "crates/xwx_engine/src/segmenter.rs"
    syllables = syllable_set(seg_rs.read_text(encoding="utf-8"))
    pinyin_of = Pinyinizer(dict_path, syllables)
    rng = random.Random(seed)

    n_sent = n_skip_word = n_oov = n_whole = 0
    out = []
    with open(corpus, encoding="u8", errors="ignore") as f:
        for line in f:
            parsed = parse_line(line.strip())
            if not parsed:
                continue
            sid, words, punct = parsed
            words = words[:40]
            if len(words) < 1:
                continue
            n_sent += 1
            if n_sent > limit:
                break

            plan_words, sent = [], []
            for w in words:
                syls = pinyin_of(w)
                if syls is None:
                    n_oov += 1
                    continue
                sent.append(w)
                ns = len(syls)
                dist = STYLE_DIST.get(ns, STYLE_LONG)
                r = rng.random()
                style = "full" if r < dist[0] else "abbrev" if r < dist[0] + dist[1] else "mixed"
                if ns == 1:
                    style = "full"
                unfold = rng.random() < unfold_p
                inp = style_input(syls, ns, style, unfold, rng)
                plan_words.append({"text": w, "style": style, "input": inp,
                                   "full": "".join(syls)})
            if not plan_words:
                continue

            out.append({"id": sid, "mode": "word", "sent": "".join(sent),
                        "punct": punct, "words": plan_words})

            if n_sent % whole_every == 0 and len(plan_words) <= 12:
                n_whole += 1
                out.append({"id": sid + ":whole", "mode": "whole",
                            "sent": "".join(sent), "punct": punct,
                            "words": [{"text": w["text"], "style": "whole",
                                       "input": w["full"], "full": w["full"]}
                                      for w in plan_words]})

    # words 压平成 text|style|input 串（模拟器按同一契约解析，见 DESIGN.md）
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as f:
        for r in out:
            flat = ",".join(f"{w['text']}|{w['style']}|{w['input']}|{w['full']}" for w in r["words"])
            f.write(json.dumps({"id": r["id"], "mode": r["mode"], "sent": r["sent"],
                                "punct": r["punct"], "words": flat},
                               ensure_ascii=False, separators=(",", ":")) + "\n")
    by_style: dict[str, int] = {}
    for p in out:
        if p["mode"] == "word":
            for w in p["words"]:
                by_style[w["style"]] = by_style.get(w["style"], 0) + 1
    print(f"句 {n_sent}（整句计划 {n_whole}），计划 {len(out)} 条，"
          f"非汉字词跳过 {n_skip_word}，OOV 跳过 {n_oov}\n词事件按 style: {by_style}\n"
          f"写出 → {out_path}")


if __name__ == "__main__":
    main()