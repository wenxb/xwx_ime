//! `english_bigram.bin` reader 的真实产物与坏文件矩阵（M2 / D10-D12）。
//!
//! 夹具由 `scripts/build_english_bigram.py` 从 `tests/fixtures/english/bigram.tsv`
//! 生成，两者都在仓库里；`data/english_bigram.bin` 存在时额外完整校验。

use std::path::{Path, PathBuf};

use xwx_engine::english::{EnglishBigram, EnglishError};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/english")
        .join(name)
}

fn small() -> EnglishBigram {
    EnglishBigram::open(fixture("bigram.bin")).expect("夹具必须能打开")
}

fn corrupt(edit: impl FnOnce(&mut Vec<u8>)) -> EnglishError {
    let mut bytes = std::fs::read(fixture("bigram.bin")).expect("读夹具");
    edit(&mut bytes);
    EnglishBigram::from_bytes(bytes).expect_err("坏文件必须被拒绝")
}

#[test]
fn fixture_loads_and_matches_the_tsv_oracle() {
    let m = small();
    assert_eq!(m.context_count(), 6);
    assert_eq!(m.edge_count(), 8);

    // bigram.tsv 里 hello world 出现三次（7852480 / 100 / 5），必须合并。
    assert_eq!(m.count("hello", "world").unwrap(), 7_852_585);
    assert_eq!(m.count("thank", "you").unwrap(), 733_376_704);
    assert_eq!(m.count("looking", "forward").unwrap(), 271_821_504);
    // 未收录的词对是 0，不是错误
    assert_eq!(m.count("hello", "thank").unwrap(), 0);
    // 不存在的 prev
    assert!(m.row("zzzz").unwrap().is_none());

    let row = m.row("hello").unwrap().expect("hello 行");
    assert_eq!(row.len(), 2, "hello → world / there");
    assert_eq!(row.observed_total, 7_852_585 + 120_000);
    let keys: Vec<&str> = row.iter(&m).map(|e| e.next).collect();
    assert_eq!(keys, vec!["world", "there"], "必须按 count 降序");
}

#[test]
fn absent_words_and_bad_keys_never_reach_the_product() {
    let m = small();
    // bigram.tsv 里 `bad_key world 3` 与 `hello zzzznotaword 7` 必须被过滤
    assert!(m.row("bad_key").unwrap().is_none());
    assert_eq!(m.count("hello", "zzzznotaword").unwrap(), 0);
}

#[test]
fn row_iteration_is_zero_allocation_and_stable() {
    let m = small();
    let row = m.row("hello").unwrap().expect("hello 行");
    let first: Vec<(String, u64)> = row.iter(&m).map(|e| (e.next.to_string(), e.count)).collect();
    let second: Vec<(String, u64)> = row.iter(&m).map(|e| (e.next.to_string(), e.count)).collect();
    assert_eq!(first, second, "同一行重复迭代必须完全一致");
}

#[test]
fn corrupt_files_are_rejected_with_a_reason() {
    // 截短
    let bytes = std::fs::read(fixture("bigram.bin")).unwrap();
    let e = EnglishBigram::from_bytes(bytes[..40].to_vec()).expect_err("过短必须被拒绝");
    assert!(format!("{e}").contains("过短"), "{e}");
    // 坏魔数
    let e = corrupt(|b| b[0] = b'Y');
    assert!(format!("{e}").contains("魔数"), "{e}");
    // 坏版本
    let e = corrupt(|b| b[8] = 9);
    assert!(format!("{e}").contains("版本"), "{e}");
    // 坏 header_len
    let e = corrupt(|b| b[12] = 32);
    assert!(format!("{e}").contains("header_len"), "{e}");
    // 头部 reserved ≠ 0
    let e = corrupt(|b| b[56] = 1);
    assert!(format!("{e}").contains("reserved"), "{e}");
    // contexts_offset 不是 64
    let e = corrupt(|b| b[24] = 72);
    assert!(format!("{e}").contains("紧接头部"), "{e}");
    // file_len 撒谎
    let e = corrupt(|b| {
        let n = b.len() as u64 + 8;
        b[48..56].copy_from_slice(&n.to_le_bytes());
    });
    assert!(format!("{e}").contains("file_len"), "{e}");
}

#[test]
fn structural_edits_that_break_invariants_are_caught() {
    // 区段重叠：把 edges_offset 指到 contexts 区中间
    let e = corrupt(|b| {
        let n = 100u64;
        b[32..40].copy_from_slice(&n.to_le_bytes());
    });
    let msg = format!("{e}");
    assert!(msg.contains("区段顺序") || msg.contains("不一致"), "{msg}");

    // context.observed_total = 0
    let e = corrupt(|b| {
        let ctx_off = u64::from_le_bytes(b[24..32].try_into().unwrap()) as usize;
        b[ctx_off + 16..ctx_off + 24].copy_from_slice(&0u64.to_le_bytes());
    });
    assert!(format!("{e}").contains("observed_total"), "{e}");

    // context.reserved ≠ 0
    let e = corrupt(|b| {
        let ctx_off = u64::from_le_bytes(b[24..32].try_into().unwrap()) as usize;
        b[ctx_off + 12] = 1;
    });
    assert!(format!("{e}").contains("reserved"), "{e}");

    // edge.count = 0
    let e = corrupt(|b| {
        let edge_off = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
        b[edge_off + 8..edge_off + 16].copy_from_slice(&0u64.to_le_bytes());
    });
    assert!(format!("{e}").contains("count"), "{e}");

    // edge.count 超过 observed_total
    let e = corrupt(|b| {
        let edge_off = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
        b[edge_off + 8..edge_off + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    });
    assert!(format!("{e}").contains("observed_total"), "{e}");

    // 行内顺序：把 `hello` 行第一条 edge 的 count 改到比第二条还小
    let e = corrupt(|b| {
        let ctx_off = u64::from_le_bytes(b[24..32].try_into().unwrap()) as usize;
        // contexts 按 prev 升序：developer, docker, hello, …
        let hello = ctx_off + 2 * 24;
        let edge_start = u32::from_le_bytes(b[hello + 8..hello + 12].try_into().unwrap()) as usize;
        let edge_off = u64::from_le_bytes(b[32..40].try_into().unwrap()) as usize;
        b[edge_off + edge_start * 16 + 8..edge_off + edge_start * 16 + 16]
            .copy_from_slice(&1u64.to_le_bytes());
    });
    assert!(format!("{e}").contains("顺序"), "{e}");

    // 字符串引用越界
    let e = corrupt(|b| {
        let ctx_off = u64::from_le_bytes(b[24..32].try_into().unwrap()) as usize;
        b[ctx_off..ctx_off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    assert!(format!("{e}").contains("越界"), "{e}");
}

#[test]
fn the_real_product_is_fully_validated_when_present() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/english_bigram.bin");
    if !path.is_file() {
        eprintln!("跳过：{} 不存在（不进 git，由构建脚本生成）", path.display());
        return;
    }
    let m = EnglishBigram::open(&path).expect("真实产物必须能打开");
    assert!(m.context_count() > 0);
    assert!(m.edge_count() >= m.context_count(), "每行至少一条边");
    // `thank → you` 是文档里点名的例子，必须真的存在
    assert!(m.count("thank", "you").unwrap() > 0, "thank you 必须在静态表里");
}
