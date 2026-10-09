#!/usr/bin/env python3
"""english.bin v1 的二进制布局、产物自检、meta.json 与原子写入。

布局：header(64) → words(16B/条) → prefixes(16B/条) → ids(u32) → strings。
全部 little-endian；字符串不带 NUL，按 (offset, length) 寻址。
"""

import hashlib
import json
import os
import re
import struct
import tempfile
from collections import namedtuple

from english.tokens import (
    BuildError, MAX_WORD_LEN, MIN_WORD_LEN, ZIPF_CENTI_MAX, ZIPF_CENTI_MIN)

MAGIC = b"XWXENG1\x00"
VERSION = 1
HEADER_LEN = 64
WORD_REC_LEN = 16
PREFIX_REC_LEN = 16
PREFIX_TOP_K = 32
PREFIX_PENALTY_CENTI = 15
MAX_WORD_COUNT = 200000  # 读取端防御上限
MAX_PREFIX_COUNT = 4000000  # 读取端防御上限

POOL_ASCII_RE = re.compile(rb"[A-Za-z]+")
HEADER = struct.Struct("<8sIIIIQQQQQ")
WORD_REC = struct.Struct("<IIHHHH")
PREFIX_REC = struct.Struct("<IHHII")

Word = namedtuple("Word", ("key", "display", "q"))


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def portable_path(path):
    """meta 不写机器绝对路径：相对路径原样，绝对路径尽量转成相对 cwd。"""
    if not os.path.isabs(path):
        return path.replace(os.sep, "/")
    try:
        rel = os.path.relpath(path, os.getcwd())
    except ValueError:
        return os.path.basename(path)
    return os.path.basename(path) if rel.startswith("..") else rel.replace(os.sep, "/")


def meta_path_for(out_path):
    """data/english.bin → data/english.meta.json（去掉 .bin 后缀）。"""
    base = out_path[:-4] if out_path.endswith(".bin") else out_path
    return base + ".meta.json"


def build_prefix_index(words):
    """为每个非空前缀（含完整词）建引用列表，返回 (前缀名升序, 列表)。

    static_score_centi = q - 15 × (len(word) - len(prefix))；前缀长度在同一个
    前缀内是常数，故等价于按 (15 × len(word) - q) 升序、并列按 key 升序遍历，
    每个前缀列表最多收 32 个 word_id。复杂度 O(N log N + 总词长)。
    """
    keys = [word.key for word in words]
    order = sorted(range(len(words)),
                   key=lambda i: (PREFIX_PENALTY_CENTI * len(keys[i]) - words[i].q, keys[i]))
    lists = {}
    for index in order:
        key = keys[index]
        for plen in range(1, len(key) + 1):
            prefix = key[:plen]
            bucket = lists.get(prefix)
            if bucket is None:
                lists[prefix] = [index]
            elif len(bucket) < PREFIX_TOP_K:
                bucket.append(index)
    names = sorted(lists)
    return names, [lists[name] for name in names]


def serialize(words, prefix_names, prefix_lists):
    """打包 header + words + prefixes + ids + strings，返回完整字节串。"""
    pool = bytearray()
    offsets = {}

    def intern(text):
        raw = text.encode("ascii")
        off = offsets.get(raw)
        if off is None:
            off = len(pool)
            offsets[raw] = off
            pool.extend(raw)
        return off

    word_blob = bytearray()
    for word in words:
        word_blob.extend(WORD_REC.pack(
            intern(word.key), intern(word.display),
            len(word.key), len(word.display), word.q, 0))

    prefix_blob = bytearray()
    id_blob = bytearray()
    start = 0
    for name, bucket in zip(prefix_names, prefix_lists):
        prefix_blob.extend(PREFIX_REC.pack(
            intern(name), len(name), len(bucket), start, 0))
        id_blob.extend(struct.pack("<%dI" % len(bucket), *bucket))
        start += len(bucket)

    words_off = HEADER_LEN
    prefixes_off = words_off + len(word_blob)
    ids_off = prefixes_off + len(prefix_blob)
    strings_off = ids_off + len(id_blob)
    file_len = strings_off + len(pool)
    header = HEADER.pack(MAGIC, VERSION, HEADER_LEN, len(words), len(prefix_names),
                         words_off, prefixes_off, ids_off, strings_off, file_len)
    return bytes(header + word_blob + prefix_blob + id_blob + pool)


