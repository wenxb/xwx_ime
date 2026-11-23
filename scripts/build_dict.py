#!/usr/bin/env python3
"""xwx_ime 词库构建脚本：Rime 词库 → dict.sqlite3（含词频去畸变与 wordfreq 校准）。

优先级：
1. fcitx5-rime 的 rime_ice 词库（带 import_tables，按顺序展开；重复词条
   "最上面的权重生效"，与 Rime 语义一致）；
2. 系统预装 luna_pinyin（/usr/share/rime-data/）。

词频校准（默认启用，可用 `--no-wordfreq` 关闭外部词频融合）：
- 融合 `wordfreq('zh')` 现代中文词频（每十亿词出现次），按 Rime 多音字占比拆回各读音；
- 剥离 rime_ice `base.dict.yaml` 的 `+500,000` 人工偏移与顶格假频/论坛脏词；
- `ext`/`tencent` 中命中 `wordfreq` 的真实词脱出 `freq=100` 填充档，其余保持 `100`。

用法：
    python3 scripts/build_dict.py [输出路径] [--no-wordfreq]
"""

import math
import os
import re
import sqlite3
import sys

FCITX5_RIME = os.path.expanduser("~/.local/share/fcitx5/rime")
RIME_ICE = os.path.join(FCITX5_RIME, "rime_ice.dict.yaml")
LUNA = "/usr/share/rime-data/luna_pinyin.dict.yaml"

WORD_RE = re.compile(r"^[一-鿿]+$")
PY_RE = re.compile(r"^[a-zv ]+$")
UML = str.maketrans({"ü": "v", "ǖ": "v", "ǘ": "v", "ǚ": "v", "ǜ": "v"})

#: 与引擎侧 `FILLER_FREQ_TIER` 同值：这一档的词条被当作「没有真实词频依据」。
FILLER_FREQ_TIER = 100


def parse_dict_lines(path: str):
    """逐行解析 .dict.yaml，yield (word, pinyin, freq)。"""
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith(("#", "---", "...")):
                continue
            if "\t" not in line:
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            word = parts[0].strip()
            if not WORD_RE.match(word):
                continue
            if len(parts) >= 3:
                pinyin = parts[1].strip()
                if not PY_RE.match(pinyin):
                    continue
                pinyin = pinyin.translate(UML)
                try:
                    freq = max(int(parts[2].strip() or 1), 1)
                except ValueError:
                    freq = 1
                yield word, pinyin, freq
            else:
                second = parts[1].strip()
                if PY_RE.match(second):
                    yield word, second.translate(UML), 1
                elif second.isdigit():
                    yield word, "", max(int(second), 1)


def collect_imports(path: str):
    """返回 .dict.yaml 头部 import_tables 引用的子词库路径列表。"""
    imports = []
    in_block = False
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line == "---":
                in_block = True
                continue
            if line == "...":
                break
            if in_block and line.startswith("- "):
                ref = line[2:].split("#")[0].strip()
                if not ref:
                    continue
                base = os.path.dirname(path)
                for cand in (ref, ref + ".dict.yaml"):
                    full = cand if os.path.isabs(cand) else os.path.join(base, cand)
                    if os.path.exists(full):
                        imports.append(full)
                        break
    return imports


def find_source():
    if os.path.exists(RIME_ICE):
        files = [RIME_ICE]
        files.extend(collect_imports(RIME_ICE))
        return "rime_ice", files
    if os.path.exists(LUNA):
        return "luna_pinyin", [LUNA]
    sys.exit(
        f"找不到 Rime 词库：{RIME_ICE} 或 {LUNA}\n"
        "可先安装：sudo apt install rime-data-luna-pinyin"
    )


def load_wordfreq(enabled: bool) -> dict[str, float]:
    if not enabled:
        return {}
    try:
        from wordfreq import get_frequency_dict

        wf = get_frequency_dict("zh")
        print(f"词频校准: 已加载 wordfreq(zh) {len(wf)} 条")
        return wf
    except Exception as exc:
        print(f"词频校准: 未加载 wordfreq({exc})，仅做 Rime 去平台与异常截顶")
        return {}


