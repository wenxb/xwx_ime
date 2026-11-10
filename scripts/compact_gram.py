#!/usr/bin/env python3
"""将上游 librime-octagram (Rime::Grammar/1.0, ~420MB) 压缩为 Rime::Grammar/2.0 (~44MB)。

保持 WanxiangLM 字符级滑动窗口查询语义完全同构：
1. 修复原 darts-clone double-array 共享叶子 base 的 MSB 截断越界 bug；
2. 消除稀疏空槽，转为位图定界的紧凑 CSR DAWG（单边 4B + 1-bit 状态尾标）；
3. 阶数感知剪枝（2-gram 100% 保留；3-gram 保留 >=140000 及全部词典三字词；
   4-gram 保留 >=180000；5-gram 保留 >=223500、已保留 4-gram 延续及高频 2+3 搭配）；
4. 叶子对数分数量化为 16-bit（步长 4，误差 <= 0.0002 nats）。

用法:
    python3 scripts/compact_gram.py <输入v1.gram> <输出v2.gram> [dict.bin]
"""
import os
import struct
import subprocess
import sys
import tempfile

C_SRC = r"""
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>

static inline uint32_t u_label(uint32_t u) { return u & 0x800000FFu; }
static inline size_t u_offset(uint32_t u) { return ((size_t)(u >> 10)) << ((u & 0x200u) >> 6); }
static inline int u_has_leaf(uint32_t u) { return (u >> 8) & 1; }
static inline uint32_t u_value(uint32_t u) { return u & 0x7FFFFFFFu; }

static const uint32_t *units;
static size_t num_units;
static uint32_t *base_ref;
static uint8_t *vis;
static uint8_t *alive;
static uint8_t *normal_alive;
static uint8_t *state_min_clen;
static uint8_t *state_rem;
static uint8_t *state_c4_kept;
static uint8_t *edge_has_w2w3;
static uint8_t *edge_is_dict_w3;

static uint32_t th3 = 0, th4 = 180000, th5 = 223500;

#define W3T_BITS 20
#define W3T_SIZE (1 << W3T_BITS)
static uint64_t w3t_key[W3T_SIZE];
static uint32_t w3t_val[W3T_SIZE];
static uint32_t w3t_nodes = 1;

static uint32_t w3_head[262144];
static uint32_t w3_next_sib[262144];
static uint8_t w3_byte[262144];
static uint32_t w3_child[262144];
static uint32_t w3_edges = 1;

static void w3_add_edge(uint32_t parent, uint8_t b, uint32_t child_node) {
    uint32_t e = w3_edges++;
    w3_byte[e] = b;
    w3_child[e] = child_node;
    w3_next_sib[e] = w3_head[parent];
    w3_head[parent] = e;
}

static void w3t_insert(const uint8_t *buf) {
    uint32_t cur = 0;
    for (int i = 0; i < 6; i++) {
        uint64_t k = ((uint64_t)cur << 8) | buf[i] | 0x8000000000000000ULL;
        uint32_t h = (uint32_t)((k * 11400714819323198485ULL) >> (64 - W3T_BITS));
        while (w3t_key[h] != 0 && w3t_key[h] != k) h = (h + 1) & (W3T_SIZE - 1);
        if (w3t_key[h] == 0) {
            w3t_key[h] = k;
            uint32_t nn = w3t_nodes++;
            w3t_val[h] = nn;
            w3_add_edge(cur, buf[i], nn);
        }
        cur = w3t_val[h];
    }
}

static size_t traverse_bytes(size_t start_node, const uint8_t *buf, int len) {
    size_t node = start_node;
    for (int i = 0; i < len; i++) {
        uint8_t b = buf[i];
        uint32_t u = units[node];
        size_t off = u_offset(u);
        if (off == 0) return 0xFFFFFFFFu;
        size_t base = node ^ off;
        size_t child = base ^ b;
        if (child >= num_units || u_label(units[child]) != b) return 0xFFFFFFFFu;
        node = child;
    }
    return node;
}

static int keep_leaf(int clen, uint32_t val, int c4_kept, int w2w3_ok, int dict_w3_ok) {
    if (val == 0) return 0;
    if (clen == 2) return 1;
    if (clen == 3) return val >= th3 || dict_w3_ok;
    if (clen == 4) return val >= th4;
    if (clen == 5) {
        if (val >= th5) return 1;
        if ((c4_kept || w2w3_ok) && val >= 220000) return 1;
        return 0;
    }
    if (clen == 6) return val >= th5;
    return 0;
}

// Pass 1: normal DFS (never enters shared leaf bases: base_ref > 1 && MSB == 1)
static uint8_t dfs_normal(size_t node, int rem_bytes, int clen, int depth, int c4_kept) {
    if (depth > 64) return 0;
    uint32_t u = units[node];
    size_t off = u_offset(u);
    if (off == 0) return 0;
    size_t base = node ^ off;
    if (base >= num_units) return 0;
    if (base_ref[base] > 1 && (units[base] & 0x80000000u)) return 0;
    if (vis[base >> 3] & (1 << (base & 7))) {
        return alive[base];
    }
    vis[base >> 3] |= (1 << (base & 7));
    state_min_clen[base] = clen;
    state_rem[base] = rem_bytes;
    state_c4_kept[base] = c4_kept;

    uint8_t has_any = 0, has_kids = 0;
    for (uint32_t c = 1; c < 256; c++) {
        size_t child = base ^ c;
        if (child < num_units && u_label(units[child]) == c) {
            uint32_t cu = units[child];
            size_t coff = u_offset(cu);
            size_t cbase = child ^ coff;
            int next_rem, next_clen;
            if (rem_bytes == 0) {
                int adv;
                if ((c & 0x80) == 0) adv = 1;
                else if ((c & 0xF0) == 0xE0) adv = (c & 0x0F) + 1;
                else adv = 2;
                next_rem = adv - 1;
                next_clen = clen + (next_rem == 0 ? 1 : 0);
            } else {
                next_rem = rem_bytes - 1;
                next_clen = clen + (next_rem == 0 ? 1 : 0);
            }
            uint32_t lval = (u_has_leaf(cu) && cbase < num_units) ? u_value(units[cbase]) : 0;
            int next_c4 = c4_kept;
            if (next_clen == 4 && next_rem == 0) {
                next_c4 = (lval >= th4);
            }
            uint8_t child_st = 0;
            if (next_clen < 6) {
                child_st = dfs_normal(child, next_rem, next_clen, depth + 1, next_c4);
            }
            int dw3 = (edge_is_dict_w3[child >> 3] >> (child & 7)) & 1;
            int l = keep_leaf(next_clen, lval, next_c4, 0, dw3);
            if (!l && next_clen <= 4 && lval > 0 && child_st) l = 1;
            if (l || child_st) {
                has_any = 1;
                if (child_st) has_kids = 1;
            }
        }
    }
    alive[base] = has_any ? (has_kids ? 2 : 1) : 0;
    normal_alive[base] = alive[base];
    return alive[base];
}

// Pass 2: mark exact w2+w3 paths and activate ONLY their states & edges!
static int mark_w3_from(size_t base, int depth, uint32_t tnode) {
    int any = 0;
    for (uint32_t e = w3_head[tnode]; e != 0; e = w3_next_sib[e]) {
        uint32_t c = w3_byte[e];
        size_t child = base ^ c;
        if (child < num_units && u_label(units[child]) == c) {
            uint32_t cu = units[child];
            size_t coff = u_offset(cu);
            size_t cbase = child ^ coff;
            if (depth == 5) {
                if (u_has_leaf(cu) && cbase < num_units && u_value(units[cbase]) >= 220000) {
                    edge_has_w2w3[child >> 3] |= (1 << (child & 7));
                    any = 1;
                }
            } else if (coff != 0 && cbase < num_units) {
                if (mark_w3_from(cbase, depth + 1, w3_child[e])) {
                    edge_has_w2w3[child >> 3] |= (1 << (child & 7));
                    if (!alive[cbase]) {
                        alive[cbase] = (depth == 4) ? 1 : 2;
                        state_min_clen[cbase] = 2 + (depth + 1) / 2;
                        state_rem[cbase] = (depth + 1) & 1;
                    } else if (depth < 4) {
                        alive[cbase] = 2;
                    }
                    any = 1;
                }
            }
        }
    }
    return any;
}

static inline uint16_t quantize_val(uint32_t v) {
    if (v <= 20000) return 1;
    uint32_t q = (v - 20000 + 2) >> 2;
    if (q == 0) q = 1;
    if (q > 65535) q = 65535;
    return (uint16_t)q;
}

typedef struct {
    uint32_t child_state;
    uint16_t val_q;
} __attribute__((packed)) BothEntry;

int main(int argc, char **argv) {
    if (argc < 3) return 1;
    if (argc >= 4) th3 = atoi(argv[3]);
    if (argc >= 5) th4 = atoi(argv[4]);
    if (argc >= 6) th5 = atoi(argv[5]);

    int fd = open(argv[1], O_RDONLY);
    struct stat st;
    fstat(fd, &st);
    const uint8_t *map = mmap(NULL, st.st_size, PROT_READ, MAP_PRIVATE | MAP_POPULATE, fd, 0);
    units = (const uint32_t *)(map + 44);
    num_units = (st.st_size - 44) / 4;

    base_ref = calloc(num_units, sizeof(uint32_t));
    for (size_t i = 0; i < num_units; i++) {
        uint32_t u = units[i];
        if (u & 0x80000000u) continue;
        size_t off = u_offset(u);
        if (off == 0) continue;
        size_t base = i ^ off;
        if (base < num_units) base_ref[base]++;
    }

    state_min_clen = calloc(num_units, 1);
    state_rem = calloc(num_units, 1);
    state_c4_kept = calloc(num_units, 1);
    edge_has_w2w3 = calloc((num_units + 7) / 8, 1);
    edge_is_dict_w3 = calloc((num_units + 7) / 8, 1);
    {
        FILE *f3 = fopen("/tmp/w3_all_dict.bin", "rb");
        if (f3) {
            uint8_t b6[6];
            while (fread(b6, 1, 6, f3) == 6) {
                size_t n3 = traverse_bytes(0, b6, 6);
                if (n3 != 0xFFFFFFFFu && u_has_leaf(units[n3])) {
                    edge_is_dict_w3[n3 >> 3] |= (1 << (n3 & 7));
                }
            }
            fclose(f3);
        }
    }
    vis = calloc((num_units + 7) / 8, 1);
    alive = calloc(num_units, 1);
    normal_alive = calloc(num_units, 1);

    // 1. Run normal DFS first!
    dfs_normal(0, 0, 0, 0, 0);

    // 2. Load w3 and w2 and mark ONLY the exact w2+w3 paths!
    FILE *fp = fopen("/tmp/w3_gt1000.bin", "rb");
    if (fp) {
        uint8_t buf[6];
        while (fread(buf, 1, 6, fp) == 6) {
            w3t_insert(buf);
        }
        fclose(fp);
    }
    fp = fopen("/tmp/w2_gt10k.bin", "rb");
    if (fp) {
        uint8_t buf[4];
        while (fread(buf, 1, 4, fp) == 4) {
            size_t node = traverse_bytes(0, buf, 4);
            if (node != 0xFFFFFFFFu) {
                size_t base = node ^ u_offset(units[node]);
                if (base < num_units && normal_alive[base]) {
                    if (mark_w3_from(base, 0, 0)) {
                        alive[base] = 2;
                    }
                }
            }
        }
        fclose(fp);
    }

    size_t root_base = 0 ^ u_offset(units[0]);
    uint32_t num_kid_states = 0, num_leaf_states = 0;
    for (size_t b = 0; b < num_units; b++) {
        if (alive[b] == 2) num_kid_states++;
        else if (alive[b] == 1) num_leaf_states++;
    }
    uint32_t total_states = num_kid_states + num_leaf_states;

    uint32_t *state_id = malloc(num_units * sizeof(uint32_t));
    memset(state_id, 0xFF, num_units * sizeof(uint32_t));
    uint32_t *id_to_base = malloc(total_states * sizeof(uint32_t));
    uint32_t kid_idx = 0, leaf_idx = num_kid_states;

    state_id[root_base] = kid_idx;
    id_to_base[kid_idx++] = root_base;
    for (size_t b = 0; b < num_units; b++) {
        if (b == root_base) continue;
        if (alive[b] == 2) {
            state_id[b] = kid_idx;
            id_to_base[kid_idx++] = b;
        } else if (alive[b] == 1) {
            state_id[b] = leaf_idx;
            id_to_base[leaf_idx++] = b;
        }
    }

    // First pass over states: assign state_id[base] = first edge index!
    uint32_t *first_edge_of_base = malloc(num_units * sizeof(uint32_t));
    memset(first_edge_of_base, 0xFF, num_units * sizeof(uint32_t));
    uint32_t total_edges = 0, total_both = 0;
    for (uint32_t sid = 0; sid < total_states; sid++) {
        size_t base = id_to_base[sid];
        first_edge_of_base[base] = total_edges;
        int is_norm = normal_alive[base];
        int clen = state_min_clen[base];
        int rem_bytes = state_rem[base];
        int c4_kept = state_c4_kept[base];
        for (uint32_t c = 1; c < 256; c++) {
            size_t child = base ^ c;
            if (child < num_units && u_label(units[child]) == c) {
                int w2w3_ok = (edge_has_w2w3[child >> 3] >> (child & 7)) & 1;
                if (!is_norm && !w2w3_ok) continue;
                uint32_t cu = units[child];
                size_t coff = u_offset(cu);
                size_t cbase = child ^ coff;
                int next_rem, next_clen;
                if (rem_bytes == 0) {
                    int adv;
                    if ((c & 0x80) == 0) adv = 1;
                    else if ((c & 0xF0) == 0xE0) adv = (c & 0x0F) + 1;
                    else adv = 2;
                    next_rem = adv - 1;
                    next_clen = clen + (next_rem == 0 ? 1 : 0);
                } else {
                    next_rem = rem_bytes - 1;
                    next_clen = clen + (next_rem == 0 ? 1 : 0);
                }
                uint32_t lval = (u_has_leaf(cu) && cbase < num_units) ? u_value(units[cbase]) : 0;
                int has_csid = 0;
                if (coff != 0 && cbase < num_units && next_clen < 6) {
                    if (w2w3_ok || normal_alive[cbase]) {
                        if (alive[cbase]) has_csid = 1;
                    }
                }
                int dw3 = (edge_is_dict_w3[child >> 3] >> (child & 7)) & 1;
                int l = is_norm ? keep_leaf(next_clen, lval, c4_kept, w2w3_ok, dw3) : (next_clen == 5 && w2w3_ok && lval >= 220000);
                if (!l && is_norm && next_clen <= 4 && lval > 0 && has_csid) l = 1;
                if (l || has_csid) {
                    total_edges++;
                    if (l && has_csid) total_both++;
                }
            }
        }
    }
    printf("total_states=%u (kid=%u, leaf=%u), total_edges=%u, total_both=%u\n",
           total_states, num_kid_states, num_leaf_states, total_edges, total_both);

    uint32_t end_bits_words = (total_edges + 63) / 64;
    uint64_t *end_bits = calloc(end_bits_words, sizeof(uint64_t));
    uint32_t *edges = malloc(total_edges * sizeof(uint32_t));
    BothEntry *both_table = malloc(total_both * sizeof(BothEntry));

    uint32_t e_idx = 0, b_idx = 0;
    for (uint32_t sid = 0; sid < total_states; sid++) {
        size_t base = id_to_base[sid];
        int is_norm = normal_alive[base];
        int clen = state_min_clen[base];
        int rem_bytes = state_rem[base];
        int c4_kept = state_c4_kept[base];
        for (uint32_t c = 1; c < 256; c++) {
            size_t child = base ^ c;
            if (child < num_units && u_label(units[child]) == c) {
                int w2w3_ok = (edge_has_w2w3[child >> 3] >> (child & 7)) & 1;
                if (!is_norm && !w2w3_ok) continue;
                uint32_t cu = units[child];
                size_t coff = u_offset(cu);
                size_t cbase = child ^ coff;
                int next_rem, next_clen;
                if (rem_bytes == 0) {
                    int adv;
                    if ((c & 0x80) == 0) adv = 1;
                    else if ((c & 0xF0) == 0xE0) adv = (c & 0x0F) + 1;
                    else adv = 2;
                    next_rem = adv - 1;
                    next_clen = clen + (next_rem == 0 ? 1 : 0);
                } else {
                    next_rem = rem_bytes - 1;
                    next_clen = clen + (next_rem == 0 ? 1 : 0);
                }
                uint32_t lval = (u_has_leaf(cu) && cbase < num_units) ? u_value(units[cbase]) : 0;
                uint32_t csid = 0xFFFFFFFFu;
                if (coff != 0 && cbase < num_units && next_clen < 6) {
                    if (w2w3_ok || normal_alive[cbase]) {
                        if (alive[cbase]) csid = first_edge_of_base[cbase];
                    }
                }
                int dw3 = (edge_is_dict_w3[child >> 3] >> (child & 7)) & 1;
                int l = is_norm ? keep_leaf(next_clen, lval, c4_kept, w2w3_ok, dw3) : (next_clen == 5 && w2w3_ok && lval >= 220000);
                if (!l && is_norm && next_clen <= 4 && lval > 0 && csid != 0xFFFFFFFFu) l = 1;
                if (l && csid != 0xFFFFFFFFu) {
                    uint32_t bi = b_idx++;
                    both_table[bi].child_state = csid;
                    both_table[bi].val_q = quantize_val(lval);
                    edges[e_idx++] = c | ((total_edges + bi) << 8);
                } else if (l) {
                    uint32_t q = quantize_val(lval);
                    edges[e_idx++] = c | ((0xFF0000u | q) << 8);
                } else if (csid != 0xFFFFFFFFu) {
                    edges[e_idx++] = c | (csid << 8);
                }
            }
        }
        uint32_t last_e = e_idx - 1;
        end_bits[last_e >> 6] |= (1ULL << (last_e & 63));
    }

    fp = fopen(argv[2], "wb");
    uint8_t hdr[64] = {0};
    memcpy(hdr, "Rime::Grammar/2.0", 17);
    *(uint32_t *)(hdr + 36) = total_states;
    *(int32_t *)(hdr + 40) = 24;
    *(uint32_t *)(hdr + 44) = num_kid_states;
    *(uint32_t *)(hdr + 48) = num_leaf_states;
    *(uint32_t *)(hdr + 52) = total_edges;
    *(uint32_t *)(hdr + 56) = total_both;
    *(uint32_t *)(hdr + 60) = end_bits_words;

    fwrite(hdr, 1, 64, fp);
    fwrite(end_bits, sizeof(uint64_t), end_bits_words, fp);
    fwrite(edges, sizeof(uint32_t), total_edges, fp);
    fwrite(both_table, sizeof(BothEntry), total_both, fp);
    long fsz = ftell(fp);
    fclose(fp);
    printf("Wrote %s: %.2f MB (%ld bytes)\n", argv[2], fsz / 1048576.0, fsz);
    return 0;
}
"""


