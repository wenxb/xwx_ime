//! crate 级单测:混排矩阵的其余条款（C03/C07/C08/C10/C12/C14）。
//!
//! `bilingual.rs` 覆盖 B 组与 C04/C11/C13；这里补齐「按规则而不是按单词
//! 名单」「中文不受损」「查询无副作用」三条主线。

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
    W { key: "women", display: "women", q: 560 },
    W { key: "hello", display: "hello", q: 450 },
    W { key: "python", display: "Python", q: 410 },
    W { key: "docker", display: "docker", q: 410 },
    W { key: "today", display: "today", q: 500 },
    W { key: "text", display: "text", q: 480 },
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

/// 首个候选的语言。
fn first_lang(cands: &[Candidate]) -> &'static str {
    match cands.first() {
        None => "none",
        Some(c) if c.echo => "echo",
        Some(c) if c.is_english() => "english",
        _ => "chinese",
    }
}

// ---------- C03/C04：契约字段 ----------

#[test]
fn c03_pyth_completion_carries_input_length_as_covered_and_hand_checked_score() {
    let engine = engine_with(&[w("python", "Python", 410)]);
    let out = lookup(&engine, "pyth", None);
    let c = out
        .iter()
        .find(|c| c.english_key() == Some("python"))
        .expect("应召回 Python");
    assert_eq!(c.word, "Python", "显示用规范大小写");
    assert_eq!(c.english_match_kind(), Some(EnglishMatchKind::Completion));
    assert_eq!(c.covered, 4, "covered 是原始输入长度，不是补全后的 6");
    assert_eq!(c.pinyin, "");
    assert!(!c.echo);
    assert_eq!(c.freq, 0, "不能伪造中文词频");
    assert!((c.score - 3.80).abs() < 1e-6, "S={}", c.score);
}

#[test]
fn c04_hello_exact_is_exact_kind_with_input_length_covered() {
    let engine = engine_with(&[w("hello", "hello", 450)]);
    let out = lookup(&engine, "hello", None);
    let c = out
        .iter()
        .find(|c| c.english_key() == Some("hello"))
        .expect("应召回 hello");
    assert_eq!(c.english_match_kind(), Some(EnglishMatchKind::Exact));
    assert_eq!(c.covered, 5);
    assert!((c.score - 4.50).abs() < 1e-6, "S={}", c.score);
}

// ---------- C07/C08：意图按规则而不是按单词名单 ----------

#[test]
fn c07_short_ambiguous_words_have_no_english_without_context() {
    // an/he/you/can/women 都是**完整单词**：无上文时 2 字母只查精确、
    // 4-5 字母正常精确召回，但不做前缀展开。
    let engine = engine_with(WORDS);
    for input in ["an", "he", "you", "can", "women"] {
        let out = lookup(&engine, input, None);
        let has = out.iter().any(|c| c.english_key() == Some(input));
        assert!(has, "{input} 是完整词，应当精确召回");
    }
    // 无上文时 2 字母不得展开前缀：`he` 不该带出 hello。
    let out = lookup(&engine, "he", None);
    assert!(
        !out.iter().any(|c| c.english_key() == Some("hello")),
        "2 字母无上文不得补全"
    );
}

#[test]
fn c07_context_can_flip_a_short_input_to_english_without_touching_chinese_order() {
    let engine = engine_with(&[w("you", "you", 700)]);
    engine.dict.upsert("有", "you", 900).expect("插入");
    // 无上文：中文「有」的 freq 高、spelling=Exact、covered=3 ⇒ strong_zh。
    let out = lookup(&engine, "you", None);
    assert_eq!(first_lang(&out), "chinese", "无上文时保护中文首选");
    // 有可信英文上文：「有」依然在（中文候选一条都不少），只是英文靠前。
    let out = lookup(&engine, "you", Some("thank"));
    assert_eq!(first_lang(&out), "english", "有上文时英文可以靠前");
    assert!(
        out.iter().any(|c| c.word == "有"),
        "中文候选不得被挤掉，只允许后移"
    );
}

