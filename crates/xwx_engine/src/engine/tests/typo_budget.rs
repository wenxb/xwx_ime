//! crate 级单测:容错的整条路径编辑预算。

use super::*;

fn edit_edge(end: usize, word: &str, pinyin: &str, edit_count: u8) -> LatticeEdge {
    LatticeEdge {
        end,
        word: word.to_string(),
        pinyin: pinyin.to_string(),
        freq: 100,
        fuzzy_penalty: 0.0,
        typo_penalty: 1.0,
        edit_count,
        gap: false,
        abbrev: false,
    }
}

fn two_edit_lattice(len: usize) -> Vec<Vec<LatticeEdge>> {
    let mut lattice: Vec<Vec<LatticeEdge>> = (0..=len).map(|_| Vec::new()).collect();
    lattice[0].push(edit_edge(3, "甲甲", "jia", 1));
    lattice[3].push(edit_edge(6, "乙乙", "jia", 1));
    lattice
}

fn engine_with_dict_entry() -> ImeEngine {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("甲甲", "jia", 10).expect("插入失败");
    engine
}

#[test]
fn path_budget_counts_edits_across_edges() {
    let engine = engine_with_dict_entry();
    let ctx = LmContext::empty();
    let input = "jiajia";
    let lattice = two_edit_lattice(input.len());

    let one = engine
        .backward_completion(input, &ctx, &lattice, 1)
        .expect("后向 DP 失败");
    assert!(!one[1][0].is_finite(), "预算 1 时两次编辑必须不可达");
    assert!(!one[0][0].is_finite());

    let two = engine
        .backward_completion(input, &ctx, &lattice, 2)
        .expect("后向 DP 失败");
    assert!(two[2][0].is_finite(), "预算 2 时两次编辑应可达");
    assert!(!two[1][0].is_finite());
    assert!(two[2][0] >= two[1][0]);
}

#[test]
fn zero_edit_readings_hold_at_every_budget_including_zero() {
    let engine = engine_with_dict_entry();
    let ctx = LmContext::empty();
    let input = "jiajia";
    let mut lattice: Vec<Vec<LatticeEdge>> = (0..=input.len()).map(|_| Vec::new()).collect();
    lattice[0].push(edit_edge(6, "甲甲", "jia", 0));

    for budget in 0..=MAX_TYPO_EDITS {
        let table = engine
            .backward_completion(input, &ctx, &lattice, budget)
            .expect("后向 DP 失败");
        assert!(table[budget as usize][0].is_finite(), "预算 {budget} 应可达");
    }
}

#[test]
fn candidate_completion_deducts_the_budget_used_by_its_first_edge() {
    let engine = engine_with_dict_entry();
    let ctx = LmContext::empty();
    let input = "jiajia";
    let lattice = two_edit_lattice(input.len());

    let suffix1 = engine
        .backward_completion(input, &ctx, &lattice, 1)
        .expect("后向 DP 失败");
    let cands1 = engine
        .lattice_candidates_scored(&ctx, &lattice, &suffix1)
        .expect("候选构建失败");
    let first1 = cands1.iter().find(|c| c.word == "甲甲").expect("缺首边候选");
    assert!(!first1.score.is_finite(), "首边用完预算后补全应不可达");

    let suffix2 = engine
        .backward_completion(input, &ctx, &lattice, 2)
        .expect("后向 DP 失败");
    let cands2 = engine
        .lattice_candidates_scored(&ctx, &lattice, &suffix2)
        .expect("候选构建失败");
    let first2 = cands2.iter().find(|c| c.word == "甲甲").expect("缺首边候选");
    assert!(first2.score.is_finite(), "预算 2 时应构成完整读法");
}

#[test]
fn typo_arcs_carry_one_edit_and_exact_edges_carry_none() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for (word, key, freq) in [
        ("你好", "ni hao", 900u64),
        ("你", "ni", 500),
        ("什么", "shen me", 800),
        ("深", "shen", 400),
    ] {
        engine.dict.upsert(word, key, freq).expect("插入失败");
    }
    let input = "nihoa";
    let exact = engine.build_lattice(input, None, None).expect("精确格失败");
    assert!(exact.iter().flatten().all(|e| e.edit_count == 0));

    let stalls = ImeEngine::first_stalls(&exact, input.len());
    assert!(!stalls.is_empty());
    let tolerant = engine
        .build_tolerant_lattice(input, None, &exact, &stalls, None)
        .expect("容错格失败");
    let bucket: Vec<&LatticeEdge> = tolerant.iter().flatten().collect();
    let typed: Vec<&&LatticeEdge> = bucket.iter().filter(|e| e.typo_penalty > 0.0).collect();
    assert!(!typed.is_empty(), "容错格应含编辑边");
    assert!(typed.iter().all(|e| e.edit_count == 1 && !e.gap));
    assert!(bucket.iter().filter(|e| e.gap).all(|e| e.edit_count == 1));
}

/// 补全超过编辑预算的候选在真实词库下判为不可达（-inf）。
#[test]
fn completion_beyond_typo_budget_scores_unreachable() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    let cands = engine
        .lookup_candidates("womenjintianqugongyuann", &LmContext::empty())
        .expect("查询失败");
    let Some(hit) = cands.iter().find(|c| c.word == "我么") else {
        return;
    };
    assert!(
        hit.score.is_infinite() && hit.score < 0.0,
        "超预算补全应判不可达，实际 {:?}",
        hit.score
    );
}
