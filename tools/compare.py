#!/usr/bin/env python3
"""Compare a librime (rime_oracle) record file against an xwx_engine record
file. Presentation and statistics only -- no PASS/FAIL verdicts.

Usage:
    python3 tools/compare.py <librime_out> <xwx_out> [corpus.txt]

Record format (both sides):
    IN\t<input>
    PREEDIT\t<preedit>
    NCAND\t<total candidate count>
    CAND\t<1-based index>\t<candidate text>   (at most 100 lines)
    END

If corpus.txt is given, `#`-comment group headers are used to break the
summary down per category.
"""

import statistics
import sys


def parse_records(path):
    """Parse a record file into {input: {preedit, ncand, cands}}.

    On duplicate IN keys the later record wins (a warning goes to stderr).
    """
    records = {}
    cur = None

    def finish():
        if cur is not None:
            if cur["input"] in records:
                print(
                    "WARNING: duplicate IN %r in %s; later record wins"
                    % (cur["input"], path),
                    file=sys.stderr,
                )
            records[cur["input"]] = cur

    with open(path, encoding="utf-8", errors="replace") as f:
        for lineno, raw in enumerate(f, 1):
            line = raw.rstrip("\n")
            if line.endswith("\r"):
                line = line[:-1]
            if not line:
                continue
            parts = line.split("\t")
            tag = parts[0]
            if tag == "IN":
                finish()
                cur = {
                    "input": parts[1] if len(parts) > 1 else "",
                    "preedit": "",
                    "ncand": 0,
                    "cands": [],
                }
            elif cur is None:
                print(
                    "WARNING: %s:%d: %s line before any IN, ignored"
                    % (path, lineno, tag),
                    file=sys.stderr,
                )
            elif tag == "PREEDIT":
                cur["preedit"] = parts[1] if len(parts) > 1 else ""
            elif tag == "NCAND":
                try:
                    cur["ncand"] = int(parts[1])
                except (IndexError, ValueError):
                    print(
                        "WARNING: %s:%d: bad NCAND line, treated as 0"
                        % (path, lineno),
                        file=sys.stderr,
                    )
            elif tag == "CAND":
                cur["cands"].append(parts[2] if len(parts) > 2 else "")
            elif tag == "END":
                finish()
                cur = None
            else:
                print(
                    "WARNING: %s:%d: unknown tag %r, ignored"
                    % (path, lineno, tag),
                    file=sys.stderr,
                )
    finish()
    return records


def parse_corpus_groups(path):
    """Parse corpus.txt into [(group_name, [inputs...])].

    Lines starting with '#' open a new group; inputs before the first '#'
    land in an '(ungrouped)' bucket.
    """
    groups = []
    name, items = "(ungrouped)", []
    with open(path, encoding="utf-8", errors="replace") as f:
        for raw in f:
            line = raw.strip()
            if not line:
                continue
            if line.startswith("#"):
                if items:
                    groups.append((name, items))
                name, items = line.lstrip("#").strip() or "(unnamed)", []
            else:
                items.append(line)
    if items or not groups:
        groups.append((name, items))
    return groups


def is_multisyllable_han(text):
    """True if the candidate text contains at least 2 CJK ideographs."""
    return sum(1 for ch in text if "一" <= ch <= "鿿") >= 2


def compute_metrics(inputs, rime, xwx):
    """Compute all report metrics for the given ordered input list."""
    m = {
        "n": 0,
        "n_both": 0,
        "zero_only_xwx": [],       # rime NCAND > 0, xwx NCAND == 0
        "zero_only_rime": [],      # xwx NCAND > 0, rime NCAND == 0 (info)
        "both_zero": 0,
        "ratios": [],              # (input, xwx_ncand, rime_ncand, ratio)
        "top1_agree": 0,
        "top1_total": 0,           # inputs where both sides have a top-1
        "overlaps": [],            # (input, overlap_size)
        "missing_multisyl": [],    # (input, candidate text)
        "recall_miss": [],         # (input, candidate text) -- in rime top-10,
                                   # nowhere in xwx CAND list (now up to 100)
        "rank_gap": [],            # (input, candidate text, xwx rank) -- in
                                   # rime top-10, in xwx list but ranked > 10
        "preedit_diff": [],        # (input, rime_preedit, xwx_preedit)
        "only_in_rime": [],        # inputs missing from the xwx file
        "only_in_xwx": [],         # inputs missing from the rime file
    }
    for inp in inputs:
        r = rime.get(inp)
        x = xwx.get(inp)
        if r is None:
            m["only_in_xwx"].append(inp)
            continue
        if x is None:
            m["only_in_rime"].append(inp)
            continue
        m["n"] += 1
        m["n_both"] += 1

        if r["ncand"] > 0 and x["ncand"] == 0:
            m["zero_only_xwx"].append(inp)
        if x["ncand"] > 0 and r["ncand"] == 0:
            m["zero_only_rime"].append(inp)
        if r["ncand"] == 0 and x["ncand"] == 0:
            m["both_zero"] += 1

        if r["ncand"] > 0:
            m["ratios"].append((inp, x["ncand"], r["ncand"],
                                x["ncand"] / r["ncand"]))

        if r["cands"] and x["cands"]:
            m["top1_total"] += 1
            if r["cands"][0] == x["cands"][0]:
                m["top1_agree"] += 1

        overlap = len(set(r["cands"]) & set(x["cands"]))
        m["overlaps"].append((inp, overlap))

        x_set = set(x["cands"])
        for cand in r["cands"]:
            if is_multisyllable_han(cand) and cand not in x_set:
                m["missing_multisyl"].append((inp, cand))

        # RECALL_MISS vs RANK_GAP: rime top-10 words, looked up in the FULL
        # xwx CAND list (up to 100 entries since the probe depth bump).
        # - RECALL_MISS: absent from the xwx list entirely -> recall problem
        # - RANK_GAP: present in the xwx list but outside xwx top-10 ->
        #   ranking problem; record its actual 1-based rank on the xwx side
        rime_top10 = r["cands"][:10]
        xwx_top10 = x["cands"][:10]
        xwx_rank = {cand: i + 1 for i, cand in enumerate(x["cands"])}
        for cand in rime_top10:
            if cand in xwx_top10:
                continue
            if cand in xwx_rank:
                m["rank_gap"].append((inp, cand, xwx_rank[cand]))
            else:
                m["recall_miss"].append((inp, cand))

        if r["preedit"] != x["preedit"]:
            m["preedit_diff"].append((inp, r["preedit"], x["preedit"]))
    return m


