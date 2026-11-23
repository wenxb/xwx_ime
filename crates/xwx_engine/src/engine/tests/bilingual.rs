//! crate 级单测:中英双通道查询、意图分档与稳定混排。

use super::*;
use crate::candidate::EnglishMatchKind;
use crate::english::{
    build_synth_bin, EnglishContext, EnglishDict, EnglishEngine, SynthWord as W,
};

fn engine_with(words: &[W]) -> ImeEngine {
    let dict = EnglishDict::from_bytes(build_synth_bin(words)).expect("合成英文词库应可读");
    ImeEngine::new(None)
        .expect("内存引擎创建失败")
        .with_english(EnglishEngine::new(dict))
}

fn w(key: &'static str, display: &'static str, q: u16) -> W {
    W { key, display, q }
}

fn english_keys(cands: &[Candidate]) -> Vec<String> {
    cands
        .iter()
        .filter_map(|c| c.english_key().map(str::to_string))
        .collect()
}

fn lookup(engine: &ImeEngine, input: &str) -> Vec<Candidate> {
    engine
        .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
        .expect("查询失败")
}

fn base_words() -> Vec<W> {
    vec![
        w("hello", "hello", 450),
        w("help", "help", 520),
        w("he", "he", 600),
        w("python", "Python", 410),
        w("you", "you", 700),
    ]
}

/// 长度与上文门槛：无上文 1 字母不召回、2 字母仅精确、≥3 字母含补全；有可信上文 1 字母可补全。
#[test]
fn b01_length_and_context_recall_gates() {
    let engine = engine_with(&base_words());

    for input in ["y", "h", "p"] {
        assert!(english_keys(&lookup(&engine, input)).is_empty(), "1 字母无上文不出英文: {input}");
    }
    let with_ctx = engine
        .lookup_composition("y", &LmContext::empty(), &EnglishContext::with_previous("thank"))
        .expect("查询失败");
    assert!(english_keys(&with_ctx).contains(&"you".to_string()), "有上文时 1 字母可补全");

    assert_eq!(english_keys(&lookup(&engine, "he")), vec!["he".to_string()], "2 字母无上文只查精确");
    assert!(english_keys(&lookup(&engine, "py")).is_empty(), "2 字母无上文不展开前缀");

    assert!(english_keys(&lookup(&engine, "hello")).contains(&"hello".to_string()));
    assert!(english_keys(&lookup(&engine, "pyt")).contains(&"python".to_string()));
    let hel = english_keys(&lookup(&engine, "hel"));
    assert!(hel.contains(&"hello".to_string()) && hel.contains(&"help".to_string()));
}

#[test]
fn b05_non_letters_and_overlong_input_never_enter_english() {
    let engine = engine_with(&base_words());
    for input in ["he llo", "he-llo", "he'llo", "abc123", "你好", "hello!"] {
        assert!(english_keys(&lookup(&engine, input)).is_empty(), "非纯字母不进英文: {input}");
    }
    let long = "a".repeat(49);
    assert!(english_keys(&lookup(&engine, &long)).is_empty(), "超 48 字母不进英文");
}

#[test]
fn b06_apostrophe_input_keeps_chinese_path() {
    let engine = engine_with(&base_words());
    let out = lookup(&engine, "xi'an");
    assert!(english_keys(&out).is_empty());
    let echo = out.iter().find(|c| c.echo).expect("应有原文兜底");
    assert_eq!(echo.word, "xi'an", "原文逐字保留，包括撇号");
}

#[test]
fn b08_english_first_when_no_chinese_candidates() {
    let engine = engine_with(&base_words());
    let out = lookup(&engine, "python");
    assert_eq!(out.first().and_then(|c| c.english_key()), Some("python"));
}

#[test]
fn b09_chinese_first_when_english_is_weak() {
    let engine = engine_with(&base_words());
    engine.dict.upsert("哈", "ha", 900).expect("插入");
    let out = lookup(&engine, "ha");
    let zh: Vec<&str> = out
        .iter()
        .filter(|c| !c.is_english() && !c.echo)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(zh.first().copied(), Some("哈"), "中文首选不得被英文顶掉");
    let epos = out.iter().position(|c| c.is_english());
    assert!(epos.is_none() || epos.unwrap() >= 1, "英文不得排在首位");
}

#[test]
fn c04_exact_beats_higher_zipf_completion() {
    // 3 字母输入同时召回精确词与高频补全，确保两者真正同场竞争。
    let engine = engine_with(&[w("hel", "hel", 100), w("helloworld", "helloworld", 899)]);
    let keys = english_keys(&lookup(&engine, "hel"));
    assert_eq!(
        keys,
        vec!["hel".to_string(), "helloworld".to_string()],
        "精确匹配应排在更高频补全之前，且两者均应召回"
    );
}

