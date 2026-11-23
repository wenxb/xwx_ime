//! 末尾半音节补全与整串精确命中优先契约。

use super::*;

fn first(engine: &ImeEngine, input: &str) -> String {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .first()
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

fn position_of(engine: &ImeEngine, input: &str, word: &str) -> Option<usize> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败")
        .iter()
        .position(|c| c.word == word)
}

fn letters(pinyin: &str) -> String {
    pinyin.chars().filter(|c| *c != ' ').collect()
}

/// 补全读法与前面词条串成整句路径。
#[test]
fn tail_completion_composes_with_a_preceding_word() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(first(engine, "nihaobeijin"), "你好北京");
    assert_eq!(first(engine, "womenjintianqugongyua"), "我们今天去公园");
}

/// 可切成 ≥2 个完整音节的窗口不做补全（如 `xian` = `xi`+`an`）。
#[test]
fn splittable_complete_syllable_is_not_completed() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(first(engine, "womenjintianquxian"), "我们今天去西安");
    assert_eq!(first(engine, "quxian"), "曲线");
}

/// 整串精确命中优先于元音/非鼻音补全；鼻音截断支配补全保留豁免。
#[test]
fn exact_match_ranks_above_completion() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    assert_eq!(first(engine, "qingxi"), "清晰");
    assert_eq!(first(engine, "xuexi"), "学习");
    assert_eq!(first(engine, "xiala"), "下拉");
    assert_eq!(first(engine, "jianji"), "剪辑");
    if let (Some(lai), Some(xia)) = (position_of(engine, "xiala", "下来"), position_of(engine, "xiala", "下")) {
        assert!(lai < xia, "补全「下来」应排在碎片单字「下」前");
    }

    assert_eq!(first(engine, "beiji"), "北京");
    assert_eq!(first(engine, "beijin"), "北京");

    // 鼻音豁免与整串词条同层竞争，元音补全让位。
    assert_eq!(position_of(engine, "weixi", "微信"), Some(0));
    let weixi_pos = position_of(engine, "weixi", "维系").expect("应含「维系」");
    assert!(weixi_pos <= 2, "整串词条「维系」应在前 3: {weixi_pos}");
    if let Some(weixiu) = position_of(engine, "weixi", "维修") {
        assert!(weixiu > weixi_pos, "元音补全「维修」不得越过整串词条「维系」");
    }
}

/// 整串读法成块排在补全之前（含真实上下文）。
#[test]
fn full_pinyin_readings_precede_completions_as_a_block() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (tail, last_word) in [("", None), ("我", Some("我")), ("今天", Some("今天"))] {
        let ctx = LmContext { tail, last_word };
        let cands = engine.lookup_candidates("liji", &ctx).expect("查询失败");
        let is_full = |c: &Candidate| letters(&c.pinyin).len() == 4;
        let is_completion = |c: &Candidate| letters(&c.pinyin).len() > 4;
        assert_eq!(cands[0].word, "立即", "上文 {tail:?}: 首位应是精确命中");
        if let (Some(lf), Some(fc)) = (cands.iter().rposition(is_full), cands.iter().position(is_completion)) {
            assert!(
                lf < fc,
                "上文 {tail:?}: 整串读法「{}」被补全「{}」插队",
                cands[lf].word,
                cands[fc].word
            );
        }
    }
}
