//! `wanxiang` 模块单测。

use super::*;

fn default_gram_path() -> Option<std::path::PathBuf> {
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let p = xdg.join("xwx").join("wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

#[test]
fn encode_grammar_known_vectors() {
    assert_eq!(encode_grammar(""), Vec::<u8>::new());
    assert_eq!(encode_grammar("a"), vec![0x61]);
    // NUL 码点编码为扩展前缀 0xE0
    let mut out = Vec::new();
    encode_cp(0, &mut out);
    assert_eq!(out, vec![0xE0]);
    // CJK（0x4000..0xA000）：两字节 {Hi+0x40, Lo}
    assert_eq!(encode_grammar("你"), vec![0x8F, 0x60]);
    assert_eq!(encode_grammar("们"), vec![0x8E, 0xEC]);
    assert_eq!(encode_grammar("天"), vec![0x99, 0x29]);
    assert_eq!(encode_grammar("气"), vec![0xAC, 0x14]);
    assert_eq!(encode_grammar("你好"), vec![0x8F, 0x60, 0x99, 0x7D]);
}

#[test]
fn tail_truncates_by_chars() {
    let n = window_chars(&WanxiangConfig::default());
    assert_eq!(tail_chars("", n), "");
    assert_eq!(tail_chars("你", n), "你");
    assert_eq!(tail_chars("你好", n), "你好");
    assert_eq!(tail_chars("一二三四五六七", n), "三四五六七");
    // 多字节字形也按 char 截断（4 字节 emoji 只算 1 个字符）
    assert_eq!(tail_chars("a😀b", 2), "😀b");
}

#[test]
fn missing_gram_returns_error() {
    assert!(WanxiangLM::open(Path::new("/nonexistent/wanxiang.gram")).is_err());
}

#[test]
fn unknown_gram_format_is_rejected() {
    // 合成 64B 合法 Metadata 头，仅将 format 填为不支持的版本，无需真实 .gram 文件。
    let mut blob = vec![0u8; 64];
    let bad = b"Rime::Grammar/9.9";
    blob[..bad.len()].copy_from_slice(bad);
    blob[36..40].copy_from_slice(&1u32.to_le_bytes()); // double_array_size
    blob[40..44].copy_from_slice(&4i32.to_le_bytes()); // OffsetPtr → offset 44

    let path = std::env::temp_dir().join(format!("xwx-bad-gram-{}.gram", std::process::id()));
    std::fs::write(&path, &blob).expect("写坏模型");
    let err = WanxiangLM::open(&path).expect_err("不支持的 format 必须被拒绝");
    let _ = std::fs::remove_file(&path);
    let msg = format!("{err}");
    assert!(msg.contains("格式") || msg.contains("format"), "报错应点名格式问题: {msg}");
}

#[test]
fn real_gram_smoke_scores_context_and_common_pairs() {
    let Some(path) = default_gram_path() else {
        return;
    };
    let lm = WanxiangLM::open(&path).expect("open 失败");
    assert!(lm.num_units() > 0);
    let no_ctx = lm.score(&lm.state_from_context(&LmContext::empty()), "天气").0;
    let ctx = lm.state_from_context(&LmContext { tail: "今天", last_word: None });
    let common = lm.score(&ctx, "天气").0;
    let rare = lm.score(&ctx, "龘").0;
    assert!(common > no_ctx, "有上文命中应高于无上文: {common} vs {no_ctx}");
    assert!(common > rare, "常见词配应高于生僻: {common} vs {rare}");
    assert!(rare <= lm.floor_log10(), "生僻组合应落到兜底: {rare}");
}
