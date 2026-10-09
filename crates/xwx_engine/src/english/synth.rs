//! 测试专用的最小 `english.bin` 构建器与坏文件改字节辅助。
//!
//! **独立于 Python 构建脚本**：它按同一份格式规范重写一遍，用来验证
//! reader 与召回逻辑，而不是验证构建端（那是 `scripts/tests/` 的事）。

use super::format::{HEADER_LEN, MAGIC, VERSION};


/// 合成词条。`q` 是百分之一 Zipf。
#[derive(Clone, Debug)]
pub(crate) struct W {
    pub(crate) key: &'static str,
    pub(crate) display: &'static str,
    pub(crate) q: u16,
}

/// 测试用的最小构建器（**独立于 Python 构建脚本**，按同一份格式规范实现）。
pub(crate) fn build(words: &[W]) -> Vec<u8> {
    let mut ws: Vec<W> = words.to_vec();
    ws.sort_by(|a, b| a.key.cmp(b.key));

    // —— 前缀 Top-32：按 (静态分降序, key 升序) 遍历，向未满的前缀表追加 ——
    let mut order: Vec<usize> = (0..ws.len()).collect();
    order.sort_by(|&a, &b| {
        let sa = crate::english::static_score_centi(ws[a].q, ws[a].key.len(), 0);
        let sb = crate::english::static_score_centi(ws[b].q, ws[b].key.len(), 0);
        // 长度在静态分里已扣过，这里只比较等价量：q - 15*len
        sb.cmp(&sa).then_with(|| ws[a].key.cmp(ws[b].key))
    });
    let mut pfx: std::collections::BTreeMap<String, Vec<u32>> = std::collections::BTreeMap::new();
    for &id in &order {
        let k = ws[id].key;
        for end in k.char_indices().skip(1).map(|(i, _)| i).chain(std::iter::once(k.len())) {
            let p = &k[..end];
            let list = pfx.entry(p.to_string()).or_default();
            if list.len() < super::format::PREFIX_TOP_LIMIT {
                list.push(id as u32);
            }
        }
    }

    // —— strings 池 ——
    let mut strings: Vec<u8> = Vec::new();
    let mut words_buf: Vec<u8> = Vec::new();
    for w in &ws {
        let k_off = strings.len() as u32;
        strings.extend_from_slice(w.key.as_bytes());
        let d_off = strings.len() as u32;
        strings.extend_from_slice(w.display.as_bytes());
        words_buf.extend_from_slice(&k_off.to_le_bytes());
        words_buf.extend_from_slice(&d_off.to_le_bytes());
        words_buf.extend_from_slice(&(w.key.len() as u16).to_le_bytes());
        words_buf.extend_from_slice(&(w.display.len() as u16).to_le_bytes());
        words_buf.extend_from_slice(&w.q.to_le_bytes());
        words_buf.extend_from_slice(&0u16.to_le_bytes());
    }
    let mut pfx_buf: Vec<u8> = Vec::new();
    let mut ids_buf: Vec<u8> = Vec::new();
    for (p, list) in &pfx {
        let p_off = strings.len() as u32;
        strings.extend_from_slice(p.as_bytes());
        pfx_buf.extend_from_slice(&p_off.to_le_bytes());
        pfx_buf.extend_from_slice(&(p.len() as u16).to_le_bytes());
        pfx_buf.extend_from_slice(&(list.len() as u16).to_le_bytes());
        pfx_buf.extend_from_slice(&((ids_buf.len() / 4) as u32).to_le_bytes());
        pfx_buf.extend_from_slice(&0u32.to_le_bytes());
        for id in list {
            ids_buf.extend_from_slice(&id.to_le_bytes());
        }
    }

    let words_off = HEADER_LEN;
    let prefixes_off = words_off + words_buf.len();
    let ids_off = prefixes_off + pfx_buf.len();
    let strings_off = ids_off + ids_buf.len();
    let file_len = strings_off + strings.len();

    let mut out = Vec::with_capacity(file_len);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    out.extend_from_slice(&(ws.len() as u32).to_le_bytes());
    out.extend_from_slice(&(pfx.len() as u32).to_le_bytes());
    out.extend_from_slice(&(words_off as u64).to_le_bytes());
    out.extend_from_slice(&(prefixes_off as u64).to_le_bytes());
    out.extend_from_slice(&(ids_off as u64).to_le_bytes());
    out.extend_from_slice(&(strings_off as u64).to_le_bytes());
    out.extend_from_slice(&(file_len as u64).to_le_bytes());
    out.extend_from_slice(&words_buf);
    out.extend_from_slice(&pfx_buf);
    out.extend_from_slice(&ids_buf);
    out.extend_from_slice(&strings);
    out
}

