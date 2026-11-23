//! 整句与填充档整串词条的首位仲裁。

use super::*;

fn first(engine: &ImeEngine, input: &str) -> String {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .first()
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

/// 填充档整串词条（`freq == FILLER_FREQ_TIER`）在组合读法 LM 证据更强时让位给整句。
#[test]
fn filler_entry_yields_to_stronger_composed_sentence() {
    let mut engine = ImeEngine::new(None).unwrap();
    if let DictStore::Sqlite(ref d) = engine.dict {
        d.upsert("杵在那", "chu zai na", FILLER_FREQ_TIER).unwrap();
        d.upsert("出", "chu", 5000).unwrap();
        d.upsert("杵", "chu", 500).unwrap();
        d.upsert("在", "zai", 5000).unwrap();
        d.upsert("哪", "na", 5000).unwrap();
        d.upsert("那", "na", 5000).unwrap();
    }
    let mut lm = BigramLM::new();
    for (w, c) in [("出", 5000), ("在", 5000), ("哪", 5000), ("杵", 10), ("那", 500)] {
        lm.add_unigram(w, c);
    }
    lm.add_bigram("出", "在", 5000);
    lm.add_bigram("在", "哪", 5000);
    engine.lm = LmStore::Bigram(lm);

    assert_eq!(first(&engine, "chuzaina"), "出在哪");
}

/// 高频可靠的整串词条必须留住首位，不被整句顶掉。
#[test]
fn reliable_whole_string_entry_keeps_first_place() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(first(engine, "nihao"), "你好");
    assert_eq!(first(engine, "womenjintian"), "我们今天");
}

/// 填充档真实短语不被组合候选挤掉；简拼链读法仍能接管首位。
#[test]
fn filler_tier_real_phrases_and_abbrev_chain_keep_first_place() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (input, want) in [
        ("zaodianxiuxi", "早点休息"),
        ("womashangdao", "我马上到"),
        ("jintiantianqizenmeyang", "今天天气怎么样"),
        ("nizaiganshenme", "你在干什么"),
        ("nihaoshijie", "你好世界"),
        ("wmjtqbj", "我们今天去北京"),
    ] {
        assert_eq!(first(engine, input), want, "{input} 首位回归");
    }
}
