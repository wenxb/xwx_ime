//! crate 级单测:一次查询用同一代用户数据。
//!
//! 这里测的是**契约**而不是现象:格的构建与屏蔽过滤都必须按「调用方传进来的
//! 那一代」工作。只有在阶段边界上把两代数据分开,才能确定性地验证——
//! 靠并发线程去撞时序的做法是 flaky 的假验证。

use super::*;

fn user_snapshot(engine: &ImeEngine) -> crate::user_lm::UserLmSnapshot {
    match &engine.lm {
        crate::lm::LmStore::Mixed(m) => m.user_entries_snapshot(),
        _ => panic!("测试必须先挂上用户层"),
    }
}

fn candidate(word: &str, pinyin: &str, freq: u64) -> Candidate {
    Candidate {
        word: word.into(),
        pinyin: pinyin.into(),
        score: 0.0,
        freq,
        spelling: SpellingKind::Exact,
        echo: false,
        covered: 0,
    }
}

/// 屏蔽过滤按**传入的那一代**做,不是按此刻的表。
///
/// 失败见证:旧实现 `filter_blocked` 自己取快照(没有这个参数),于是下面
/// 「查询开始 → 用户按 Ctrl+Delete → 查询收口」的时序里,本次查询会把
/// 「你好」滤掉——前半段用旧代、收口用新代,正是 A3 说的混用。
#[test]
fn blocked_filter_uses_the_generation_it_was_given() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    // 查询开始:屏蔽表还是空的。
    let at_query_start = user_snapshot(&engine);
    // 查询途中:用户 Ctrl+Delete 把「你好」屏蔽了(新一代)。
    engine
        .user_lm()
        .expect("用户层没接上")
        .block_candidate("你好", "ni hao");

    let mut ranked = vec![
        candidate("你好", "ni hao", 900),
        candidate("拟好", "ni hao", 10),
    ];
    engine.filter_blocked(&mut ranked, Some(&at_query_start));
    assert_eq!(
        ranked.len(),
        2,
        "本次查询必须按查询开始那一刻的屏蔽表过滤,不能看见中途的删除"
    );

    // 下一次查询取新快照,才看得见屏蔽。
    let next_query = user_snapshot(&engine);
    engine.filter_blocked(&mut ranked, Some(&next_query));
    assert_eq!(ranked.len(), 1, "下一代查询应当过滤掉被屏蔽的候选");
    assert_eq!(ranked[0].word, "拟好");
}

/// 格构建同样只按传入的那一代合并用户造词条目。
#[test]
fn lattice_build_uses_the_generation_it_was_given() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    let engine = Arc::new(engine.with_user_lm(UserLM::in_memory()));

    let at_query_start = user_snapshot(&engine);
    // 查询途中用户造了一个新词。
    engine
        .user_lm()
        .expect("用户层没接上")
        .add_entry("拟好", "ni hao");

    let words = |engine: &ImeEngine, snap: &crate::user_lm::UserLmSnapshot| -> Vec<String> {
        engine
            .build_lattice("nihao", None, Some(snap))
            .expect("建格失败")
            .iter()
            .flatten()
            .map(|e| e.word.clone())
            .collect()
    };
    assert!(
        !words(&engine, &at_query_start).contains(&"拟好".to_string()),
        "本次查询不该看见查询开始之后才造出来的条目"
    );
    let next_query = user_snapshot(&engine);
    assert!(
        words(&engine, &next_query).contains(&"拟好".to_string()),
        "下一代查询应当看得见新造的条目"
    );
}
