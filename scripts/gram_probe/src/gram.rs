//! `abbrev.bin` / `.gram` 的二进制结构读取与查询(从 `main.rs` 拆出)。
//!
//! 这里的 `Header`/`DoubleArray` 是**独立复刻**的:探针刻意不依赖
//! `xwx_engine`,以便在引擎还没实现某能力时先用它量出真实数据。

use std::collections::HashMap;

use memmap2::Mmap;

// ---------------------------------------------------------------------------
// 文件头
// ---------------------------------------------------------------------------

/// librime-octagram src/gram_db.h:
/// struct Metadata { char format[32]; uint32_t db_checksum;
///                   uint32_t double_array_size; OffsetPtr<char> double_array; }
/// = 44 字节，无 padding。
pub(crate) struct Header {
    pub(crate) format: String,
    pub(crate) db_checksum: u32,
    pub(crate) double_array_size: u32, // 单元数（GramDb::Build: array_size = trie_->size()）
    pub(crate) array_file_offset: usize, // double-array 在文件中的起始字节偏移
}

impl Header {
    pub(crate) fn parse(buf: &[u8]) -> Result<Header, String> {
        if buf.len() < 44 {
            return Err("文件头不足 44 字节".into());
        }
        let zero_pos = buf[..32].iter().position(|&b| b == 0).unwrap_or(32);
        let format = String::from_utf8_lossy(&buf[..zero_pos]).into_owned();
        let db_checksum = u32::from_le_bytes(buf[32..36].try_into().unwrap());
        let double_array_size = u32::from_le_bytes(buf[36..40].try_into().unwrap());
        // OffsetPtr<char> 是 int32，语义（librime mapped_file.h）：
        //   T* get() const { return (char*)&offset_ + offset_; }
        // 即“相对该偏移字段自身地址的符号偏移”。字段位于文件偏移 40。
        let ptr = i32::from_le_bytes(buf[40..44].try_into().unwrap());
        let array_file_offset = 40i64 + ptr as i64;
        if array_file_offset < 0 || array_file_offset as usize > buf.len() {
            return Err(format!("OffsetPtr 值异常: {ptr}"));
        }
        Ok(Header {
            format,
            db_checksum,
            double_array_size,
            array_file_offset: array_file_offset as usize,
        })
    }
}

// ---------------------------------------------------------------------------
// gram_encoding.cc 的 encode()：逐 Unicode 码点编码（非 UTF-8 原样）
// ---------------------------------------------------------------------------
pub(crate) fn encode_grammar(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in s.chars() {
        encode_cp(ch as u32, &mut out);
    }
    out
}

/// 单码点编码；语义与 C++ uint32 完全一致（含回绕）。
pub(crate) fn encode_cp(mut u: u32, out: &mut Vec<u8>) {
    if u < 0x80 {
        out.push(if u == 0 { 0xE0 } else { u as u8 });
    } else if (0x4000..0xA000).contains(&u) {
        if (u & 0xFF) == 0 {
            out.push(0xE1);
            out.push(((u >> 8) + 0x40) as u8);
        } else {
            out.push(((u >> 8) + 0x40) as u8);
            out.push((u & 0xFF) as u8);
        }
    } else {
        let mut bits = 32;
        while bits > 0 && (u & 0xFE00_0000) == 0 {
            bits -= 7;
            u = u.wrapping_shl(7);
        }
        let mut bytes_to_encode = (bits + 6) / 7;
        out.push(0xE0 | bytes_to_encode as u8);
        while bytes_to_encode > 0 {
            bytes_to_encode -= 1;
            out.push((((u >> 25) & 0x7F) | 0x80) as u8);
            u = u.wrapping_shl(7);
        }
    }
}

// ---------------------------------------------------------------------------
// double-array 访问（darts-clone 0.32：单元 4 字节）
// ---------------------------------------------------------------------------
pub(crate) struct DoubleArray<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) num_units: usize, // 单元个数
}

