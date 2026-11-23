//! Python writer ↔ Rust reader 线级一致性验证（基于入库夹具 `tests/fixtures/english/`）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use xwx_engine::english::EnglishBigram;
use xwx_engine::EnglishDict;

const PREFIX_SCORE_STEP: i32 = 15;
const PREFIX_TOP_LIMIT: usize = 32;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/english")
        .join(name)
}

fn read_words_tsv() -> Vec<(String, String, u16)> {
    let text = std::fs::read_to_string(fixture("words.tsv")).expect("读 words.tsv");
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
    assert!(!out.is_empty());
    out
}

fn oracle_top(words: &[(String, String, u16)], prefix: &str) -> Vec<String> {
    let mut hits: Vec<(i32, String)> = words
        .iter()
        .filter(|(k, _, _)| k.starts_with(prefix))
        .map(|(k, _, q)| (*q as i32 - PREFIX_SCORE_STEP * (k.len() - prefix.len()) as i32, k.clone()))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    hits.truncate(PREFIX_TOP_LIMIT);
    hits.into_iter().map(|(_, k)| k).collect()
}

#[test]
fn words_bin_matches_tsv_exact_and_prefix_top_oracle() {
    let dict = EnglishDict::open(fixture("words.bin")).expect("打开 words.bin");
    let words = read_words_tsv();
    assert_eq!(dict.word_count(), words.len());
    assert!(dict.prefix_count() > 0 && dict.file_len() >= 64);

    let mut prefixes = BTreeSet::new();
    for (key, display, q) in &words {
        let e = dict.exact(key).unwrap().unwrap_or_else(|| panic!("{key} 应精确命中"));
        assert_eq!(e.display, display);
        assert_eq!(e.zipf_centi, *q);
        for end in 1..=key.len() {
            prefixes.insert(key[..end].to_string());
        }
    }
    assert!(dict.exact("zzzznotaword").unwrap().is_none());
    assert!(prefixes.len() > 10);
    for p in &prefixes {
        let got: Vec<String> = dict.prefix_top(p).unwrap().iter().map(|e| e.key.to_string()).collect();
        assert_eq!(got, oracle_top(&words, p), "前缀 {p}");
    }
}

#[test]
fn bigram_bin_matches_tsv_oracle() {
    let m = EnglishBigram::open(fixture("bigram.bin")).expect("打开 bigram.bin");
    assert_eq!(m.context_count(), 6);
    assert_eq!(m.edge_count(), 8);

    // bigram.tsv 里 hello world 出现三次（7852480 / 100 / 5），必须合并。
    assert_eq!(m.count("hello", "world").unwrap(), 7_852_585);
    assert_eq!(m.count("thank", "you").unwrap(), 733_376_704);
    assert_eq!(m.count("looking", "forward").unwrap(), 271_821_504);
    assert_eq!(m.count("hello", "thank").unwrap(), 0);
    assert!(m.row("zzzz").unwrap().is_none());
    // 非法键与词表外词已由构建端滤掉
    assert!(m.row("bad_key").unwrap().is_none());
    assert_eq!(m.count("hello", "zzzznotaword").unwrap(), 0);

    let row = m.row("hello").unwrap().expect("hello 行");
    assert_eq!(row.len(), 2);
    assert_eq!(row.observed_total, 7_852_585 + 120_000);
    let keys: Vec<&str> = row.iter(&m).map(|e| e.next).collect();
    assert_eq!(keys, vec!["world", "there"]);
}
