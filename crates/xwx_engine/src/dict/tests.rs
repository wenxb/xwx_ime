//! `dict` 模块单测（全部基于合成二进制，零本地 `data/*.bin` 依赖）。

use std::fs;

use super::*;

type BinGroup<'a, S> = (&'a [u8], Vec<(S, u32)>);
type TestGroups<'a> = Vec<BinGroup<'a, &'a str>>;
type TestAbbrevGroups<'a> = Vec<(&'a [u8], Vec<(&'a str, &'a str, u32)>)>;

/// 按 dict.bin v1/v2 布局手工构造词库（`groups` 须按拼音字节序严格升序，组内 freq 降序）。
fn build_bin_from<S: AsRef<str>>(version: u32, groups: &[BinGroup<'_, S>]) -> Vec<u8> {
    assert!(groups.windows(2).all(|w| w[0].0 < w[1].0), "测试词库拼音未升序");
    let mut strings = Vec::new();
    let mut entries = Vec::new();
    let mut records = Vec::new();
    let mut total = 0u64;
    let mut freq_sum = 0u64;
    for (py, ws) in groups {
        let str_off = strings.len() as u32;
        strings.extend_from_slice(py);
        let ent_off = entries.len() as u32;
        let ent_start = entries.len();
        for (w, f) in ws {
            let wb = w.as_ref().as_bytes();
            entries.push(wb.len() as u8);
            entries.extend_from_slice(wb);
            entries.extend_from_slice(&f.to_le_bytes());
            freq_sum += *f as u64;
        }
        records.extend_from_slice(&str_off.to_le_bytes());
        records.extend_from_slice(&(py.len() as u16).to_le_bytes());
        records.extend_from_slice(&ent_off.to_le_bytes());
        records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
        total += ws.len() as u64;
    }
    let header_len = if version >= 2 { MMAP_HEADER_LEN } else { MMAP_HEADER_LEN_V1 };
    let records_off = header_len as u64;
    let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
    let entries_off = strings_off + strings.len() as u64;

    let mut out = Vec::new();
    out.extend_from_slice(MMAP_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&records_off.to_le_bytes());
    out.extend_from_slice(&strings_off.to_le_bytes());
    out.extend_from_slice(&entries_off.to_le_bytes());
    if version >= 2 {
        out.extend_from_slice(&freq_sum.to_le_bytes());
    }
    out.extend_from_slice(&records);
    out.extend_from_slice(&strings);
    out.extend_from_slice(&entries);
    out
}

/// 迷你测试词库（供本模块及 `builder`/`syllable_graph` 测试复用）。
pub(super) fn build_test_bin(version: u32) -> Vec<u8> {
    let groups: TestGroups = vec![
        (b"hao", vec![("好", 80)]),
        (b"ni", vec![("你", 300), ("妮", 200), ("尼", 20)]),
        (b"ni hao", vec![("你好", 400)]),
    ];
    build_bin_from(version, &groups)
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("xwx_test_dict_{name}_{}.bin", std::process::id()))
}