impl<'a> DoubleArray<'a> {
    pub(crate) fn new(mmap: &'a Mmap, header: &Header) -> Result<DoubleArray<'a>, String> {
        let num_units = header.double_array_size as usize;
        let bytes = num_units
            .checked_mul(4)
            .ok_or("double_array_size 溢出")?;
        if header.array_file_offset + bytes > mmap.len() {
            return Err(format!(
                "数组越界: offset {} + {} > 文件长度 {}",
                header.array_file_offset,
                bytes,
                mmap.len()
            ));
        }
        Ok(DoubleArray {
            data: &mmap[header.array_file_offset..header.array_file_offset + bytes],
            num_units,
        })
    }

    #[inline]
    pub(crate) fn unit(&self, idx: usize) -> Option<u32> {
        if idx >= self.num_units {
            return None;
        }
        let off = idx * 4;
        let b = self.data.get(off..off + 4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }
}

// darts-clone 0.32 DoubleArrayUnit 位布局（librime include/darts.h）
#[inline]
pub(crate) fn u_label(u: u32) -> u32 {
    u & 0x8000_00FF
}
#[inline]
pub(crate) fn u_offset(u: u32) -> usize {
    ((u >> 10) << ((u & 0x200) >> 6)) as usize
}
#[inline]
pub(crate) fn u_has_leaf(u: u32) -> bool {
    ((u >> 8) & 1) == 1
}
#[inline]
pub(crate) fn u_value(u: u32) -> u32 {
    u & 0x7FFF_FFFF
}

impl<'a> DoubleArray<'a> {
    /// 从 node_pos 沿 key 前进；返回 (成功?, 最终 node_pos, 已匹配字节数)
    pub(crate) fn traverse(&self, key: &[u8], start_node: usize) -> (bool, usize, usize) {
        let mut node = start_node;
        let mut matched = 0usize;
        for &b in key {
            let u = match self.unit(node) {
                Some(u) => u,
                None => return (false, node, matched),
            };
            node ^= u_offset(u) ^ b as usize;
            match self.unit(node) {
                Some(nu) if u_label(nu) as u8 == b => {
                    matched += 1;
                }
                _ => return (false, node, matched),
            }
        }
        (true, node, matched)
    }

    /// commonPrefixSearch：从 node_pos 出发在 key 上做前缀搜索。
    pub(crate) fn common_prefix_search(
        &self,
        key: &[u8],
        node_pos: usize,
        max_results: usize,
    ) -> Vec<(i32, usize)> {
        let mut res = Vec::new();
        let mut node = node_pos;
        let unit0 = match self.unit(node) {
            Some(u) => u,
            None => return res,
        };
        node ^= u_offset(unit0);
        let mut pos = 0usize;
        for &b in key {
            pos += 1;
            node ^= b as usize;
            let unit = match self.unit(node) {
                Some(u) => u,
                None => break,
            };
            if u_label(unit) as u8 != b {
                break;
            }
            let has_leaf = u_has_leaf(unit);
            node ^= u_offset(unit);
            if has_leaf {
                if let Some(leaf_unit) = self.unit(node) {
                    let v = u_value(leaf_unit) as i32;
                    res.push((v, pos));
                    if res.len() >= max_results {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        res
    }
}

// 解码匹配字节数 → unicode 字符数（gram_encoding.cc advance() 规则）
pub(crate) fn unicode_length(encoded: &[u8], byte_len: usize) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < byte_len {
        let b = encoded[i];
        let adv = if (b & 0x80) == 0 {
            1
        } else if (b & 0xF0) == 0xE0 {
            (b & 0x0F) as usize + 1
        } else {
            2
        };
        i += adv;
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// 查询（对齐 librime-octagram src/gram_db.cc 的 Lookup：
//   traverse(context) 若 key_pos == context.length() 则 commonPrefixSearch(word)）
// ---------------------------------------------------------------------------
#[allow(dead_code)]
pub(crate) fn lookup(
    da: &DoubleArray,
    context: &str,
    word: &str,
) -> Option<Vec<(i32, usize)>> {
    let ctx = encode_grammar(context);
    let wd = encode_grammar(word);
    let (ok, node, matched) = da.traverse(&ctx, 0);
    if !ok || matched != ctx.len() {
        return None;
    }
    Some(da.common_prefix_search(&wd, node, 8))
}

/// 对齐 octagram.cc Query() 的滑动窗口语义：
/// - context 取末尾 n 字（n = min(8, collocation_max_length-1)，本文档最大长度 6 → 5）
/// - word 取开头 n 字
/// - context 后缀从长到短逐级 Lookup（k 从 n 递减到 1）
/// 返回 (是否命中, 是否「整段 context 精确命中」, 最大 log 值)
pub(crate) fn query_like_octagram(
    da: &DoubleArray,
    context: &str,
    word: &str,
) -> (bool, bool, f64) {
    let n = 5usize;
    let ctx: Vec<char> = context.chars().collect();
    let wd: Vec<char> = word.chars().collect();
    let ctx_n = ctx.len().min(n);
    let wd_n = wd.len().min(n);
    let wd_str: String = wd[..wd_n].iter().collect();
    let wd_enc = encode_grammar(&wd_str);
    let mut best = -1e18f64;
    let mut any = false;
    let mut exact = false;
    for k in (1..=ctx_n).rev() {
        let sub: String = ctx[ctx.len() - k..].iter().collect();
        let ctx_enc = encode_grammar(&sub);
        let (ok, node, matched) = da.traverse(&ctx_enc, 0);
        if !ok || matched != ctx_enc.len() {
            continue;
        }
        let res = da.common_prefix_search(&wd_enc, node, 8);
        if !res.is_empty() {
            any = true;
            if k == ctx.len() {
                exact = true;
            }
            for (v, _) in res {
                best = best.max(v as f64 / 10000.0);
            }
        }
    }
    (any, exact, best)
}

// ---------------------------------------------------------------------------
// 键枚举：从根 DFS，还原全部键（编码字节）
// ---------------------------------------------------------------------------
pub(crate) fn enumerate_keys(
    da: &DoubleArray,
    mut visit: impl FnMut(&[u8], i32) -> bool,
) {
    /// DFS 统计：被剪枝的伪路径数（double-array 中偶发 label 误匹配/环）
    struct Ctx {
        pruned: u64,
        capped: u64,
    }
    pub(crate) fn walk(
        da: &DoubleArray,
        pos: usize,
        path: &mut Vec<u8>,
        path_pos: &mut Vec<usize>,
        depth: usize,
        cull: &mut Ctx,
        visit: &mut impl FnMut(&[u8], i32) -> bool,
    ) -> bool {
        let Some(u) = da.unit(pos) else {
            return true;
        };
        let base = pos ^ u_offset(u);
        if u_has_leaf(u) {
            if let Some(leaf) = da.unit(base) {
                if !visit(path, u_value(leaf) as i32) {
                    return false;
                }
            }
        }
        for b in 1u16..=255u16 {
            let cpos = base ^ b as usize;
            if path_pos.contains(&cpos) {
                cull.pruned += 1; // 位置环：同一路径上重复访问 → 伪节点
                continue;
            }
            let Some(nu) = da.unit(cpos) else {
                continue;
            };
            if u_label(nu) as u8 == b as u8 {
                if depth >= 48 {
                    cull.capped += 1; // 超过合理键长（≤32B），剪枝
                    continue;
                }
                path.push(b as u8);
                path_pos.push(cpos);
                let ok = walk(da, cpos, path, path_pos, depth + 1, cull, visit);
                path_pos.pop();
                path.pop();
                if !ok {
                    return false;
                }
            }
        }
        true
    }
    let mut cull = Ctx { pruned: 0, capped: 0 };
    let mut path = Vec::with_capacity(24);
    let mut path_pos = Vec::with_capacity(24);
    walk(da, 0, &mut path, &mut path_pos, 0, &mut cull, &mut visit);
    if cull.pruned > 0 || cull.capped > 0 {
        eprintln!(
            "[enum] 剪枝统计: 疑似伪节点环 {} 个, 超深 {} 个（不影响真键）",
            cull.pruned, cull.capped
        );
    }
}

// ---------------------------------------------------------------------------
// 键解码：编码在 0x4000..0xA000 之外非双射，用「码点 → 编码」反查表。
// ---------------------------------------------------------------------------
pub(crate) fn build_decode_map() -> HashMap<Vec<u8>, char> {
    let mut map: HashMap<Vec<u8>, char> = HashMap::new();
    for (lo, hi) in [
        (0x0000u32, 0xD7FFu32),
        (0xE000u32, 0x2FFFFu32),
        (0x30000u32, 0x3134Au32),
    ] {
        for cp in lo..=hi {
            if (0xD800..0xE000).contains(&cp) {
                continue;
            }
            let mut enc = Vec::with_capacity(4);
            encode_cp(cp, &mut enc);
            map.entry(enc).or_insert_with(|| char::from_u32(cp).unwrap_or('�'));
        }
    }
    map
}

/// 把键的编码字节解码成显示文本；未知字节按 advance() 步长输出 <hex>。
pub(crate) fn decode_key_display(encoded: &[u8], map: &HashMap<Vec<u8>, char>) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < encoded.len() {
        let b = encoded[i];
        let adv = if (b & 0x80) == 0 {
            1
        } else if (b & 0xF0) == 0xE0 {
            (b & 0x0F) as usize + 1
        } else {
            2
        };
        let end = (i + adv).min(encoded.len());
        match map.get(&encoded[i..end]) {
            Some(c) => s.push(*c),
            None => s.push_str(&format!("<{:02x}>", b)),
        }
        i = end;
    }
    s
}
