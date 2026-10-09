"""`english_bigram.bin` v1 的布局、序列化、自检与原子写入。

格式见 `docs/english-input/04-prediction.md` 第 4 节。这是 `english.bin`
（`english.pack`）的姊妹格式：同样的「固定长度头 + 定长记录 + 字符串池」
思路，但记录形状不同，所以单独成模块而不是往 `pack.py` 里塞。

    header(64) → contexts(24/条) → edges(16/条) → strings(UTF-8 池)
"""

import json
import os
import struct
from pathlib import Path

from english.tokens import BuildError

#: 魔数：`XWXENB1` 后接一个零字节。
BIGRAM_MAGIC = b"XWXENB1\x00"
BIGRAM_VERSION = 1
HEADER_LEN = 64
CONTEXT_RECORD_LEN = 24
EDGE_RECORD_LEN = 16

MAX_FILE_LEN = 256 * 1024 * 1024
MAX_CONTEXTS = 2_000_000
MAX_EDGES = 64_000_000
#: 每个 prev 保留的 Top-N（与运行时召回池上限一致）。
TOP_PER_CONTEXT = 32
MAX_KEY_LEN = 48

HEADER = struct.Struct("<8sIIIIQQQQQ")  # magic,version,header_len,ctx,edge,ctx_off,edge_off,str_off,file_len,reserved
CONTEXT = struct.Struct("<IHHIIQ")
EDGE = struct.Struct("<IHHQ")


def serialize(contexts, edges):
    """`contexts`/`edges` → 完整字节串。

    `contexts` 是 `(prev, edge_count, edge_start, observed_total)` 列表，
    `edges` 是 `(next, count)` 列表；字符串池按「先 prev 后 next」的首次出现
    顺序去重追加（与 `pack.py` 同一取舍：池里不放 NUL）。
    """
    strings = bytearray()
    offsets = {}

    def intern(text):
        if text in offsets:
            return offsets[text]
        off = len(strings)
        raw = text.encode("utf-8")
        strings.extend(raw)
        offsets[text] = off
        return off

    context_buf = bytearray()
    for prev, edge_count, edge_start, observed_total in contexts:
        prev_off = intern(prev)
        context_buf.extend(
            CONTEXT.pack(prev_off, len(prev), edge_count, edge_start, 0, observed_total)
        )

    edge_buf = bytearray()
    for nxt, count in edges:
        next_off = intern(nxt)
        edge_buf.extend(EDGE.pack(next_off, len(nxt), 0, count))

    contexts_off = HEADER_LEN
    edges_off = contexts_off + len(context_buf)
    strings_off = edges_off + len(edge_buf)
    file_len = strings_off + len(strings)

    header = HEADER.pack(
        BIGRAM_MAGIC,
        BIGRAM_VERSION,
        HEADER_LEN,
        len(contexts),
        len(edges),
        contexts_off,
        edges_off,
        strings_off,
        file_len,
        0,  # reserved
    )
    assert len(header) == HEADER_LEN, len(header)
    return bytes(header) + bytes(context_buf) + bytes(edge_buf) + bytes(strings)


def _decode_strings(blob, off, length):
    strings_off = struct.unpack_from("<Q", blob, 40)[0]
    string_len = struct.unpack_from("<Q", blob, 48)[0] - strings_off
    if off + length > string_len:
        raise BuildError(f"字符串引用越界: off={off} len={length} 池长={string_len}")
    return blob[strings_off + off : strings_off + off + length].decode("utf-8")