def fmt_pct(a, b):
    return "%d/%d (%.1f%%)" % (a, b, 100.0 * a / b) if b else "0/0"


def report_group(name, inputs, rime, xwx):
    m = compute_metrics(inputs, rime, xwx)
    print("=" * 72)
    print("GROUP: %s   (inputs: %d, compared: %d)" % (name, len(inputs), m["n"]))
    print("=" * 72)

    print("[ZERO_ONLY_XWX] rime has candidates but xwx has none: %d"
          % len(m["zero_only_xwx"]))
    for inp in m["zero_only_xwx"]:
        r, x = rime[inp], xwx[inp]
        rime_top1 = r["cands"][0] if r["cands"] else ""
        print("    %-40s rime_NCAND=%-6d rime_top1=%s"
              % (inp, r["ncand"], rime_top1))

    if m["zero_only_rime"]:
        print("[ZERO_ONLY_RIME] xwx has candidates but rime has none: %d"
              % len(m["zero_only_rime"]))
        for inp in m["zero_only_rime"]:
            print("    %-40s xwx_NCAND=%d" % (inp, xwx[inp]["ncand"]))

    if m["ratios"]:
        vals = [t[3] for t in m["ratios"]]
        print("[NCAND_RATIO] xwx_NCAND / rime_NCAND, median over %d inputs: %.4f"
              % (len(vals), statistics.median(vals)))
        for inp, xn, rn, ratio in m["ratios"]:
            print("    %-40s xwx=%-6d rime=%-6d ratio=%.4f"
                  % (inp, xn, rn, ratio))
    else:
        print("[NCAND_RATIO] no inputs with rime_NCAND > 0")

    print("[TOP1_AGREE] %s" % fmt_pct(m["top1_agree"], m["top1_total"]))

    if m["overlaps"]:
        avg = statistics.mean(o for _, o in m["overlaps"])
        print("[TOP10_OVERLAP] mean |rime_top10 ∩ xwx_top10| over %d inputs: %.2f"
              % (len(m["overlaps"]), avg))
    else:
        print("[TOP10_OVERLAP] no comparable inputs")

    print("[MISSING_MULTISYL] rime top-10 multi-char words absent from xwx "
          "top-10 (i.e. top-10 vs top-10; superseded by RECALL_MISS/RANK_GAP "
          "below): %d" % len(m["missing_multisyl"]))
    for inp, cand in m["missing_multisyl"]:
        print("    %-40s %s" % (inp, cand))

    print("[RECALL_MISS] rime top-10 words NOT FOUND anywhere in xwx CAND "
          "list (up to 100): %d -- true recall gaps" % len(m["recall_miss"]))
    for inp, cand in m["recall_miss"]:
        print("    %-40s %s" % (inp, cand))

    print("[RANK_GAP] rime top-10 words recalled by xwx but ranked OUTSIDE "
          "xwx top-10: %d -- ranking problems" % len(m["rank_gap"]))
    for inp, cand, rank in m["rank_gap"]:
        print("    %-40s %-20s xwx_rank=%d" % (inp, cand, rank))

    print("[PREEDIT_DIFF] informational only, NOT a failure: %d"
          % len(m["preedit_diff"]))
    for inp, rp, xp in m["preedit_diff"]:
        print("    %-40s rime=%r  xwx=%r" % (inp, rp, xp))

    for inp in m["only_in_rime"]:
        print("    NOTE: %r only in rime file" % inp)
    for inp in m["only_in_xwx"]:
        print("    NOTE: %r only in xwx file" % inp)
    print()
    return m


def main(argv):
    if len(argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    rime = parse_records(argv[1])
    xwx = parse_records(argv[2])

    if len(argv) >= 4:
        groups = parse_corpus_groups(argv[3])
    else:
        groups = [("(all inputs)", sorted(set(rime) | set(xwx)))]

    print("librime file: %s  (%d records)" % (argv[1], len(rime)))
    print("xwx file:     %s  (%d records)" % (argv[2], len(xwx)))
    print()

    # Print groups in order.
    for name, inputs in groups:
        report_group(name, inputs, rime, xwx)

    # Overall summary across the union of all inputs, in corpus order.
    seen = set()
    all_inputs = []
    for _, inputs in groups:
        for inp in inputs:
            if inp not in seen:
                seen.add(inp)
                all_inputs.append(inp)
    report_group("(overall)", all_inputs, rime, xwx)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
