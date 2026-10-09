#!/usr/bin/env python3
"""把静态英文二元词频表编译成 `english_bigram.bin`（M2 首版数据产品）。

规划用法（见 `docs/english-input/04-prediction.md` 第 3 节）：

    python3 scripts/build_english_bigram.py \\
        --input /path/to/pinned-bigram.txt --lexicon data/english.bin \\
        --out data/english_bigram.bin

输入是「prev next count」三列（空格或 TAB 分隔）。测试用同一脚本读小型
人工表，**不联网**。

产物（little-endian）：64 字节头 → contexts → edges → strings。
`english.bin` 的 SHA-256 记进 meta.json，便于检测数据过期；运行时**不使用
跨文件 word_id**，所以旧编号永远不会被解释成另一个词。
"""

import argparse
import json
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from english.bigram import (  # noqa: E402
    BIGRAM_MAGIC,
    BIGRAM_VERSION,
    HEADER_LEN,
    MAX_FILE_LEN,
    MAX_EDGES,
    MAX_CONTEXTS,
    TOP_PER_CONTEXT,
    build_meta,
    serialize,
    verify_blob,
    write_verified,
)
from english.pack import sha256_file  # noqa: E402
from english.tokens import BuildError  # noqa: E402

ENGLISH_MAGIC = b"XWXENG1\x00"


def read_lexicon(path):
    """独立最小 reader：`english.bin` 的 key -> display（只为校验存在性）。"""
    b = path.read_bytes()
    if b[:8] != ENGLISH_MAGIC:
        raise BuildError(f"{path} 不是 english.bin（魔数不匹配）")
    version, header_len, word_count, _pc = struct.unpack_from("<IIII", b, 8)
    if version != 1 or header_len != 64:
        raise BuildError(f"{path} 版本/头长不兼容")
    _wo, _po, _io, strings_off, file_len = struct.unpack_from("<QQQQQ", b, 24)
    if file_len != len(b):
        raise BuildError(f"{path} file_len 与实际长度不一致")
    words_off = 64
    keys = {}
    for i in range(word_count):
        ko, do, kl, dl, _q, _r = struct.unpack_from("<IIHHHH", b, words_off + i * 16)
        keys[b[strings_off + ko : strings_off + ko + kl].decode("utf-8")] = (
            b[strings_off + do : strings_off + do + dl].decode("utf-8")
        )
    return keys


def valid_key(s):
    return 1 <= len(s) <= 48 and s.isascii() and s.isalpha() and s.islower()


def parse_input(path, lexicon):
    """解析三列表，返回 `{(prev, next): count}` 与统计。

    空行、`#` 注释行跳过；其余格式错误**带行号**报错。
    """
    pairs = {}
    stats = {
        "lines": 0,
        "skipped_comment": 0,
        "drop_invalid_key": 0,
        "drop_absent": 0,
        "merged_duplicates": 0,
    }
    with path.open(encoding="utf-8") as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                stats["skipped_comment"] += 1
                continue
            stats["lines"] += 1
            fields = line.split()
            if len(fields) != 3:
                raise BuildError(f"{path}:{lineno} 期望「prev next count」三列，实际 {line!r}")
            prev, nxt, count_s = fields
            try:
                count = int(count_s)
            except ValueError:
                raise BuildError(f"{path}:{lineno} count 不是整数: {count_s!r}") from None
            if count <= 0:
                raise BuildError(f"{path}:{lineno} count 必须是正的 u64: {count}")
            if count > 0xFFFFFFFFFFFFFFFF:
                raise BuildError(f"{path}:{lineno} count 超出 u64: {count}")
            prev, nxt = prev.lower(), nxt.lower()
            # 1) 键规则（与 M1 英文 key 同口径）
            if not valid_key(prev) or not valid_key(nxt):
                stats["drop_invalid_key"] += 1
                continue
            # 2) 两个词都必须在构建时的词库里
            if prev not in lexicon or nxt not in lexicon:
                stats["drop_absent"] += 1
                continue
            key = (prev, nxt)
            if key in pairs:
                # 3) 归一化后的重复词对合并，溢出报错
                total = pairs[key] + count
                if total > 0xFFFFFFFFFFFFFFFF:
                    raise BuildError(f"{path}:{lineno} 重复词对合并溢出: {prev} {nxt}")
                pairs[key] = total
                stats["merged_duplicates"] += 1
            else:
                pairs[key] = count
    return pairs, stats


