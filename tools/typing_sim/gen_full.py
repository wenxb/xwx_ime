#!/usr/bin/env python3
"""全拼精确命中样本：把词条音节串直接当输入（用户把拼音打全）。

补的是评测集的盲区——`gen_abbrev.py` 的四种 style 全是简拼/混拼，
「打全了拼音却被补全/预测压住」这类问题在旧口径里根本没有样本
（2026-10-05 用户报 `liji` 首位是「理解」就是这么漏掉的）。

用法：
  python3 tools/typing_sim/gen_full.py --samples data/abbrev_samples.jsonl \
      --out data/full_samples.jsonl [--min-freq 100000] [--max-syl 4]
"""
import argparse
import json
import random


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", default="tools/typing_sim/data/abbrev_samples.jsonl")
    ap.add_argument("--out", default="tools/typing_sim/data/full_samples.jsonl")
    ap.add_argument("--min-freq", type=int, default=100_000)
    ap.add_argument("--max-syl", type=int, default=4)
    ap.add_argument("--seed", type=int, default=20261005)
    args = ap.parse_args()

    seen = {}
    with open(args.samples, encoding="utf-8") as fh:
        for line in fh:
            r = json.loads(line)
            syls = r["syls"].split()
            if not syls or len(syls) > args.max_syl:
                continue
            # 带分隔符（儿化/歧义）的音节串不是「直接打全」的形状，跳过
            if any("'" in s or not s.isalpha() for s in syls):
                continue
            if r["freq"] < args.min_freq:
                continue
            inp = "".join(syls)
            seen.setdefault(inp, r)

    rows = sorted(seen.values(), key=lambda r: (-r["freq"], r["word"]))
    random.Random(args.seed).shuffle(rows)
    with open(args.out, "w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps({
                "input": "".join(r["syls"].split()),
                "word": r["word"],
                "style": "full",
                "pat": r["pat"],
                "syls": r["syls"],
                "freq": r["freq"],
            }, ensure_ascii=False) + "\n")
    print(f"写出 {len(rows)} 条 → {args.out}")


if __name__ == "__main__":
    main()