/// 合成一条二元边：`(prev, next, count)`。
#[derive(Clone, Copy, Debug)]
pub(crate) struct BigramEdgeSpec {
    pub(crate) prev: &'static str,
    pub(crate) next: &'static str,
    pub(crate) count: u64,
}

/// 测试用的最小 `english_bigram.bin` 构建器（与 Python 端同一份格式规范）。
///
/// 行内按 (count 降序, next 升序) 排；`observed_total` 取该 prev 下**全部**
/// 边的和（测试夹具不超过 32 条边，不会触发截断）。
pub(crate) fn build_bigram(edges: &[BigramEdgeSpec]) -> Vec<u8> {
    use super::bigram::{BIGRAM_HEADER_LEN, BIGRAM_MAGIC, BIGRAM_VERSION};
    let mut by_prev: std::collections::BTreeMap<&str, Vec<(&str, u64)>> =
        std::collections::BTreeMap::new();
    for e in edges {
        by_prev.entry(e.prev).or_default().push((e.next, e.count));
    }
    let mut strings: Vec<u8> = Vec::new();
    let mut ctx_buf: Vec<u8> = Vec::new();
    let mut edge_buf: Vec<u8> = Vec::new();
    let intern = |s: &str, strings: &mut Vec<u8>| -> (u32, u16) {
        let off = strings.len() as u32;
        strings.extend_from_slice(s.as_bytes());
        (off, s.len() as u16)
    };
    let mut n_edge = 0usize;
    for (prev, list) in &by_prev {
        let mut rows = list.clone();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let total: u64 = rows.iter().map(|(_, c)| *c).sum();
        let (p_off, p_len) = intern(prev, &mut strings);
        ctx_buf.extend_from_slice(&p_off.to_le_bytes());
        ctx_buf.extend_from_slice(&p_len.to_le_bytes());
        ctx_buf.extend_from_slice(&(rows.len() as u16).to_le_bytes());
        ctx_buf.extend_from_slice(&(n_edge as u32).to_le_bytes());
        ctx_buf.extend_from_slice(&0u32.to_le_bytes());
        ctx_buf.extend_from_slice(&total.to_le_bytes());
        for (next, count) in rows {
            let (n_off, n_len) = intern(next, &mut strings);
            edge_buf.extend_from_slice(&n_off.to_le_bytes());
            edge_buf.extend_from_slice(&n_len.to_le_bytes());
            edge_buf.extend_from_slice(&0u16.to_le_bytes());
            edge_buf.extend_from_slice(&count.to_le_bytes());
            n_edge += 1;
        }
    }
    let ctx_off = BIGRAM_HEADER_LEN;
    let edge_off = ctx_off + ctx_buf.len();
    let strings_off = edge_off + edge_buf.len();
    let file_len = strings_off + strings.len();
    let mut out = Vec::with_capacity(file_len);
    out.extend_from_slice(&BIGRAM_MAGIC);
    out.extend_from_slice(&BIGRAM_VERSION.to_le_bytes());
    out.extend_from_slice(&(BIGRAM_HEADER_LEN as u32).to_le_bytes());
    out.extend_from_slice(&(by_prev.len() as u32).to_le_bytes());
    out.extend_from_slice(&(n_edge as u32).to_le_bytes());
    out.extend_from_slice(&(ctx_off as u64).to_le_bytes());
    out.extend_from_slice(&(edge_off as u64).to_le_bytes());
    out.extend_from_slice(&(strings_off as u64).to_le_bytes());
    out.extend_from_slice(&(file_len as u64).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&ctx_buf);
    out.extend_from_slice(&edge_buf);
    out.extend_from_slice(&strings);
    out
}

pub(crate) fn patch_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn patch_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn patch_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}


/// 生成纯字母后缀的 key（键规则只允许 ASCII 字母，不能带数字）。
pub(crate) fn alpha_key(prefix: &str, i: usize) -> String {
    let a = (b'a' + (i / 26) as u8) as char;
    let b = (b'a' + (i % 26) as u8) as char;
    format!("{prefix}{a}{b}")
}
