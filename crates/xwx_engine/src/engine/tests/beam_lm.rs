//! crate 级单测:beam_lm。

use super::*;

#[test]
fn real_dict_bin_loads() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    let DictStore::Mmap(dict) = &engine.dict else {
        panic!("dict.bin 应加载为 Mmap 词库");
    };
    assert!(dict.count() >= 1_000_000, "词条数应超过百万: {}", dict.count());
    let cands = dict.lookup("ni").expect("查询失败");
    assert!(!cands.is_empty(), "ni 应有候选");
    assert_eq!(cands[0].word, "你", "最高频词应为 你");
    assert!(!dict.lookup_prefix("zhon").expect("前缀查询失败").is_empty());
}

/// `xi'an` 消歧：不得召回只能由 `[xian]` 切出的高频单字，且「西安」进前 10。
#[test]
fn real_dict_apostrophe_disambiguates_xian() {
    let Some(engine) = real_dict_engine() else {
        return;
    };

    let plain = engine
        .lookup_candidates("xian", &LmContext::empty())
        .expect("查询失败");
    assert!(
        plain.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
        "对照组 `xian` 应含「先」"
    );

    let cands = engine
        .lookup_candidates("xi'an", &LmContext::empty())
        .expect("查询失败");
    for w in ["先", "现", "显", "线", "险", "仙"] {
        assert!(
            !cands.iter().any(|c| c.word == w && c.pinyin == "xian"),
            "xi'an 不应召回 `[xian]` 的「{w}」"
        );
    }
    let rank = cands
        .iter()
        .position(|c| c.word == "西安")
        .expect("xi'an 应含「西安」");
    assert!(rank < 10, "xi'an 的「西安」应进前 10，实际第 {} 名", rank + 1);
}

#[test]
fn context_changes_candidate_ranking() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("国", "guo", 10).expect("插入失败");
    engine.dict.upsert("过", "guo", 20).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram");
    };
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 100);
    lm.add_unigram("过", 100);
    lm.add_bigram("中", "国", 90);

    let plain = engine
        .lookup_candidates("guo", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(plain[0].word, "过", "无上文时词频高的「过」在前");

    let after = engine
        .lookup_candidates("guo", &LmContext { tail: "中", last_word: Some("中") })
        .expect("查询失败");
    assert_eq!(after[0].word, "国", "上文「中」应让「国」压过「过」");
}

/// 整句 beam 找到全局最优 [泥,门]（优于 k=1 贪心的 [你,门]）；前缀候选仍按近似 LM 排序。
#[test]
fn whole_path_beam_beats_greedy() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("泥", "ni", 90).expect("插入失败");
    engine.dict.upsert("门", "men", 50).expect("插入失败");
    engine.dict.upsert("们", "men", 50).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram");
    };
    lm.add_unigram("你", 100);
    lm.add_unigram("泥", 10);
    lm.add_unigram("门", 100);
    lm.add_bigram("泥", "门", 5000);

    let ctx = LmContext::empty();
    let greedy = engine.decode_paths("nimen", &ctx, 1).expect("解码失败");
    let beam = engine.decode_paths("nimen", &ctx, BEAM_WIDTH).expect("解码失败");

    assert_eq!(greedy[0].words, vec!["你", "门"], "贪心停在局部最优");
    assert_eq!(beam[0].words, vec!["泥", "门"], "beam 应找到全局最优");
    assert!(beam[0].score > greedy[0].score, "整句解码分应更高");

    let cands = engine.lookup_candidates("nimen", &ctx).expect("查询失败");
    assert_eq!(cands[0].covered, 5, "整串读法应排首位");
    let first_prefix = cands
        .iter()
        .find(|c| c.covered < "nimen".len())
        .expect("应有只覆盖前缀的候选");
    assert_eq!(first_prefix.word, "你", "前缀候选按近似 LM 排序");
}

