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
# v2 在 v1 的 52 字节头尾部追加 `total_freq`(u64)。这一列让引擎启动时不必
# 把整个词条区扫一遍——实测那次扫描让 dict.bin 的常驻从 ~30MB 涨到 63MB。
VERSION = 2
HEADER_FMT = "<8sIQQQQQQ"  # magic + version + n_keys + total + 3 偏移 + total_freq
HEADER_LEN = 60  # 8 + 4 + 6×8
RECORD_LEN = 12  # u32 str_off + u16 str_len + u32 ent_off + u16 ent_len
FREQ_MAX = 0xFFFFFFFF  # u32 上限，超出截断（真实词频远达不到）


def _validate(blob):
    """写出后自检：头部自洽、区段顺序与范围、记录数组越界、拼音 key 严格升序。

    与 `abbrev_to_bin.py` 同一套判据——读取端（Rust）拿到坏文件只会静默降级，
    所以产物必须在**构建这一侧**先自证。
    """
    magic, version, n_keys, _total, rec_off, str_off, ent_off, total_freq = struct.unpack_from(
        HEADER_FMT, blob, 0
    )
    if magic != MAGIC or version != VERSION:
        raise SystemExit("产物魔数/版本不符")
    if total_freq == 0:
        raise SystemExit("total_freq 为 0：词频总和必须非零（引擎按它归一化）")
    if not (HEADER_LEN <= rec_off <= str_off <= ent_off <= len(blob)):
        raise SystemExit(
            f"区段顺序/范围不合法: rec={rec_off} str={str_off} ent={ent_off} len={len(blob)}"
        )
    if rec_off + n_keys * RECORD_LEN > str_off:
        raise SystemExit("记录数组越界")
    prev = None
    for i in range(n_keys):
        o, sl, eo, el = struct.unpack_from("<IHIH", blob, rec_off + i * RECORD_LEN)
        if str_off + o + sl > ent_off or ent_off + eo + el > len(blob):
            raise SystemExit(f"第 {i} 条记录越界")
        key = blob[str_off + o: str_off + o + sl]
        if prev is not None and key <= prev:
            raise SystemExit(f"拼音 key 未严格升序: {prev!r} → {key!r}")
        prev = key


def _write_atomically(src, dst, blob):
    """校验后原子替换（Q4）：拒绝「输入=输出」，先写同目录 `.tmp`，自检通过再 replace。

    直接覆盖目的文件：构建失败或中途被杀会留下半文件，正在 mmap 这张表的进程
    会读到残缺数据。
    """
    if os.path.abspath(src) == os.path.abspath(dst):
        raise SystemExit(f"输入与输出是同一个文件: {dst}（会先截断源文件，拒绝）")
    if len(blob) < HEADER_LEN:
        raise SystemExit("产物过短，拒绝写出")
    _validate(blob)
    tmp = dst + ".tmp"
    with open(tmp, "wb") as f:
        f.write(blob)
    os.replace(tmp, dst)


def main() -> None:
    src = sys.argv[1] if len(sys.argv) > 1 else "data/dict.sqlite3"
    dst = sys.argv[2] if len(sys.argv) > 2 else "data/dict.bin"
    # 「输入=输出」在**解析之前**就拒绝：等写完再查就晚了（源文件已被截断）。
    if os.path.abspath(src) == os.path.abspath(dst):
        raise SystemExit(f"输入与输出是同一个文件: {dst}（会先截断源文件，拒绝）")

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
    freq_sum = 0  # 全表词频总和（v2 头部字段，引擎按它做 P(词)=freq/总频 归一化）
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
            fq = min(freq, FREQ_MAX)
            entries.extend(struct.pack("<I", fq))
            freq_sum += fq
        records.extend(struct.pack("<IHIH", str_off, len(pb), ent_off, len(entries) - ent_off))
        total += len(groups[py])

    records_off = HEADER_LEN
    strings_off = records_off + len(records)
    entries_off = strings_off + len(strings)
    header = struct.pack(
        HEADER_FMT,
        MAGIC, VERSION, len(pinyins), total,
        records_off, strings_off, entries_off, freq_sum,
    )
    assert len(header) == HEADER_LEN, "头部布局与 Rust 端不一致"

    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    _write_atomically(src, dst, header + records + strings + entries)
    print(f"已写出: {dst}（{os.path.getsize(dst) / 1024 / 1024:.1f} MB，总频 {freq_sum}）")


if __name__ == "__main__":
    main()
