#!/usr/bin/env python3
"""dict.bin → abbrev.bin(简拼/首字母副索引,只读 mmap 二进制)。

路线 (c)(B3 spike 实测结论):给每个词条算「拼音各
音节首字母串」作为副索引 key(`"zhong guo ren"` → `"zgr"`),建成与主索引
同构的排序 mmap 数组。运行时 `MmapDict` 对纯简拼(`zgr`)直接二分这张表,
无需回主索引。

**只读主索引 `dict.bin`,绝不修改它**;产物是独立文件 `data/abbrev.bin`。

二进制布局与 `dict.bin` v1 完全同构(见 `data/README.md`「abbrev.bin
二进制格式」):同样的 52 字节头、12 字节记录、词条区按 freq 降序;唯一差别
是词条区每条多带一个「全拼」字段,好让查出的词能回送后续打分流程。

混合简拼(`zhongguor`)不走这张表,由 `MmapDict::lookup_mixed` 用编译期静态
音节表 `segmenter::SYLLABLES` 切分输入后回主索引前缀扫描实现——音节表是
编译进二进制的静态数据(410 项),无需构建期写盘,也无需运行期扫描
(旧实现「首次混合简拼懒扫描 173 万主索引 key」实测约 840ms,发生在按键
处理路径上,不可接受)。

用法:
    python3 scripts/abbrev_to_bin.py [输入dict.bin] [输出abbrev.bin]
    # 默认:data/dict.bin → data/abbrev.bin
"""

import os
import struct
import sys

MAGIC = b"XWXABR1\0"   # 副索引魔数(与主索引 XWXIDM1 区分)
VERSION = 3            # v3: 8B 哨兵记录 + 2B CJK 词条 + 6-bit 音节下标 + 变长词频
HEADER_LEN = 52        # 8s + I + 5×Q(与 dict.bin v1 同构)
RECORD_LEN = 8         # v3: u32 str_off + u32 ent_off
FREQ_MAX = 0xFFFFFFFF  # u32 上限,超出截断(真实词频远达不到)

DICT_MAGIC = b"XWXIDM1\0"
DICT_MIN_VERSION = 1
DICT_MAX_VERSION = 3
DICT_HEADER_LEN = 52
DICT_RECORD_LEN = 12


def _decode_dict_v3_entry(block: bytes, pos: int):
    hdr = block[pos]
    fcode = hdr >> 6
    clen = hdr & 0x3F
    if fcode < 3:
        wchars = [chr(0x4E00 + struct.unpack_from("<H", block, pos + 1 + i * 2)[0]) for i in range(clen)]
        w = "".join(wchars)
        foff = pos + 1 + clen * 2
        if fcode == 0:
            return w, 100, foff
        if fcode == 1:
            return w, struct.unpack_from("<H", block, foff)[0], foff + 2
        return w, struct.unpack_from("<I", block, foff)[0], foff + 4
    wl = block[pos + 1]
    w = block[pos + 2 : pos + 2 + wl].decode("utf-8")
    fq = struct.unpack_from("<I", block, pos + 2 + wl)[0]
    return w, fq, pos + 2 + wl + 4