def verify_blob(blob):
    """从**磁盘上的字节**独立重读一遍，逐条核对结构不变量。

    这不是重复 `serialize` 的断言：它只看字节，所以能抓到序列化里的
    偏移/计数写错（例如 edge_start 差一）。
    """
    if len(blob) < HEADER_LEN:
        raise BuildError("产物比头部还短")
    magic, version, header_len, n_ctx, n_edge = struct.unpack_from("<8sIIII", blob, 0)
    if magic != BIGRAM_MAGIC:
        raise BuildError("魔数不匹配")
    if version != BIGRAM_VERSION or header_len != HEADER_LEN:
        raise BuildError("版本/头长不匹配")
    ctx_off, edge_off, strings_off, file_len = struct.unpack_from("<QQQQ", blob, 24)
    reserved = struct.unpack_from("<Q", blob, 56)[0]
    if reserved != 0:
        raise BuildError("头部 reserved 必须为 0")
    if file_len != len(blob):
        raise BuildError(f"file_len {file_len} ≠ 实际长度 {len(blob)}")
    if not (HEADER_LEN <= ctx_off <= edge_off <= strings_off <= file_len):
        raise BuildError("区段顺序不合法")
    if ctx_off != HEADER_LEN:
        raise BuildError("contexts 必须紧接头部")
    if ctx_off + n_ctx * CONTEXT_RECORD_LEN != edge_off:
        raise BuildError("contexts 区长度与 context_count 不一致")
    if edge_off + n_edge * EDGE_RECORD_LEN != strings_off:
        raise BuildError("edges 区长度与 edge_count 不一致")

    prev_keys = []
    seen_edges = 0
    for i in range(n_ctx):
        base = ctx_off + i * CONTEXT_RECORD_LEN
        prev_off, prev_len, edge_count, edge_start, rsv, observed_total = CONTEXT.unpack_from(blob, base)
        if rsv != 0:
            raise BuildError(f"context {i} 的 reserved 必须为 0")
        if not 1 <= edge_count <= TOP_PER_CONTEXT:
            raise BuildError(f"context {i} 的 edge_count {edge_count} 不在 1..={TOP_PER_CONTEXT}")
        if observed_total == 0:
            raise BuildError(f"context {i} 的 observed_total 必须 > 0")
        prev = _decode_strings(blob, prev_off, prev_len)
        if not prev.isascii() or not prev.isalpha() or not prev.islower() or len(prev) > MAX_KEY_LEN:
            raise BuildError(f"context {i} 的 prev {prev!r} 不是合法英文键")
        if prev_keys and prev_keys[-1] >= prev:
            raise BuildError(f"context 未按 prev 严格升序: {prev_keys[-1]!r} >= {prev!r}")
        prev_keys.append(prev)
        if edge_start + edge_count > n_edge:
            raise BuildError(f"context {i} 的 edges 引用越界")
        seen_edges += edge_count

        prev_edge = None
        total = 0
        for j in range(edge_count):
            eb = edge_off + (edge_start + j) * EDGE_RECORD_LEN
            next_off, next_len, ersv, count = EDGE.unpack_from(blob, eb)
            if ersv != 0:
                raise BuildError(f"context {i} 的 edge {j} reserved 必须为 0")
            if count == 0:
                raise BuildError(f"context {i} 的 edge {j} count 必须 > 0")
            if count > observed_total:
                raise BuildError(f"context {i} 的 edge {j} count 超过 observed_total")
            nxt = _decode_strings(blob, next_off, next_len)
            if not nxt.isascii() or not nxt.isalpha() or not nxt.islower() or len(nxt) > MAX_KEY_LEN:
                raise BuildError(f"context {i} 的 next {nxt!r} 不是合法英文键")
            if prev_edge is not None:
                pc, pn = prev_edge
                if pc < count or (pc == count and pn >= nxt):
                    raise BuildError(f"context {i} 的行内顺序不对: {pn}({pc}) 在 {nxt}({count}) 前")
                if pn == nxt:
                    raise BuildError(f"context {i} 有重复 next {nxt!r}")
            prev_edge = (count, nxt)
            total += count
        if total > observed_total:
            raise BuildError(f"context {i} 的行内 count 总和 {total} 超过 observed_total")
    if seen_edges != n_edge:
        raise BuildError(f"edge 引用总数 {seen_edges} ≠ edge_count {n_edge}")
    return {
        "context_count": n_ctx,
        "edge_count": n_edge,
        "file_len": file_len,
        "string_bytes": file_len - strings_off,
    }


def build_meta(*, product_name, lexicon_path, lexicon_sha256, input_path, input_sha256, stats,
               contexts, edges, blob, source_url, source_sha256):
    """确定性 meta：无时间戳、无随机、无绝对路径。"""
    ctx_totals = [c[3] for c in contexts]
    return {
        "format": {
            "magic": BIGRAM_MAGIC.decode("latin-1"),
            "version": BIGRAM_VERSION,
            "header_len": HEADER_LEN,
            "context_record_len": CONTEXT_RECORD_LEN,
            "edge_record_len": EDGE_RECORD_LEN,
            "top_per_context": TOP_PER_CONTEXT,
        },
        "product": {
            "name": product_name,
            "bytes": len(blob),
            "sha256": __import__("hashlib").sha256(blob).hexdigest(),
            **verify_blob(blob),
        },
        "lexicon": {
            "file": Path(lexicon_path).name,
            "sha256": lexicon_sha256,
        },
        "source": {
            "file": Path(input_path).name,
            "sha256": input_sha256,
            "url": source_url or None,
            "pinned_sha256": source_sha256 or None,
        },
        "parameters": {"top_per_context": TOP_PER_CONTEXT},
        "stats": {
            **stats,
            "contexts": len(contexts),
            "edges": len(edges),
            "observed_total_sum": sum(ctx_totals),
            "observed_total_max": max(ctx_totals) if ctx_totals else 0,
        },
    }


def write_verified(path, blob):
    """同目录临时文件 + fsync → 从磁盘独立重读自检 → `os.replace`。

    绝不原地截断：任何时刻目标路径要么不存在、要么是完整的一代快照。
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "wb") as f:
        f.write(blob)
        f.flush()
        os.fsync(f.fileno())
    try:
        verify_blob(tmp.read_bytes())
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
