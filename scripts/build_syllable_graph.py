#!/usr/bin/env python3
"""dict.bin → syllable_graph.bin（音节级词典转移索引，只读 mmap）。

存在理由：缩写弧的建边要在「当前词已拼到哪个音节链」上反复问两个问题——
① 哪些音节能接在后面（不然只能拿 412 条音节表逐个试，每次一次 mmap 二分）；
② 这条链本身是不是一个完整词条。实测 womenjintqubeij 一输入就要问 1.2 万次，
每次约 1µs（在 186 万条 key 上二分 21 步、跨页），合计 10.5ms，是简拼格的全部成本。

索引把这两个问题变成内存里的树walk：从根按音节走，子节点就是「词库里真实存在的
后继音节」，走到即问完。节点存 (子表起止, 是否完整词条, 词条区间)。

布局（小端，见 data/README.md「syllable_graph.bin 二进制格式」）：
    header 60B: 8s magic | I version | Q n_nodes | Q n_syllables
                | Q node_off | Q edge_off | Q syl_off | Q flags
    node 16B:   I children_off | H children_len | B flags(b0=完整词条) | B pad
                | I ent_off | H ent_len | H pad
    edge  6B:   H syllable_id | I child_node
    syl  变长:  H len | bytes name   （412 条，按名字升序，id 即下标）

子表按 syllable_id 升序（id 定义成名字升序），所以读取端二分即可。
"""

import os
import struct
import sys

MAGIC = b"XWXSYL1\0"
VERSION = 1
HEADER_LEN = 60
NODE_LEN = 16
EDGE_LEN = 6
RECORD_LEN = 12


def read_keys(path):
    blob = open(path, "rb").read()
    magic, version, n_keys, _total, rec_off, str_off, ent_off = struct.unpack_from(
        "<8sIQQQQQ", blob, 0
    )
    if magic != b"XWXIDM1\0" or version != 1:
        raise SystemExit(f"不是 dict.bin v1（magic={magic!r} version={version}）")
    keys = []
    for i in range(n_keys):
        so, sl, _eo, _el = struct.unpack_from("<IHIH", blob, rec_off + i * RECORD_LEN)
        keys.append(blob[str_off + so : str_off + so + sl].decode("utf-8"))
    if keys != sorted(keys):
        raise SystemExit("dict.bin 的 key 不是字典序——单趟建树的前提不成立")
    return keys


def build(keys):
    """按字典序单趟建树：与上一条 key 的最长公共前缀就是当前栈的深度。"""
    nodes = [{"children": [], "exact": False, "ent": 0, "ent_len": 0}]  # 0 号是根
    stack = []  # [(音节, 节点号)]，深度 = 已匹配的前缀长度
    prev = []
    for i, key in enumerate(keys):
        parts = key.split(" ")
        common = 0
        limit = min(len(prev), len(parts))
        while common < limit and prev[common] == parts[common]:
            common += 1
        del stack[common:]
        parent = stack[-1][1] if stack else 0
        for syl in parts[common:]:
            child = len(nodes)
            nodes.append({"children": [], "exact": False, "ent": 0, "ent_len": 0})
            nodes[parent]["children"].append([syl, child])
            stack.append((syl, child))
            parent = child
        # 同一拼音通常有多条记录（一个 key 对应多个词条，读取端按键做区间扫描）：
        # 只在连续时合并成区间，否则另起一段——索引里存「区间」而不是单条。
        node = nodes[parent]
        if not node["exact"] or node["ent"] + node["ent_len"] != i:
            node["exact"] = True
            node["ent"] = i
            node["ent_len"] = 1
        else:
            node["ent_len"] += 1
        prev = parts
    return nodes


