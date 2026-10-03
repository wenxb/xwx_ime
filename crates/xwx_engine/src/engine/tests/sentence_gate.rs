//! 阶段 3B 的验收线：什么时候该让整句接管首位。
//!
//! 上游判据是 `has_reliable_phrase`（`script_translator.cc:503-508`）：
//! 「≥2 音节 且没有**可靠**的整串词条」才生成整句。现状把「有覆盖整串的词条」
//! 直接当成「可靠」——`nihao`（你好 freq=332885）与 `chuzaina` 被同等对待。
//!
//! 现在的机制不是加旗标，而是修打分：LM 的「无证据基线分」不再按词数复付
//! （见 `decode.rs` 的 `lm_floor`），于是「出+在+哪」凭条件证据压过单词
//! 「杵在那」（freq=100 的填充条目）。第一半是不许弄坏的回归保险。

use super::*;

/// 真实词库 + 副索引 + **真实 LM**（没有 .gram 时用空模型，只跑得动第一半）。
fn real_engine() -> Option<ImeEngine> {
    let dict = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict.is_file() || !abbrev.is_file() {
        return None;
    }
    let gram = gram_path();
    let mut engine = ImeEngine::new_with_lm(Some(&dict), gram.as_deref()).ok()?;
    engine.dict = engine.dict.with_abbrev(&abbrev);
    Some(engine)
}

fn gram_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = std::path::PathBuf::from(home).join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

fn first(engine: &ImeEngine, input: &str) -> String {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .first()
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

/// 端一：覆盖整串的词条**可靠**（高频词），必须留住首位。
#[test]
fn reliable_whole_string_entry_keeps_first_place() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "nihao"), "你好", "可靠整串词条不该被整句顶掉");
    assert_eq!(first(&engine, "womenjintian"), "我们今天");
}

/// 端二：覆盖整串的词条**不可靠**（`chu zai na` 全库只有「杵在那」=freq 100
/// 的填充条目），整句「出+在+哪」应接管首位。
///
/// 这半必须带 LM：基线补偿要 `floor_log10()`，没有模型时它取默认 0.0，
/// 补偿不发生、首位仍是「杵在那」（见 `decode.rs` 的 `lm_floor`）。
#[test]
fn unreliable_whole_string_entry_yields_to_the_sentence() {
    if gram_path().is_none() {
        eprintln!("跳过：没有 .gram 模型（基线补偿不发生，见 decode.rs 的 lm_floor）");
        return;
    }
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "chuzaina"), "出在哪", "不可靠整串词条应让位给整句");
}
