//! 本模块的单测(从同一个文件里拆出,2026-10-02:`mod.rs` 只管实现,
//! 测试单独一个文件;`use super::*` 让它们照旧能碰私有条目)。

use super::*;

fn default_gram_path() -> Option<std::path::PathBuf> {
    // 与 src/main.rs resolve_data_path 的默认值一致
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
    assert_eq!(encode_grammar("你"), vec![0x8F, 0x60]); // U+4F60
    assert_eq!(encode_grammar("们"), vec![0x8E, 0xEC]); // U+4EEC
    assert_eq!(encode_grammar("天"), vec![0x99, 0x29]); // U+5929
    assert_eq!(encode_grammar("气"), vec![0xAC, 0x14]); // U+6C14
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
fn log10_conversion_structure() {
    // 无文件依赖：验证换算公式的形状和单调性。
    let cfg = WanxiangConfig::default();
    let conv = |v: i32, p: f64| {
        (v as f64 / 10_000.0 + p) / std::f64::consts::LN_10 - cfg.log10_corpus_size
    };
    let common = conv(154_000, cfg.collocation_penalty);
    let floor = conv(0, cfg.non_collocation_penalty);
    let weak = conv(154_000, cfg.weak_collocation_penalty);
    assert!((-12.0..-6.0).contains(&common), "常见词配应在 -12..-6: {common}");
    assert!(common > floor, "有上文命中应高于兜底: {common} vs {floor}");
    assert!(floor > weak, "弱词配应低于兜底: {floor} vs {weak}");
    // 同一惩罚下原始值越大分越高（常见 > 生僻的数量级来自原始 v）
    assert!(conv(160_000, cfg.collocation_penalty) > common);
    assert!(common.is_finite() && floor.is_finite());
}

#[test]
fn missing_gram_returns_error() {
    // 文件不存在 → open 报错（引擎层负责优雅退化，见 lib.rs 测试）
    assert!(WanxiangLM::open(Path::new("/nonexistent/wanxiang.gram")).is_err());
}

#[test]
fn real_gram_context_beats_no_context() {
    let Some(path) = default_gram_path() else {
        eprintln!("未找到 .gram 文件，跳过");
        return;
    };
    let lm = WanxiangLM::open(&path).expect("open 失败");
    // 44 + 单元数×4 必须正好等于文件长度（实测事实）
    assert_eq!(
        44 + lm.array.num_units * 4,
        lm.mmap.len(),
        "44 + 单元数×4 应精确等于文件长度"
    );
    let no_ctx = lm.score(&lm.state_from_context(&LmContext::empty()), "天气").0;
    let ctx = lm.state_from_context(&LmContext { tail: "今天", last_word: None });
    let with_ctx = lm.score(&ctx, "天气").0;
    assert!(
        with_ctx > no_ctx,
        "有上文「今天」命中「天气」应高于无上文: {with_ctx} vs {no_ctx}"
    );
}

#[test]
fn real_gram_common_beats_rare() {
    let Some(path) = default_gram_path() else {
        return;
    };
    let lm = WanxiangLM::open(&path).expect("open 失败");
    let ctx = lm.state_from_context(&LmContext { tail: "今天", last_word: None });
    let common = lm.score(&ctx, "天气").0;
    let rare = lm.score(&ctx, "龘").0;
    assert!(common > rare, "常见词配应高于生僻: {common} vs {rare}");
    assert!(rare <= lm.floor_log10(), "生僻组合应无命中落到兜底: {rare}");
}

#[test]
fn real_gram_state_advances() {
    let Some(path) = default_gram_path() else {
        return;
    };
    let lm = WanxiangLM::open(&path).expect("open 失败");
    let s0 = lm.state_from_context(&LmContext::empty());
    assert_eq!(s0.tail, "");
    let (_, s1) = lm.score(&s0, "今天");
    assert_eq!(s1.tail, "今天", "消费词后状态并入该词");
    let (_, s2) = lm.score(&s1, "天气");
    assert_eq!(s2.tail, "今天天气", "继续并入");
    // 超过 5 字按字符截断
    let ctx5 = lm.state_from_context(&LmContext { tail: "一二三四五", last_word: None });
    let (_, s3) = lm.score(&ctx5, "六七八九");
    assert_eq!(s3.tail, "五六七八九", "并入后应只留末 5 字");
}