def build_contexts(pairs):
    """按 prev 分组：`observed_total` **在 Top-32 截断之前**求和。

    返回 `(contexts, edges)`：contexts 按 prev 升序；每个 context 的 edges 按
    count 降序、next 升序，最多 `TOP_PER_CONTEXT` 条。
    """
    by_prev = {}
    for (prev, nxt), count in pairs.items():
        by_prev.setdefault(prev, []).append((nxt, count))

    contexts, edges = [], []
    for prev in sorted(by_prev):
        rows = by_prev[prev]
        observed_total = sum(c for _n, c in rows)
        if observed_total <= 0:
            raise BuildError(f"prev {prev!r} 的 observed_total 必须 > 0")
        rows.sort(key=lambda t: (-t[1], t[0]))
        keep = rows[:TOP_PER_CONTEXT]
        edge_start = len(edges)
        for nxt, count in keep:
            if count > observed_total:
                raise BuildError(f"词对 {prev} {nxt} 的 count 超过 observed_total")
            edges.append((nxt, count))
        contexts.append((prev, len(keep), edge_start, observed_total))
    return contexts, edges


def main():
    ap = argparse.ArgumentParser(description="构建 english_bigram.bin（M2 静态二元数据）")
    ap.add_argument("--input", required=True, help="三列文本表：prev next count")
    ap.add_argument("--lexicon", required=True, help="构建时的 english.bin（用于词表过滤）")
    ap.add_argument("--out", required=True, help="产物路径")
    ap.add_argument("--source-url", default="", help="写进 meta 的来源 URL")
    ap.add_argument("--source-sha256", default="", help="写进 meta 的来源文件 SHA-256")
    args = ap.parse_args()

    inp = Path(args.input)
    lexicon_path = Path(args.lexicon)
    for what, p in (("输入表", inp), ("英文词库", lexicon_path)):
        if not p.is_file():
            raise SystemExit(f"缺{what}: {p}")

    lexicon = read_lexicon(lexicon_path)
    print(f"英文词库: {lexicon_path} ({len(lexicon)} 词)", file=sys.stderr)
    pairs, stats = parse_input(inp, lexicon)
    contexts, edges = build_contexts(pairs)
    if len(contexts) > MAX_CONTEXTS:
        raise SystemExit(f"context 数 {len(contexts)} 超过上限 {MAX_CONTEXTS}")
    if len(edges) > MAX_EDGES:
        raise SystemExit(f"edge 数 {len(edges)} 超过上限 {MAX_EDGES}")

    blob = serialize(contexts, edges)
    if len(blob) > MAX_FILE_LEN:
        raise SystemExit(f"产物 {len(blob)} B 超过上限 {MAX_FILE_LEN}")
    verify_blob(blob)

    out = Path(args.out)
    meta = build_meta(
        product_name=out.name,
        lexicon_path=lexicon_path,
        lexicon_sha256=sha256_file(lexicon_path),
        input_path=inp,
        input_sha256=sha256_file(inp),
        stats=stats,
        contexts=contexts,
        edges=edges,
        blob=blob,
        source_url=args.source_url,
        source_sha256=args.source_sha256,
    )
    write_verified(out, blob)
    meta_path = out.with_name(out.name.replace(".bin", "") + ".meta.json")
    meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
                         encoding="utf-8")

    print(
        f"词对 {len(pairs)}，context {len(contexts)}，edge {len(edges)}，"
        f"产物 {len(blob)} B；过滤：非法键 {stats['drop_invalid_key']}、"
        f"词表外 {stats['drop_absent']}、合并重复 {stats['merged_duplicates']}",
        file=sys.stderr,
    )
    print(f"写出 {out}\n写出 {meta_path}", file=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except BuildError as e:
        raise SystemExit(f"构建失败: {e}") from None
