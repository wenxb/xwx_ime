//! 投影层填充档判断（`FILLER_FREQ_TIER`）的钉子用例。
//!
//! 背景：词库里大量条目没有真实词频（`freq = 100` 的填充档），其中不少其实是
//! **真短语**（早点休息、我马上到）。投影层旧规则只看档位，一律把 `freq = 0`
//! 的组合候选提到首位，于是把这些正确读法挤成 #2（daily 语料的 MRR 0.958 就是
//! 这么来的）。现在的判据是：**两边都按词串口径比**（`floor + Σ证据量`，
//! 基线每候选只付一次），词面取候选自己的汉字。

use super::*;

/// 真实词库 + 副索引 + 真实 LM（没有 .gram 就跳过：胜负由 LM 定）。
fn real_engine() -> Option<ImeEngine> {
    let dict = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict.is_file() || !abbrev.is_file() {
        return None;
    }
    let gram = gram_path()?;
    let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?;
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

/// 填充档的**真短语**不被组合候选挤掉（LM 明确偏好它们）。
#[test]
fn filler_tier_real_phrases_keep_first_place() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "zaodianxiuxi"), "早点休息", "修鞋 在 LM 下是兜底分");
    assert_eq!(first(&engine, "womashangdao"), "我马上到", "道 在 LM 下是兜底分");
    assert_eq!(first(&engine, "jintiantianqizenmeyang"), "今天天气怎么样");
    assert_eq!(first(&engine, "nizaiganshenme"), "你在干什么");
    assert_eq!(first(&engine, "nihaoshijie"), "你好世界");
}

/// 该换位的仍然换位（组合读法的证据更强），验收形态不许退化。
#[test]
fn composed_reading_with_stronger_evidence_still_wins() {
    let Some(engine) = real_engine() else {
        return;
    };
    assert_eq!(first(&engine, "chuzaina"), "出在哪");
    assert_eq!(first(&engine, "shanghaai"), "上海");
    assert_eq!(first(&engine, "wmjtqbj"), "我们今天去北京");
}
