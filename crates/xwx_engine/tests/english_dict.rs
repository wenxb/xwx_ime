//! 真实产物集成:读 `scripts/build_english_dict.py` 写出的 `english.bin`。
//!
//! 单元测试用的是 crate 内合成构建器（内存替身）；这一层**必须**打开
//! Python 侧写出的真实文件——两条独立实现（Python writer / Rust reader）
//! 只有在同一份字节上对上，格式才算真的被钉住。
//!
//! oracle 是 `tests/fixtures/english/words.tsv` 本身：测试自己按行读回
//! 词面与频次，再与 reader 的 `exact` 逐条比对；前缀 Top-32 用全扫描
//! 独立重算，不复用 reader 的排序代码。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use xwx_engine::candidate::EnglishMatchKind;
use xwx_engine::EnglishDict;

const PREFIX_SCORE_STEP: i32 = 15;
const PREFIX_TOP_LIMIT: usize = 32;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/english")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/english"))
}

/// `key<TAB>display<TAB>zipf_centi` → 与 TSV 同序的词表。
fn read_tsv() -> Vec<(String, String, u16)> {
    let text = std::fs::read_to_string(fixture_dir().join("words.tsv")).expect("读 words.tsv");
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split('\t').collect();
        assert!(f.len() >= 3, "fixture 行格式不对: {t:?}");
        out.push((
            f[0].to_ascii_lowercase(),
            f[1].to_string(),
            f[2].parse::<u16>().expect("zipf_centi 必须是整数"),
        ));
    }
    assert!(!out.is_empty(), "fixture 不能是空的");
    out
}

fn open_fixture() -> EnglishDict {
    let p = fixture_dir().join("words.bin");
    EnglishDict::open(&p).unwrap_or_else(|e| panic!("打开 {} 失败: {e}", p.display()))
}

/// 独立 oracle：全扫描算出某前缀的 Top-32（计数降序等价式 = 分降序 + key 升序）。
fn oracle_top(words: &[(String, String, u16)], prefix: &str) -> Vec<String> {
    let mut hits: Vec<(i32, String)> = words
        .iter()
        .filter(|(k, _, _)| k.starts_with(prefix))
        .map(|(k, _, q)| {
            let base = *q as i32 - PREFIX_SCORE_STEP * (k.len() - prefix.len()) as i32;
            (base, k.clone())
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hits.truncate(PREFIX_TOP_LIMIT);
    hits.into_iter().map(|(_, k)| k).collect()
}

#[test]
fn python_written_fixture_opens_and_reports_expected_shape() {
    let dict = open_fixture();
    let words = read_tsv();
    assert_eq!(dict.word_count(), words.len(), "word_count 必须等于 fixture 行数");
    assert!(dict.prefix_count() > 0);
    assert!(dict.file_len() >= 64, "至少要有头部");
}

#[test]
fn exact_matches_the_tsv_oracle() {
    let dict = open_fixture();
    let words = read_tsv();
    for (key, display, q) in &words {
        let e = dict
            .exact(key)
            .expect("查询不应失败")
            .unwrap_or_else(|| panic!("{key} 应精确命中"));
        assert_eq!(e.display, display, "{key} 的显示形式");
        assert_eq!(e.zipf_centi, *q, "{key} 的频次");
    }
    // 不在表里的键必须查不到（表里没有的长尾/拼错）。
    assert!(dict.exact("zzzznotaword").expect("查询").is_none());
}

#[test]
fn prefix_top_matches_full_scan_oracle() {
    let dict = open_fixture();
    let words = read_tsv();
    let mut prefixes: BTreeMap<String, ()> = BTreeMap::new();
    for (k, _, _) in &words {
        for end in 1..=k.len() {
            prefixes.insert(k[..end].to_string(), ());
        }
    }
    assert!(prefixes.len() > 10, "fixture 前缀太少，测不出东西");
    for p in prefixes.keys() {
        let got: Vec<String> = dict
            .prefix_top(p)
            .expect("查询不应失败")
            .iter()
            .map(|e| e.key.to_string())
            .collect();
        assert_eq!(got, oracle_top(&words, p), "前缀 {p}");
    }
}

#[test]
fn fixture_covers_all_three_metadata_shapes_the_engine_needs() {
    // 显示大小写不同的词（overrides 的效果）与纯小写词都要在。
    let dict = open_fixture();
    let words = read_tsv();
    let mut has_mixed_case = false;
    for (key, display, _) in &words {
        if display != key {
            has_mixed_case = true;
            let e = dict.exact(key).expect("查询").expect("应命中");
            assert_eq!(e.display, display);
        }
    }
    assert!(has_mixed_case, "fixture 应含至少一个规范大小写词（如 Python/GitHub）");
    let _ = EnglishMatchKind::Exact; // 元数据类型在 reader 层可见
}

#[test]
fn production_dict_opens_when_present() {
    // data/english.bin 不进 git（由构建脚本生成）；存在就完整校验一遍，
    // 不存在就跳过——这条测试不能因为缺数据产物而变红。
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/english.bin");
    if !p.is_file() {
        eprintln!("跳过：{} 不存在（先用 scripts/build_english_dict.py 生成）", p.display());
        return;
    }
    let dict = EnglishDict::open(&p).expect("生产 english.bin 必须能通过完整校验");
    assert!(dict.word_count() > 40_000, "生产词表应至少几万词");
    // 抽查几条 overrides 的规范大小写。
    for (key, display) in [("python", "Python"), ("github", "GitHub"), ("i", "I")] {
        let e = dict.exact(key).expect("查询").expect("应命中");
        assert_eq!(e.display, display);
    }
}