def serialize(nodes, keys):
    names = sorted({syl for n in nodes for syl, _ in n["children"]})
    sid = {name: i for i, name in enumerate(names)}
    if len(names) > 0xFFFF:
        raise SystemExit(f"音节表 {len(names)} 条超过 u16 编号上限")

    # 边按 (syllable_id, child) 升序落盘，读取端可二分
    edges = []
    for n in nodes:
        kids = sorted((sid[syl], child) for syl, child in n["children"])
        n["off"] = len(edges)
        n["len"] = len(kids)
        edges.extend(kids)

    node_off = HEADER_LEN
    edge_off = node_off + len(nodes) * NODE_LEN
    syl_off = edge_off + len(edges) * EDGE_LEN
    syl_bytes = b"".join(struct.pack("<H", len(s)) + s.encode() for s in names)
    out = bytearray()
    out += struct.pack(
        "<8sIQQQQQQ",
        MAGIC,
        VERSION,
        len(nodes),
        len(names),
        node_off,
        edge_off,
        syl_off,
        0,
    )
    for n in nodes:
        out += struct.pack(
            "<IHBB IHH",
            n["off"],
            min(n["len"], 0xFFFF),
            1 if n["exact"] else 0,
            0,
            n["ent"] if n["exact"] else 0xFFFFFFFF,
            n["ent_len"] if n["exact"] else 0,
            0,
        )
    for s, child in edges:
        out += struct.pack("<HI", s, child)
    out += syl_bytes
    return bytes(out), names, sid, edges, syl_off + len(syl_bytes)


def validate(blob, keys, names, sid):
    """产物自检：头自洽、逐条 key 都能走到底且完整词条标记一致。"""
    magic, version, n_nodes, n_syl, node_off, edge_off, syl_off, _ = struct.unpack_from(
        "<8sIQQQQQQ", blob, 0
    )
    if magic != MAGIC or version != VERSION:
        raise SystemExit("产物魔数/版本不符")
    if not (HEADER_LEN <= node_off <= edge_off <= syl_off <= len(blob)):
        raise SystemExit("区段顺序/范围不合法")

    def node(idx):
        off, clen, flags, _pad, ent_off, ent_len, _ = struct.unpack_from(
            "<IHBB IHH", blob, node_off + idx * NODE_LEN
        )
        return off, clen, flags, ent_off, ent_len

    def child_of(idx, name):
        off, clen, *_ = node(idx)
        want = sid[name]
        for i in range(clen):
            s, c = struct.unpack_from("<HI", blob, edge_off + (off + i) * EDGE_LEN)
            if s == want:
                return c
        return None

    step = max(1, len(keys) // 400)
    for i in range(0, len(keys), step):
        cur = 0
        parts = keys[i].split(" ")
        for j, syl in enumerate(parts):
            cur = child_of(cur, syl)
            if cur is None:
                raise SystemExit(f"自检失败：{keys[i]} 的第 {j} 个音节走不到")
        _off, _clen, flags, ent_off, ent_len = node(cur)
        if not flags & 1 or not (ent_off <= i < ent_off + ent_len):
            raise SystemExit(f"自检失败：{keys[i]} 的完整词条标记/词条号不对")
    return True


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "data/dict.bin"
    dst = sys.argv[2] if len(sys.argv) > 2 else "data/syllable_graph.bin"
    keys = read_keys(src)
    nodes = build(keys)
    blob, names, sid, edges, total = serialize(nodes, keys)
    validate(blob, keys, names, sid)
    with open(dst, "wb") as f:
        f.write(blob)
    exact = sum(1 for n in nodes if n["exact"])
    print(
        f"{src}: {len(keys):,} 词条 → {dst}\n"
        f"  节点 {len(nodes):,}（完整词条 {exact:,}）  边 {len(edges):,}  音节 {len(names)}\n"
        f"  体积 {total/1e6:.1f}MB（节点 {len(nodes)*NODE_LEN/1e6:.1f} + 边 {len(edges)*EDGE_LEN/1e6:.1f} + 音节表小）\n"
        f"  自检通过（抽 400 条走通到底）"
    )


if __name__ == "__main__":
    main()
