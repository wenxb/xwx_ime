//! crate 级单测:completion（前缀补全、声母枚举与全拼首屏恢复）。

use super::*;

/// 打字中的联想不被排序首键打散：`ni`/`zhon`/`zhongg` 目标字词保持前 10。
#[test]
fn typing_completion_not_scattered() {
    let Some(engine) = real_full_engine() else {
        return;
    };

    for (input, wants) in [
        ("ni", &["你", "尼", "泥"][..]),
        ("zhon", &["中", "种", "重"][..]),
        ("zhongg", &["中国"][..]),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        for w in wants {
            let pos = cands
                .iter()
                .position(|c| c.word == *w)
                .unwrap_or_else(|| panic!("{input} 应含 {w}"));
            assert!(pos < 10, "{input} 的 {w} 应在前 10，实际第 {} 名", pos + 1);
        }
    }
}

/// 全拼首屏：高频单字/词在前 10，拼音凑巧等长的生僻组合词沉底。
#[test]
fn full_pinyin_first_page_recovers() {
    let Some(engine) = real_full_engine() else {
        return;
    };

    let must_in: [(&str, &[&str]); 6] = [
        ("beijing", &["北京", "被", "北"]),
        ("diannao", &["电脑", "点", "电"]),
        ("zhongguo", &["中国", "中", "种"]),
        ("gongzuo", &["工作", "公", "工"]),
        ("jinan", &["济南", "进"]),
        ("mingtian", &["明天", "明", "名"]),
    ];
    for (input, wants) in must_in {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
        for w in wants {
            assert!(top10.contains(w), "{input} 的 {w} 应在前 10: {top10:?}");
        }
    }

    let must_out: [(&str, &[&str]); 3] = [
        ("beijing", &["被禁锢", "悲寂", "背脊"]),
        ("diannao", &["狄安娜", "蒂安娜"]),
        ("jinan", &["进啊"]),
    ];
    for (input, bads) in must_out {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let top10: Vec<&str> = cands.iter().take(10).map(|c| c.word.as_str()).collect();
        for w in bads {
            assert!(!top10.contains(w), "{input} 的 {w} 不应在前 10: {top10:?}");
        }
    }
}

/// 声母前缀按音节枚举并逐音节限流（`SYLLABLE_ENUM_QUOTA`），打破字典序首韵母独占。
#[test]
fn initial_only_prefix_recovers_all_finals() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for i in 0..(SYLLABLE_ENUM_QUOTA + 6) as u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine
            .dict
            .upsert(&w, "zha", 10_000 - i as u64)
            .expect("插入失败");
    }
    engine.dict.upsert("这", "zhe", 50).expect("插入失败");
    engine.dict.upsert("中", "zhong", 40).expect("插入失败");

    let cands = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "这"));
    assert!(cands.iter().any(|c| c.word == "中"));
    let n_zha = cands.iter().filter(|c| c.pinyin == "zha").count();
    assert!(n_zha <= SYLLABLE_ENUM_QUOTA, "zha 系应受逐音节配额限流: {n_zha}");
}

/// `z`/`c`/`s` 单字母枚举覆盖平/翘舌两套声母族，且受逐音节配额限流。
#[test]
fn single_letter_initial_covers_both_sibilant_families() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for i in 0..(SYLLABLE_ENUM_QUOTA + 5) as u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine
            .dict
            .upsert(&w, "zai", 10_000 - i as u64)
            .expect("插入失败");
    }
    engine.dict.upsert("中", "zhong", 1).expect("插入失败");

    let cands = engine
        .lookup_candidates("z", &LmContext::empty())
        .expect("查询失败");
    let pos_zhong = cands
        .iter()
        .position(|c| c.word == "中")
        .expect("z 应召回 zh 系的「中」");
    assert!(
        pos_zhong <= SYLLABLE_ENUM_QUOTA,
        "zai 系被限流后「中」不应排第 {} 名",
        pos_zhong + 1
    );
}

/// 逐音节枚举对多音字按 word 去重，同一汉字不重复占位。
#[test]
fn enum_expansion_dedups_polyphonic_char_by_word() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("这", "zhe", 100).expect("插入失败");
    engine.dict.upsert("这", "zhei", 100).expect("插入失败");

    let cands = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.iter().filter(|c| c.word == "这").count(), 1);
}

