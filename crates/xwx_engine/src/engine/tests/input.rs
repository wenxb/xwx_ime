//! crate 级单测:输入解释的共享数据。
//!
//! 这里钉住的是「三套坐标不许混用」与「音节匹配只有一份口径」两类不变量。
//! 真实词库缺失时按既有约定跳过(与 `completion.rs` 等同一 fixture 口径)。

use super::*;
use crate::engine::input::NormalizedInput;
use crate::segmenter::{self, SYLLABLES};

fn real_engine() -> Option<ImeEngine> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict_path = root.join("dict.bin");
    let abbrev_path = root.join("abbrev.bin");
    if !dict_path.is_file() || !abbrev_path.is_file() {
        return None;
    }
    let mut engine = ImeEngine::new_with_lm(Some(&dict_path), None).expect("引擎创建失败");
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    Some(engine)
}

/// 原文 / 干净串 / 候选规范拼音是三种坐标;归一化只改中间那一套。
#[test]
fn normalized_input_keeps_raw_clean_and_bounds_apart() {
    let a = NormalizedInput::new("xi'an");
    assert_eq!(a.raw(), "xi'an", "原文必须原样保留(展示坐标)");
    assert_eq!(a.clean(), "xian", "干净串去掉 `'`(切分/查表坐标)");
    assert_eq!(a.bounds(), Some(&[2usize][..]), "边界是干净串里的字节偏移");
    assert_eq!(a.clean_len(), 4);
    // 消费长度换算:`xi`(干净 2)含它后面那个 `'` → 3;整串 → 5。
    assert_eq!(a.to_raw_len(0), 0);
    assert_eq!(a.to_raw_len(2), 3, "覆盖停在分隔符前也要吃掉分隔符");
    assert_eq!(a.to_raw_len(4), 5);
    assert_eq!(a.to_raw_len(9), 5, "越界长度钳在整串");

    // 无 `'`:零分配路径,映射是恒等。
    let b = NormalizedInput::new("nihao");
    assert_eq!(b.raw(), "nihao");
    assert_eq!(b.clean(), "nihao");
    assert_eq!(b.bounds(), None);
    for k in 0..=5 {
        assert_eq!(b.to_raw_len(k), k);
    }

    // 全是 `'`:干净串为空,每个 `'` 都算被消费。
    let c = NormalizedInput::new("'''");
    assert!(c.is_empty());
    assert_eq!(c.clean_len(), 0);
    assert_eq!(c.to_raw_len(0), 3);
}

/// 音节匹配只有一份口径:静态表里的每个音节(含 üe 的两种写法)都必须
/// 被 [`unreachable_tail`] 认成「从位置 0 可达」,`lue`/`nue` 归一成
/// 词库写法,而 `lu`/`nu` 不许被当成 `lv`/`nv`(用户已裁决)。
#[test]
fn syllable_matching_has_a_single_normalization() {
    assert_eq!(segmenter::syllable_at("lue", 0, 3), Some("lve"));
    assert_eq!(segmenter::syllable_at("nue", 0, 3), Some("nve"));
    assert_eq!(segmenter::syllable_at("lve", 0, 3), Some("lve"));
    assert_eq!(segmenter::syllable_at("lu", 0, 2), Some("lu"));
    assert_eq!(segmenter::syllable_at("nu", 0, 2), Some("nu"));
    assert_eq!(segmenter::syllable_at("xianq", 0, 4), Some("xian"));
    assert_eq!(segmenter::syllable_at("xian", 4, 1), None, "越界切片不是音节");
    assert_eq!(segmenter::syllable_at("zhong", 0, 4), None, "`zhon` 不是音节");
    assert_eq!(segmenter::syllable_at("xianq", 4, 1), None, "`q` 不是音节");

    // 归一化确实必要:静态表里只有词库写法。
    assert!(SYLLABLES.contains(&"lve") && !SYLLABLES.contains(&"lue"));
    assert!(SYLLABLES.contains(&"nve") && !SYLLABLES.contains(&"nue"));

    let seg = Segmenter::new();
    for syl in ["lue", "nue", "lve", "nve", "lu", "nu", "luan", "xian", "zhong"] {
        assert!(seg.is_syllable(syl), "分词器应认得 {syl}");
        assert_eq!(
            unreachable_tail(syl, None)[0],
            0,
            "{syl} 在 u_min 表里必须是「从 0 可达」——排序桶与格构建同一口径"
        );
    }
    // 整串不可切分的输入仍要如实报「不可避免的未解释字母数」:
    // 表里存的是**到最近可达点的距离**(不是布尔),`q` 不是音节 ⇒
    // 从 0 出发要走到串尾(位置 5)才可达,故 umin[0] = 5。
    assert_eq!(unreachable_tail("xian", None)[0], 0);
    assert_eq!(unreachable_tail("xianq", None)[0], 5);
    assert!(unreachable_tail("qqq", None)[0] > 0);
}

