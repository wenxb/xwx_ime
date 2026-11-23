//! 简拼纠错：证据闸、变体生成与正负例端到端。

use super::*;

fn top(engine: &ImeEngine, input: &str, n: usize) -> Vec<String> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .unwrap()
        .into_iter()
        .take(n)
        .map(|c| c.word)
        .collect()
}

/// 强原串读法受证据闸保护不被扰动；邻键替换（链级）与多打字母（词条级）正确纠到首位。
#[test]
fn abbrev_correction_guards_strong_inputs_and_fixes_typos() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    for (input, expected) in [
        ("wsm", "为什么"),
        ("zgr", "中国人"),
        ("nh", "女孩"),
        ("zgrm", "中国人民"),
        ("gshj", "工商局"),
        ("dhj", "电话机"),
        ("zrg", "责任感"),
        ("wnjtqbj", "我们今天去北京"),
        ("zgrr", "中国人"),
    ] {
        assert_eq!(top(engine, input, 1), vec![expected], "{input} 首位不符");
    }
    let wrsm = top(engine, "wrsm", 5);
    assert_ne!(wrsm[0], "为什么", "wrsm 不应被误纠成为什么: {wrsm:?}");
    assert!(wrsm.iter().any(|w| w == "我认识吗"), "wrsm 应保留「我认识吗」: {wrsm:?}");
}

/// 变体生成：换位/删字母/邻键替换三类齐全、去重、排除原串、受上限约束。
#[test]
fn variant_generation_shape() {
    let vs = super::super::abbrev_correct::generate_variants("wnjtqbj");
    assert!(vs.len() <= MAX_ABBREV_CORRECTION_VARIANTS);
    assert!(vs.iter().all(|v| v.key != "wnjtqbj"), "排除原串");
    let keys: Vec<&str> = vs.iter().map(|v| v.key.as_str()).collect();
    let uniq: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(uniq.len(), keys.len(), "变体去重");
    assert!(keys.contains(&"wmjtqbj"), "同行邻键替换 n→m");
    assert!(keys.contains(&"nwjtqbj"), "换位");
    assert!(keys.contains(&"njtqbj"), "删首字母");

    let trans = vs.iter().find(|v| v.key == "nwjtqbj").unwrap();
    assert!((trans.cost - 1.523).abs() < 0.01, "换位代价 {}", trans.cost);
    let sub = vs.iter().find(|v| v.key == "wmjtqbj").unwrap();
    assert!((sub.cost - 1.301).abs() < 0.01, "同行替换代价 {}", sub.cost);

    let vs3 = super::super::abbrev_correct::generate_variants("dhj");
    assert!(vs3.iter().any(|v| v.key == "dyj"), "同列邻键替换 h→y");

    let vs_dup = super::super::abbrev_correct::generate_variants("zgrr");
    let zgr = vs_dup.iter().find(|v| v.key == "zgr").unwrap();
    assert!((zgr.cost - 0.602).abs() < 0.01, "连敲回弹代价 {}", zgr.cost);
}

/// 词条级与链级证据探针如实报告真实词库命中。
#[test]
fn abbrev_entry_evidence_reports_real_entries() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    let DictStore::Mmap(d) = &engine.dict else {
        return;
    };
    let hit = d.abbrev_entry_evidence("zgr").unwrap();
    assert!(hit.is_some_and(|f| f > 100_000), "zgr 应命中中国人 {hit:?}");
    assert!(d.abbrev_entry_evidence("wnjtqbj").unwrap().is_none());
    let chain = d.abbrev_chain_evidence("wmjtqbj", 4, 100).unwrap();
    assert!(chain.is_some(), "wmjt 前缀应有链级证据");
}
