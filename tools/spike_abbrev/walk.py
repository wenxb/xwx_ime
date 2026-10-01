#!/usr/bin/env python3
"""简拼 trie-walk 原型:验证「全拼音节与首字母任意交错」的召回可行性。

**为什么需要这个 spike**:现有 `lookup_mixed` 的形状是「完整音节前缀 + 尾部
残余声母」,结构上只支持「缩写全在末尾」。用户真实输入是任意交错的——
`feicde` = fei + c(hang) + de、`shouzm` = shou + z(i) + m(u)、
`nhao` = n(i) + hao(缩写在**开头**)。这些在现有实现下召回为 0 或近 0。

本原型直接在主索引(dict.bin,key 形如 `"bei jing da xue"`)上做 DFS:
每一步或消耗一个**字面音节**(全拼),或消耗 1 个首字母(zh/ch/sh 额外试
2 字符),后者要在该位置**枚举所有可能的音节补全**再往下走。

输出用于量化两件事:召回是否覆盖真实输入形状、二分次数是否在预算内。
排序在这里只是粗排(先按缩写位数,再按词频),**不是**最终排序方案。

跑法:python3 tools/spike_abbrev/walk.py [dict.bin]
"""
import mmap
import re
import struct
import sys
import time

DICT = sys.argv[1] if len(sys.argv) > 1 else 'data/dict.bin'

f = open(DICT, 'rb')
m = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
CNT, TOT, ROFF, SOFF, EOFF = struct.unpack_from('<QQQQQ', m, 12)

sys.setrecursionlimit(10000)

MAX_RESULTS = 500
BUDGET = 20000   # 二分次数上限,防某些输入把分支炸开

NB = 0           # 本次查询的二分次数(代价度量)


def rec(i):
    return struct.unpack_from('<IHIH', m, ROFF + i * 12)


def key(i):
    so, sl, _, _ = rec(i)
    return m[SOFF + so:SOFF + so + sl]


def lower(p):
    """第一条 >= p 的记录下标(主索引按 key 字节序排好)。"""
    global NB
    NB += 1
    lo, hi = 0, CNT
    while lo < hi:
        mid = (lo + hi) // 2
        if key(mid) < p:
            lo = mid + 1
        else:
            hi = mid
    return lo


def entries(i):
    _, _, eo, el = rec(i)
    b = m[EOFF + eo:EOFF + eo + el]
    out, pos = [], 0
    while pos < len(b):
        wl = b[pos]
        w = b[pos + 1:pos + 1 + wl].decode()
        fr = struct.unpack_from('<I', b, pos + 1 + wl)[0]
        out.append((w, fr))
        pos += 1 + wl + 4
    return out


def load_syllables(path):
    """从 segmenter.rs 抽 SYLLABLES 静态表(410 项)。

    必须**先逐行剥掉 `//` 注释再抓引号**:表里每个声母分组前有一行 `// d`
    这样的注释,若先把整块换行压成空格,注释文本会把紧随其后的第一个音节
    粘住,于是 a/ba/ca/da/ji/... 每组首个音节全部丢失(字面分支形同虚设,
    `bjdaxue` 召回为 0)。这个 bug 真的发生过。
    """
    src = open(path).read()
    blk = src.split('pub const SYLLABLES: &[&str] = &[')[1].split('];')[0]
    clean = '\n'.join(ln.split('//')[0] for ln in blk.split('\n'))
    return set(re.findall(r'"([a-z]+)"', clean))


SYL = load_syllables('crates/xwx_engine/src/segmenter.rs')
assert len(SYL) == 410, f'音节表解析出 {len(SYL)} 项,期望 410'
assert {'a', 'ba', 'da', 'ji'} <= SYL, '注释行后的首个音节又被吃掉了'
BY_LEN = sorted(SYL, key=len, reverse=True)


def search(inp, allow_zcsh=True):
    """返回 ([(word, key, freq, n_abbrev)], 二分次数)。"""
    global NB
    NB = 0
    out, seen = [], set()

    def distinct_tokens(prefix):
        """枚举 [prefix*) 区间内**该音节位**的所有不同取值,靠二分跳转。

        跳转用 `prefix + t + b'!'`(0x21):它 > 空格、< 任何小写字母,
        于是既跳过 `t` 自身与 `t ...`,又保留 `ta`/`tan` 这类**更长的
        同前缀音节**。用 b'\\xff' 会把后者一并跳掉。
        """
        toks = []
        i = lower(prefix)
        while i < CNT and NB < BUDGET:
            k = key(i)
            if not k.startswith(prefix):
                break
            t = k[len(prefix):].split(b' ')[0]
            toks.append(t)
            i = lower(prefix + t + b'!')
        return toks

    def collect(prefix, nab):
        """输入耗尽:收 [prefix*) 下的全部词条(含更长的词,即前缀联想)。"""
        i = lower(prefix)
        while i < CNT and len(out) < MAX_RESULTS:
            k = key(i)
            if not k.startswith(prefix):
                break
            for w, fr in entries(i):
                if (w, k) not in seen:
                    seen.add((w, k))
                    out.append((w, k.decode(), fr, nab))
            i += 1

    def dfs(pos, prefix, nab):
        if len(out) >= MAX_RESULTS or NB >= BUDGET:
            return
        if pos == len(inp):
            collect(prefix, nab)
            return
        rest = inp[pos:]
        # (a) 字面音节分支(全拼):剪枝——该前缀在索引里无命中就不往下走
        for s in BY_LEN:
            if rest.startswith(s):
                np = prefix + s.encode()
                i = lower(np)
                if i < CNT and key(i).startswith(np):
                    nxt = np if pos + len(s) == len(inp) else np + b' '
                    dfs(pos + len(s), nxt, nab)
        # (b) 首字母分支(简拼):1 字符,zh/ch/sh 额外试 2 字符
        cands = [rest[0]]
        if allow_zcsh and rest[:2] in ('zh', 'ch', 'sh'):
            cands.append(rest[:2])
        for c in cands:
            np = prefix + c.encode()
            if pos + len(c) == len(inp):
                dfs(pos + len(c), np, nab + 1)
            else:
                for t in distinct_tokens(np):
                    dfs(pos + len(c), np + t + b' ', nab + 1)

    dfs(0, b'', 0)
    return out, NB


CASES = [
    # 现有 lookup_mixed 覆盖不到的形状(缩写在中间/开头)
    'nhao', 'zguo', 'wmen', 'feicde', 'shouzm', 'bjdaxue', 'xxie', 'zgren',
    # 现有实现已经能做对的,用来看有没有被弄坏
    'zgr', 'zhongguor', 'renm', 'beijdx',
    # 纯全拼,必须不退化
    'beijing', 'nihao', 'zhongguo', 'womenjintianquxian',
]

if __name__ == '__main__':
    for inp in CASES:
        t0 = time.time()
        res, nb = search(inp)
        dt = (time.time() - t0) * 1000
        res.sort(key=lambda r: (r[3], -r[2]))
        top = ' '.join(w for w, k, fr, na in res[:6])
        print(f'{inp:20} n={len(res):4d} 二分={nb:6d} {dt:7.1f}ms  {top}')
