//! crate 级单测:个性化学习、查询快照隔离与只读预热。

use super::*;

fn user_snapshot(engine: &ImeEngine) -> crate::user_lm::UserLmSnapshot {
    match &engine.lm {
        crate::lm::LmStore::Mixed(m) => m.user_entries_snapshot(),
        _ => panic!("测试必须先挂上用户层"),
    }
}

/// 纯简拼下用户学过的词排首位，且未学过的竞争者分数不被连带改动。
#[test]
fn real_dict_learned_word_tops_pure_abbrev() {
    let Some(bare) = real_dict_engine() else {
        return;
    };
    if !bare.dict.has_abbrev() {
        return;
    }
    let Some(fresh) = fresh_real_dict_engine() else {
        return;
    };
    let user = crate::UserLM::in_memory();
    for _ in 0..3 {
        user.learn(None, "可以");
    }
    let with_user = fresh.with_user_lm(user);

    let score_of = |e: &ImeEngine, word: &str| -> f32 {
        e.lookup_candidates("ky", &LmContext::empty())
            .expect("查询失败")
            .into_iter()
            .find(|c| c.word == word)
            .unwrap_or_else(|| panic!("ky 应含「{word}」"))
            .score
    };

    assert!(score_of(&with_user, "可以") > score_of(bare, "可以"));
    for rival in ["跨越", "可用"] {
        assert_eq!(score_of(bare, rival), score_of(&with_user, rival), "未学词不应被改动");
    }

    let cands = with_user
        .lookup_candidates("ky", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands[0].word, "可以", "学过的「可以」应排第一");
    assert!(score_of(&with_user, "可以") > score_of(&with_user, "跨越"));
}

/// 同类候选选过一次即登顶。
#[test]
fn real_dict_single_pick_tops_same_class_rivals() {
    let Some(engine) = fresh_real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let engine = engine.with_user_lm(crate::UserLM::in_memory());
    let rank_of = |w: &str| {
        engine
            .lookup_candidates("xian", &LmContext::empty())
            .expect("查询失败")
            .iter()
            .position(|c| c.word == w)
            .map(|i| i + 1)
    };

    assert!(rank_of("闲").expect("应含「闲」") > 1);
    engine.user_lm().expect("应挂用户层").learn(None, "闲");
    assert_eq!(rank_of("闲"), Some(1));
}

/// 结构墙：高频学习前缀单字仍不能翻过整词。
#[test]
fn real_dict_structural_wall_survives_learning() {
    let Some(engine) = fresh_real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let engine = engine.with_user_lm(crate::UserLM::in_memory());
    let user = engine.user_lm().expect("应挂用户层");

    let mut learned = 0usize;
    for n in [1usize, 3, 10, 100] {
        while learned < n {
            user.learn(None, "我");
            learned += 1;
        }
        let cands = engine
            .lookup_candidates("women", &LmContext::empty())
            .expect("查询失败");
        let pos_word = cands.iter().position(|c| c.word == "我们").expect("缺「我们」");
        let pos_char = cands.iter().position(|c| c.word == "我").expect("缺「我」");
        assert_eq!(cands[0].word, "我们", "学过 {n} 次「我」后整词仍须第一");
        assert!(pos_char > pos_word);
    }
}

/// 屏蔽过滤按传入的那一代快照工作，不混用查询中途产生的新屏蔽。
#[test]
fn blocked_filter_uses_the_generation_it_was_given() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let at_query_start = user_snapshot(&engine);
    engine
        .user_lm()
        .expect("用户层没接上")
        .block_candidate("你好", "ni hao");

    let mut ranked = vec![
        Candidate::new("你好", "ni hao", 900),
        Candidate::new("拟好", "ni hao", 10),
    ];
    engine.filter_blocked(&mut ranked, Some(&at_query_start));
    assert_eq!(ranked.len(), 2, "旧代快照不应看见中途屏蔽");

    let next_query = user_snapshot(&engine);
    engine.filter_blocked(&mut ranked, Some(&next_query));
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].word, "拟好");
}

/// 格构建同样只按传入的那一代合并用户造词条目。
#[test]
fn lattice_build_uses_the_generation_it_was_given() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let at_query_start = user_snapshot(&engine);
    engine.user_lm().expect("用户层没接上").add_entry("拟好", "ni hao");

    let words = |snap: &crate::user_lm::UserLmSnapshot| -> Vec<String> {
        engine
            .build_lattice("nihao", None, Some(snap))
            .expect("建格失败")
            .iter()
            .flatten()
            .map(|e| e.word.clone())
            .collect()
    };
    assert!(!words(&at_query_start).contains(&"拟好".to_string()));
    let next_query = user_snapshot(&engine);
    assert!(words(&next_query).contains(&"拟好".to_string()));
}

/// 启动预热（`prewarm` / `prepare_startup`）只读且可重复调用：不改查询结果、词频总量或用户模型。
#[test]
fn prewarm_and_prepare_startup_are_read_only() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for (w, py, f) in [
        ("你好", "ni hao", 900u64),
        ("拟好", "ni hao", 500),
        ("我们", "wo men", 880),
        ("为", "wei", 700),
        ("是", "shi", 600),
        ("你", "ni", 500),
    ] {
        engine.dict.upsert(w, py, f).expect("插入失败");
    }
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let ctx = LmContext::empty();
    let snapshot = || -> Vec<Vec<String>> {
        ["nihao", "weis", "wo", "ns", "nihoa"]
            .iter()
            .map(|p| {
                engine
                    .lookup_candidates(p, &ctx)
                    .expect("查询失败")
                    .into_iter()
                    .map(|c| c.word)
                    .collect()
            })
            .collect()
    };

    let before = snapshot();
    let total_before = engine.dict.total_freq().expect("词频总量失败");
    engine.prepare_startup();
    engine.prepare_startup();
    engine.prewarm();
    engine.prewarm();

    assert_eq!(before, snapshot(), "预热/启动初始化不得改变查询结果");
    assert_eq!(total_before, engine.dict.total_freq().expect("词频总量失败"));
    let user = engine.user_lm().expect("用户层没接上");
    assert_eq!(user.dirty_count(), 0);
    assert_eq!(user.unigram_len(), 0);
    assert_eq!(user.entry_len(), 0);
    assert_eq!(user.blocklist_len(), 0);
}
