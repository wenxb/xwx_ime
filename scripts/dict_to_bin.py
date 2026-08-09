#!/usr/bin/env python3
"""dict.sqlite3 → dict.bin（只读 mmap 二进制词库）。

把 SQLite 词库转成 `MmapDict` 直接映射的紧凑二进制（布局见
data/README.md「dict.bin 二进制格式」）：拼音索引按字典序排序，
查询靠二分；前缀联想靠区间扫描，不再有 LIKE 全表扫描。

用法：
    python3 scripts/dict_to_bin.py [输入.sqlite3] [输出.bin]
    # 默认：data/dict.sqlite3 → data/dict.bin
"""

import os
import sqlite3
import struct
import sys

MAGIC = b"XWXIDM1\0"
VERSION = 1
HEADER_LEN = 52  # 8s + I + 5×Q
RECORD_LEN = 12  # u32 str_off + u16 str_len + u32 ent_off + u16 ent_len
FREQ_MAX = 0xFFFFFFFF  # u32 上限，超出截断（真实词频远达不到）


def main() -> None:
    src = sys.argv[1] if len(sys.argv) > 1 else "data/dict.sqlite3"
    dst = sys.argv[2] if len(sys.argv) > 2 else "data/dict.bin"

    print(f"读取词库: {src}")
    # 只读打开，转换过程不动原库
    uri = f"file:{os.path.abspath(src)}?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    rows = conn.execute("SELECT pinyin, word, freq FROM entries").fetchall()
    conn.close()
    print(f"词条数: {len(rows)}")

    # 按 pinyin 分组，组内按 freq 降序（对应引擎 ORDER BY freq DESC 语义）
    groups: dict[str, list[tuple[str, int]]] = {}
    for pinyin, word, freq in rows:
        groups.setdefault(pinyin, []).append((word, freq))
    for g in groups.values():
        g.sort(key=lambda wf: wf[1], reverse=True)
    pinyins = sorted(groups)  # 字节序排序 → 引擎二分查找
    print(f"拼音 key 数: {len(pinyins)}")

    # 构造三个区：records（拼音索引）→ strings（拼音串）→ entries（词条）
    strings = bytearray()
    entries = bytearray()
    records = bytearray()
    total = 0
    for py in pinyins:
        pb = py.encode("utf-8")
        str_off = len(strings)
        strings.extend(pb)
        ent_off = len(entries)
        for word, freq in groups[py]:
            wb = word.encode("utf-8")
            if len(wb) > 255:
                raise SystemExit(f"词条过长（>255 字节）: {word!r}")
            entries.append(len(wb))
            entries.extend(wb)
            entries.extend(struct.pack("<I", min(freq, FREQ_MAX)))
        records.extend(struct.pack("<IHIH", str_off, len(pb), ent_off, len(entries) - ent_off))
        total += len(groups[py])

    records_off = HEADER_LEN
    strings_off = records_off + len(records)
    entries_off = strings_off + len(strings)
    header = struct.pack(
        "<8sIQQQQQ",
        MAGIC, VERSION, len(pinyins), total,
        records_off, strings_off, entries_off,
    )
    assert len(header) == HEADER_LEN, "头部布局与 Rust 端不一致"

    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    with open(dst, "wb") as f:
        f.write(header)
        f.write(records)
        f.write(strings)
        f.write(entries)
    print(f"已写出: {dst}（{os.path.getsize(dst) / 1024 / 1024:.1f} MB）")


if __name__ == "__main__":
    main()