def calibrate_freq(
    word: str,
    raw_freq: int,
    src_stem: str,
    ratio: float,
    wf: dict[str, float],
) -> int:
    """将各来源词频统一校准到现代语料每十亿次尺度，消除 +500k 平台与假频污染。"""
    wf_p = wf.get(word, 0.0)
    wf_f = int(round(wf_p * 1e9)) if wf_p > 0.0 else 0
    n = len(word)

    if n == 1:
        # 单字在 wordfreq（jieba 分词）中对构词字（先/工/公）偏低、对百科专名（县）偏高；
        # 与 8105 字频几何融合兼顾单字常用度与现代语料尺度。
        if wf_f > 0:
            wf_split = max(1, int(round(wf_f * ratio)))
            if raw_freq > FILLER_FREQ_TIER:
                rime_scaled = max(1.0, raw_freq * 0.45)
                return max(1, int(round((rime_scaled * wf_split) ** 0.5)))
            return wf_split
        return min(raw_freq, 5_000) if raw_freq > FILLER_FREQ_TIER else raw_freq

    if src_stem in ("ext", "tencent") and raw_freq == FILLER_FREQ_TIER:
        if wf_f > FILLER_FREQ_TIER:
            return max(FILLER_FREQ_TIER + 1, int(round(wf_f * ratio)))
        return FILLER_FREQ_TIER

    if n == 2:
        if wf_f > 0:
            wf_split = max(1, int(round(wf_f * ratio)))
            if 500_000 <= raw_freq <= 515_000:
                return max(wf_split, int(round(25_000 * ratio)))
            return wf_split
        if 500_000 <= raw_freq <= 515_000:
            return 100_000 + (raw_freq - 500_000) * 200
        if raw_freq > 515_000:
            return 20_000
        if raw_freq > FILLER_FREQ_TIER:
            return max(FILLER_FREQ_TIER + 1, int(round(raw_freq * 0.05)))
        return raw_freq

    # 3+ 字短语：wordfreq 往往按子词切分（如 中国人 仅 525、不知道 为 0）；
    # 正常区间单调融合，极端假频（>2M 如 江泽民同志/三国群英传）截顶，
    # +500k 档无 wordfreq 的高频口语短语用子词几何均值与论坛脏词（阅读权限）区分。
    if wf_f > 0:
        wf_split = max(1, int(round(wf_f * ratio)))
        if raw_freq > 2_000_000:
            return wf_split
        if raw_freq > FILLER_FREQ_TIER:
            return max(wf_split, int(round(raw_freq * 0.45)))
        return wf_split

    if raw_freq > 2_000_000:
        return 2_000
    if raw_freq >= 500_000:
        best_geo = 0.0
        for k in range(1, n):
            fl = wf.get(word[:k], 0)
            fr = wf.get(word[k:], 0)
            if fl > 0 and fr > 0:
                g = math.sqrt(fl * fr) * 1e9
                if g > best_geo:
                    best_geo = g
        return min(80_000, max(2_000, int(round(best_geo * 0.035 * ratio))))
    if raw_freq > FILLER_FREQ_TIER:
        return max(FILLER_FREQ_TIER + 1, int(round(raw_freq * 0.35)))
    return raw_freq


def main():
    args = sys.argv[1:]
    use_wf = True
    if "--no-wordfreq" in args:
        use_wf = False
        args.remove("--no-wordfreq")

    out = args[0] if args else os.path.join(
        os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share")),
        "xwx",
        "dict.sqlite3",
    )
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)

    src_name, files = find_source()
    print(f"词库源: {src_name}")
    for f in files:
        print(f"  + {os.path.basename(f)}")
    wf = load_wordfreq(use_wf)

    seen_count = 0
    auto_filled = 0
    omitted = 0
    char_pinyin: dict[str, str] = {}
    char_weight: dict[str, int] = {}
    seen_pw: set[tuple[str, str]] = set()
    entries: list[tuple[str, str, int, str]] = []
    word_raw_sum: dict[str, int] = {}

    for path in files:
        fname = os.path.basename(path)
        src_stem = fname.split(".")[0]
        for word, pinyin, freq in parse_dict_lines(path):
            seen_count += 1
            if pinyin and len(word) == 1 and freq > char_weight.get(word, 0):
                char_pinyin[word] = pinyin
                char_weight[word] = freq
            if not pinyin:
                syllables = [char_pinyin.get(c, "") for c in word]
                if not all(syllables):
                    omitted += 1
                    continue
                pinyin = " ".join(syllables)
                auto_filled += 1
            key = (pinyin, word)
            if key in seen_pw:
                continue
            seen_pw.add(key)
            entries.append((pinyin, word, freq, src_stem))
            word_raw_sum[word] = word_raw_sum.get(word, 0) + freq

    conn = sqlite3.connect(out)
    conn.execute(
        """CREATE TABLE IF NOT EXISTS entries (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pinyin TEXT NOT NULL,
        word TEXT NOT NULL,
        freq INTEGER NOT NULL DEFAULT 0)"""
    )
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_entries_pinyin ON entries(pinyin)")
    conn.execute("DELETE FROM entries")

    src_stats: dict[str, list[int]] = {}
    batch = []
    for pinyin, word, raw_freq, src_stem in entries:
        total_w = word_raw_sum[word]
        ratio = (raw_freq / total_w) if total_w > 0 else 1.0
        freq = calibrate_freq(word, raw_freq, src_stem, ratio, wf)
        batch.append((pinyin, word, freq))
        st = src_stats.setdefault(src_stem, [0, 0])
        st[0] += 1
        st[1] += freq <= FILLER_FREQ_TIER
        if len(batch) >= 100_000:
            flush(conn, batch)
            batch = []
    flush(conn, batch)
    conn.commit()

    n = conn.execute("SELECT COUNT(*) FROM entries").fetchone()[0]
    print(
        f"解析 {seen_count} 条（自动注音 {auto_filled} 条，缺字跳过 {omitted} 条），"
        f"过滤 {seen_count - omitted - n} 条重复/无效，入库 {n} 条 → {out}"
    )
    print(f"  各来源校准后分布（填充档 = freq ≤ {FILLER_FREQ_TIER}）：")
    for name, (cnt, low) in sorted(src_stats.items(), key=lambda kv: -kv[1][0]):
        pct = low / cnt if cnt else 0.0
        print(f"    {name:16} {cnt:9} 条，填充档 {pct:6.1%}")

    for py, expect in (("ni hao", "你好"), ("zhi dao", "知道"), ("zhen de", "真的")):
        rows_ = conn.execute(
            "SELECT word, freq FROM entries WHERE pinyin=? ORDER BY freq DESC LIMIT 3",
            (py,),
        ).fetchall()
        print(f"  验证 pinyin='{py}': {rows_}")
        assert any(r[0] == expect for r in rows_), f"缺少 {expect}"
    conn.close()


def flush(conn, rows):
    conn.executemany(
        "INSERT OR IGNORE INTO entries (pinyin, word, freq) VALUES (?,?,?)",
        rows,
    )
    conn.commit()


if __name__ == "__main__":
    main()
