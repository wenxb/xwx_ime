//! 损坏/异代数据的拒绝路径。
//!
//! 从 `dict/tests.rs` 拆出：单文件须 ≤600 行。子模块可以直接用父模块
//! （`dict::tests`）里的私有 helper —— Rust 的私有可见性覆盖后代模块。

use super::*;

#[test]
fn corrupt_section_offsets_are_rejected_at_open() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("xwx-bad-header-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("bad.bin");
    let mut hdr = Vec::new();
    hdr.extend_from_slice(MMAP_MAGIC);
    hdr.extend_from_slice(&MMAP_VERSION.to_le_bytes());
    hdr.extend_from_slice(&1u64.to_le_bytes()); // pinyin_count
    hdr.extend_from_slice(&0u64.to_le_bytes()); // total_entries
    hdr.extend_from_slice(&200u64.to_le_bytes()); // records_off（跑到 strings 之后）
    hdr.extend_from_slice(&100u64.to_le_bytes()); // strings_off
    hdr.extend_from_slice(&300u64.to_le_bytes()); // entries_off
    hdr.resize(512, 0);
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&hdr))
        .expect("写坏文件");
    let err = MmapDict::open(&path).expect_err("区段顺序非法的头必须被拒绝");
    let msg = format!("{err}");
    assert!(msg.contains("区段") || msg.contains("越界"), "报错应点名区段问题: {msg}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 副索引的区段顺序非法同样必须拒绝（与主索引同一套判据）。
#[test]
fn corrupt_abbrev_section_offsets_are_rejected_at_open() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("xwx-bad-abbrev-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("bad-abbrev.bin");
    let mut hdr = Vec::new();
    hdr.extend_from_slice(super::ABBREV_MAGIC);
    hdr.extend_from_slice(&super::ABBREV_VERSION.to_le_bytes());
    hdr.extend_from_slice(&1u64.to_le_bytes()); // key_count
    hdr.extend_from_slice(&0u64.to_le_bytes());
    hdr.extend_from_slice(&400u64.to_le_bytes()); // records_off（跑过头）
    hdr.extend_from_slice(&200u64.to_le_bytes()); // strings_off
    hdr.extend_from_slice(&300u64.to_le_bytes()); // entries_off
    hdr.resize(600, 0);
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&hdr))
        .expect("写坏文件");
    let err = super::AbbrevIndex::open(&path).expect_err("副索引区段非法必须被拒绝");
    let msg = format!("{err}");
    assert!(msg.contains("区段") || msg.contains("越界"), "报错应点名区段: {msg}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **拿错代的副索引不能被装配**（Q4 发布契约）：副索引由主索引生成，
/// 关联数恒等于词条总数。这里把真实 `data/abbrev.bin` 的关联数改掉，
/// 装配后必须 `has_abbrev() == false`——静默给出错候选比装不上更糟。
#[test]
fn abbrev_from_another_generation_is_not_attached() {
    let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let (dict_p, abbrev_p) = (data.join("dict.bin"), data.join("abbrev.bin"));
    if !dict_p.is_file() || !abbrev_p.is_file() {
        return; // 这一条依赖真实数据；缺数据的验收由 tests/required_data.rs 负责
    }
    let mut blob = std::fs::read(&abbrev_p).expect("读副索引");
    // 头部布局：8s 魔数 + u32 版本 + 5×u64（key 数、关联数、三个区段起点）。
    let wrong = u64::from_le_bytes(blob[20..28].try_into().unwrap()).wrapping_add(1);
    blob[20..28].copy_from_slice(&wrong.to_le_bytes());

    let dir = std::env::temp_dir().join(format!("xwx-wrong-gen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let bad = dir.join("abbrev-other-gen.bin");
    std::fs::write(&bad, &blob).expect("写副索引");

    let dict = MmapDict::open(&dict_p).expect("开词库").with_abbrev(&bad);
    assert!(
        !dict.has_abbrev(),
        "关联数不符的副索引不该被装配（拿错代只会静默给错候选）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