/// 不变量:非回声候选都从输入位置 0 开始覆盖,**且覆盖长度落在原文坐标的
/// 合法区间**里(`0 < covered <= raw.len()`)。回声候选是「整串原样上屏」,
/// 同样满足——它是唯一允许覆盖整串而词库解不出的来源。
#[test]
fn candidates_cover_a_nonzero_prefix_in_raw_coordinates() {
    let Some(engine) = real_engine() else {
        return;
    };
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
                "{raw} 的候选 {} 覆盖 {} 字节,超出 0..={}",
                c.word,
                c.covered,
                raw.len()
            );
        }
    }
}

/// 不变量:`'` 不允许**音节**跨越,但允许多音节词跨过分隔符。
///
/// `xi'an` 的「西安」由 `xi` + `an` 两段拼成,跨过分隔符 ⇒ 合法;
/// 任何候选的拼音里都不允许出现一个**音节**横跨 `'` 落在的位置。
#[test]
fn separator_blocks_syllables_but_not_words() {
    let Some(engine) = real_engine() else {
        return;
    };
    let cands = engine
        .lookup_candidates("xi'an", &LmContext::empty())
        .expect("查询失败");
    let xian = cands
        .iter()
        .find(|c| c.word == "西安")
        .expect("`xi'an` 应召回「西安」(多音节词跨分隔符)");
    assert_eq!(xian.covered, 5, "「西安」吃掉整串原文(含那个 `'`)");

    let boundary = 2usize; // 干净串 `xian` 里 `'` 的位置
    for c in &cands {
        if c.echo {
            continue;
        }
        let mut pos = 0usize;
        for syl in c.pinyin.split(' ') {
            let end = pos + syl.len();
            assert!(
                !(pos < boundary && boundary < end),
                "候选 {} 的音节 {syl} 跨过了分隔符(干净串 {pos}..{end})",
                c.word
            );
            pos = end + 1; // 空格也算一个字节,与词典 key 的写法一致
        }
    }
}

/// 冻结的旧例外:`en` 是合法音节,补全语义下
/// 「当前音节可以补足,但不能凭空增加未输入的音节」;而 `en` 这条路上
/// 旧实现会连后续音节一起召回(「婀娜多姿」= e nuo duo zi)。本用例把
/// 现状**钉住**,避免改排序时被顺手改掉而又无人知晓;要修就是一次
/// 独立的行为修订(改 `en` 的候选,需单独批准)。
#[test]
fn frozen_outlier_en_recalls_words_with_untyped_syllables() {
    let Some(engine) = real_engine() else {
        return;
    };
    let cands = engine
        .lookup_candidates("en", &LmContext::empty())
        .expect("查询失败");
    let words: Vec<&str> = cands.iter().map(|c| c.word.as_str()).collect();
    assert!(
        words.contains(&"婀娜多姿"),
        "旧例外已被改动:en 的候选里不再有「婀娜多姿」,若是有意修订请同步本用例与文档"
    );
    // 对照:完整音节输入时不会凭空引入后续音节。
    let nihao = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert!(
        nihao.iter().all(|c| c.pinyin.split(' ').count() <= 2),
        "nihao 不该召回三音节词"
    );
}
