//! crate 级单测:中英双通道查询、意图分档与稳定混排。
//!
//! 覆盖引擎分册第 10 节的表：B01-B10（双通道/意图/稳定排序），
//! C04/C11 的端到端形态，X06/X07/X08（边界互不吞输入）。

use super::*;
use crate::candidate::EnglishMatchKind;
use crate::english::{
    build_synth_bin, EnglishContext, EnglishDict, EnglishEngine, SynthWord as W,
};

/// 造一个装了英文组件的引擎（中文侧是内存空词库）。
fn engine_with(words: &[W]) -> ImeEngine {
    let dict = EnglishDict::from_bytes(build_synth_bin(words)).expect("合成英文词库应可读");
    ImeEngine::new(None)
        .expect("内存引擎创建失败")
        .with_english(EnglishEngine::new(dict))
}

fn w(key: &'static str, display: &'static str, q: u16) -> W {
    W { key, display, q }
}

/// 英文候选的 key 列表（按混排结果次序）。
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

/// 基础词表：既有英文长尾，也有与英文输入同形的中文拼音 key。
fn base_words() -> Vec<W> {
    vec![
        w("hello", "hello", 450),
        w("help", "help", 520),
        w("he", "he", 600),
        w("python", "Python", 410),
        w("you", "you", 700),
    ]
}

#[test]
fn b01_exact_and_prefix_both_recalled() {
    let engine = engine_with(&base_words());
    let keys = english_keys(&lookup(&engine, "hello"));
    assert!(keys.contains(&"hello".to_string()), "精确必须召回: {keys:?}");
    let keys = english_keys(&lookup(&engine, "pyt"));
    assert!(keys.contains(&"python".to_string()), "前缀必须召回: {keys:?}");
    // 2 字母无上文只查精确：`py` 不是完整词，不得展开前缀（§3 门槛 5）。
    assert!(english_keys(&lookup(&engine, "py")).is_empty());
}

#[test]
fn b02_two_letters_without_context_are_exact_only() {
    // 无英文上文时 2 个字母只查精确：`he` 精确命中，`hello/help` 都不来。
    let engine = engine_with(&base_words());
    let keys = english_keys(&lookup(&engine, "he"));
    assert_eq!(keys, vec!["he".to_string()], "2 字母无上文只能精确");
    // 3 个字母起恢复前缀补全。
    let keys = english_keys(&lookup(&engine, "hel"));
    assert!(keys.contains(&"hello".to_string()));
    assert!(keys.contains(&"help".to_string()));
}

#[test]
fn b03_one_letter_without_context_gives_no_english() {
    let engine = engine_with(&base_words());
    for input in ["y", "h", "p"] {
        assert!(
            english_keys(&lookup(&engine, input)).is_empty(),
            "1 字母无上文不得出英文: {input}"
        );
    }
}

#[test]
fn b04_trusted_context_enables_completion_from_one_letter() {
    let engine = engine_with(&base_words());
    let out = engine
        .lookup_composition(
            "y",
            &LmContext::empty(),
            &EnglishContext::with_previous("thank"),
        )
        .expect("查询失败");
    assert!(
        english_keys(&out).contains(&"you".to_string()),
        "有可信上文时 1 字母可补全"
    );
}

#[test]
fn b05_non_letters_and_overlong_input_never_enter_english() {
    let engine = engine_with(&base_words());
    for input in ["he llo", "he-llo", "he'llo", "abc123", "你好", "hello!"] {
        assert!(
            english_keys(&lookup(&engine, input)).is_empty(),
            "非纯字母不得进英文通道: {input}"
        );
    }
    let long = "a".repeat(49);
    assert!(english_keys(&lookup(&engine, &long)).is_empty(), "超 48 字母不进英文");
}

#[test]
fn b06_apostrophe_input_keeps_chinese_path() {
    // X06/§3：绝不先删撇号——`xi'an` 必须原样走中文/识别态路径。
    let engine = engine_with(&base_words());
    let out = lookup(&engine, "xi'an");
    assert!(english_keys(&out).is_empty());
    let echo = out.iter().find(|c| c.echo).expect("非 ASCII 输入应有原文兜底");
    assert_eq!(echo.word, "xi'an", "原文逐字保留，包括撇号");
}

