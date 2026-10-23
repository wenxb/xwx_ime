//! 完整尾音的整句读法不能在补全路径剪枝时丢失。
use super::*;

#[test]
fn literal_sentence_survives_high_frequency_completions() {
    let engine = ImeEngine::new(None).unwrap();
    for (word, key, freq) in [
        ("还会", "hai hui", 500_000),
        ("卡", "ka", 400),
        ("看", "kan", 8_000_000),
        ("开", "kai", 2_000_000),
        ("考", "kao", 1_000_000),
        ("抗", "kang", 500_000),
    ] {
        engine.dict.upsert(word, key, freq).unwrap();
    }
    let cands = engine.lookup_candidates("haihuika", &LmContext::empty()).unwrap();
    assert!(cands.iter().any(|c| c.word == "还会看"), "补全仍应可选");
    assert_eq!(cands[0].word, "还会卡", "完整 ka 不应被 kan/kai/kao 抢占");
    assert_eq!(cands[0].covered, 8);
}

#[test]
fn open_syllable_sentences_precede_nasal_guesses() {
    for (tail, full) in [("ba", "ban"), ("da", "dan"), ("ma", "man"), ("li", "lin")] {
        let engine = ImeEngine::new(None).unwrap();
        engine.dict.upsert("我们", "wo men", 500_000).unwrap();
        engine.dict.upsert("甲", tail, 100).unwrap();
        engine.dict.upsert("乙", full, 8_000_000).unwrap();
        let input = format!("women{tail}");
        let cands = engine.lookup_candidates(&input, &LmContext::empty()).unwrap();
        assert_eq!(cands[0].word, "我们甲", "{input}: 鼻音补全不能仅凭 LM 越级");
        assert!(cands.iter().any(|c| c.word == "我们乙"), "补全应保留");
    }
}

#[test]
fn real_literal_tail_is_preserved_with_context_and_session() {
    let Some(engine) = real_bilingual_engine() else { return };
    for tail in ["", "输入法", "以后"] {
        let ctx = LmContext { tail, last_word: (!tail.is_empty()).then_some(tail) };
        let cands = engine.lookup_candidates("haihuika", &ctx).unwrap();
        assert_eq!(cands[0].word, "还会卡", "上文 {tail:?}");
        assert_eq!(cands[0].pinyin, "hai hui ka");
        assert!(cands.iter().any(|c| c.word == "还会看"));
    }
    for (input, want) in [("haihuikan", "还会看"), ("haihuik", "还会看")] {
        assert_eq!(engine.lookup_candidates(input, &LmContext::empty()).unwrap()[0].word, want);
    }
    let engine = std::sync::Arc::new(engine);
    let mut session = engine.create_session();
    for ch in "haihuika".chars() {
        session.handle(InputEvent::Char(ch)).unwrap();
    }
    assert_eq!(session.candidates()[0].word, "还会卡");
    session.handle(InputEvent::Char('n')).unwrap();
    assert_eq!(session.candidates()[0].word, "还会看");
    session.handle(InputEvent::Backspace).unwrap();
    assert_eq!(session.candidates()[0].word, "还会卡");
    assert_eq!(session.candidates()[0].covered, "haihuika".len());
    let output = session.handle(InputEvent::Space).unwrap();
    assert!(matches!(output, SessionOutput::Commit(ref text) if text == "还会卡"));
    assert!(session.candidates().is_empty(), "选中整句后不应留下半截拼音");
}
