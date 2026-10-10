#!/usr/bin/env python3
"""构建英文词频数据 english.bin（格式 v1）与 english.meta.json。

来源二选一：
  --wordfreq   wordfreq 3.1.1 英文 large 词表（仅构建期依赖，运行时不加载）
  --entries    TAB 分隔的 key/display/zipf_centi 清单（离线，仅标准库）
  --reference  参考词表（可多次；纯文本，每行 `word` 或 `word count`）。
               给了它才做质量过滤：把 wordfreq 里的非单词碎片（`ve`/`gra`/
               `dont`/`lol`）挡在门外，详见 english/quality.py。

格式见 docs/english-input/01-data.md 第 4～5 节：全部 little-endian，
header(64) → words(16B/条) → prefixes(16B/条) → ids(u32) → strings（无 NUL）。
布局、自检、meta 与原子写入在 scripts/english/pack.py，清洗与表解析在同包内。

用法：
    python3 scripts/build_english_dict.py --wordfreq --limit 50000 \
      --overrides data/english/overrides.tsv --out data/english.bin
    python3 scripts/build_english_dict.py \
      --entries tests/fixtures/english/words.tsv --out /tmp/test-english.bin
"""

import argparse
import json
import struct
import sys

from english.pack import (
    MAX_PREFIX_COUNT, MAX_WORD_COUNT, PREFIX_PENALTY_CENTI, PREFIX_TOP_K, VERSION, Word,
    build_meta, build_prefix_index, meta_path_for, portable_path, serialize, sha256_file,
    verify_blob, write_atomic, write_verified)
from english.quality import filter_entries, load_references
from english.tables import apply_overrides, parse_entries_file, parse_overrides_file
from english.tokens import (
    BuildError, MAX_WORD_LEN, SINGLE_LETTERS, SKIP_CATEGORIES, ZIPF_CENTI_MAX,
    ZIPF_CENTI_MIN, quantize_zipf, select_base_words)

DEFAULT_LIMIT = 50000
WORDFREQ_LANG = "en"
WORDFREQ_WORDLIST = "large"
WORDFREQ_PINNED_VERSION = "3.1.1"


# ---------------------------------------------------------------------------
# 来源
# ---------------------------------------------------------------------------

def _package_version(name):
    try:
        from importlib.metadata import version as _version  # py3.8+
        return _version(name)
    except Exception:
        return "unknown"


def build_from_wordfreq(limit):
    """--wordfreq：延迟导入 wordfreq，按 large 词表降频扫描。

    只调用 iter_wordlist 与 zipf_frequency，不使用随机词接口。
    """
    import wordfreq  # 只有此分支导入；--entries 路径完全不依赖它

    source = {
        "kind": "wordfreq",
        "package": "wordfreq",
        "version": _package_version("wordfreq"),
        "pinned_version": WORDFREQ_PINNED_VERSION,
        "language": WORDFREQ_LANG,
        "wordlist": WORDFREQ_WORDLIST,
        "iterator": "wordfreq.iter_wordlist(%r, %r)" % (WORDFREQ_LANG, WORDFREQ_WORDLIST),
    }
    candidates = ((word, wordfreq.zipf_frequency(word, WORDFREQ_LANG, WORDFREQ_WORDLIST))
                  for word in wordfreq.iter_wordlist(WORDFREQ_LANG, WORDFREQ_WORDLIST))
    pairs, stats = select_base_words(candidates, limit)
    entries = {}
    for key, zipf in pairs:
        entries[key] = (key, quantize_zipf(zipf, "（key=%s）" % key))
    return entries, source, stats


def build_from_entries(path):
    """--entries：离线清单，q 已是量化值；非法输入报错并给出行号。"""
    entries = parse_entries_file(path)
    source = {
        "kind": "entries",
        "path": portable_path(path),
        "sha256": sha256_file(path),
        "entries": len(entries),
    }
    stats = {"scanned": len(entries), "skipped": {}, "merged": 0,
             "unique_keys": len(entries), "selected": len(entries)}
    return entries, source, stats


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args(argv):
    parser = argparse.ArgumentParser(
        prog="build_english_dict.py",
        description="构建 english.bin（格式 v%d）与 english.meta.json" % VERSION)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--wordfreq", action="store_true",
                       help="用 wordfreq %s 的英文 %s 词表构建（需构建环境安装 wordfreq）"
                            % (WORDFREQ_PINNED_VERSION, WORDFREQ_WORDLIST))
    group.add_argument("--entries", metavar="TSV",
                       help="离线词条清单：key<TAB>display<TAB>zipf_centi")
    parser.add_argument("--limit", type=int, default=DEFAULT_LIMIT,
                        help="基础词条上限，只对 --wordfreq 有意义（默认 %d）" % DEFAULT_LIMIT)
    parser.add_argument("--overrides", metavar="TSV", default=None,
                        help="人工覆盖表；未给出时不应用任何覆盖")
    parser.add_argument(
        "--reference", metavar="WORDS", action="append", default=[],
        help="参考词表路径（可重复）；给了才启用质量过滤")
    parser.add_argument("--out", metavar="BIN", required=True, help="输出 english.bin 路径")
    return parser.parse_args(argv)