fn build_test_abbrev_bin(version: u32) -> Vec<u8> {
    let groups: TestAbbrevGroups = vec![
        (b"bjdx", vec![("北京大学", "bei jing da xue", 211845)]),
        (b"zgr", vec![
            ("中国人", "zhong guo ren", 451625),
            ("这个人", "zhe ge ren", 167480),
        ]),
    ];
    let mut buckets: [Vec<&str>; 26] = std::array::from_fn(|_| Vec::new());
    let mut syl_table = Vec::new();
    if version >= 2 {
        for (_, ws) in &groups {
            for &(_, py, _) in ws {
                for syl in py.split(' ') {
                    let b = &mut buckets[(syl.as_bytes()[0] - b'a') as usize];
                    if !b.contains(&syl) {
                        b.push(syl);
                        b.sort_unstable();
                    }
                }
            }
        }
        for b in &buckets {
            syl_table.push(b.len() as u8);
            for s in b {
                syl_table.push(s.len() as u8);
                syl_table.extend_from_slice(s.as_bytes());
            }
        }
    }
    let mut strings = Vec::new();
    let mut entries = Vec::new();
    let mut records = Vec::new();
    let mut total = 0u64;
    for (key, ws) in &groups {
        let str_off = strings.len() as u32;
        strings.extend_from_slice(key);
        let ent_off = entries.len() as u32;
        let ent_start = entries.len();
        for (w, py, f) in ws {
            entries.push(w.len() as u8);
            entries.extend_from_slice(w.as_bytes());
            if version >= 2 {
                for syl in py.split(' ') {
                    let b = &buckets[(syl.as_bytes()[0] - b'a') as usize];
                    entries.push(b.iter().position(|&x| x == syl).unwrap() as u8);
                }
            } else {
                entries.push(py.len() as u8);
                entries.extend_from_slice(py.as_bytes());
            }
            entries.extend_from_slice(&f.to_le_bytes());
        }
        records.extend_from_slice(&str_off.to_le_bytes());
        records.extend_from_slice(&(key.len() as u16).to_le_bytes());
        records.extend_from_slice(&ent_off.to_le_bytes());
        records.extend_from_slice(&((entries.len() - ent_start) as u16).to_le_bytes());
        total += ws.len() as u64;
    }
    let records_off = (ABBREV_HEADER_LEN + syl_table.len()) as u64;
    let strings_off = records_off + (groups.len() as u64) * MMAP_RECORD_LEN as u64;
    let entries_off = strings_off + strings.len() as u64;
    let mut out = Vec::new();
    out.extend_from_slice(ABBREV_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(groups.len() as u64).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&records_off.to_le_bytes());
    out.extend_from_slice(&strings_off.to_le_bytes());
    out.extend_from_slice(&entries_off.to_le_bytes());
    out.extend_from_slice(&syl_table);
    out.extend_from_slice(&records);
    out.extend_from_slice(&strings);
    out.extend_from_slice(&entries);
    out
}

#[test]
fn mmap_lookup_exact() {
    let path = temp_path("lookup");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    let ni = dict.lookup("ni").unwrap();
    let words: Vec<(&str, u64)> = ni.iter().map(|c| (c.word.as_str(), c.freq)).collect();
    assert_eq!(words, vec![("你", 300), ("妮", 200), ("尼", 20)]);
    assert!(ni.iter().all(|c| c.pinyin == "ni"));
    assert_eq!(dict.lookup("hao").unwrap().len(), 1);
    assert!(dict.lookup("x").unwrap().is_empty());
}

#[test]
fn mmap_lookup_phrase_with_space() {
    let path = temp_path("phrase");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    let hi = dict.lookup("ni hao").unwrap();
    assert_eq!(hi.len(), 1);
    assert_eq!(hi[0].word, "你好");
}

#[test]
fn mmap_lookup_prefix_range() {
    let path = temp_path("prefix");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    let words: Vec<String> = dict.lookup_prefix("n").unwrap().into_iter().map(|c| c.word).collect();
    assert_eq!(words, vec!["你", "妮", "尼", "你好"]);
    assert!(dict.lookup_prefix("x").unwrap().is_empty());
}

#[test]
fn v2_header_supplies_total_freq_without_scan() {
    let path = temp_path("v2_total");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    assert_eq!(dict.total_freq().unwrap(), 1000);
    assert!(dict.total_freq.get().is_some(), "v2 不该留待懒扫描");
}