#[test]
fn blocked_english_target_disappears_from_all_sources() {
    let engine = engine_with(&base_words());
    let en = engine.english().expect("应装配英文");
    assert!(english_keys(&lookup(&engine, "hel")).contains(&"help".to_string()));
    en.user()
        .block_candidate("help", crate::english::recall::ENGLISH_BLOCK_NAMESPACE);
    let keys = english_keys(&lookup(&engine, "hel"));
    assert!(!keys.contains(&"help".to_string()), "屏蔽目标不得再回来");
    assert!(keys.contains(&"hello".to_string()), "屏蔽一个词不影响别的词");
}

#[test]
fn learned_word_enters_recall_from_user_index() {
    let mut words: Vec<W> = (0..40)
        .map(|i| {
            let k: &'static str = Box::leak(crate::english::alpha_key("pta", i).into_boxed_str());
            W { key: k, display: k, q: 300 }
        })
        .collect();
    words.push(w("ptatarget", "ptatarget", 300));
    let engine = engine_with(&words);
    let en = engine.english().expect("应装配英文");
    assert!(!english_keys(&lookup(&engine, "pta")).contains(&"ptatarget".to_string()));
    let snapshot = en.user().snapshot();
    let hits = crate::english::recall_english(
        en.dict(),
        "pta",
        &EnglishContext::empty(),
        &snapshot,
        None,
    )
    .expect("召回失败");
    assert!(!hits.iter().any(|h| h.key == "ptatarget"));

    for _ in 0..20 {
        en.user().learn(None, "ptatarget");
    }
    let snapshot = en.user().snapshot();
    let hits = crate::english::recall_english(
        en.dict(),
        "pta",
        &EnglishContext::empty(),
        &snapshot,
        None,
    )
    .expect("召回失败");
    assert!(hits.iter().any(|h| h.key == "ptatarget"), "学过之后应进召回池");
    assert!(
        english_keys(&lookup(&engine, "pta")).contains(&"ptatarget".to_string()),
        "计数足够后应出现在展示面"
    );
}

#[test]
fn learned_bigram_promotes_the_next_word() {
    let engine = engine_with(&base_words());
    let en = engine.english().expect("应装配英文");
    for _ in 0..5 {
        en.user().learn(Some("hello"), "you");
    }
    let ctx = EnglishContext::with_previous("hello");
    let out = engine
        .lookup_composition("y", &LmContext::empty(), &ctx)
        .expect("查询失败");
    assert!(english_keys(&out).contains(&"you".to_string()));
    let first = out.iter().find(|c| c.is_english()).expect("应有英文候选");
    assert!(first.score > 0.0, "学过的词分数应为正: {}", first.score);
}

#[test]
fn escaped_word_is_not_revived_by_stale_user_counts() {
    let engine = engine_with(&base_words());
    let en = engine.english().expect("应装配英文");
    for _ in 0..5 {
        en.user().learn(None, "goneword");
    }
    assert!(english_keys(&lookup(&engine, "gone")).is_empty());
}

#[test]
fn blend_template_places_sources_by_band() {
    let mk = |word: &str, en: bool| -> Candidate {
        if en {
            Candidate::new_english(word, word, EnglishMatchKind::Completion, 1.0, 1)
        } else {
            Candidate::new(word, "ci", 100)
        }
    };
    let z: Vec<Candidate> = (0..9).map(|i| mk(&format!("中{i}"), false)).collect();
    let e: Vec<Candidate> = (0..3).map(|i| mk(&format!("e{i}"), true)).collect();

    let ef = crate::engine::blend::stable_blend(
        z.clone(),
        e.clone(),
        crate::engine::blend::IntentBand::EnglishFirst,
    );
    let head: Vec<bool> = ef.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![true, true, true, false, false, false, false, false, false]);

    let m = crate::engine::blend::stable_blend(
        z.clone(),
        e.clone(),
        crate::engine::blend::IntentBand::Mixed,
    );
    let head: Vec<bool> = m.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![false, true, false, true, false, false, false, false, false]);

    let cf = crate::engine::blend::stable_blend(
        z,
        e,
        crate::engine::blend::IntentBand::ChineseFirst,
    );
    let head: Vec<bool> = cf.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![false, false, false, false, false, false, false, false, true]);
}

#[test]
fn blend_dedupes_echo_but_keeps_case_different_candidate() {
    let mut zh = vec![Candidate::new("Python", "py", 5)];
    let mut echo = crate::engine::ImeEngine::echo_candidate("Python");
    echo.covered = 2;
    zh.push(echo);
    let en = vec![Candidate::new_english(
        "Python",
        "python",
        EnglishMatchKind::Completion,
        3.0,
        2,
    )];
    let out = crate::engine::blend::stable_blend(zh, en, crate::engine::blend::IntentBand::Mixed);
    assert_eq!(out.iter().filter(|c| c.echo).count(), 0);

    let mut zh = vec![crate::engine::ImeEngine::echo_candidate("python")];
    zh[0].covered = 2;
    let en = vec![Candidate::new_english(
        "Python",
        "python",
        EnglishMatchKind::Completion,
        3.0,
        5,
    )];
    let out = crate::engine::blend::stable_blend(zh, en, crate::engine::blend::IntentBand::Mixed);
    assert_eq!(out.len(), 2, "大小写或 covered 不同不算重复");
}
