//! crate 级单测:混排矩阵契约（英文候选字段、意图保护、中文无损与确定性）。

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

const WORDS: &[W] = &[
    W { key: "an", display: "an", q: 620 },
    W { key: "he", display: "he", q: 600 },
    W { key: "you", display: "you", q: 700 },
    W { key: "can", display: "can", q: 640 },
    W { key: "hello", display: "hello", q: 450 },
    W { key: "python", display: "Python", q: 410 },
];

fn lookup(engine: &ImeEngine, input: &str, prev: Option<&str>) -> Vec<Candidate> {
    let ctx = match prev {
        Some(p) => EnglishContext::with_previous(p),
        None => EnglishContext::empty(),
    };
    engine
        .lookup_composition(input, &LmContext::empty(), &ctx)
        .expect("查询失败")
}

fn first_lang(cands: &[Candidate]) -> &'static str {
    match cands.first() {
        None => "none",
        Some(c) if c.echo => "echo",
        Some(c) if c.is_english() => "english",
        _ => "chinese",
    }
}

#[test]
fn c03_c04_english_candidate_contract_fields() {
    let engine = engine_with(&[w("python", "Python", 410), w("hello", "hello", 450)]);

    let pyth = lookup(&engine, "pyth", None);
    let c = pyth
        .iter()
        .find(|c| c.english_key() == Some("python"))
        .expect("应召回 Python");
    assert_eq!(c.word, "Python");
    assert_eq!(c.english_match_kind(), Some(EnglishMatchKind::Completion));
    assert_eq!(c.covered, 4, "covered 为原始输入长度");
    assert_eq!(c.pinyin, "");
    assert!(!c.echo);
    assert_eq!(c.freq, 0);
    assert!((c.score - 3.80).abs() < 1e-6, "S={}", c.score);

    let hello = lookup(&engine, "hello", None);
    let c = hello
        .iter()
        .find(|c| c.english_key() == Some("hello"))
        .expect("应召回 hello");
    assert_eq!(c.english_match_kind(), Some(EnglishMatchKind::Exact));
    assert_eq!(c.covered, 5);
    assert!((c.score - 4.50).abs() < 1e-6, "S={}", c.score);
}

#[test]
fn c07_context_can_flip_a_short_input_to_english_without_touching_chinese_order() {
    let engine = engine_with(&[w("you", "you", 700)]);
    engine.dict.upsert("有", "you", 900).expect("插入");
    assert_eq!(first_lang(&lookup(&engine, "you", None)), "chinese");
    let out = lookup(&engine, "you", Some("thank"));
    assert_eq!(first_lang(&out), "english");
    assert!(out.iter().any(|c| c.word == "有"), "中文候选不得被挤掉");
}

#[test]
fn c07b_completion_only_english_yields_to_readable_chinese_even_after_english() {
    let engine = engine_with(&[w("keep", "keep", 520), w("key", "key", 540)]);
    engine.dict.upsert("可", "ke", 900).expect("插入");
    let out = lookup(&engine, "ke", Some("thank"));
    assert_eq!(first_lang(&out), "chinese");
    assert_eq!(out.get(1).map(|c| c.is_english()), Some(true));
}

#[test]
fn c07c_abbrev_reading_counts_as_readable_chinese() {
    use crate::engine::blend::pinyin_reads_input as reads;
    assert!(reads(b"yidd", "yi dian dian"));
    assert!(reads(b"yid", "yi dian dian"));
    assert!(reads(b"zhonggr", "zhong guo ren"));
    assert!(!reads(b"zhgr", "zhong guo ren"));
    assert!(!reads(b"ydd", "yi dian dian"));
    assert!(!reads(b"hello", "hei long"));

    let Some(engine) = real_bilingual_engine() else {
        return;
    };
    for (input, word) in [("ann", "按钮"), ("ans", "按时"), ("yidd", "一点点")] {
        for prev in [None, Some("hello")] {
            let out = lookup(engine, input, prev);
            assert_eq!(first_lang(&out), "chinese", "{input}, prev={prev:?}");
            assert!(
                out.iter().take(3).any(|c| c.word == word),
                "{input}, prev={prev:?}: 前 3 应含「{word}」, 实际 {:?}",
                out.iter().take(4).map(|c| &c.word).collect::<Vec<_>>()
            );
            assert!(out.iter().any(|c| c.is_english()), "英文入口保留: {input}");
        }
    }
    let out = lookup(engine, "yiddish", None);
    assert_eq!(first_lang(&out), "english", "完整英文词仍排前");
}

#[test]
fn c07d_pure_consonant_abbrev_with_real_words_keeps_chinese_first() {
    let Some(engine) = real_bilingual_engine() else {
        return;
    };
    for input in ["shy", "sms", "sds", "ssh"] {
        for prev in [None, Some("hello")] {
            let out = lookup(engine, input, prev);
            assert_eq!(first_lang(&out), "chinese", "{input}, prev={prev:?}");
            assert!(out.iter().any(|c| c.is_english()), "英文入口保留: {input}");
        }
    }
    for input in ["cof", "doc", "pyt", "app"] {
        let out = lookup(engine, input, None);
        assert_eq!(first_lang(&out), "english", "{input} 应保持英文首选");
    }
}

#[test]
fn c10_no_english_component_equals_old_entry_point_including_json() {
    let bare = ImeEngine::new(None).expect("内存引擎创建失败");
    bare.dict.upsert("你好", "ni hao", 10).expect("插入");
    let with_en = engine_with(WORDS);
    with_en.dict.upsert("你好", "ni hao", 10).expect("插入");
    for input in ["ni", "niha", "xi'an", "", "12345"] {
        let a = lookup(&bare, input, None);
        let b: Vec<_> = lookup(&with_en, input, None)
            .into_iter()
            .filter(|c| !c.is_english())
            .collect();
        assert_eq!(
            serde_json::to_string(&a).expect("序列化"),
            serde_json::to_string(&b).expect("序列化"),
            "{input}: 无英文命中时须与旧入口逐字节一致"
        );
    }
}

#[test]
fn c12_full_chinese_page_keeps_every_chinese_candidate() {
    use crate::engine::blend::{stable_blend, IntentBand};
    let full = crate::engine::max_candidates();
    let z: Vec<Candidate> = (0..full)
        .map(|i| Candidate::new(format!("中{i}"), "ci", 100))
        .collect();
    let e = vec![Candidate::new_english(
        "Python",
        "python",
        EnglishMatchKind::Completion,
        1.0,
        1,
    )];
    let out = stable_blend(z.clone(), e, IntentBand::ChineseFirst);
    for c in &z {
        assert!(out.iter().any(|o| o.word == c.word), "满上限时丢了 {}", c.word);
    }
    assert!(out.len() > full, "英文应追加而不是替换");
}

#[test]
fn c14_queries_are_deterministic_and_never_touch_the_user_model() {
    let engine = engine_with(WORDS);
    let en = engine.english().expect("应装配英文");
    let before = en.user().snapshot();
    let a = lookup(&engine, "pyt", None);
    for _ in 0..5 {
        let b = lookup(&engine, "pyt", None);
        assert_eq!(
            serde_json::to_string(&a).expect("序列化"),
            serde_json::to_string(&b).expect("序列化")
        );
        assert!(b.iter().all(|c| c.score.is_finite()));
    }
    assert_eq!(before, en.user().snapshot(), "查询不得改动用户模型");
}