#[test]
fn v1_file_still_loads_and_scans_total_freq() {
    let path = temp_path("v1_total");
    fs::write(&path, build_test_bin(1)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    assert_eq!(dict.total_freq().unwrap(), 1000);
    assert_eq!(dict.lookup("ni").unwrap().len(), 3);
}

#[test]
fn mmap_rejects_corrupt_magic() {
    let path = temp_path("corrupt");
    fs::write(&path, b"this is not a dict.bin file").unwrap();
    assert!(MmapDict::open(&path).is_err());
    fs::remove_file(&path).ok();
}

#[test]
fn mmap_rejects_version_mismatch() {
    let path = temp_path("version");
    fs::write(&path, build_test_bin(MMAP_VERSION + 1)).unwrap();
    assert!(MmapDict::open(&path).is_err());
    fs::remove_file(&path).ok();
}

#[test]
fn dict_store_routes_to_sqlite() {
    let store = DictStore::Sqlite(SqliteDict::open_in_memory().unwrap());
    store.upsert("你好", "ni hao", 10).unwrap();
    assert_eq!(store.lookup("ni hao").unwrap().len(), 1);
    assert!(!store.lookup_prefix("ni").unwrap().is_empty());
}

#[test]
fn abbrev_index_lookup_pure() {
    for version in [1, 2] {
        let path = temp_path(&format!("abbrev_pure_v{version}"));
        fs::write(&path, build_test_abbrev_bin(version)).unwrap();
        let idx = AbbrevIndex::open(&path).unwrap();
        fs::remove_file(&path).ok();
        let res = idx.lookup_prefix("zgr").unwrap();
        assert!(res.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"));
        assert_eq!(res[0].word, "中国人");
        let bjdx = idx.lookup_prefix("bjdx").unwrap();
        assert!(bjdx.iter().any(|c| c.word == "北京大学" && c.pinyin == "bei jing da xue"));
        assert!(idx.lookup_prefix("q").unwrap().is_empty());
    }
}

#[test]
fn abbrev_index_rejects_bad_magic() {
    let path = temp_path("abbrev_bad");
    fs::write(&path, b"not an abbrev index").unwrap();
    assert!(AbbrevIndex::open(&path).is_err());
    fs::remove_file(&path).ok();
}

#[test]
fn mmap_dict_without_abbrev_degrades_gracefully() {
    let path = temp_path("no_abbrev");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).ok();

    assert!(!dict.has_abbrev());
    assert!(dict.lookup_abbrev("zgr").unwrap().is_empty());
    assert_eq!(dict.lookup("ni").unwrap().len(), 3);
}

#[test]
fn mmap_dict_with_missing_abbrev_file_still_works() {
    let path = temp_path("dict_ok");
    fs::write(&path, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&path).unwrap().with_abbrev("/nonexistent/path/abbrev.bin");
    fs::remove_file(&path).ok();

    assert!(!dict.has_abbrev());
    assert!(dict.lookup_abbrev("zgr").unwrap().is_empty());
    assert_eq!(dict.lookup("ni hao").unwrap()[0].word, "你好");
}

#[test]
fn corrupt_section_offsets_are_rejected_at_open() {
    let path = temp_path("bad_header");
    let mut hdr = Vec::new();
    hdr.extend_from_slice(MMAP_MAGIC);
    hdr.extend_from_slice(&MMAP_VERSION.to_le_bytes());
    hdr.extend_from_slice(&1u64.to_le_bytes()); // pinyin_count
    hdr.extend_from_slice(&0u64.to_le_bytes()); // total_entries
    hdr.extend_from_slice(&200u64.to_le_bytes()); // records_off > strings_off
    hdr.extend_from_slice(&100u64.to_le_bytes()); // strings_off
    hdr.extend_from_slice(&300u64.to_le_bytes()); // entries_off
    hdr.extend_from_slice(&0u64.to_le_bytes()); // total_freq
    hdr.resize(512, 0);
    fs::write(&path, &hdr).unwrap();
    let msg = format!("{}", MmapDict::open(&path).expect_err("区段非法必须被拒绝"));
    fs::remove_file(&path).ok();
    assert!(msg.contains("区段") || msg.contains("越界"), "{msg}");
}

#[test]
fn corrupt_abbrev_section_offsets_are_rejected_at_open() {
    let path = temp_path("bad_abbrev_header");
    let mut hdr = Vec::new();
    hdr.extend_from_slice(ABBREV_MAGIC);
    hdr.extend_from_slice(&ABBREV_VERSION.to_le_bytes());
    hdr.extend_from_slice(&1u64.to_le_bytes()); // key_count
    hdr.extend_from_slice(&0u64.to_le_bytes());
    hdr.extend_from_slice(&400u64.to_le_bytes()); // records_off > strings_off
    hdr.extend_from_slice(&200u64.to_le_bytes()); // strings_off
    hdr.extend_from_slice(&300u64.to_le_bytes()); // entries_off
    hdr.resize(600, 0);
    fs::write(&path, &hdr).unwrap();
    let msg = format!("{}", AbbrevIndex::open(&path).expect_err("副索引区段非法必须被拒绝"));
    fs::remove_file(&path).ok();
    assert!(msg.contains("区段") || msg.contains("越界"), "{msg}");
}

#[test]
fn abbrev_from_another_generation_is_not_attached() {
    let dict_p = temp_path("gen_dict");
    let abbrev_p = temp_path("gen_abbrev");
    let bad_p = temp_path("gen_abbrev_bad");
    fs::write(&dict_p, build_test_bin(2)).unwrap();
    let dict = MmapDict::open(&dict_p).unwrap();
    dict.build_abbrev_file(&abbrev_p).unwrap();
    assert!(MmapDict::open(&dict_p).unwrap().with_abbrev(&abbrev_p).has_abbrev());

    // 篡改头部 association_count（偏移 20..28）模拟异代副索引
    let mut blob = fs::read(&abbrev_p).unwrap();
    let wrong = u64::from_le_bytes(blob[20..28].try_into().unwrap()).wrapping_add(1);
    blob[20..28].copy_from_slice(&wrong.to_le_bytes());
    fs::write(&bad_p, &blob).unwrap();

    let mismatched = MmapDict::open(&dict_p).unwrap().with_abbrev(&bad_p);
    assert!(!mismatched.has_abbrev(), "关联数不符的副索引不该被装配");
    for p in [&dict_p, &abbrev_p, &bad_p] {
        fs::remove_file(p).ok();
    }
}

#[test]
fn borrowed_frequency_matches_lookup_first_entry_for_both_versions() {
    for version in [1, 2] {
        let path = temp_path(&format!("freq_{version}"));
        fs::write(&path, build_test_bin(version)).unwrap();
        let dict = MmapDict::open(&path).unwrap();
        fs::remove_file(&path).unwrap();
        for key in ["ni", "hao", "ni hao", "missing"] {
            for word in ["你", "好", "你好", "missing"] {
                let expected = dict
                    .lookup(key)
                    .unwrap()
                    .into_iter()
                    .find(|c| c.word == word)
                    .map(|c| c.freq);
                assert_eq!(dict.entry_freq(key, word).unwrap(), expected);
            }
        }
    }
}

#[test]
fn frequency_lookup_keeps_first_duplicate_even_when_its_frequency_is_zero() {
    let mut blob = build_test_bin(2);
    let entries = u64::from_le_bytes(blob[44..52].try_into().unwrap()) as usize;
    let first = entries + (1 + "好".len() + 4);
    blob[first + 1 + "你".len()..first + 1 + "你".len() + 4].fill(0);
    let second = first + (1 + "你".len() + 4);
    blob[second + 1..second + 1 + "妮".len()].copy_from_slice("你".as_bytes());
    let path = temp_path("freq_duplicate");
    fs::write(&path, blob).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(dict.entry_freq("ni", "你").unwrap(), Some(0));
    assert_eq!(dict.lookup("ni").unwrap()[0].freq, 0);
}

#[test]
fn frequency_lookup_still_validates_entries_after_a_match() {
    let mut blob = build_test_bin(2);
    let entries = u64::from_le_bytes(blob[44..52].try_into().unwrap()) as usize;
    // hao 之后是 ni 的「你」「妮」「尼」；破坏「妮」的 UTF-8。
    let second_ni = entries + (1 + "好".len() + 4) + (1 + "你".len() + 4);
    blob[second_ni + 1] = 0xff;
    let path = temp_path("freq_corrupt");
    fs::write(&path, blob).unwrap();
    let dict = MmapDict::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let old_error = dict.lookup("ni").unwrap_err().to_string();
    assert_eq!(dict.entry_freq("ni", "你").unwrap_err().to_string(), old_error);
    assert_eq!(dict.entry_freq("ni", "missing").unwrap_err().to_string(), old_error);
}

#[test]
fn lookup_mixed_and_abbrev_algorithmic_invariants() {
    let groups: TestGroups = vec![
        (b"bei jing", vec![("北京", 500)]),
        (b"fei chang de", vec![("非常地", 300)]),
        (b"hao", vec![("好", 80)]),
        (b"he ao", vec![("河岙", 90)]),
        (b"ni hao", vec![("你好", 400)]),
        (b"nin hao", vec![("您好", 350)]),
        (b"wei shen me", vec![("为什么", 450)]),
        (b"wei shen me yao", vec![("为什么要", 460)]),
        (b"zhong", vec![("中", 200)]),
        (b"zhong guo", vec![("中国", 500)]),
        (b"zhong guo ren", vec![("中国人", 600)]),
    ];
    let dict_p = temp_path("mixed_inv");
    let abbrev_p = temp_path("mixed_inv_abr");
    fs::write(&dict_p, build_bin_from(2, &groups)).unwrap();
    let dict = MmapDict::open(&dict_p).unwrap();
    dict.build_abbrev_file(&abbrev_p).unwrap();
    let dict = dict.with_abbrev(&abbrev_p);
    fs::remove_file(&dict_p).ok();
    fs::remove_file(&abbrev_p).ok();

    // 末尾/开头/中间首字母混合查询均返回补全后的全拼
    let zgr = dict.lookup_mixed("zhongguor").unwrap();
    assert!(zgr.iter().any(|c| c.word == "中国人" && c.pinyin == "zhong guo ren"));
    let nhao = dict.lookup_mixed("nhao").unwrap();
    assert!(nhao.iter().any(|c| c.word == "您好" && c.pinyin == "nin hao"));
    assert!(nhao.iter().any(|c| c.word == "你好" && c.pinyin == "ni hao"));
    let fcd = dict.lookup_mixed("feicde").unwrap();
    assert!(fcd.iter().any(|c| c.word == "非常地" && c.pinyin == "fei chang de"));

    // 合法全拼走精确分支，不引入缩略读法（h+ao → 河岙）
    let hao = dict.lookup_mixed("hao").unwrap();
    assert!(hao.iter().any(|c| c.word == "好"));
    assert!(!hao.iter().any(|c| c.word == "河岙"));

    // 纯首字母串归副索引，lookup_mixed 返回空
    for input in ["zgr", "n", "wsm"] {
        assert!(dict.lookup_mixed(input).unwrap().is_empty(), "{input}");
    }

    // 末尾半音节补全，且一字一字母不补未敲音节（zho → 中，不含中国）
    let zho = dict.lookup_mixed("zho").unwrap();
    assert!(zho.iter().any(|c| c.word == "中"));
    assert!(!zho.iter().any(|c| c.word == "中国"));
    assert!(dict.lookup_mixed("niha").unwrap().iter().any(|c| c.word == "你好"));
    assert!(dict.lookup_mixed("beijin").unwrap().iter().any(|c| c.word == "北京"));

    // 副索引一字一字母：wsm 命中「为什么」，排除 4 字的「为什么要」
    let wsm = dict.lookup_abbrev("wsm").unwrap();
    assert!(wsm.iter().any(|c| c.word == "为什么"));
    assert!(!wsm.iter().any(|c| c.word == "为什么要"));
    assert!(wsm.iter().all(|c| c.pinyin.split(' ').count() <= 3));
}

#[test]
fn lookup_mixed_and_abbrev_cap_and_truncate_by_frequency() {
    // 4 个早探索 key 各 150 条低频词（共 600 > 500），最高频词「没有」在最后一个 key
    let mut groups: Vec<BinGroup<'_, String>> = Vec::new();
    for (k, py) in [b"mei ya".as_slice(), b"mei yan", b"mei ye", b"mei yi"].into_iter().enumerate() {
        let ws = (0..150).map(|i| (format!("w{k}_{i:03}"), 250 - i as u32)).collect();
        groups.push((py, ws));
    }
    groups.push((b"mei you", vec![("没有".to_string(), 999_999)]));

    let dict_p = temp_path("freq_trunc");
    let abbrev_p = temp_path("freq_trunc_abr");
    fs::write(&dict_p, build_bin_from(2, &groups)).unwrap();
    let dict = MmapDict::open(&dict_p).unwrap();
    dict.build_abbrev_file(&abbrev_p).unwrap();
    let dict = dict.with_abbrev(&abbrev_p);
    fs::remove_file(&dict_p).ok();
    fs::remove_file(&abbrev_p).ok();

    let mixed = dict.lookup_mixed("meiy").unwrap();
    assert_eq!(mixed.len(), MAX_LOOKUP_RESULTS as usize);
    assert_eq!(mixed[0].word, "没有", "排序后按词频截断不得丢高频词");
    assert!(mixed.windows(2).all(|w| w[0].freq >= w[1].freq));

    let abbrev = dict.lookup_abbrev("my").unwrap();
    assert_eq!(abbrev.len(), MAX_LOOKUP_RESULTS as usize);
    assert_eq!(abbrev[0].word, "没有");
}
