//! crate 级单测:输入坐标分离与音节归一口径。

use super::*;
use crate::engine::input::NormalizedInput;
use crate::segmenter::{self, SYLLABLES};

/// 原文 / 干净串 / 边界坐标分离及消费长度换算。
#[test]
fn normalized_input_keeps_raw_clean_and_bounds_apart() {
    let a = NormalizedInput::new("xi'an");
    assert_eq!(a.raw(), "xi'an");
    assert_eq!(a.clean(), "xian");
    assert_eq!(a.bounds(), Some(&[2usize][..]));
    assert_eq!(a.clean_len(), 4);
    assert_eq!(a.to_raw_len(0), 0);
    assert_eq!(a.to_raw_len(2), 3, "覆盖停在分隔符前应吃掉分隔符");
    assert_eq!(a.to_raw_len(4), 5);
    assert_eq!(a.to_raw_len(9), 5, "越界长度钳在整串");

    let b = NormalizedInput::new("nihao");
    assert_eq!(b.raw(), "nihao");
    assert_eq!(b.clean(), "nihao");
    assert_eq!(b.bounds(), None);
    for k in 0..=5 {
        assert_eq!(b.to_raw_len(k), k);
    }

    let c = NormalizedInput::new("'''");
    assert!(c.is_empty());
    assert_eq!(c.clean_len(), 0);
    assert_eq!(c.to_raw_len(0), 3);
}

/// 音节归一口径唯一：`lue`/`nue` 归一到 `lve`/`nve`，`lu`/`nu` 不归一，且与 `unreachable_tail` 一致。
#[test]
fn syllable_matching_has_a_single_normalization() {
    assert_eq!(segmenter::syllable_at("lue", 0, 3), Some("lve"));
    assert_eq!(segmenter::syllable_at("nue", 0, 3), Some("nve"));
    assert_eq!(segmenter::syllable_at("lve", 0, 3), Some("lve"));
    assert_eq!(segmenter::syllable_at("lu", 0, 2), Some("lu"));
    assert_eq!(segmenter::syllable_at("nu", 0, 2), Some("nu"));
    assert_eq!(segmenter::syllable_at("xianq", 0, 4), Some("xian"));
    assert_eq!(segmenter::syllable_at("xian", 4, 1), None);
    assert_eq!(segmenter::syllable_at("zhong", 0, 4), None);
    assert_eq!(segmenter::syllable_at("xianq", 4, 1), None);

    assert!(SYLLABLES.contains(&"lve") && !SYLLABLES.contains(&"lue"));
    assert!(SYLLABLES.contains(&"nve") && !SYLLABLES.contains(&"nue"));

    let seg = Segmenter::new();
    for syl in ["lue", "nue", "lve", "nve", "lu", "nu", "luan", "xian", "zhong"] {
        assert!(seg.is_syllable(syl), "分词器应认得 {syl}");
        assert_eq!(unreachable_tail(syl, None)[0], 0, "{syl} 应从 0 可达");
    }
    assert_eq!(unreachable_tail("xian", None)[0], 0);
    assert_eq!(unreachable_tail("xianq", None)[0], 5);
    assert!(unreachable_tail("qqq", None)[0] > 0);
}

/// 所有候选覆盖长度均落在原文坐标的 `(0, raw.len()]` 区间内。
#[test]
fn candidates_cover_a_nonzero_prefix_in_raw_coordinates() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    for raw in [
        "nihao",
        "zhongg",
        "xi'an",
        "niha",
        "lue",
        "womenjintianquxian",
        "nihoa",
    ] {
        let cands = engine
            .lookup_candidates(raw, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "{raw} 不该没有候选");
        for c in &cands {
            assert!(
                c.covered > 0 && c.covered <= raw.len(),
                "{raw} 的候选 {} 覆盖 {} 字节，超出 1..={}",
                c.word,
                c.covered,
                raw.len()
            );
        }
    }
}

/// `'` 禁止单个音节跨越，但允许多音节词（如 `xi'an` → 西安）跨过分隔符。
#[test]
fn separator_blocks_syllables_but_not_words() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let cands = engine
        .lookup_candidates("xi'an", &LmContext::empty())
        .expect("查询失败");
    let xian = cands
        .iter()
        .find(|c| c.word == "西安")
        .expect("`xi'an` 应召回「西安」");
    assert_eq!(xian.covered, 5, "「西安」覆盖整串原文（含 `'`）");

    let boundary = 2usize; // 干净串 `xian` 中 `'` 的字节偏移
    for c in &cands {
        if c.echo {
            continue;
        }
        let mut pos = 0usize;
        for syl in c.pinyin.split(' ') {
            let end = pos + syl.len();
            assert!(
                !(pos < boundary && boundary < end),
                "候选 {} 的音节 {syl} 跨过分隔符(干净串 {pos}..{end})",
                c.word
            );
            pos = end;
        }
    }
}

/// 合法音节输入不再凭空召回含未输入音节的词条。
#[test]
fn en_no_longer_recalls_words_with_untyped_syllables() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let cands = engine
        .lookup_candidates("en", &LmContext::empty())
        .expect("查询失败");
    let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
    assert!(words.contains(&"恩"));
    assert!(!words.contains(&"婀娜多姿"), "en 不应跨界召回未输入音节");

    let nihao = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert!(nihao.iter().all(|c| c.pinyin.split(' ').count() <= 2));
}