#[test]
fn c07b_completion_only_english_yields_to_readable_chinese_even_after_english() {
    // ke 在英文词库里只是 keep/key 的**前缀补全**（Completion），不是
    // 完整词——证明不了「用户在连着打英文」，管不了分档：可 是中文
    // 音节，中文在前，英文退到第 2 位仍可点（用户实测的 ke 案例）。
    let engine = engine_with(&[w("keep", "keep", 520), w("key", "key", 540)]);
    engine.dict.upsert("可", "ke", 900).expect("插入");
    let out = lookup(&engine, "ke", Some("thank"));
    assert_eq!(first_lang(&out), "chinese", "补全英文不得顶掉中文音节");
    assert!(out.get(1).map(|c| c.is_english()) == Some(true), "英文仍在第 2 位: {out:?}");
}

#[test]
fn c07c_abbrev_reading_counts_as_readable_chinese() {
    // 简拼读法判据的对齐器：`yidd` → `yi dian dian`（yi | d | d）读得
    // 出来；`hello`（辅音连缀）读不出来。**首音节必须打全**：纯声母
    // 开头的 `zhgr`/`ydd` 不算——那是英文前缀的形状（pyt/app 同款），
    // 它们没有英文竞争时轮不到本判据。
    use crate::engine::blend::pinyin_reads_input as reads;
    assert!(reads(b"yidd", "yi dian dian"));
    assert!(reads(b"yid", "yi dian dian"), "没打完的尾巴也是读法");
    assert!(reads(b"zhonggr", "zhong guo ren"), "zhong 打全后 g、r 是简拼");
    assert!(!reads(b"zhgr", "zhong guo ren"), "纯声母开头不算");
    assert!(!reads(b"ydd", "yi dian dian"), "纯声母开头不算");
    assert!(!reads(b"hello", "hei long"));
    // 端到端（真实词库 + 真实英文词库，缺件跳过）：yiddish 不得顶掉
    // 一点点；整词输入照旧英文优先——修的是简拼读法漏判，不是压英文。
    let Some(engine) = crate::engine::tests::real_bilingual_engine() else {
        return;
    };
    let out = engine
        .lookup_composition("yidd", &LmContext::empty(), &EnglishContext::empty())
        .expect("查询失败");
    assert!(!out.first().expect("候选非空").is_english(), "yidd 首选应是中文: {out:?}");
    let out = engine
        .lookup_composition("yiddish", &LmContext::empty(), &EnglishContext::empty())
        .expect("查询失败");
    assert_eq!(first_lang(&out), "english", "完整英文词仍应排前");
}

#[test]
fn c07d_pure_consonant_abbrev_with_real_words_keeps_chinese_first() {
    // 纯声母简拼（`shy`/`sms`/`sds`）走不了「首音节打全」的简拼读法，
    // 于是 E0 只要 Exact 就落到 EnglishFirst、占掉前三个槽位——用户实测
    // 「打拼音被无效的英文单词挡在前面」。修法：中文整串简拼候选的词频
    // 先验压过 E0 时，按 Mixed 排（中文回到首位，英文仍在第 2 位）。
    let Some(engine) = crate::engine::tests::real_bilingual_engine() else {
        return;
    };
    for input in ["shy", "sms", "sds", "ssh", "ssh"] {
        let out = engine
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        assert_eq!(
            first_lang(&out),
            "chinese",
            "{input} 的中文简拼词条词频远高于 E0，中文应首选: {out:?}"
        );
        assert!(
            out.iter().any(|c| c.is_english()),
            "{input} 的英文入口不得消失（只调次序）: {out:?}"
        );
    }
    // 反向：中文端没有整串简拼词条，或中文词频明显更低 ⇒ 英文照旧靠前。
    for input in ["cof", "doc", "pyt", "app"] {
        let out = engine
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        assert_eq!(first_lang(&out), "english", "{input} 应保持英文首选: {out:?}");
    }
}