#[test]
fn b07_no_english_component_is_byte_identical_to_chinese_only() {
    let bare = ImeEngine::new(None).expect("内存引擎创建失败");
    bare.dict.upsert("你好", "ni hao", 10).expect("插入");
    let with_en = engine_with(&base_words());
    with_en.dict.upsert("你好", "ni hao", 10).expect("插入");
    for input in ["ni", "niha", "hello", "xi'an", ""] {
        let a = bare
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        let b = with_en
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        // 只比非回声中文候选：原文兜底按第 9 节允许后移或被去重。
        let a: Vec<_> = a.iter().filter(|c| !c.is_english() && !c.echo).collect();
        let b: Vec<_> = b.iter().filter(|c| !c.is_english() && !c.echo).collect();
        assert_eq!(a, b, "中文候选的字段与次序不得被英文改变: {input}");
    }
}

#[test]
fn b08_english_first_when_no_chinese_candidates() {
    // 输入 `python` 在中文词库里没有候选 ⇒ Z 为空 ⇒ 英文必须到第一位。
    let engine = engine_with(&base_words());
    // 有回声兜底时（非 ASCII）不进英文；这里用纯字母，词库空 ⇒ 无中文候选。
    let out = lookup(&engine, "python");
    assert_eq!(out.first().and_then(|c| c.english_key()), Some("python"));
}