/// 上文命中 bigram 后整条路径从 [你,门] 翻盘为 [泥,门]，候选排序同步反映。
#[test]
fn context_changes_whole_path_choice() {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("泥", "ni", 90).expect("插入失败");
    engine.dict.upsert("门", "men", 50).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram");
    };
    lm.add_unigram("你", 100);
    lm.add_unigram("泥", 10);
    lm.add_unigram("门", 100);
    lm.add_unigram("澡", 100);
    lm.add_bigram("澡", "泥", 900);

    let no_ctx = LmContext::empty();
    let with_ctx = LmContext { tail: "澡", last_word: Some("澡") };

    let before = engine.decode_paths("nimen", &no_ctx, BEAM_WIDTH).expect("解码失败");
    let after = engine.decode_paths("nimen", &with_ctx, BEAM_WIDTH).expect("解码失败");
    assert_eq!(before[0].words, vec!["你", "门"]);
    assert_eq!(after[0].words, vec!["泥", "门"]);

    let cands = engine.lookup_candidates("nimen", &with_ctx).expect("查询失败");
    let rank = |word: &str| cands.iter().position(|c| c.word == word);
    assert!(rank("泥") < rank("你"), "上文命中 bigram 后「泥」应排在「你」前");
}

/// beam 宽度边界、路径单调性与格可达性一致性。
#[test]
fn beam_width_bounds_exploration() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("逆", "ni", 90).expect("插入失败");
    engine.dict.upsert("妮", "ni", 80).expect("插入失败");
    let ctx = LmContext::empty();

    for k in 0..=3 {
        assert_eq!(engine.decode_paths("ni", &ctx, k).expect("解码失败").len(), k);
    }
    let narrow = engine.decode_paths("ni", &ctx, 3).expect("解码失败");
    let wide = engine.decode_paths("ni", &ctx, BEAM_WIDTH).expect("解码失败");
    for p in &narrow {
        assert!(wide.iter().any(|w| w.words == p.words), "k 增大后不应丢路径");
    }
    assert_eq!(narrow[0].words, wide[0].words);
    assert!(narrow[0].score >= wide[0].score);

    let lattice = engine.build_lattice("ni", None, None).expect("建格失败");
    let from_lattice = engine
        .decode_beam_with_lattice("ni", &ctx, BEAM_WIDTH, &lattice)
        .expect("格解码失败");
    assert_eq!(ImeEngine::lattice_reachable(&lattice, "ni".len()), !from_lattice.is_empty());
    assert_eq!(from_lattice.len(), wide.len());

    let cands = engine.lookup_candidates("ni", &ctx).expect("查询失败");
    let beam_words: Vec<(String, String)> = wide
        .iter()
        .flat_map(|p| p.words.iter().cloned().zip(p.pinyin.iter().cloned()))
        .collect();
    for c in &cands {
        assert!(beam_words.contains(&(c.word.clone(), c.pinyin.clone())));
    }
}

/// 病态输入（全同音/无音节/双歧义/超长串）正常终止且候选数受上限约束。
#[test]
fn pathological_inputs_do_not_explode() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for (w, py, f) in [
        ("啊", "a", 100),
        ("爱", "ai", 90),
        ("安", "an", 80),
        ("昂", "ang", 70),
        ("那", "na", 100),
        ("难", "nan", 90),
    ] {
        engine.dict.upsert(w, py, f).expect("插入失败");
    }
    let ctx = LmContext::empty();
    let cases = [
        "aaaaaaaaaaaa".to_string(),
        "zzzzzzzz".to_string(),
        "nananananana".to_string(),
        "anang".to_string(),
        "a".repeat(40),
        "n".repeat(30),
    ];
    for input in cases {
        let cands = engine.lookup_candidates(&input, &ctx).expect("查询失败");
        assert!(!cands.is_empty(), "{input:?} 至少应有候选或回声");
        assert!(cands.len() <= MAX_CANDIDATES, "{input:?} 候选数超上限");
    }
}

/// 万象 `.gram` 缺失时退回空 bigram，查询正常工作。
#[test]
fn wanxiang_missing_gram_degrades_gracefully() {
    let engine =
        ImeEngine::new_with_lm(None, Some(Path::new("/nonexistent/wanxiang.gram")))
            .expect("模型缺失时引擎仍应创建成功");
    assert!(matches!(engine.lm, LmStore::Bigram(_)), "缺失 .gram 应退回空 bigram");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询不应失败");
    assert!(!cands.is_empty(), "退化后查询应正常工作");
}
