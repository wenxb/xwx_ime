#!/usr/bin/env python3
"""dict.bin v1 → v2（就地升级头部，加全表词频总和）。

为什么需要单独一支脚本：v2 只在头部尾部追加了 `total_freq`(u64)，词条内容
一字不改；而重建一次 v2 需要 `dict.sqlite3`（只是构建中间格式，机器上通常
早就删了）。这里直接读旧文件、算出总和、把三个区段起点各 +8，再原子替换。

用法：
    python3 scripts/dict_migrate_v2.py [输入dict.bin] [输出dict.bin]
    # 默认原地升级 data/dict.bin（先写 .tmp 再 replace，进程 mmap 的旧 inode 不受影响）

注意：升级后 **abbrev.bin / syllable_graph.bin 不需要重建**——它们只依赖词库
的 key 数与内容，两者都没变；「同代校验」读的也不是文件字节哈希。
"""

import os
import struct
import sys

MAGIC = b"XWXIDM1\0"
V1_HEADER_LEN = 52
V2_HEADER_LEN = 60
RECORD_LEN = 12

def read_v1(blob):
    """解析 v1 头并校验区段；返回 (n_keys, total, rec_off, str_off, ent_off)。"""
    if len(blob) < V1_HEADER_LEN:
        raise SystemExit("文件过短，不是完整 dict.bin")
    magic, version = struct.unpack_from("<8sI", blob, 0)
    if magic != MAGIC:
        raise SystemExit("魔数不匹配，不是 dict.bin")
    if version == 2:
        raise SystemExit("已经是 v2，无需迁移")
    if version != 1:
        raise SystemExit(f"不支持的版本号: {version}")
    n_keys, total, rec_off, str_off, ent_off = struct.unpack_from("<QQQQQ", blob, 12)
    if not (V1_HEADER_LEN <= rec_off <= str_off <= ent_off <= len(blob)):
        raise SystemExit("区段顺序/范围不合法，拒绝迁移")
    if rec_off + n_keys * RECORD_LEN > str_off:
        raise SystemExit("记录数组越界，拒绝迁移")
    return n_keys, total, rec_off, str_off, ent_off

def scan_freq_sum(blob, ent_off, n_keys, rec_off):
    """按「最高词频」判一遍词块边界，同时累加总和（与 Rust 的扫描同口径）。"""
    total_freq = 0
    for i in range(n_keys):
        ent_start, ent_len = struct.unpack_from("<IH", blob, rec_off + i * RECORD_LEN + 6)
        pos = ent_off + ent_start
        end = pos + ent_len
        while pos < end:
            word_len = blob[pos]
            freq_off = pos + 1 + word_len
            if freq_off + 4 > end:
                raise SystemExit(f"第 {i} 条拼音的词块越界，拒绝迁移")
            total_freq += struct.unpack_from("<I", blob, freq_off)[0]
            pos = freq_off + 4
    return total_freq

def main() -> None:
    src = sys.argv[1] if len(sys.argv) > 1 else "data/dict.bin"
    dst = sys.argv[2] if len(sys.argv) > 2 else src

    with open(src, "rb") as f:
        blob = f.read()
    n_keys, total, rec_off, str_off, ent_off = read_v1(blob)
    total_freq = scan_freq_sum(blob, ent_off, n_keys, rec_off)
    if total_freq == 0:
        raise SystemExit("词频总和为 0，数据可疑，拒绝迁移")

    # 三个区段整体后移 8 字节（头部变长），内容原样拷贝。
    shift = V2_HEADER_LEN - V1_HEADER_LEN
    header = struct.pack(
        "<8sIQQQQQQ",
        MAGIC, 2, n_keys, total,
        rec_off + shift, str_off + shift, ent_off + shift, total_freq,
    )
    assert len(header) == V2_HEADER_LEN, "头部布局与 Rust 端不一致"
    out = header + blob[V1_HEADER_LEN:]

    tmp = dst + ".tmp"
    with open(tmp, "wb") as f:
        f.write(out)
    os.replace(tmp, dst)  # 原子替换：正在 mmap 旧文件的进程继续读旧 inode
    print(f"已升级: {dst}（v1 → v2，{os.path.getsize(dst) / 1024 / 1024:.1f} MB，"
          f"拼音 key {n_keys}，词条 {total}，词频总和 {total_freq}）")

if __name__ == "__main__":
    main()
