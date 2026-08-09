#!/usr/bin/env python3
"""简拼索引路线 spike:解析 dict.bin,实测路线 (a) 膨胀与路线 (c) 副索引。

只做测量,不是实现。格式定义以 crates/xwx_engine/src/dict.rs 为准(v1,小端)。
"""
import bisect
import mmap
import os
import struct
import sys
import time
from collections import defaultdict

DICT = "/home/youngwen/Desktop/xwx_ime/data/dict.bin"
OUT_DIR = os.path.dirname(os.path.abspath(__file__))


def p(*a, **kw):
    print(*a, **kw, flush=True)


def load_dict(path):
    """解析 dict.bin,返回 (keys, groups, key_bytes)。"""
    with open(path, "rb") as f:
        mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    assert mm[:8] == b"XWXIDM1\0"
    ver = struct.unpack_from("<I", mm, 8)[0]
    assert ver == 1
    pinyin_cnt, total_entries, records_off, strings_off, entries_off = struct.unpack_from(
        "<QQQQQ", mm, 12
    )
    p(f"[load] pinyin_keys={pinyin_cnt} total_entries={total_entries}")
    keys = []
    key_bytes = []
    groups = []
    for i in range(pinyin_cnt):
        str_off, str_len, ent_off, ent_len = struct.unpack_from(
            "<IHIH", mm, records_off + i * 12
        )
        kb = bytes(mm[strings_off + str_off: strings_off + str_off + str_len])
        py = kb.decode("utf-8")
        block = mm[entries_off + ent_off: entries_off + ent_off + ent_len]
        words = []
        pos = 0
        bl = len(block)
        while pos < bl:
            wl = block[pos]
            w = block[pos + 1: pos + 1 + wl].decode("utf-8")
            fq = struct.unpack_from("<I", block, pos + 1 + wl)[0]
            words.append((w, fq))
            pos += 1 + wl + 4
        keys.append(py)
        key_bytes.append(kb)
        groups.append(words)
    mm.close()
    return keys, key_bytes, groups


def initial_variants(syl):
    if len(syl) >= 2 and syl[:2] in ("zh", "ch", "sh"):
        return (syl[0], syl[:2])
    return (syl[0],)