def load_dict(path):
    """解析 dict.bin (v1/v2/v3),返回 [(pinyin_key, [(word, freq), ...]), ...](key 已排序)。"""
    with open(path, "rb") as f:
        blob = f.read()
    if blob[:8] != DICT_MAGIC:
        raise SystemExit(f"主索引魔数不匹配,不是 dict.bin: {path}")
    version = struct.unpack_from("<I", blob, 8)[0]
    if not DICT_MIN_VERSION <= version <= DICT_MAX_VERSION:
        raise SystemExit(f"主索引版本不兼容: {version}")
    pinyin_cnt, _total, records_off, strings_off, entries_off = struct.unpack_from(
        "<QQQQQ", blob, 12
    )
    syls = []
    if version >= 3:
        n_syls = struct.unpack_from("<H", blob, 60)[0]
        cur = 62
        for _ in range(n_syls):
            slen = blob[cur]
            cur += 1
            syls.append(blob[cur : cur + slen].decode("ascii"))
            cur += slen
    groups = []
    for i in range(pinyin_cnt):
        if version >= 3:
            str_off, ent_off, next_so, next_eo = struct.unpack_from("<IIII", blob, records_off + i * 8)
            str_len, ent_len = next_so - str_off, next_eo - ent_off
            raw_k = blob[strings_off + str_off : strings_off + str_off + str_len]
            kp, kparts = 0, []
            while kp < len(raw_k):
                b = raw_k[kp]
                kp += 1
                if b < 254:
                    sid = b
                else:
                    sid = 254 + ((b - 254) << 8) + raw_k[kp]
                    kp += 1
                kparts.append(syls[sid])
            pinyin = " ".join(kparts)
        else:
            str_off, str_len, ent_off, ent_len = struct.unpack_from(
                "<IHIH", blob, records_off + i * DICT_RECORD_LEN
            )
            pinyin = blob[strings_off + str_off: strings_off + str_off + str_len].decode("utf-8")
        block = blob[entries_off + ent_off: entries_off + ent_off + ent_len]
        words = []
        pos = 0
        while pos < len(block):
            if version >= 3:
                w, fq, pos = _decode_dict_v3_entry(block, pos)
            else:
                wl = block[pos]
                w = block[pos + 1: pos + 1 + wl].decode("utf-8")
                fq = struct.unpack_from("<I", block, pos + 1 + wl)[0]
                pos += 1 + wl + 4
            words.append((w, fq))
        groups.append((pinyin, words))
    return groups


def initial_key(pinyin):
    """拼音 key → 首字母串:`"zhong guo ren"` → `"zgr"`(取每音节首字符)。"""
    return "".join(syl[0] for syl in pinyin.split(" ") if syl)


def _validate(blob):
    """写出前后自检：头部自洽、记录数组在范围内、key 严格升序（Q4）。"""
    magic, version, n_keys, _n_assoc, rec_off, str_off, ent_off = struct.unpack_from(
        "<8sIQQQQQ", blob, 0
    )
    if magic != MAGIC or version != VERSION:
        raise SystemExit("产物魔数/版本不符")
    if not (HEADER_LEN <= rec_off <= str_off <= ent_off <= len(blob)):
        raise SystemExit(
            f"区段顺序/范围不合法: rec={rec_off} str={str_off} ent={ent_off} len={len(blob)}"
        )
    if rec_off + (n_keys + 1) * RECORD_LEN > str_off:
        raise SystemExit("记录数组越界")
    prev = None
    for i in range(n_keys):
        o, eo, next_o, next_eo = struct.unpack_from("<IIII", blob, rec_off + i * RECORD_LEN)
        if next_o < o or next_eo < eo or str_off + next_o > ent_off or ent_off + next_eo > len(blob):
            raise SystemExit(f"第 {i} 条记录越界")
        key = blob[str_off + o: str_off + next_o]
        if prev is not None and key <= prev:
            raise SystemExit(f"key 未严格升序: {prev!r} → {key!r}")
        prev = key


def _write_atomically(src, dst, blob):
    """校验后**原子替换**。

    直接覆盖目的文件有两个后果：构建失败或中途被杀会留下**半文件**，而正在用
    这张表的进程（mmap）会读到残缺数据；在线重建时也会先截断旧文件。
    所以：拒绝「输入=输出」，先写同目录临时文件、自检通过再 `os.replace`。
    """
    if os.path.abspath(src) == os.path.abspath(dst):
        raise SystemExit(f"输入与输出是同一个文件: {dst}（会先截断源文件，拒绝）")
    _validate(blob)
    tmp = dst + ".tmp"
    with open(tmp, "wb") as f:
        f.write(blob)
    os.replace(tmp, dst)


