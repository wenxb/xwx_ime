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