def verify_blob(blob):
    """产物结构自检：头、区段边界、记录不变量、前缀引用与覆盖性。"""
    if len(blob) < HEADER_LEN:
        raise BuildError("产物短于头部：%d 字节" % len(blob))
    (magic, version, header_len, word_count, prefix_count,
     words_off, prefixes_off, ids_off, strings_off, file_len) = HEADER.unpack_from(blob, 0)
    if magic != MAGIC:
        raise BuildError("魔数错误：%r" % magic)
    if version != VERSION or header_len != HEADER_LEN:
        raise BuildError("版本/头长错误：version=%d header_len=%d" % (version, header_len))
    if file_len != len(blob):
        raise BuildError("file_len=%d 与实际长度 %d 不一致" % (file_len, len(blob)))
    if words_off != HEADER_LEN:
        raise BuildError("words_offset=%d 应为 %d" % (words_off, HEADER_LEN))
    if prefixes_off != words_off + word_count * WORD_REC_LEN:
        raise BuildError("prefixes_offset 与 word_count 不一致")
    if ids_off != prefixes_off + prefix_count * PREFIX_REC_LEN:
        raise BuildError("ids_offset 与 prefix_count 不一致")
    if strings_off < ids_off or (strings_off - ids_off) % 4:
        raise BuildError("ids 区段边界非法：ids=[%d,%d)" % (ids_off, strings_off))
    pool = blob[strings_off:]

    def read_string(off, length):
        if off + length > len(pool):
            raise BuildError("字符串引用越界：off=%d len=%d" % (off, length))
        return pool[off:off + length]

    keys = []
    centi = []
    previous = b""
    for i in range(word_count):
        ko, do, klen, dlen, q, reserved = WORD_REC.unpack_from(
            blob, words_off + i * WORD_REC_LEN)
        if reserved != 0:
            raise BuildError("word[%d] reserved 非零" % i)
        if not MIN_WORD_LEN <= klen <= MAX_WORD_LEN or not MIN_WORD_LEN <= dlen <= MAX_WORD_LEN:
            raise BuildError("word[%d] 长度越界：key=%d display=%d" % (i, klen, dlen))
        if not ZIPF_CENTI_MIN <= q <= ZIPF_CENTI_MAX:
            raise BuildError("word[%d] zipf_centi 越界：%d" % (i, q))
        key = read_string(ko, klen)
        display = read_string(do, dlen)
        if not POOL_ASCII_RE.fullmatch(key) or not POOL_ASCII_RE.fullmatch(display):
            raise BuildError("word[%d] key/display 含非字母字符" % i)
        if key != key.lower() or display.lower() != key:
            raise BuildError("word[%d] key/display 关系非法：%r / %r" % (i, key, display))
        if key <= previous:
            raise BuildError("word[%d] key 未严格升序：%r" % (i, key))
        previous = key
        keys.append(key)
        centi.append(q)

    total_ids = (strings_off - ids_off) // 4
    covered = set()
    previous = b""
    for i in range(prefix_count):
        po, plen, id_count, start, reserved = PREFIX_REC.unpack_from(
            blob, prefixes_off + i * PREFIX_REC_LEN)
        if reserved != 0:
            raise BuildError("prefix[%d] reserved 非零" % i)
        if not MIN_WORD_LEN <= plen <= MAX_WORD_LEN:
            raise BuildError("prefix[%d] 长度越界：%d" % (i, plen))
        if not 1 <= id_count <= PREFIX_TOP_K:
            raise BuildError("prefix[%d] id_count 越界：%d" % (i, id_count))
        if start + id_count > total_ids:
            raise BuildError("prefix[%d] 引用越过 ids 区段" % i)
        prefix = read_string(po, plen)
        if not POOL_ASCII_RE.fullmatch(prefix):
            raise BuildError("prefix[%d] 含非字母字符" % i)
        if prefix <= previous:
            raise BuildError("prefix[%d] 未严格升序：%r" % (i, prefix))
        previous = prefix
        ids = struct.unpack_from("<%dI" % id_count, blob, ids_off + start * 4)
        if len(set(ids)) != id_count:
            raise BuildError("prefix %r 引用列表有重复 ID" % prefix)
        last_rank = None
        for wid in ids:
            if wid >= word_count:
                raise BuildError("prefix %r 的 word_id %d 越界" % (prefix, wid))
            key = keys[wid]
            if not key.startswith(prefix):
                raise BuildError("prefix %r 引用的 %r 不以其开头" % (prefix, key))
            rank = (PREFIX_PENALTY_CENTI * len(key) - centi[wid], key)
            if last_rank is not None and rank < last_rank:
                raise BuildError("prefix %r 的引用列表未按静态分排序" % prefix)
            last_rank = rank
        covered.add(prefix)

    for key in keys:  # 每个词的每个非空前缀都必须有记录（含完整词）
        for plen in range(1, len(key) + 1):
            if key[:plen] not in covered:
                raise BuildError("缺少前缀记录：%r" % key[:plen])