def build_parameters(limit_applied):
    return {
        "limit": limit_applied,
        "prefix_top_k": PREFIX_TOP_K,
        "prefix_penalty_centi": PREFIX_PENALTY_CENTI,
        "max_word_len": MAX_WORD_LEN,
        "single_letter_allowlist": sorted(SINGLE_LETTERS),
        "zipf_centi_range": [ZIPF_CENTI_MIN, ZIPF_CENTI_MAX],
    }


def _report(meta_path, out_path, blob, meta, stats, added, entries_before, entries_after):
    print("词条 %d（基础 %d → 覆盖后 %d），前缀 %d，ids %d，字符串 %d B，产物 %d B（%.2f MiB）"
          % (meta["word_count"], entries_before, entries_after, meta["prefix_count"],
             meta["id_count"], meta["strings_bytes"], len(blob), len(blob) / 1048576.0))
    print("sha256 %s" % meta["sha256"])
    if stats["skipped"]:
        items = ["%s=%d" % (name, stats["skipped"][name])
                 for name in SKIP_CATEGORIES if stats["skipped"].get(name)]
        print("跳过 token 共 %d 个：%s" % (sum(stats["skipped"].values()), "，".join(items)))
    print("候选 %d，清洗后唯一 key %d，合并同 key 重复 %d，基础词 %d，覆盖新增 %d"
          % (stats["scanned"], stats["unique_keys"], stats["merged"],
             stats["selected"], added))
    print("写出 %s" % out_path)
    print("写出 %s" % meta_path)


def run(args):
    """执行一次构建，返回 (outputs)。"""
    if args.wordfreq and not 1 <= args.limit <= MAX_WORD_COUNT:
        raise BuildError("--limit 必须在 1..%d（读取端 word_count 上限）" % MAX_WORD_COUNT)
    overrides = parse_overrides_file(args.overrides) if args.overrides else []
    if args.wordfreq:
        entries, source, stats = build_from_wordfreq(args.limit)
        limit_applied = args.limit
    else:
        entries, source, stats = build_from_entries(args.entries)
        limit_applied = None  # --entries 不做截断

    entries_before = len(entries)
    entries, added = apply_overrides(entries, overrides)
    overrides_keys = {o.key for o in overrides}
    # 质量过滤：人工清单（--entries 与 --overrides）永远保留，其余按参考词表判定。
    # 没给 --reference 时是恒等变换（旧行为、旧产物逐字节不变）。
    protected = set(entries) if not args.reference else set(overrides_keys)
    if not args.reference and args.entries:
        protected = set(entries)  # --entries 是纯人工清单，全量保留
    quality_ref = load_references(args.reference)
    entries, quality_stats = filter_entries(entries, quality_ref, protected)
    if not entries:
        raise BuildError("没有可用词条")
    words = [Word(key, value[0], value[1]) for key, value in sorted(entries.items())]
    if len(words) > MAX_WORD_COUNT:
        raise BuildError("词条数 %d 超过读取端上限 %d" % (len(words), MAX_WORD_COUNT))
    prefix_names, prefix_lists = build_prefix_index(words)
    if len(prefix_names) > MAX_PREFIX_COUNT:
        raise BuildError("前缀数 %d 超过读取端上限 %d" % (len(prefix_names), MAX_PREFIX_COUNT))

    blob = serialize(words, prefix_names, prefix_lists)
    verify_blob(blob)
    reference_info = {
        "paths": [portable_path(p) for p in args.reference],
        "sha256": [sha256_file(p) for p in args.reference],
        "words": len(quality_ref),
        "reasons": quality_stats,
    }
    overrides_info = {
        "path": portable_path(args.overrides) if args.overrides else None,
        "sha256": sha256_file(args.overrides) if args.overrides else None,
        "entries": len(overrides),
        "added_words": added,
    }
    meta_bytes = build_meta(words, prefix_names, prefix_lists,
                            len(blob) - struct.unpack_from("<Q", blob, 48)[0],
                            blob, source, build_parameters(limit_applied),
                            overrides_info, stats, reference_info)
    meta_path = meta_path_for(args.out)
    write_verified(args.out, blob)
    write_atomic(meta_path, meta_bytes)
    if limit_applied is not None and stats["selected"] < limit_applied:
        print("提示：有效基础词只有 %d 个，少于 --limit %d（按实际数量构建，不补齐）"
              % (stats["selected"], limit_applied), file=sys.stderr)
    return (meta_path, blob, json.loads(meta_bytes.decode("utf-8")), stats, added,
            entries_before, len(entries))


def main(argv=None):
    args = parse_args(list(sys.argv[1:]) if argv is None else list(argv))
    try:
        meta_path, blob, meta, stats, added, before, after = run(args)
    except BuildError as exc:
        print("构建失败：%s" % exc, file=sys.stderr)
        return 1
    _report(meta_path, args.out, blob, meta, stats, added, before, after)
    return 0


if __name__ == "__main__":
    sys.exit(main())
