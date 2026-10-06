#!/usr/bin/env python3
"""汇总 typing_sim 结果 → 问题报告（tools/typing_sim/REPORT.md + stdout）。

输入：word_results.jsonl（词级召回/名次）、session_results.jsonl（会话模拟）、
可选 abbrev_samples.jsonl（提供 pat 模式位与词长分层）。
问题桶定义见 DESIGN.md「问题暴露口径」。
"""

import json
import difflib
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).parent


def load(name):
    p = HERE / "data" / name
    if not p.is_file():
        return []
    return [json.loads(l) for l in open(p, encoding="utf-8") if l.strip()]


def rank_num(r):
    return None if r == "-" else int(r)


def pct(n, d):
    return f"{n / d:.1%}" if d else "-"


def freq_weighted(rows, samples):
    """按词频加权的召回/首选（使用分布口径）。

    词级抽样是**按词库分层**抽的（每层封顶），与真实打字分布不是一回事：
    均匀口径下「2 字母简拼首选 1.7%」约等于「组内目标词数的倒数」，测的是输入
    歧义度而不是排序质量。这里用词条词频当使用频率的代理再加权一次；两个口径
    一起看，才知道改动帮到了哪些词。`smap` 是 main 里建的全局样本索引（本文件
    的既有写法，`agg_word` 同源）。
    """
    by_style = defaultdict(lambda: [0.0, 0.0, 0.0])
    for r in rows:
        s = smap.get((r["input"], r["word"]))
        w = float(s.get("freq", 0)) if s else 0.0
        if w <= 0:
            continue
        b = by_style[r["style"]]
        b[0] += w
        b[1] += w if r["found"] else 0.0
        b[2] += w if rank_num(r["rank"]) == 0 else 0.0
    return {st: (pct(f_, tot), pct(t1, tot)) for st, (tot, f_, t1) in sorted(by_style.items())}


def agg_word(rows, samples):
    """词级：召回/名次/写法差/模式差/慢查询。返回 (markdown 段落, 问题清单)。"""
    n_all = len(rows)
    by_style = defaultdict(list)
    for r in rows:
        by_style[r["style"]].append(r)
    lines = [f"样本 {n_all} 条，按 style："]
    issues = []
    style_stats = {}
    fw = freq_weighted(rows, samples) if samples else {}
    for st, rs in sorted(by_style.items()):
        found = sum(r["found"] for r in rs)
        ranks = [rank_num(r["rank"]) for r in rs if r["found"]]
        top1 = sum(1 for r in ranks if r == 0)
        fs = sum(1 for r in ranks if r is not None and r < 9)
        style_stats[st] = (len(rs), found, top1, fs)
        w_rec, w_top1 = fw.get(st, ("-", "-"))
        lines.append(
            f"- `{st}`：{len(rs)} 条，召回 {pct(found, len(rs))}，"
            f"首选 {pct(top1, len(rs))}，首屏 {pct(fs, len(rs))}"
            f"（词频加权：召回 {w_rec}，首选 {w_top1}）"
        )

    # 词长分层（词字符数）
    for st in ("abbrev", "mixed"):
        rs = by_style.get(st, [])
        if not rs:
            continue
        bl = defaultdict(lambda: [0, 0, 0, 0])
        for r in rs:
            L = len(r["word"])
            k = L if L < 5 else 5
            b = bl[k]
            b[0] += 1
            b[1] += r["found"]
            if r["found"]:
                rn = rank_num(r["rank"])
                b[2] += rn == 0
                b[3] += rn < 9
        lines.append(f"- `{st}` 按词长（词字符数）：")
        for k in sorted(bl):
            n, f_, t1, fs = bl[k]
            lines.append(f"  - {k} 字：{n} 条，召回 {pct(f_, n)}，首选 {pct(t1, n)}，首屏 {pct(fs, n)}")

    # 折叠 vs 全声母（同词对照）
    pairs = defaultdict(dict)
    for r in rows:
        if r["style"] in ("abbrev", "abbrev_unfold"):
            pairs[r["word"]][r["style"]] = r
    fold_miss = unfol_miss = 0
    examples = []
    for w, d in pairs.items():
        a, b = d.get("abbrev"), d.get("abbrev_unfold")
        if not (a and b):
            continue
        if a["found"] != b["found"]:
            if not b["found"]:
                unfol_miss += 1
                examples.append((w, a["input"], b["input"]))
            else:
                fold_miss += 1
    if pairs:
        lines.append(
            f"- zh/ch/sh 双写法对照（{len(pairs)} 词都有两种样本）："
            f"折叠召回失败后全声母召回成功 {fold_miss}；全声母失败而折叠成功 {unfol_miss}"
        )
        for w, fi, un in examples[:10]:
            issues.append(f"全声母写法不可达：{fi}（折叠）可召回「{w}」，{un} 不可召回")

    # 混合模式位差（需要 pat）
    if samples:
        pat_stats = defaultdict(lambda: [0, 0])
        for r in rows:
            if r["style"] not in ("mixed", "mixed_unfold"):
                continue
            s = smap.get((r["input"], r["word"]))
            if not s:
                continue
            pat = s["pat"]
            # 形状分类：头缩写 = 1*0*（k≥1 个 1），尾缩写 = 0*1*，全 1 = 纯简拼，其余交错
            n = len(pat)
            if pat.count("1") == n:
                shape = "all_init"
            elif pat.rstrip("0") and pat.rstrip("0").lstrip("1") == "":
                shape = "head"  # 1*0*
            elif pat.lstrip("0") and pat.lstrip("0").rstrip("1") == "":
                shape = "tail"  # 0*1*
            else:
                shape = "interior"
            k = (len(pat), shape)
            pat_stats[k][0] += 1
            pat_stats[k][1] += r["found"]
        lines.append("- 混合缩写按模式形状（音节数×头/尾/交错/纯简拼）：")
        for k in sorted(pat_stats):
            n, f_ = pat_stats[k]
            lines.append(f"  - {k[0]} 音节 {k[1]}：{n} 条，召回 {pct(f_, n)}")

    # 未召回实例（按词频降序最有影响的 30 条）
    misses = [r for r in rows if not r["found"]]
    misses.sort(key=lambda r: -r.get("freq", 0) if "freq" in r else 0)
    lines.append(f"\n未召回共 {len(misses)} 条；词频最高的 30 条：")
    for r in misses[:30]:
        lines.append(f"  - `{r['input']}` → 「{r['word']}」（{r['style']}）top3: {r['top3']}")
    for r in misses[:15]:
        issues.append(f"未召回 `{r['input']}`（{r['style']}）→ 「{r['word']}」，top3 = {r['top3']}")

    # 慢查询
    slow = [r for r in rows if r.get("ms_max", 0) > 12.0]
    slow.sort(key=lambda r: -r["ms_max"])
    lines.append(f"\n慢查询（>12ms）{len(slow)} 条，最慢 10 条：")
    for r in slow[:10]:
        lines.append(f"  - `{r['input']}` ms_max={r['ms_max']:.1f} slow={r['slow']}")
    return "\n".join(lines), issues