def build_meta(words, prefix_names, prefix_lists, strings_bytes, blob,
               source, parameters, overrides_info, stats):
    """构造 meta 字节串；只含确定性字段（无时间、随机数、机器绝对路径）。"""
    meta = {
        "format": {"name": "english.bin", "magic": "XWXENG1", "version": VERSION,
                   "header_len": HEADER_LEN, "endian": "little",
                   "word_record_len": WORD_REC_LEN, "prefix_record_len": PREFIX_REC_LEN},
        "word_count": len(words),
        "prefix_count": len(prefix_names),
        "id_count": sum(len(bucket) for bucket in prefix_lists),
        "strings_bytes": strings_bytes,
        "file_len": len(blob),
        "sha256": sha256_bytes(blob),
        "source": source,
        "parameters": parameters,
        "overrides": overrides_info,
        "skipped_tokens": dict(sorted(stats["skipped"].items())),
        "counts": {
            "candidates_scanned": stats["scanned"],
            "unique_keys_after_cleaning": stats["unique_keys"],
            "merged_duplicate_keys": stats["merged"],
            "base_words_selected": stats["selected"],
        },
    }
    return (json.dumps(meta, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")


def _unlink(path):
    try:
        os.unlink(path)
    except OSError:
        pass


def _write_tmp(path, data):
    """在目标同目录写临时文件并 fsync，返回临时文件路径。"""
    directory = os.path.dirname(os.path.abspath(path))
    os.makedirs(directory, exist_ok=True)
    fd, tmp = tempfile.mkstemp(
        prefix="." + os.path.basename(path) + ".", suffix=".tmp", dir=directory)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        _unlink(tmp)
        raise
    return tmp


def write_verified(path, blob):
    """同目录临时文件 → 读回自检 → os.replace；绝不在原文件上截断重写。"""
    tmp = _write_tmp(path, blob)
    try:
        with open(tmp, "rb") as handle:
            verify_blob(handle.read())
        os.replace(tmp, path)
    except BaseException:
        _unlink(tmp)
        raise


def write_atomic(path, data):
    """同目录临时文件 → os.replace（meta 不需自检）。"""
    tmp = _write_tmp(path, data)
    try:
        os.replace(tmp, path)
    except BaseException:
        _unlink(tmp)
        raise