/// 合法音节不走枚举分支，不混入其他音节候选。
#[test]
fn legal_syllable_not_touched_by_enum_expansion() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("这", "zhe", 100).expect("插入失败");
    engine.dict.upsert("扎", "zha", 1000).expect("插入失败");
    engine.dict.upsert("中", "zhong", 900).expect("插入失败");

    let cands = engine
        .lookup_candidates("zhe", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "这"));
    assert!(cands.iter().all(|c| c.pinyin == "zhe"));
}

/// 真实词库下声母前缀召回各韵母高频字，前 10 不被最小字典序韵母锁死。
#[test]
fn real_dict_initial_only_prefix_first_page() {
    let Some(engine) = real_dict_engine() else {
        return;
    };

    for (input, wants) in [
        ("zh", &["中", "这", "着"][..]),
        ("x", &["先", "新", "学"][..]),
        ("q", &["钱", "区", "强"][..]),
        ("zhon", &["中", "种", "重"][..]),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        for w in wants {
            assert!(cands.iter().any(|c| c.word == *w), "{input} 应召回「{w}」");
        }
    }

    let zh = engine
        .lookup_candidates("zh", &LmContext::empty())
        .expect("查询失败");
    let top10: Vec<&str> = zh.iter().take(10).map(|c| c.word.as_str()).collect();
    assert!(
        !top10
            .iter()
            .all(|w| ["扎", "炸", "眨", "诈", "乍", "喳", "渣", "闸", "栅", "榨"].contains(w)),
        "zh 前 10 不得全是 zha 系: {top10:?}"
    );
    let zhon = engine
        .lookup_candidates("zhon", &LmContext::empty())
        .expect("查询失败");
    let n_zhong = zhon.iter().take(10).filter(|c| c.pinyin == "zhong").count();
    assert!(n_zhong >= 5, "zhon 前 10 应以 zhong 系为主: {n_zhong}/10");
}

/// 全拼前缀 + 尾部声母混拼优先整串词条读法，并保留容错备选与整串块置顶。
#[test]
fn mixed_initial_tail_prefers_entry_reading() {
    let Some(engine) = real_full_engine() else {
        return;
    };

    for (input, want_top1) in
        [("yiwanc", "已完成"), ("yiwanch", "已完成"), ("buyy", "不一样")]
    {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands.first().map(|c| c.word.as_str()), Some(want_top1), "{input}");
    }
    let yiwanc = engine
        .lookup_candidates("yiwanc", &LmContext::empty())
        .expect("查询失败");
    assert!(yiwanc.iter().any(|c| c.word == "一万"), "精确前缀「一万」应保留");

    let mq = engine
        .lookup_candidates("mqweizhi", &LmContext::empty())
        .expect("查询失败");
    let pos = mq.iter().position(|c| c.word == "目前为止").expect("应含「目前为止」");
    assert!(pos < 3, "mqweizhi 的「目前为止」应在前 3，实际第 {} 名", pos + 1);

    // 混合简拼无伪双首字母污染、单字去重、中段简拼与尾部高频单字简拼回归。
    for (input, want) in [
        ("womenq", "我们去"),
        ("jintq", "今天去"),
        ("womenqbeijing", "我们去北京"),
        ("mingtw", "明天我"),
        ("nihaoq", "你好"),
        ("zhongguoo", "中国"),
    ] {
        let cands = engine.lookup_candidates(input, &LmContext::empty()).expect("查询失败");
        assert_eq!(
            cands.first().map(|c| c.word.as_str()),
            Some(want),
            "{input} 首位应是「{want}」，实际前 5: {:?}",
            cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
    for (input, bad) in [("womenjt", "我哦"), ("zhonggr", "紫红"), ("shuruf", "似乎"), ("houxuanc", "海鸥")] {
        let cands = engine.lookup_candidates(input, &LmContext::empty()).expect("查询失败");
        assert!(
            !cands.iter().take(9).any(|c| c.word == bad),
            "{input} 首屏不应出现伪双首字母「{bad}」"
        );
    }
    let kend = engine.lookup_candidates("kend", &LmContext::empty()).expect("查询失败");
    assert_eq!(
        kend.iter().filter(|c| c.word == "可能得").count().min(1),
        kend.iter().filter(|c| c.word == "可能得").count(),
        "展示层按词面去重，不应出现重复「可能得」"
    );
}