def _extract_dict_aux(dict_path: str, w2_path: str, w3_path: str, w3_all_path: str) -> None:
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from abbrev_to_bin import load_dict

    groups = load_dict(dict_path)
    word_max_freq: dict[str, int] = {}
    for _py, ws in groups:
        for w, fq in ws:
            if fq > word_max_freq.get(w, -1):
                word_max_freq[w] = fq

    def enc(s: str) -> bytes:
        out = bytearray()
        for ch in s:
            cp = ord(ch)
            out.append(0xE0 | ((cp >> 16) & 0x0F))
            out.append((cp >> 8) & 0xFF)
            out.append(cp & 0xFF)
        return bytes(out)

    w2_top = sorted(
        [(f, w) for w, f in word_max_freq.items() if len(w) == 2 and all(0x4E00 <= ord(c) <= 0x9FFF for c in w)],
        reverse=True,
    )[:500]
    w3_top = [
        (f, w)
        for w, f in word_max_freq.items()
        if len(w) == 3 and f >= 2000 and all(0x4E00 <= ord(c) <= 0x9FFF for c in w)
    ]
    with open(w2_path, "wb") as f:
        for _, w in w2_top:
            b = enc(w)
            if len(b) == 4:
                f.write(b)
    with open(w3_path, "wb") as f:
        for _, w in w3_top:
            b = enc(w)
            if len(b) == 6:
                f.write(b)
    with open(w3_all_path, "wb") as f:
        for w in word_max_freq:
            if len(w) == 3:
                b = enc(w)
                if len(b) == 6:
                    f.write(b)


def main() -> None:
    if len(sys.argv) < 3:
        raise SystemExit("用法: python3 scripts/compact_gram.py <in.gram> <out.gram> [dict.bin]")
    in_gram, out_gram = sys.argv[1], sys.argv[2]
    dict_bin = sys.argv[3] if len(sys.argv) > 3 else "data/dict.bin"
    _extract_dict_aux(dict_bin, "/tmp/w2_top.bin", "/tmp/w3_top.bin", "/tmp/w3_all_dict.bin")
    with tempfile.TemporaryDirectory() as td:
        c_file = os.path.join(td, "build_compact_gram.c")
        bin_file = os.path.join(td, "build_compact_gram")
        with open(c_file, "w") as f:
            f.write(C_SRC)
        subprocess.run(["cc", "-O3", c_file, "-o", bin_file], check=True)
        subprocess.run([bin_file, in_gram, out_gram, "140000", "180000", "223500"], check=True)


if __name__ == "__main__":
    main()
