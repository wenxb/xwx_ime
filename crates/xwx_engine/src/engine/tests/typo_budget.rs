//! crate 级单测:容错的**整条路径**编辑预算(codex 审查 D13)。
//!
//! 改动前 `MAX_TYPO_EDITS` 只卡「单次链枚举」:一条「音节级 ≤1 编辑」弧落成
//! [`LatticeEdge`] 之后只剩罚分,后向 DP 无从限制整条路径的累计编辑数——
//! 「两个各错一处的词」能拼出一条两次编辑的读法,与「整条路径最多一次编辑」
//! 的口径自相矛盾。
//!
//! 这里用**手工构造的格**把预算语义钉死:0→3 一条编辑边、3→6 再一条编辑边,
//! 预算 1 必须到不了串尾,预算 2 才到得了;候选那一侧还要验证「首边用掉的
//! 编辑数先从补全预算里扣掉」。

use super::*;

/// 一条「引入 `edit_count` 次容错编辑」的格边。
///
/// `LatticeEdge` 的字段是私有的,但本模块是 `engine` 的后代,可以直接构造。
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
    }
}

/// 「必须用两次编辑才能到串尾」的格:0→3、3→6 各一条编辑边,输入 6 字节。
fn two_edit_lattice(len: usize) -> Vec<Vec<LatticeEdge>> {
    let mut lattice: Vec<Vec<LatticeEdge>> = (0..=len).map(|_| Vec::new()).collect();
    lattice[0].push(edit_edge(3, "甲甲", "jia", 1));
    lattice[3].push(edit_edge(6, "乙乙", "jia", 1));
    lattice
}

fn engine_with_dict_entry() -> ImeEngine {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 后向 DP 要词频分母,空词库会让它退化成 0 分口径;插一条只为让
    // `total_freq > 0`。
    engine
        .dict
        .upsert("甲甲", "jia", 10)
        .expect("插入测试词条失败");
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
    assert!(
        !one[1][0].is_finite(),
        "预算 1:两次编辑的读法必须不可达(改动前这里会因为不累计而可达)"
    );
    assert!(!one[0][0].is_finite(), "预算 0:两条边都是编辑边,同样不可达");

    let two = engine
        .backward_completion(input, &ctx, &lattice, 2)
        .expect("后向 DP 失败");
    assert!(two[2][0].is_finite(), "预算 2:两次编辑的读法应可达");
    assert!(
        !two[1][0].is_finite(),
        "同一张格在预算 1 下仍必须不可达(预算不是摆设)"
    );
    assert!(
        two[2][0] >= two[1][0],
        "用不完的预算不强制花掉:预算更大的一行不得更低"
    );
}

/// 零编辑读法在任何预算下都必须成立——包括预算 0(`result[预算]` 自己
/// 那一行)。真实的精确格全是零编辑边,这条不成立的话生产查询会直接越界。
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
        assert!(
            table[budget as usize][0].is_finite(),
            "预算 {budget}:全零编辑的读法必须可达"
        );
    }
}

#[test]
fn candidate_completion_deducts_the_budget_used_by_its_first_edge() {
    let engine = engine_with_dict_entry();
    let ctx = LmContext::empty();
    let input = "jiajia";
    let lattice = two_edit_lattice(input.len());

    // 预算 1:首边自己就用了 1 次编辑,补全只剩 0,而 3→6 也是编辑边 ⇒
    // 首边候选不该拿到完整补全分(改动前这里会拿到有限分)。
    let suffix1 = engine
        .backward_completion(input, &ctx, &lattice, 1)
        .expect("后向 DP 失败");
    let cands1 = engine
        .lattice_candidates_scored(&ctx, &lattice, &suffix1)
        .expect("候选构建失败");
    let first1 = cands1
        .iter()
        .find(|c| c.word == "甲甲")
        .expect("首边应产出候选");
    assert!(
        !first1.score.is_finite(),
        "首边已用 1 次编辑、补全预算只剩 0 ⇒ 不应有覆盖整串的读法"
    );

    // 预算 2:首边扣掉 1 之后补全还有 1,两次编辑的读法成立。
    let suffix2 = engine
        .backward_completion(input, &ctx, &lattice, 2)
        .expect("后向 DP 失败");
    let cands2 = engine
        .lattice_candidates_scored(&ctx, &lattice, &suffix2)
        .expect("候选构建失败");
    let first2 = cands2
        .iter()
        .find(|c| c.word == "甲甲")
        .expect("首边应产出候选");
    assert!(
        first2.score.is_finite(),
        "预算 2:首边扣掉 1 之后补全还有 1,应构成完整读法"
    );
}

#[test]
fn typo_arcs_carry_one_edit_and_exact_edges_carry_none() {
    // 结构性检查:真实输入的容错格里,编辑次数必须落在边上——
    // 音节级编辑弧记 1,精确边记 0。若哪天有人在构造点漏填 `edit_count`,
    // 预算这一维会静默失效(所有边都像 0 次编辑),这条用例会先红。
    // 与 `engine/tests/lookup.rs::typo_pinyin_still_yields_the_intended_word`
    // 同一套玩具词库:`nihoa` 靠「ni + hoa→hao」一次编辑读成「你好」。
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
    let exact = engine
        .build_lattice(input, None, None)
        .expect("精确格构建失败");
    assert!(
        exact.iter().flatten().all(|e| e.edit_count == 0),
        "精确格不该有任何编辑边"
    );

    let stalls = ImeEngine::first_stalls(&exact, input.len());
    assert!(!stalls.is_empty(), "nihoa 应当存在走不动的位置");
    let tolerant = engine
        .build_tolerant_lattice(input, None, &exact, &stalls, None)
        .expect("容错格构建失败");
    let bucket: Vec<&LatticeEdge> = tolerant.iter().flatten().collect();
    let typed: Vec<&&LatticeEdge> = bucket.iter().filter(|e| e.typo_penalty > 0.0).collect();
    assert!(
        !typed.is_empty(),
        "容错格必须含带惩罚的编辑边(否则预算这一维无从生效): {bucket:?}"
    );
    assert!(
        typed.iter().all(|e| e.edit_count == 1),
        "每条容错弧记 1 次编辑(口径是「坏了几处」)"
    );
    assert!(
        typed.iter().all(|e| !e.gap),
        "nihoa 的容错读法是音节级编辑弧,不是空词弧"
    );
    assert!(
        bucket
            .iter()
            .filter(|e| e.gap)
            .all(|e| e.edit_count == 1),
        "空词边若出现也必须是 1 次编辑(口径一致)"
    );
}
