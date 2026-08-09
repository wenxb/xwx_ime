#!/usr/bin/env python3
"""xwx_ime 词库构建脚本：Rime 词库 → dict.sqlite3。

优先级：
1. fcitx5-rime 的 rime_ice 词库（带 import_tables，按顺序展开；重复词条
   "最上面的权重生效"，与 Rime 语义一致 → 用 INSERT OR IGNORE）；
2. 系统预装 luna_pinyin（/usr/share/rime-data/）。

用法：
    python3 scripts/build_dict.py [输出路径]
    # 默认输出 $XDG_DATA_HOME/xwx/dict.sqlite3（通常 ~/.local/share/xwx/）
"""

import os
import re
import sqlite3
import sys

# ---------------------------------------------------------------------------
# 路径发现
# ---------------------------------------------------------------------------

FCITX5_RIME = os.path.expanduser("~/.local/share/fcitx5/rime")
RIME_ICE = os.path.join(FCITX5_RIME, "rime_ice.dict.yaml")
LUNA = "/usr/share/rime-data/luna_pinyin.dict.yaml"

# 只保留纯汉字词条（rime_ice 里有 A A / DNA 这类字母词，拼音输入法不需要）
WORD_RE = re.compile(r"^[一-鿿]+$")
# 拼音只允许小写字母 + 空格（v 表示 ü）
PY_RE = re.compile(r"^[a-zv ]+$")
# ü 的各种 Unicode 形式 → v
UML = str.maketrans({"ü": "v", "ǖ": "v", "ǘ": "v", "ǚ": "v", "ǜ": "v"})


def parse_dict_lines(path: str):
    """逐行解析 .dict.yaml：跳过 YAML 头（---/...）、# 注释、无 tab 的行。

    yield (word, pinyin, freq)；pinyin 为空字符串表示"无注音条目"（需按
    单字表自动补全，对应 Rime 自动注音语义，见 rime_ice README 对 tencent
    表的说明）：
    - 三列：`词<TAB>拼音<TAB>权重`；
    - 两列且第二列是拼音：`词<TAB>拼音`（权重按 1）；
    - 两列且第二列是数字：`词<TAB>权重`（无注音 → pinyin 为空串）。

    freq 非整数时给 1。
    """
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith(("#", "---", "...")):
                continue
            if "\t" not in line:
                continue  # YAML 元信息行
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
                    # 带注音但无权重
                    yield word, second.translate(UML), 1
                elif second.isdigit():
                    # 无注音：词<TAB>权重（如 tencent 大词库）
                    yield word, "", max(int(second), 1)
                else:
                    continue  # 既不是拼音也不是权重：无效行跳过


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
                ref = line[2:].split("#")[0].strip()  # 去掉行内注释
                if not ref:
                    continue
                # 路径相对 rime 配置目录；可能带 .dict.yaml 后缀或没有
                base = os.path.dirname(path)
                for cand in (ref, ref + ".dict.yaml"):
                    full = cand if os.path.isabs(cand) else os.path.join(base, cand)
                    if os.path.exists(full):
                        imports.append(full)
                        break
    return imports


def find_source():
    """返回 (主词库路径, 展开顺序的完整词库文件列表)。"""
    if os.path.exists(RIME_ICE):
        files = [RIME_ICE]
        files.extend(collect_imports(RIME_ICE))
        return "rime_ice", files
    if os.path.exists(LUNA):
        return "luna_pinyin", [LUNA]
    sys.exit(f"找不到 Rime 词库：{RIME_ICE} 或 {LUNA}\n"
             "可先安装：sudo apt install rime-data-luna-pinyin")


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share")),
        "xwx", "dict.sqlite3",
    )
    os.makedirs(os.path.dirname(out), exist_ok=True)

    src_name, files = find_source()
    print(f"词库源: {src_name}")
    for f in files:
        print(f"  + {os.path.basename(f)}")

    conn = sqlite3.connect(out)
    conn.execute("""CREATE TABLE IF NOT EXISTS entries (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pinyin TEXT NOT NULL,
        word TEXT NOT NULL,
        freq INTEGER NOT NULL DEFAULT 0)""")
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_pinyin_word ON entries(pinyin, word)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_entries_pinyin ON entries(pinyin)")
    conn.execute("DELETE FROM entries")  # 全量重建

    seen = 0
    auto_filled = 0
    omitted = 0
    rows = []
    # 单字 → 读音表：从单字条建立。8105 字表一个字可能有多个读音行
    # （每行带权重），自动注音应取**权重最高**（最常用）的读音：
    # 如 召 shao(1)/zhao(128448) → zhao。权重相同时保留先出现者。
    char_pinyin: dict[str, str] = {}
    char_weight: dict[str, int] = {}
    for path in files:
        for word, pinyin, freq in parse_dict_lines(path):
            seen += 1
            if pinyin and len(word) == 1:
                if freq > char_weight.get(word, 0):
                    char_pinyin[word] = pinyin
                    char_weight[word] = freq
            if not pinyin:
                # 自动注音：逐字查表拼接；任何字查不到读音就跳过该词——
                # 宁可少收，不给错误的拼音。
                syllables = [char_pinyin.get(c, "") for c in word]
                if not all(syllables):
                    omitted += 1
                    continue
                pinyin = " ".join(syllables)
                auto_filled += 1
            rows.append((pinyin, word, freq))
            if len(rows) >= 100_000:
                flush(conn, rows)
                rows = []
    flush(conn, rows)
    conn.commit()

    n = conn.execute("SELECT COUNT(*) FROM entries").fetchone()[0]
    print(
        f"解析 {seen} 条（自动注音 {auto_filled} 条，缺字跳过 {omitted} 条），"
        f"过滤 {seen - omitted - n} 条重复/无效，入库 {n} 条 → {out}"
    )

    # 验证
    for py, expect in (("ni hao", "你好"), ("zhongguo", None)):
        rows_ = conn.execute(
            "SELECT word, freq FROM entries WHERE pinyin=? ORDER BY freq DESC LIMIT 3",
            (py,),
        ).fetchall()
        print(f"  验证 pinyin='{py}': {rows_}")
        if expect:
            assert any(r[0] == expect for r in rows_), f"缺少 {expect}"
    conn.close()


def flush(conn, rows):
    # INSERT OR IGNORE：重复词条保留先导入的（Rime"最上面的权重生效"语义）
    conn.executemany(
        "INSERT OR IGNORE INTO entries (pinyin, word, freq) VALUES (?,?,?)",
        rows,
    )
    conn.commit()


if __name__ == "__main__":
    main()