def main() -> None:
    src = sys.argv[1] if len(sys.argv) > 1 else "data/dict.bin"
    dst = sys.argv[2] if len(sys.argv) > 2 else "data/abbrev.bin"

    print(f"读取主索引: {src}")
    groups = load_dict(src)
    n_entries = sum(len(ws) for _, ws in groups)
    print(f"拼音 key 数: {len(groups)}  词条数: {n_entries}")

    # 首字母 key → [(word, pinyin, freq)];同一词条只进一个首字母 key,
    # 因此关联总数恒等于词条数(spike 已实测 1.00×,无组合爆炸)。
    abbrev = {}
    for pinyin, words in groups:
        ik = initial_key(pinyin)
        if not ik:
            continue
        bucket = abbrev.setdefault(ik, [])
        for word, freq in words:
            bucket.append((word, pinyin, freq))

    keys = sorted(abbrev)  # 字节序排序 → 运行时二分查找
    n_assoc = sum(len(abbrev[k]) for k in keys)
    print(f"唯一首字母 key 数: {len(keys)}  关联数: {n_assoc} "
          f"(词条的 {n_assoc / n_entries:.2f}x)")

    # 按首字母建 26 个音节桶(每桶 ≤37 项,1 字节下标寻址,省掉 30.6MB 原始全拼串)。
    by_init = [set() for _ in range(26)]
    for pinyin, _ in groups:
        for syl in pinyin.split(" "):
            if syl:
                by_init[ord(syl[0]) - ord("a")].add(syl)
    buckets = [sorted(s) for s in by_init]
    syl_id = [{s: i for i, s in enumerate(b)} for b in buckets]
    syl_table = bytearray()
    for b in buckets:
        if len(b) > 255:
            raise SystemExit(f"单字母音节桶超 255: {len(b)}")
        syl_table.append(len(b))
        for s in b:
            sb = s.encode("ascii")
            syl_table.append(len(sb))
            syl_table.extend(sb)

    strings = bytearray()
    entries = bytearray()
    records = bytearray()
    for ik in keys:
        records.extend(struct.pack("<II", len(strings), len(entries)))
        strings.extend(ik.encode("ascii"))
        sl = len(ik)
        syl_bytes = (sl * 6 + 7) // 8
        for word, pinyin, freq in sorted(abbrev[ik], key=lambda x: -x[2]):
            fq = min(freq, FREQ_MAX)
            sids = [syl_id[ord(syl[0]) - ord("a")][syl] for syl in pinyin.split(" ") if syl]
            packed = bytearray(syl_bytes)
            for k, sid_val in enumerate(sids):
                bit_off = k * 6
                b_idx = bit_off >> 3
                shift = bit_off & 7
                v = (sid_val & 0x3F) << shift
                packed[b_idx] |= v & 0xFF
                if b_idx + 1 < syl_bytes:
                    packed[b_idx + 1] |= (v >> 8) & 0xFF
            if 1 <= len(word) <= 63 and all(0x4E00 <= ord(c) <= 0x9FFF for c in word):
                fcode = 0 if fq == 100 else (1 if fq <= 0xFFFF else 2)
                entries.append((fcode << 6) | len(word))
                for c in word:
                    entries.extend(struct.pack("<H", ord(c) - 0x4E00))
                entries.extend(packed)
                if fcode == 1:
                    entries.extend(struct.pack("<H", fq))
                elif fcode == 2:
                    entries.extend(struct.pack("<I", fq))
            else:
                wb = word.encode("utf-8")
                entries.extend([0xC0, len(wb)])
                entries.extend(wb)
                entries.extend(packed)
                entries.extend(struct.pack("<I", fq))
    records.extend(struct.pack("<II", len(strings), len(entries)))
    records_off = HEADER_LEN + len(syl_table)
    strings_off = records_off + len(records)
    entries_off = strings_off + len(strings)
    header = struct.pack(
        "<8sIQQQQQ",
        MAGIC, VERSION, len(keys), n_assoc,
        records_off, strings_off, entries_off,
    )
    assert len(header) == HEADER_LEN, "头部布局与 Rust 端不一致"

    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    _write_atomically(src, dst, header + syl_table + records + strings + entries)
    size = os.path.getsize(dst)
    main_size = os.path.getsize(src)
    print(f"已写出: {dst}({size / 1024 / 1024:.1f} MB,为主索引的 {size / main_size:.2f}x)")


if __name__ == "__main__":
    main()
