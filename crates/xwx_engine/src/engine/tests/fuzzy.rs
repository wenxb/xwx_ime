//! crate 级单测:fuzzy（模糊拼音惩罚、分层排序与环境变量解析）。

use super::*;

#[test]
fn exact_match_outranks_fuzzy_match() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("里", "li", 100).expect("插入失败");
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    let (ri, rl) = (rank("你").expect("缺你"), rank("里").expect("缺里"));
    assert!(ri < rl, "精确「你」应排在模糊「里」前");
    let diff = cands[ri].score - cands[rl].score;
    assert!((diff - 0.0969).abs() < 1e-2, "n/l 单步惩罚应≈0.097, 实际 {diff}");
}

#[test]
fn multi_confusion_penalty_accumulates() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("难", "nan", 100).expect("插入失败");
    engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
    engine.dict.upsert("狼", "lang", 100).expect("插入失败");
    engine.fuzzy_pairs = vec![
        ConfusionPair::new("n", "l", 0.8),
        ConfusionPair::new("an", "ang", 0.5),
    ];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("nan", &LmContext::empty())
        .expect("查询失败");
    let score = |word: &str| cands.iter().find(|c| c.word == word).expect(word).score;
    let (snan, slan, slang) = (score("难"), score("蓝"), score("狼"));
    assert!(snan > slan && slan > slang);
    assert!((snan - slan - 0.0969).abs() < 1e-2);
    assert!((slan - slang - (-0.5f32.log10())).abs() < 1e-2);
}

#[test]
fn exact_layer_outranks_fuzzy_despite_much_higher_freq() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("蓝", "lan", 100).expect("插入失败");
    engine.dict.upsert("然", "ran", 1000).expect("插入失败");
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("lan", &LmContext::empty())
        .expect("查询失败");
    let pos = |word: &str| cands.iter().position(|c| c.word == word).expect(word);
    let (pl, pr) = (pos("蓝"), pos("然"));
    assert!(pl < pr, "精确层「蓝」应排在高频模糊「然」前");
    assert!(cands[pr].score > cands[pl].score, "然分数更高，靠分层保住精确优先");
    assert_eq!(cands[pl].spelling, SpellingKind::Exact);
    assert_eq!(cands[pr].spelling, SpellingKind::Fuzzy);
    if let Some(ff) = cands.iter().position(|c| c.spelling == SpellingKind::Fuzzy) {
        assert!(cands[..ff].iter().all(|c| c.spelling == SpellingKind::Exact));
    }
}

/// 模糊层整体在精确层之后，层内按混淆对 weight（-log10 惩罚）排序。
#[test]
fn fuzzy_layer_internally_sorted_by_penalty() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("精", "a", 10).expect("插入失败");
    engine.dict.upsert("高", "b", 100).expect("插入失败");
    engine.dict.upsert("低", "c", 100).expect("插入失败");
    engine.fuzzy_pairs = vec![
        ConfusionPair::new("a", "b", 0.8),
        ConfusionPair::new("a", "c", 0.2),
    ];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("a", &LmContext::empty())
        .expect("查询失败");
    let pos = |word: &str| cands.iter().position(|c| c.word == word).expect(word);
    let (pe, ph, pl) = (pos("精"), pos("高"), pos("低"));
    assert!(pe < ph && ph < pl);
    assert_eq!(cands[pe].spelling, SpellingKind::Exact);
    assert_eq!(cands[ph].spelling, SpellingKind::Fuzzy);
    assert_eq!(cands[pl].spelling, SpellingKind::Fuzzy);
    // -log10(0.2) - (-log10(0.8)) ≈ 0.602
    let diff = cands[ph].score - cands[pl].score;
    assert!((diff - 0.602).abs() < 1e-2, "分差应≈0.602, 实际 {diff}");
}

