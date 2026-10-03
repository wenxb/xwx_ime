#!/usr/bin/env python3
"""v1 对拍判据：**首位 0 变化 + 漂移逐条点名**（取代旧的「逐字节 0 变化」）。

为什么改判据（docs/VERIFY.md「对拍判据」、docs/HANDOFF.md ⑤⑩④）：
`distribution_probe --format v1` 的输出分 A（`lookup_candidates`）/ B（会话层）两节，
每节按输入切段。降级世界（`DEMOTE_OLD_ABBREV_CHANNEL = true`）里旧简拼通道退出供给，
**分数刻度会整体重算**（同一个词的分从 −15.7892 变 −4.1834），逐字节比对必然大面积
不同；但其中「正确答案被顶掉」才是真退化——实测 1207 行差异里首位变化 **0**。
所以判据看两件事：① 逐段首位候选是否变化（必须 0）；② 每条漂移是否可解释、逐条点名。

用法：
    regression_probe --format v1 --stdout 2>/dev/null | grep -v 'min_ms=' > new.txt
    python3 scripts/probe_diff.py baseline_v1.txt new.txt

点名例外（APPROVED_FIRST_CHANGE）：首位变化里**已被语义裁决接受**的段。
门槛不变：不在名 list 里的首位变化照样置退出码。入 list 必须附带裁决记录
（docs/HANDOFF.md 对应条目）。
"""
import re
import sys

# ⑤⑩⑩（2026-10-03，折叠变体音节校验）：`sh` 的旧首位 所(suo)、`zh` 的旧首位
# 在(zai) 来自「去 h」折叠变体的**无校验**命中——suo/zai 不是 sh/zh 声母，
# 打 "sh"/"zh" 不可能是指它们。校验后新首位 谁(shei)/这(zhe) 都是真 sh/zh 声母词。
APPROVED_FIRST_CHANGE = {
    "A:sh": ("所", "谁"),
    "A:zh": ("在", "这"),
}


def parse(path):
    """切成 (节名, 输入, [候选行]) 的段列表，段的顺序即文件顺序。"""
    blocks, cur, sect = [], None, None
    for line in open(path, encoding="utf-8"):
        line = line.rstrip("\n")
        if line.startswith("## "):
            sect = line[3:].strip()
            continue
        m = re.match(r"^### (\S+)$", line)
        if m:
            cur = [sect, m.group(1), []]
            blocks.append(cur)
            continue
        if cur is not None and line.strip():
            cur[2].append(line)
    return blocks


def word_of(line):
    """候选行的第三列是词面（`序号|词|拼音|词频|分|层|echo`）。"""
    parts = line.split("|")
    return parts[1] if len(parts) > 1 else None


def first_word(block):
    return word_of(block[2][0]) if block[2] else None


def words_of(block):
    return {w for w in map(word_of, block[2]) if w is not None}


def main():
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    old, new = parse(sys.argv[1]), parse(sys.argv[2])
    if len(old) != len(new):
        print(f"!! 段数不同: {len(old)} vs {len(new)}（判据不成立）")
        return 1
    same = diff_lines = 0
    first_changed, count_changed, named = [], [], []
    unexpected_first = []
    for a, b in zip(old, new):
        tag = f"{a[0][:1]}:{a[1]}"  # 节名首字母 + 输入
        if a[2] == b[2]:
            same += 1
            continue
        diff_lines += sum(1 for l in a[2] if l not in b[2])
        diff_lines += sum(1 for l in b[2] if l not in a[2])
        if first_word(a) != first_word(b):
            pair = (first_word(a), first_word(b))
            first_changed.append(f"{tag}: {pair[0]} → {pair[1]}")
            if APPROVED_FIRST_CHANGE.get(tag) != pair:
                unexpected_first.append(tag)
        if len(a[2]) != len(b[2]):
            count_changed.append(f"{tag}: {len(a[2])} → {len(b[2])}")
        added, gone = words_of(b) - words_of(a), words_of(a) - words_of(b)
        if added or gone:
            named.append(f"{tag}: +{sorted(added)[:3]} -{sorted(gone)[:3]}")
    n = len(old)
    print(
        f"段数 {n} · 逐字节相同 {same}/{n} · **首位变化 {len(first_changed)}**"
        f" · 条数变化 {len(count_changed)} · 差异行 {diff_lines}"
    )
    for x in first_changed:
        print("  [首位] " + x)
    for x in count_changed:
        print("  [条数] " + x)
    for x in named:
        print("  [点名] " + x)
    # 判据①：未点名的首位变化必须为 0（这是脚本的退出码，方便挂进门槛）。
    # 已裁决的例外见 APPROVED_FIRST_CHANGE；对不上例外记录的照样红。
    return 1 if unexpected_first else 0


if __name__ == "__main__":
    sys.exit(main())