def main():
    file_size = os.path.getsize(DICT)
    p(f"[load] dict.bin = {file_size} bytes ({file_size/1e6:.1f} MB)")
    t0 = time.perf_counter()
    keys, key_bytes, groups = load_dict(DICT)
    t1 = time.perf_counter()
    p(f"[load] 解析耗时 {t1-t0:.2f}s,keys={len(keys)}")

    n_entries = sum(len(g) for g in groups)
    p(f"[load] 词条总数 = {n_entries}")

    # ---------- 路线 (a) ----------
    p("\n========== 路线 (a):构建期写入全部简码组合 ==========")
    t2 = time.perf_counter()
    # 统计每个 key 的简码组合数
    combo_hist = defaultdict(int)
    zhchsh_keys = 0
    total_refs = 0           # 所有 key 的简码组合数总和(去重前)
    abbrev_seen = set()      # 唯一简码 key
    strings_bytes_main = 0
    for py, kb in zip(keys, key_bytes):
        strings_bytes_main += len(kb)
        syls = py.split(" ")
        per = [initial_variants(s) for s in syls]
        n_combo = 1
        for v in per:
            n_combo *= len(v)
        combo_hist[n_combo] += 1
        if n_combo > 1:
            zhchsh_keys += 1
        total_refs += n_combo
        # 枚举所有组合加入唯一集合
        combos = [""]
        for v in per:
            combos = [c + x for c in combos for x in v]
        for c in combos:
            abbrev_seen.add(c)
    t3 = time.perf_counter()
    n_merged_keys = len(abbrev_seen)
    p(f"[a] 枚举耗时 {t3-t2:.2f}s")
    p(f"[a] 主 key 数 = {len(keys)},含 zh/ch/sh 的 key 数 = {zhchsh_keys}")
    p(f"[a] 组合数分布(组合数→key 数): {dict(sorted(combo_hist.items()))}")
    p(f"[a] 简码引用总数(= 词条-简码 关联数,去重前) = {total_refs}")
    p(f"[a] 归并后唯一简码 key 数(含主 key) = {n_merged_keys} "
      f"(索引条数膨胀 {n_merged_keys/len(keys):.2f}x)")
    # 体积估算
    avg_entry_bytes = sum(1 + len(w.encode("utf-8")) + 4 for g in groups for (w, _) in g) / n_entries
    strings_bytes_merged = sum(len(c.encode("utf-8")) for c in abbrev_seen)
    # a1: 词条区存 u32 指针
    size_a1 = 52 + n_merged_keys * 12 + strings_bytes_merged + total_refs * 4
    # a2: 词条区复制词条(与主区同构)
    size_a2 = 52 + n_merged_keys * 12 + strings_bytes_merged + int(total_refs * avg_entry_bytes)
    p(f"[a] 词条平均字节数 = {avg_entry_bytes:.1f}")
    p(f"[a] 当前 dict.bin = {file_size/1e6:.1f} MB")
    p(f"[a] 方案 a1(指针式)估算 = {size_a1/1e6:.1f} MB ({size_a1/file_size:.2f}x)")
    p(f"[a] 方案 a2(复制词条)估算 = {size_a2/1e6:.1f} MB ({size_a2/file_size:.2f}x)")

    # ---------- 路线 (c) ----------
    p("\n========== 路线 (c):首字母串 -> 词条 副索引 ==========")
    t4 = time.perf_counter()
    abbrev_map = defaultdict(list)
    for py, g in zip(keys, groups):
        ik = "".join(s[0] for s in py.split(" "))
        for (w, fq) in g:
            abbrev_map[ik].append((w, py, fq))
    t5 = time.perf_counter()
    total_assoc = sum(len(v) for v in abbrev_map.values())
    p(f"[c] 建 map 耗时 {t5-t4:.2f}s,唯一首字母 key 数 = {len(abbrev_map)}")
    p(f"[c] 关联总数 = {total_assoc} (词条数 {n_entries} 的 {total_assoc/n_entries:.2f}x)")

    out_bin = os.path.join(OUT_DIR, "abbrev_index.bin")
    t6 = time.perf_counter()
    abbrev_keys_sorted = sorted(abbrev_map.keys())
    strings = bytearray()
    entries = bytearray()
    records = bytearray()
    for ik in abbrev_keys_sorted:
        items = sorted(abbrev_map[ik], key=lambda x: -x[2])
        str_off = len(strings)
        strings.extend(ik.encode("ascii"))
        ent_off = len(entries)
        ent_start = len(entries)
        for (w, py, fq) in items:
            wb = w.encode("utf-8")
            pb = py.encode("utf-8")
            entries.append(len(wb))
            entries.extend(wb)
            entries.append(len(pb))
            entries.extend(pb)
            entries.extend(struct.pack("<I", fq))
        records.extend(struct.pack("<IHIH", str_off, len(ik),
                                   ent_off, len(entries) - ent_start))
    header = b"XWXABR1\0" + struct.pack("<I", 1) + struct.pack(
        "<QQQQQ",
        len(abbrev_keys_sorted), total_assoc,
        52, 52 + len(records), 52 + len(records) + len(strings),
    )
    with open(out_bin, "wb") as f:
        f.write(header); f.write(records); f.write(strings); f.write(entries)
    t7 = time.perf_counter()
    ab_size = os.path.getsize(out_bin)
    p(f"[c] 副索引写盘耗时 {t7-t6:.2f}s")
    p(f"[c] 副索引体积 = {ab_size} bytes ({ab_size/1e6:.1f} MB),"
      f"为主索引的 {ab_size/file_size:.2f}x")
    p(f"[c] 总构建耗时(map+写盘) {t7-t4:.2f}s")

    # 释放 abbrev_map,后面用 mmap 查
    del abbrev_map

    # ---------- 路线 (c) 查询 ----------
    p("\n========== 路线 (c) 查询延迟与召回 ==========")
    with open(out_bin, "rb") as f:
        mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    ab_pinyin_cnt = struct.unpack_from("<Q", mm, 12)[0]
    ab_records_off = struct.unpack_from("<Q", mm, 28)[0]
    ab_strings_off = struct.unpack_from("<Q", mm, 36)[0]
    ab_entries_off = struct.unpack_from("<Q", mm, 44)[0]

    def ab_key(i):
        so, sl, eo, el = struct.unpack_from("<IHIH", mm, ab_records_off + i * 12)
        return mm[ab_strings_off + so: ab_strings_off + so + sl], (eo, el)

    def ab_lower_bound(prefix: bytes):
        lo, hi = 0, ab_pinyin_cnt
        while lo < hi:
            mid = (lo + hi) // 2
            k, _ = ab_key(mid)
            if k < prefix:
                lo = mid + 1
            else:
                hi = mid
        return lo

    def ab_lookup_prefix(prefix: str):
        pb = prefix.encode("ascii")
        start = ab_lower_bound(pb)
        out = []
        i = start
        while i < ab_pinyin_cnt:
            k, (eo, el) = ab_key(i)
            if not k.startswith(pb):
                break
            block = mm[ab_entries_off + eo: ab_entries_off + eo + el]
            pos = 0
            bl = len(block)
            while pos < bl:
                wl = block[pos]
                w = block[pos + 1: pos + 1 + wl].decode("utf-8")
                pl = block[pos + 1 + wl]
                ppy = block[pos + 2 + wl: pos + 2 + wl + pl].decode("utf-8")
                fq = struct.unpack_from("<I", block, pos + 2 + wl + pl)[0]
                out.append((w, ppy, fq))
                pos += 2 + wl + pl + 4
            i += 1
        return out

    def bench(fn, *args, repeat=50):
        fn(*args)
        t0 = time.perf_counter()
        for _ in range(repeat):
            fn(*args)
        t1 = time.perf_counter()
        return (t1 - t0) / repeat * 1e3

    test_inputs = ["zgr", "bjdx", "xx", "nh"]
    results = {}
    for q in test_inputs:
        lat = bench(ab_lookup_prefix, q)
        res = ab_lookup_prefix(q)
        seen = {}
        for (w, ppy, fq) in res:
            cur = seen.get(w)
            if cur is None or fq > cur[1]:
                seen[w] = (ppy, fq)
        dedup = sorted(seen.items(), key=lambda kv: -kv[1][1])
        results[q] = (lat, len(res), dedup)
        p(f"[c] 查询 {q!r}: 延迟 {lat:.3f} ms,原始 {len(res)} 条,去重 {len(dedup)} 词")
        p(f"    top-10: {[w for w, _ in dedup[:10]]}")

    # ---------- 混合简拼 ----------
    p("\n========== 路线 (c) 混合简拼 ==========")
    # 预建 key->idx 与按字节二分的列表
    key_idx = {k: i for i, k in enumerate(keys)}

    def main_prefix_keys(prefix):
        pb = prefix.encode("utf-8")
        lo = bisect.bisect_left(key_bytes, pb)
        out = []
        i = lo
        n = len(key_bytes)
        while i < n and key_bytes[i].startswith(pb):
            out.append(i)
            i += 1
        return out

    def mixed_lookup(s):
        """「精确前缀 + 首字母过滤」拼接。

        对每个切分点 i: prefix=s[:i] 作为主索引前缀,tail=s[i:] 作为剩余
        音节首字母序列。要求主索引命中的 key 形如 prefix + " " + rest,
        且 rest 各音节首字母前缀匹配 tail。
        """
        best = {}
        scanned = 0
        for i in range(1, len(s) + 1):
            prefix = s[:i]
            tail = s[i:]
            for ki in main_prefix_keys(prefix):
                scanned += 1
                k = keys[ki]
                if k == prefix:
                    if not tail:
                        for (w, fq) in groups[ki]:
                            cur = best.get((w, k))
                            if cur is None or fq > cur:
                                best[(w, k)] = fq
                    continue
                if not k.startswith(prefix + " "):
                    continue
                if not tail:
                    continue
                rest = k[len(prefix) + 1:]
                syls = rest.split(" ")
                initials = "".join(x[0] for x in syls)
                if len(tail) <= len(syls) and initials.startswith(tail):
                    for (w, fq) in groups[ki]:
                        cur = best.get((w, k))
                        if cur is None or fq > cur:
                            best[(w, k)] = fq
        out = sorted(best.items(), key=lambda kv: -kv[1])
        return out, scanned

    mixed = ["zhongg", "nih", "zhongguor"]
    for q in mixed:
        t8 = time.perf_counter()
        res, scanned = mixed_lookup(q)
        t9 = time.perf_counter()
        p(f"[c-mix] {q!r}: 延迟 {(t9-t8)*1e3:.2f} ms,扫描 key {scanned} 个,"
          f"命中 {len(res)} 条")
        p(f"    top: {[(w, pp, fq) for ((w, pp), fq) in res[:10]]}")

    import json
    dump = {}
    for q in test_inputs:
        lat, raw, dedup = results[q]
        dump[q] = {
            "latency_ms": lat,
            "raw_hits": raw,
            "dedup_count": len(dedup),
            "top10": [w for w, _ in dedup[:10]],
        }
    with open(os.path.join(OUT_DIR, "spike_results.json"), "w", encoding="utf-8") as f:
        json.dump(dump, f, ensure_ascii=False, indent=2)
    mm.close()
    p("\n[done]")


if __name__ == "__main__":
    main()