#[test]
fn mixed_exact_and_fuzzy_edges_belong_to_fuzzy_layer() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("那", "na", 100).expect("插入失败");
    engine.dict.upsert("哈", "ha", 100).expect("插入失败");
    engine.dict.upsert("那哈", "na ha", 50).expect("插入失败");
    engine.dict.upsert("拉哈", "la ha", 10000).expect("插入失败");
    engine.fuzzy_pairs = vec![ConfusionPair::new("n", "l", 0.8)];
    engine.fuzzy_enabled = true;

    let cands = engine
        .lookup_candidates("naha", &LmContext::empty())
        .expect("查询失败");
    let pure = cands.iter().find(|c| c.word == "那哈").expect("缺那哈");
    let mixed = cands.iter().find(|c| c.word == "拉哈").expect("缺拉哈");
    assert_eq!(pure.spelling, SpellingKind::Exact);
    assert_eq!(mixed.spelling, SpellingKind::Fuzzy);
    let pos = |word: &str| cands.iter().position(|c| c.word == word);
    assert!(pos("那哈") < pos("拉哈"));
    assert!(!cands.iter().any(|c| c.word == "哈"), "不召回后段音节「哈」");
}

#[test]
fn layered_sort_disabled_matches_legacy_exactly() {
    let build = |fuzzy: bool| {
        let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
        for (w, py, f) in [
            ("你", "ni", 100),
            ("逆", "ni", 50),
            ("妮", "ni", 80),
            ("泥", "ni", 60),
            ("里", "li", 1000),
        ] {
            engine.dict.upsert(w, py, f).expect("插入失败");
        }
        engine.fuzzy_enabled = fuzzy;
        engine
    };

    let off = build(false)
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    let on = build(true)
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");

    assert!(!off.iter().any(|c| c.word == "里"));
    assert!(off.iter().all(|c| c.spelling == SpellingKind::Exact));
    let off_words: Vec<&str> = off.iter().map(|c| c.word.as_str()).collect();
    assert_eq!(off_words, ["你", "妮", "泥", "逆"]);

    let on_exact: Vec<&str> = on
        .iter()
        .filter(|c| c.spelling == SpellingKind::Exact)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(on_exact, off_words, "开启模糊后精确层逐位不变");
    let on_fuzzy: Vec<&str> = on
        .iter()
        .filter(|c| c.spelling == SpellingKind::Fuzzy)
        .map(|c| c.word.as_str())
        .collect();
    assert_eq!(on_fuzzy, ["里"]);
}

/// 环境变量解析全表与默认关闭契约。
#[test]
fn fuzzy_env_parsing_table() {
    const {
        assert!(
            !DEFAULT_FUZZY_ENABLED,
            "默认必须关闭：改默认值前先重跑 examples/fuzzy_bench.rs"
        );
    }
    assert_eq!(FUZZY_ENV, "XWX_FUZZY");

    for v in ["1", "true", "TRUE", "True", "on", "ON", "yes", "Yes", " 1 ", " on "] {
        assert!(parse_fuzzy_env(Some(v)), "{v:?} 应解析为开启");
    }
    for v in ["0", "false", "FALSE", "off", "OFF", "no", "NO", "", "  "] {
        assert!(!parse_fuzzy_env(Some(v)), "{v:?} 应解析为关闭");
    }
    assert_eq!(parse_fuzzy_env(None), DEFAULT_FUZZY_ENABLED);
    for v in ["2", "maybe", "enable", "模糊", "y", "n", "t", "f"] {
        assert_eq!(parse_fuzzy_env(Some(v)), DEFAULT_FUZZY_ENABLED, "{v:?} 应回落默认");
    }
    assert!(fuzzy_env_value_recognized(None));
    for v in ["1", "true", "on", "yes", "0", "false", "off", "no", "", " ON "] {
        assert!(fuzzy_env_value_recognized(Some(v)), "{v:?} 应认得");
    }
    for v in ["2", "maybe", "enable", "模糊", "y"] {
        assert!(!fuzzy_env_value_recognized(Some(v)), "{v:?} 不应认得");
    }
}
