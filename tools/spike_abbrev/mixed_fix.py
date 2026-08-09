#!/usr/bin/env python3
"""混合简拼修补版:验证路线 (c) 在「先切音节、再逐位匹配」下能否 work。

路线 (c) 原始拼接方案失败的根因:主索引 key 带空格("zhong guo ren"),
用户输入 "zhongguor" 不带空格,前缀二分命不中任何 key。

修补思路:不切用户输入,改为
  1. 从主索引扫出全部合法单音节(拼音的不重复首段),作为音节表
  2. 把用户输入做贪心最长音节切分,得到「完整音节段 + 末尾可能的声母残余」
  3. 对每个切分结果,用「完整音节 + 残余声母」去主索引/副索引联合查

这只验证 zhongg / nih / zhongguor 三个输入能不能出正确候选。
"""
import bisect
import mmap
import struct
import time

DICT = "/home/youngwen/Desktop/xwx_ime/data/dict.bin"


def main():
    with open(DICT, "rb") as f:
        mm = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    pinyin_cnt, total_entries, records_off, strings_off, entries_off = struct.unpack_from(
        "<QQQQQ", mm, 12
    )

    # 扫出全部 key 的字节形式,并构建「合法音节表」
    keys_bytes = []
    syllables = set()
    for i in range(pinyin_cnt):
        so, sl, eo, el = struct.unpack_from("<IHIH", mm, records_off + i * 12)
        kb = bytes(mm[strings_off + so: strings_off + so + sl])
        keys_bytes.append(kb)
        for syl in kb.split(b" "):
            syllables.add(syl)
    print(f"[mix-fix] 合法音节数 = {len(syllables)}")
    # 按长度降序,贪心匹配用
    syl_list = sorted(syllables, key=lambda s: -len(s))

    def load_group(idx):
        so, sl, eo, el = struct.unpack_from("<IHIH", mm, records_off + idx * 12)
        block = mm[entries_off + eo: entries_off + eo + el]
        out = []
        pos = 0
        while pos < len(block):
            wl = block[pos]
            w = block[pos + 1: pos + 1 + wl].decode("utf-8")
            fq = struct.unpack_from("<I", block, pos + 1 + wl)[0]
            out.append((w, fq))
            pos += 1 + wl + 4
        return out

    def segment_input(s):
        """把输入 s 切成 ([完整音节...], 残余声母串)。

        贪心:在每个位置找最长合法音节。返回所有可行切分(至多枚举前几种)。
        """
        results = []

        def rec(pos, syls):
            if pos == len(s):
                results.append((list(syls), ""))
                return
            # 尝试最长音节
            matched_any = False
            for syl in syl_list:
                if s.startswith(syl.decode(), pos):
                    # 剪枝:最多枚举 8 种
                    if len(results) >= 8:
                        return
                    syls.append(syl.decode())
                    rec(pos + len(syl), syls)
                    syls.pop()
                    matched_any = True
                    # 贪心:找到第一个(最长)就停?不行,因为可能错切。
                    # 但枚举全部又爆炸。限制:只在音节长度 2..6 之间枚举。
            # 如果当前位置不能成音节,把剩余全部当声母残余
            if not matched_any:
                results.append((list(syls), s[pos:]))

        rec(0, [])
        # 额外:最后一段如果是「完整音节 + 多余声母」,rec 里不会自动产生
        # 例如 zhongguor:rec 会切成 [zhong, guo] + 'r' 作为「not matched_any」分支
        return results

    def lookup_mixed(s, topn=10):
        segs = segment_input(s)
        print(f"  切分结果: {segs}")
        best = {}
        for (syls, tail) in segs:
            if not syls:
                continue
            # 主索引前缀:完整音节用空格拼接
            prefix = " ".join(syls)
            pb = prefix.encode("utf-8")
            lo = bisect.bisect_left(keys_bytes, pb)
            i = lo
            while i < len(keys_bytes) and keys_bytes[i].startswith(pb):
                kb = keys_bytes[i]
                # 要求 kb == prefix 或 kb 以 prefix + ' ' 开头
                if kb == pb:
                    if not tail:
                        for (w, fq) in load_group(i):
                            cur = best.get((w, kb.decode()))
                            if cur is None or fq > cur:
                                best[(w, kb.decode())] = fq
                    i += 1
                    continue
                if not kb.startswith(pb + b" "):
                    i += 1
                    continue
                if not tail:
                    i += 1
                    continue
                rest = kb[len(pb) + 1:].decode("utf-8")
                rest_syls = rest.split(" ")
                initials = "".join(x[0] for x in rest_syls)
                if len(tail) <= len(rest_syls) and initials.startswith(tail):
                    for (w, fq) in load_group(i):
                        cur = best.get((w, kb.decode()))
                        if cur is None or fq > cur:
                            best[(w, kb.decode())] = fq
                i += 1
        out = sorted(best.items(), key=lambda kv: -kv[1])
        return out[:topn]

    for q in ["zhongg", "nih", "zhongguor"]:
        t0 = time.perf_counter()
        res = lookup_mixed(q)
        t1 = time.perf_counter()
        print(f"[mix-fix] {q!r}: 延迟 {(t1-t0)*1e3:.2f} ms, top-{len(res)}:")
        for ((w, p), fq) in res:
            print(f"    {w}  ({p})  freq={fq}")
        print()


if __name__ == "__main__":
    main()