#[test]
fn c08_english_first_is_a_rule_not_a_word_list() {
    // 同一批词：做成完整词输入时不抢中文；做成超出静态池的长尾输入时
    // 由 good_en/strong_zh 判据决定，而不是「词表里是否有这个词」。
    let engine = engine_with(&[w("docker", "docker", 410)]);
    // 词库里没有 docker 的中文候选 ⇒ Z 为空 ⇒ EnglishFirst
    let out = lookup(&engine, "docker", None);
    assert_eq!(first_lang(&out), "english");
    // 加一个高频同形中文词 ⇒ strong_zh 成立 ⇒ 中文首选
    engine.dict.upsert("多云", "duoyun", 100).expect("插入");
    let out = lookup(&engine, "duoyun", None);
    assert_eq!(first_lang(&out), "chinese", "强中文证据时按规则让位");
}

// ---------- C10：不装英文 / 召回为空 = 旧入口 ----------

#[test]
fn c10_no_english_component_equals_old_entry_point_including_json() {
    let bare = ImeEngine::new(None).expect("内存引擎创建失败");
    bare.dict.upsert("你好", "ni hao", 10).expect("插入");
    let with_en = engine_with(WORDS);
    with_en.dict.upsert("你好", "ni hao", 10).expect("插入");
    // 只挑**英文召回为空**的输入：`hello` 会被英文真实召回，那时按第 9 节
    // 可以去掉同 word+covered 的原文回声（B 组的 b07 已单独钉住这一点）。
    for input in ["ni", "niha", "xi'an", "", "12345"] {
        let a = bare
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        let b = with_en
            .lookup_composition(input, &LmContext::empty(), &EnglishContext::empty())
            .expect("查询失败");
        assert_eq!(
            serde_json::to_string(&a).expect("序列化"),
            serde_json::to_string(&b.iter().filter(|c| !c.is_english()).cloned().collect::<Vec<_>>())
                .expect("序列化"),
            "{input}: 关掉英文时结果必须与旧入口逐字节相同"
        );
    }
}

#[test]
fn c10_empty_english_recall_returns_the_chinese_vector_untouched() {
    // 英文池为空（这里用不含任何英文词的词库）⇒ 原样返回，不重排。
    let engine = engine_with(&[w("zzz", "zzz", 100)]);
    engine.dict.upsert("你好", "ni hao", 100).expect("插入");
    let zh = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    let out = lookup(&engine, "ni", None);
    assert_eq!(
        serde_json::to_string(&zh).expect("序列化"),
        serde_json::to_string(&out).expect("序列化")
    );
}

// ---------- D09：坏英文不影响中文 ----------

#[test]
fn d09_a_corrupt_english_file_leaves_chinese_queries_untouched() {
    // 坏文件在**装配期**就被拒（`EnglishDict::from_bytes` 返回 Err），
    // 装配层随之只关闭英文。这里钉住「关掉之后中文查询逐字节不变」。
    let good = build_synth_bin(WORDS);
    let mut bad = good.clone();
    bad[1] = b'Z'; // 坏魔数（下标 0 本来就是 'X'）
    assert!(EnglishDict::from_bytes(bad).is_err(), "坏文件必须被拒绝");

    let zh_only = ImeEngine::new(None).expect("内存引擎创建失败");
    zh_only.dict.upsert("你好", "ni hao", 50).expect("插入");
    let still_chinese = engine_with(WORDS);
    still_chinese.dict.upsert("你好", "ni hao", 50).expect("插入");
    for input in ["ni", "niha", "nihao"] {
        let a = zh_only
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let b = still_chinese
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert_eq!(
            serde_json::to_string(&a).expect("序列化"),
            serde_json::to_string(&b).expect("序列化"),
            "{input}: 中文路径不得受英文装配影响"
        );
    }
}

// ---------- C12：中文满上限时不被混排挤掉 ----------

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
        assert!(
            out.iter().any(|o| o.word == c.word),
            "满上限时丢了中文候选 {}",
            c.word
        );
    }
    assert!(
        out.len() > full,
        "英文应当追加而不是替换：{} ≤ {}",
        out.len(),
        full
    );
}

// ---------- C14：确定性与无副作用 ----------

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
            serde_json::to_string(&b).expect("序列化"),
            "同一查询必须逐字节确定"
        );
        for c in &b {
            assert!(c.score.is_finite(), "英文分数必须有限: {}", c.score);
        }
    }
    let after = en.user().snapshot();
    assert_eq!(
        before, after,
        "查询路径不得改动用户模型（快照的 Arc 都没换过）"
    );
}