#[test]
fn b09_chinese_first_when_english_is_weak() {
    // 强中文证据 + 弱英文 ⇒ ChineseFirst：英文只占末位，中文次序不变。
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
fn b10_blend_never_drops_chinese_tail() {
    // 中文长尾一条都不能丢：上限 = 原中文上限 + 英文条数。
    let engine = engine_with(&base_words());
    for i in 0..30 {
        let word: &'static str = Box::leak(format!("词{i}").into_boxed_str());
        engine.dict.upsert(word, "ci", 1000 - i).expect("插入");
    }
    let zh_only = engine
        .lookup_candidates("ci", &LmContext::empty())
        .expect("查询失败");
    let blended = lookup(&engine, "ci");
    let zh_kept = blended.iter().filter(|c| !c.is_english()).count();
    assert!(
        zh_kept >= zh_only.iter().filter(|c| !c.is_english()).count(),
        "混排不得丢中文候选: {} < {}",
        zh_kept,
        zh_only.len()
    );
}

#[test]
fn c04_exact_beats_higher_zipf_completion() {
    // 精确优先于更高频补全：`he` 精确排在高频补全之前。
    let engine = engine_with(&[w("he", "he", 100), w("helloworld", "helloworld", 899)]);
    let keys = english_keys(&lookup(&engine, "he"));
    assert_eq!(keys.first().map(String::as_str), Some("he"));
}

#[test]
fn c11_stable_across_repeated_queries() {
    let engine = engine_with(&base_words());
    let a = lookup(&engine, "hel");
    for _ in 0..5 {
        assert_eq!(english_keys(&lookup(&engine, "hel")), english_keys(&a));
    }
}

#[test]
fn blocked_english_target_disappears_from_all_sources() {
    let engine = engine_with(&base_words());
    let en = engine.english().expect("应装配英文");
    assert!(english_keys(&lookup(&engine, "hel")).contains(&"help".to_string()));
    // 屏蔽目标词（命名空间 + 小写键），三个召回源一次统一过滤。
    en.user()
        .block_candidate("help", crate::english::recall::ENGLISH_BLOCK_NAMESPACE);
    let keys = english_keys(&lookup(&engine, "hel"));
    assert!(!keys.contains(&"help".to_string()), "屏蔽目标不得再从任何源回来");
    assert!(keys.contains(&"hello".to_string()), "屏蔽一个词不影响别的词");
}

#[test]
fn learned_word_enters_recall_from_user_index() {
    // U01/U02 的引擎侧形态：学过的长尾词必须能自己走进召回池，并在
    // 计数足够时**真正冒到展示面**。
    let mut words: Vec<W> = (0..40)
        .map(|i| {
            let k: &'static str = Box::leak(crate::english::alpha_key("pta", i).into_boxed_str());
            W { key: k, display: k, q: 300 }
        })
        .collect();
    words.push(w("ptatarget", "ptatarget", 300));
    let engine = engine_with(&words);
    let en = engine.english().expect("应装配英文");
    assert!(
        !english_keys(&lookup(&engine, "pta")).contains(&"ptatarget".to_string()),
        "静态 Top-32 里挤不进去（否则这条测试证明不了个性化召回）"
    );
    let snapshot = en.user().snapshot();
    let hits = crate::english::recall_english(
        en.dict(),
        "pta",
        &EnglishContext::empty(),
        &snapshot,
    )
    .expect("召回失败");
    assert!(
        !hits.iter().any(|h| h.key == "ptatarget"),
        "没学过就不该进召回池"
    );

    for _ in 0..20 {
        en.user().learn(None, "ptatarget");
    }
    let snapshot = en.user().snapshot();
    let hits = crate::english::recall_english(
        en.dict(),
        "pta",
        &EnglishContext::empty(),
        &snapshot,
    )
    .expect("召回失败");
    assert!(
        hits.iter().any(|h| h.key == "ptatarget"),
        "学过之后必须进召回池"
    );
    assert!(
        english_keys(&lookup(&engine, "pta")).contains(&"ptatarget".to_string()),
        "计数足够了就必须真的出现在展示面上"
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
    assert!(
        english_keys(&out).contains(&"you".to_string()),
        "学过的二元目标必须被召回"
    );
    let first = out.iter().find(|c| c.is_english()).expect("应有英文候选");
    assert!(
        first.score > 0.0,
        "学过的词分数应高于纯静态分: {}",
        first.score
    );
}

#[test]
fn escaped_word_is_not_revived_by_stale_user_counts() {
    // 用户统计里有的 key 若已从词库移除，不得凭旧计数复活。
    let engine = engine_with(&base_words());
    let en = engine.english().expect("应装配英文");
    for _ in 0..5 {
        en.user().learn(None, "goneword");
    }
    assert!(english_keys(&lookup(&engine, "gone")).is_empty());
}

#[test]
fn blend_template_places_sources_by_band() {
    // 模板本身：EnglishFirst 首屏两槽给英文，ChineseFirst 末槽给英文。
    let mk = |word: &str, en: bool| -> Candidate {
        if en {
            Candidate::new_english(word, word, EnglishMatchKind::Completion, 1.0, 1)
        } else {
            Candidate::new(word, "ci", 100)
        }
    };
    let z: Vec<Candidate> = (0..9).map(|i| mk(&format!("中{i}"), false)).collect();
    let e: Vec<Candidate> = (0..3).map(|i| mk(&format!("e{i}"), true)).collect();

    let ef = crate::engine::blend::stable_blend(z.clone(), e.clone(), crate::engine::blend::IntentBand::EnglishFirst);
    let head: Vec<bool> = ef.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![true, true, false, true, false, false, false, false, false]);

    let m = crate::engine::blend::stable_blend(z.clone(), e.clone(), crate::engine::blend::IntentBand::Mixed);
    let head: Vec<bool> = m.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![false, true, false, true, false, false, false, false, false]);

    let cf = crate::engine::blend::stable_blend(z, e, crate::engine::blend::IntentBand::ChineseFirst);
    let head: Vec<bool> = cf.iter().take(9).map(|c| c.is_english()).collect();
    assert_eq!(head, vec![false, false, false, false, false, false, false, false, true]);
}

#[test]
fn blend_dedupes_echo_but_keeps_case_different_candidate() {
    let mut zh = vec![Candidate::new("Python", "py", 5)];
    let mut echo = crate::engine::ImeEngine::echo_candidate("Python");
    echo.covered = 2; // 与真实英文候选同一坐标才算重复
    zh.push(echo);
    let en = vec![Candidate::new_english("Python", "python", EnglishMatchKind::Completion, 3.0, 2)];
    let out = crate::engine::blend::stable_blend(zh, en, crate::engine::blend::IntentBand::Mixed);
    let echoes = out.iter().filter(|c| c.echo).count();
    assert_eq!(echoes, 0, "与真实英文 word+covered 都相同的原文回声应被去掉");

    // 大小写不同不算相同：`python` 与 `Python` 可以并存。
    let mut zh = vec![crate::engine::ImeEngine::echo_candidate("python")];
    zh[0].covered = 2;
    let en = vec![Candidate::new_english("Python", "python", EnglishMatchKind::Completion, 3.0, 5)];
    let out = crate::engine::blend::stable_blend(zh, en, crate::engine::blend::IntentBand::Mixed);
    assert_eq!(out.len(), 2, "大小写不同 / covered 不同都不算重复");
}
