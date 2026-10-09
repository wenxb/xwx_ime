//! 英文词库 reader 的测试：合成二进制走同一条 reader，oracle 独立实现。
//!
//! 这里**不验证构建端**（那是 `scripts/tests/test_english*.py` 的事），
//! 只验证「读出来的东西与一个独立全扫描实现一致」以及「坏文件全部报错」。

use super::dict::{static_score_centi, EnglishDict};
use super::format::{
    HEADER_LEN, MAGIC, PREFIX_RECORD_LEN, PREFIX_TOP_LIMIT, VERSION, WORD_RECORD_LEN,
};

/// 合成词条。`q` 是百分之一 Zipf。
#[derive(Clone, Debug)]
struct W {
    key: &'static str,
    display: &'static str,
    q: u16,
}

/// 测试用的最小构建器（**独立于 Python 构建脚本**，按同一份格式规范实现）。
fn build(words: &[W]) -> Vec<u8> {
    let mut ws: Vec<W> = words.to_vec();
    ws.sort_by(|a, b| a.key.cmp(b.key));

    // —— 前缀 Top-32：按 (静态分降序, key 升序) 遍历，向未满的前缀表追加 ——
    let mut order: Vec<usize> = (0..ws.len()).collect();
    order.sort_by(|&a, &b| {
        let sa = static_score_centi(ws[a].q, ws[a].key.len(), 0);
        let sb = static_score_centi(ws[b].q, ws[b].key.len(), 0);
        // 长度在静态分里已扣过，这里只比较等价量：q - 15*len
        sb.cmp(&sa).then_with(|| ws[a].key.cmp(ws[b].key))
    });
    let mut pfx: std::collections::BTreeMap<String, Vec<u32>> = std::collections::BTreeMap::new();
    for &id in &order {
        let k = ws[id].key;
        for end in k.char_indices().skip(1).map(|(i, _)| i).chain(std::iter::once(k.len())) {
            let p = &k[..end];
            let list = pfx.entry(p.to_string()).or_default();
            if list.len() < PREFIX_TOP_LIMIT {
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

fn patch_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn patch_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn patch_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// 独立 oracle：直接对词表做一次全扫描，不复用 reader 的任何排序代码。
fn oracle_prefix_top(words: &[W], prefix: &str) -> Vec<(i32, String)> {
    let mut hits: Vec<(i32, String)> = words
        .iter()
        .filter(|w| w.key.starts_with(prefix))
        .map(|w| {
            let extra = w.key.len() - prefix.len();
            (w.q as i32 - 15 * extra as i32, w.key.to_string())
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hits.truncate(PREFIX_TOP_LIMIT);
    hits
}

/// 生成纯字母后缀的 key（键规则只允许 ASCII 字母，不能带数字）。
fn alpha_key(prefix: &str, i: usize) -> String {
    let a = (b'a' + (i / 26) as u8) as char;
    let b = (b'a' + (i % 26) as u8) as char;
    format!("{prefix}{a}{b}")
}

const BASE: &[W] = &[
    W { key: "a", display: "a", q: 700 },
    W { key: "an", display: "an", q: 600 },
    W { key: "he", display: "he", q: 600 },
    W { key: "hello", display: "hello", q: 450 },
    W { key: "help", display: "help", q: 520 },
    W { key: "python", display: "Python", q: 410 },
    W { key: "docker", display: "Docker", q: 410 },
];

#[test]
fn exact_matches_independent_oracle() {
    let dict = EnglishDict::from_bytes(build(BASE)).expect("合成文件应可读");
    assert_eq!(dict.word_count(), BASE.len());
    for w in BASE {
        let e = dict.exact(w.key).expect("查询不应失败").expect("应命中");
        assert_eq!(e.key, w.key);
        assert_eq!(e.display, w.display, "显示形式用规范大小写");
        assert_eq!(e.zipf_centi, w.q);
    }
    assert!(dict.exact("pytho").expect("查询不应失败").is_none());
    assert!(dict.contains("python"));
    assert!(!dict.contains("pytho"));
}

#[test]
fn prefix_top_matches_full_scan_oracle_for_40_same_prefix_words() {
    // 40 个同前缀词：刻意让字典序与静态分序不一致（越靠后的 q 越高）。
    let keys: Vec<String> = (0..40).map(|i| alpha_key("p", i)).collect();
    let words: Vec<W> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| W {
            key: Box::leak(k.clone().into_boxed_str()),
            display: Box::leak(k.clone().into_boxed_str()),
            q: 300 + i as u16,
        })
        .collect();
    let dict = EnglishDict::from_bytes(build(&words)).expect("合成文件应可读");
    let got: Vec<(i32, String)> = dict
        .prefix_top("p")
        .expect("前缀查询不应失败")
        .iter()
        .map(|e| (static_score_centi(e.zipf_centi, e.key.len(), 1), e.key.to_string()))
        .collect();
    assert_eq!(got.len(), PREFIX_TOP_LIMIT, "缓存已满时正好 32 条");
    assert_eq!(got, oracle_prefix_top(&words, "p"));
}

#[test]
fn dictionary_order_last_but_high_frequency_enters_top32() {
    // 「字典序找前 32 个就返回」会把 zzzz… 截掉：它的字典序在最后。
    let mut words: Vec<W> = (0..40)
        .map(|i| {
            let k: &'static str = Box::leak(alpha_key("aaa", i).into_boxed_str());
            W { key: k, display: k, q: 300 }
        })
        .collect();
    words.push(W { key: "aaazz", display: "aaazz", q: 900 });
    let dict = EnglishDict::from_bytes(build(&words)).expect("合成文件应可读");
    let top = dict.prefix_top("aaa").expect("前缀查询不应失败");
    assert_eq!(top.len(), PREFIX_TOP_LIMIT);
    assert_eq!(top[0].key, "aaazz", "字典序末尾的高频词必须在缓存首位");
    let got: Vec<(i32, String)> = top
        .iter()
        .map(|e| (static_score_centi(e.zipf_centi, e.key.len(), 3), e.key.to_string()))
        .collect();
    assert_eq!(got, oracle_prefix_top(&words, "aaa"));
}

#[test]
fn low_frequency_exact_word_missing_from_top32_is_still_exact() {
    let mut words: Vec<W> = (0..40)
        .map(|i| {
            let k: &'static str = Box::leak(alpha_key("ppp", i).into_boxed_str());
            W { key: k, display: k, q: 500 }
        })
        .collect();
    words.push(W { key: "ppptarget", display: "ppptarget", q: 105 });
    let dict = EnglishDict::from_bytes(build(&words)).expect("合成文件应可读");
    let top = dict.prefix_top("ppp").expect("前缀查询不应失败");
    assert!(
        !top.iter().any(|e| e.key == "ppptarget"),
        "低频词不该挤进 Top-32（否则这条测试证明不了精确召回的独立性）"
    );
    let e = dict.exact("ppptarget").expect("查询不应失败").expect("精确必须命中");
    assert_eq!(e.zipf_centi, 105);
}

#[test]
fn empty_or_oversized_prefix_returns_empty_without_touching_index() {
    let dict = EnglishDict::from_bytes(build(BASE)).expect("合成文件应可读");
    assert!(dict.prefix_top("").expect("空前缀不应失败").is_empty());
    assert!(dict.prefix_top(&"a".repeat(49)).expect("超长前缀不应失败").is_empty());
    assert!(dict.prefix_top("你").expect("非 ASCII 前缀不应失败").is_empty());
}

#[test]
fn truncated_file_is_rejected() {
    let full = build(BASE);
    for cut in [0usize, 8, 40, HEADER_LEN - 1] {
        let e = EnglishDict::from_bytes(full[..cut].to_vec()).unwrap_err();
        assert!(format!("{e}").contains("损坏"), "cut={cut}: {e}");
    }
    // 头声明 file_len 大于实际长度
    let mut b = full.clone();
    patch_u64(&mut b, 56, full.len() as u64 + 1);
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn bad_magic_and_version_are_rejected() {
    let mut b = build(BASE);
    b[0] = b'Y';
    assert!(EnglishDict::from_bytes(b).is_err());

    let mut b = build(BASE);
    patch_u32(&mut b, 8, 2);
    assert!(EnglishDict::from_bytes(b).is_err());

    let mut b = build(BASE);
    patch_u32(&mut b, 12, 32);
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn u64_extremes_and_overlapping_sections_are_rejected() {
    // words_offset 极大 → u64→usize 或区段顺序校验必须拦住
    let mut b = build(BASE);
    patch_u64(&mut b, 24, u64::MAX);
    assert!(EnglishDict::from_bytes(b).is_err());

    // prefixes_offset 落在 words 区里（重叠）
    let mut b = build(BASE);
    let words_end = HEADER_LEN + BASE.len() * WORD_RECORD_LEN;
    patch_u64(&mut b, 32, (words_end - 1) as u64);
    assert!(EnglishDict::from_bytes(b).is_err());

    // ids 区起点越到 strings 之后
    let mut b = build(BASE);
    let strings_off = u64::from_le_bytes(b[48..56].try_into().unwrap());
    patch_u64(&mut b, 40, strings_off + 1);
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn non_multiple_of_four_ids_section_is_rejected() {
    let mut b = build(BASE);
    let ids_off = u64::from_le_bytes(b[40..48].try_into().unwrap());
    let strings_off = u64::from_le_bytes(b[48..56].try_into().unwrap());
    assert!((strings_off - ids_off) % 4 == 0);
    // 把 strings 区往前挪 2 字节：file_len 同步改，区段顺序仍合法，只有 ids 长度不合法
    patch_u64(&mut b, 48, strings_off - 1);
    let new_len = (b.len() - 1) as u64;
    patch_u64(&mut b, 56, new_len);
    b.truncate(new_len as usize);
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn unsorted_or_duplicate_keys_are_rejected() {
    let words = [
        W { key: "bb", display: "bb", q: 300 },
        W { key: "aa", display: "aa", q: 300 },
    ];
    let mut b = build(&words);
    // 交换两条 word 记录，破坏 key 升序
    let a = HEADER_LEN;
    let c = HEADER_LEN + WORD_RECORD_LEN;
    for i in 0..WORD_RECORD_LEN {
        b.swap(a + i, c + i);
    }
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn bad_id_q_and_reserved_are_rejected() {
    // q > 900
    let mut b = build(BASE);
    patch_u16(&mut b, HEADER_LEN + 12, 901);
    assert!(EnglishDict::from_bytes(b).is_err());

    // 非零 reserved（word）
    let mut b = build(BASE);
    patch_u16(&mut b, HEADER_LEN + 14, 1);
    assert!(EnglishDict::from_bytes(b).is_err());

    // 非零 reserved（prefix）
    let mut b = build(BASE);
    let prefixes_off = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
    patch_u32(&mut b, prefixes_off + 12, 7);
    assert!(EnglishDict::from_bytes(b).is_err());

    // ids 引用不存在的 word_id
    let mut b = build(BASE);
    let ids_off = u64::from_le_bytes(b[40..48].try_into().unwrap()) as usize;
    patch_u32(&mut b, ids_off, 9999);
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn duplicate_word_id_in_prefix_list_is_rejected() {
    let words = [
        W { key: "ab", display: "ab", q: 400 },
        W { key: "ac", display: "ac", q: 300 },
    ];
    let mut b = build(&words);
    // 前缀 "a" 的记录：把第 2 个引用改成第 1 个（重复）
    let prefixes_off = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
    let ids_off = u64::from_le_bytes(b[40..48].try_into().unwrap()) as usize;
    let mut patched = false;
    for i in 0..2 {
        let off = prefixes_off + i * PREFIX_RECORD_LEN;
        let p_len = u16::from_le_bytes(b[off + 4..off + 6].try_into().unwrap()) as usize;
        if p_len == 1 {
            let start = u32::from_le_bytes(b[off + 8..off + 12].try_into().unwrap()) as usize;
            patch_u32(&mut b, ids_off + start * 4 + 4, 0);
            patched = true;
            break;
        }
    }
    assert!(patched, "应找到前缀 \"a\" 的记录");
    assert!(EnglishDict::from_bytes(b).is_err());
}

#[test]
fn display_must_lowercase_to_key() {
    let words = [W { key: "python", display: "Py thon", q: 400 }];
    assert!(EnglishDict::from_bytes(build(&words)).is_err());
    let words = [W { key: "python", display: "Python!", q: 400 }];
    assert!(EnglishDict::from_bytes(build(&words)).is_err());
}

#[test]
fn non_ascii_key_is_rejected() {
    let words = [W { key: "pythön", display: "pythön", q: 400 }];
    // 合成器本身不拦，校验必须拦
    assert!(EnglishDict::from_bytes(build(&words)).is_err());
}

#[test]
fn static_score_centi_never_underflows() {
    assert_eq!(static_score_centi(410, 6, 4), 380, "pyth → Python: 4.10 - 0.30");
    assert_eq!(static_score_centi(450, 5, 5), 450, "hello 精确补全无 extra");
    assert_eq!(static_score_centi(0, 48, 1), -705, "极短 q 也不回绕");
    assert_eq!(static_score_centi(900, 1, 48), 900, "前缀比词长时不加分");
}