def agg_session(rows, pmap):
    """会话级：选字结果/抖动/标点/整句字准/击键/延迟。"""
    word_rows = []
    whole = []
    punct_flags = Counter()
    for r in rows:
        if r["mode"] == "whole":
            whole.append(r)
        for w in r["words"]:
            if "outcome" in w:
                word_rows.append(w)
            if "flag" in w:
                punct_flags[w["flag"]] += 1

    n = len(word_rows)
    oc = Counter(w["outcome"] for w in word_rows)
    churn = Counter(w["churn"] for w in word_rows)
    lines = [f"词事件 {n} 个，选字结果："]
    for k in ("space", "select", "select10", "deep_fallback", "leftover",
              "no_cand", "mid_commit", "full_retry", "retry_fail"):
        if oc.get(k):
            lines.append(f"- `{k}`：{oc[k]}（{pct(oc[k], n)}）")
    lines.append(f"前缀抖动：lost={churn.get('lost', 0)}（{pct(churn.get('lost', 0), n)}），"
                 f"fs_churn={churn.get('fs_churn', 0)}（{pct(churn.get('fs_churn', 0), n)}）")
    if punct_flags:
        lines.append(f"句末标点：{dict(punct_flags)}")

    # 按风格
    st = defaultdict(lambda: Counter())
    for w in word_rows:
        s = w.get("style", "?")
        st[s]["n"] += 1
        st[s][w["outcome"]] += 1
        if w["churn"] != "none":
            st[s][f"churn:{w['churn']}"] += 1
    lines.append("按 style：")
    for s, c in sorted(st.items()):
        top1 = c.get("space", 0)
        lines.append(f"- `{s}`：{c['n']} 事件，首选直上 {pct(top1, c['n'])}，"
                     f"数字选词 {pct(c.get('select', 0) + c.get('select10', 0), c['n'])}，"
                     f"深排名 {pct(c.get('deep_fallback', 0), c['n'])}，"
                     f"无候选 {pct(c.get('no_cand', 0), c['n'])}，"
                     f"残留 {pct(c.get('leftover', 0), c['n'])}，"
                     f"重打全拼 {pct(c.get('full_retry', 0), c['n'])}")

    # 深排名/无候选实例
    deep = [w for w in word_rows if w["outcome"] == "deep_fallback"]
    deep.sort(key=lambda w: -rank_num(w["rank"]) if rank_num(w["rank"]) else 0)
    lines.append(f"\n深排名（≥10）{len(deep)} 个，名次最深的 15 个：")
    for w in deep[:15]:
        lines.append(f"  - `{w['input']}` → 「{w['text']}」rank={w['rank']}")
    noc = [w for w in word_rows if w["outcome"] == "no_cand"]
    lines.append(f"无候选 {len(noc)} 个，前 15：")
    for w in noc[:15]:
        lines.append(f"  - `{w['input']}` → 「{w['text']}」（{w.get('style','?')}）")

    # 整句字准 + 击键效率（KSR：与「全拼逐字打」这条基准比省了多少击键）
    if whole:
        accs = []
        for r in whole:
            sm = difflib.SequenceMatcher(None, r["got"], r["sent"])
            accs.append((sm.ratio(), r))
        accs.sort(key=lambda x: x[0])
        mean = sum(a for a, _ in accs) / len(accs)
        exact = sum(1 for r in whole if r["got"] == r["sent"])
        keys = sum(r.get("keys", 0) for r in whole)
        chars = sum(len(r["sent"]) for r in whole)
        base = 0
        for r in whole:
            p = pmap.get(r["id"])
            if not p:
                continue
            for w in p["words"].split(","):
                f = w.split("|")
                if len(f) > 3:
                    base += len(f[3]) + 1  # 全拼逐字母 + 一次上屏键
        lines.append(
            f"\n整句零干预（{len(whole)} 句）：整句完全正确 {pct(exact, len(whole))}，"
            f"平均字转换相似度 {mean:.3f}"
        )
        if base:
            lines.append(
                f"- 击键：实际 {keys} 键 / 全拼基准 {base} 键 ⇒ "
                f"平均每字 {keys / chars:.2f} 键，击键节省率 {pct(base - keys, base)}"
            )
        lines.append("最差 10 句：")
        for a, r in accs[:10]:
            lines.append(f"  - 相似 {a:.2f} [{r['id']}] 期望 {r['sent'][:24]}… / 实得 {r['got'][:24]}…")

    # 延迟：词事件总耗时分布 + 估算逐键（总耗时/字母数，不含选字键）
    per_word = [w["ms"] for w in word_rows if w["ms"] > 0]
    per_key = []
    for w in word_rows:
        k = max(1, len(w["input"]))
        per_key.append(w["ms"] / k)
    slow_words = [w for w in word_rows if w["ms_max"] > 12.0]
    lines.append(f"\n词事件耗时 p50/p95：{quant(per_word, .5):.2f}/{quant(per_word, .95):.2f} ms；"
                 f"估算逐键 p50/p95：{quant(per_key, .5):.2f}/{quant(per_key, .95):.2f} ms；"
                 f"最慢键超 12ms 的词事件 {len(slow_words)} 个（{pct(len(slow_words), n)}）")
    slow_words.sort(key=lambda w: -w["ms_max"])
    for w in slow_words[:10]:
        lines.append(f"  - `{w['input']}` → 「{w['text']}」ms_max={w['ms_max']:.1f}")
    return "\n".join(lines)


def quant(xs, q):
    if not xs:
        return 0.0
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * q))]


if __name__ == "__main__":
    samples = load("abbrev_samples.jsonl")
    smap = {(s["input"], s["word"]): s for s in samples}
    wrows = load("word_results.jsonl")
    srows = load("session_results.jsonl")

    parts = ["# 打字模拟评测报告（自动聚合）\n"]
    if wrows:
        body, issues = agg_word(wrows, samples)
        parts += ["\n## 词级（简拼/混合，覆盖 dict 抽样）\n", body]
        if issues:
            parts += ["\n### 词级问题清单（前若干）\n"] + [f"- {i}" for i in issues[:40]]
    if srows:
        body = agg_session(srows, {p["id"]: p for p in load("plans.jsonl")})
        parts += ["\n## 会话级（人民日报 1998-01 句子打字模拟）\n", body]
    report = "\n".join(parts) + "\n"
    (HERE / "REPORT.md").write_text(report, encoding="utf-8")
    print(report)