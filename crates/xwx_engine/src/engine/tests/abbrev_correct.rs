//! 简拼纠错（阶段 B）：证据闸、变体生成、正负例端到端。
//!
//! 现状（真词库 + 副索引 + 真 LM；缺文件时跳过）：
//! - 触发闸：原串有词条级证据的输入（`wsm`=为什么、`zgr`=中国人、
//!   `gshj`=工商局、`dhj`=打火机、`zgrm`=中国人民）**不纠**，首位逐条不变；
//! - 正例：`wnjtqbj`（邻键 m↔n，链级）→我们今天去北京 #1；
//!   `zgrr`（多打 r，词条级）→中国人 #1；
//! - 变体生成：换位/删字母/邻键替换三类、去重、上限 20。

use super::*;

fn real_engine() -> Option<ImeEngine> {
    let dict = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    let abbrev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/abbrev.bin");
    if !dict.is_file() || !abbrev.is_file() {
        return None;
    }
    let gram = gram_path()?;
    let mut engine = ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?;
    engine.dict = engine.dict.with_abbrev(&abbrev);
    Some(engine)
}

fn gram_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = std::path::PathBuf::from(home).join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

fn top(engine: &ImeEngine, input: &str, n: usize) -> Vec<String> {
    engine
        .lookup_candidates(input, &LmContext::empty())
        .unwrap()
        .into_iter()
        .take(n)
        .map(|c| c.word)
        .collect()
}

/// **负例**：原串有词条级证据（freq > 填充档）的输入，纠错闸必须挡住，
/// 首位逐条不变（它们本身就是合法读法，纠错只会砸精度）。
/// `gshj`（工商局，sh 双字母声母字面读法）与 `dhj`（打火机）当年被列为
/// 纠错正例，实测原串读法强——保守裁决下正确行为就是**不纠**，这里钉死。
#[test]
fn strong_original_inputs_are_not_corrected() {
    let Some(engine) = real_engine() else { return };
    for (input, expected) in [
        ("wsm", "为什么"),
        ("zgr", "中国人"),
        ("nh", "女孩"),
        ("wrsm", "我认识吗"),
        ("zgrm", "中国人民"),
        ("gshj", "工商局"),
        ("dhj", "电话机"),
        ("zrg", "责任感"),
    ] {
        assert_eq!(
            top(&engine, input, 1),
            vec![expected],
            "{input} 的原串读法强,不该被纠错扰动"
        );
    }
}

/// **正例（链级）**：`wnjtqbj`（想打 `wmjtqbj`,m↔n 同行邻键）原串只剩
/// 垃圾链（五年级提前布局类），纠正变体 `wmjtqbj` 的链读法「我们今天去北京」
/// 要进首位。
#[test]
fn neighbor_substitution_chain_correction() {
    let Some(engine) = real_engine() else { return };
    assert_eq!(top(&engine, "wnjtqbj", 1), vec!["我们今天去北京"]);
}

/// **正例（词条级）**：`zgrr`（多打一个 r）原串整串候选全是填充档,
/// 删除变体 `zgr` 精确命中「中国人」（词条级证据,词频 45 万）→ #1。
#[test]
fn deletion_entry_correction() {
    let Some(engine) = real_engine() else { return };
    assert_eq!(top(&engine, "zgrr", 1), vec!["中国人"]);
}

/// 变体生成：三类编辑齐全、去重、排除原串、上限 [`MAX_ABBREV_CORRECTION_VARIANTS`]。
#[test]
fn variant_generation_shape() {
    // `wnjtqbj`（7 字母）：换位 6 + 删 7 = 13 条之后轮到同行替换,
    // `n→m`（想打 wmjtqbj）在第 15/16 个,上限 20 必须保住它。
    let vs = super::super::abbrev_correct::generate_variants("wnjtqbj");
    assert!(vs.len() <= MAX_ABBREV_CORRECTION_VARIANTS);
    assert!(vs.iter().all(|v| v.key != "wnjtqbj"), "排除原串");
    let keys: Vec<&str> = vs.iter().map(|v| v.key.as_str()).collect();
    let uniq = keys.iter().collect::<std::collections::HashSet<_>>();
    assert_eq!(uniq.len(), keys.len(), "变体去重");
    assert!(keys.contains(&"wmjtqbj"), "同行邻键替换 n→m 必须生成");
    assert!(keys.contains(&"nwjtqbj"), "换位必须生成");
    assert!(keys.contains(&"njtqbj"), "删首字母必须生成");
    // 代价口径:换位 1.523、同行替换 1.301（-log10 0.03 / 0.05）。
    let trans = vs.iter().find(|v| v.key == "nwjtqbj").unwrap();
    assert!((trans.cost - 1.523).abs() < 0.01, "换位代价 {}", trans.cost);
    let sub = vs.iter().find(|v| v.key == "wmjtqbj").unwrap();
    assert!((sub.cost - 1.301).abs() < 0.01, "同行替换代价 {}", sub.cost);
    // `dhj`（3 字母）变体面全量生成,同列替换 `h→y`（想打 dyj=打印机）在内。
    let vs3 = super::super::abbrev_correct::generate_variants("dhj");
    assert!(
        vs3.iter().any(|v| v.key == "dyj"),
        "同列邻键替换 h→y 必须生成"
    );
}

/// 词条级证据探针：真实词条的精确 key 命中（含填充档过滤由调用方做,
/// 这里只验证「如实报告」）。
#[test]
fn abbrev_entry_evidence_reports_real_entries() {
    let Some(engine) = real_engine() else { return };
    let DictStore::Mmap(d) = &engine.dict else { return };
    // `zgr` = 中国人(词频 45 万);`wnjtqbj` 无词条级命中。
    let hit = d.abbrev_entry_evidence("zgr").unwrap();
    assert!(hit.is_some_and(|f| f > 100_000), "zgr 应命中中国人 {hit:?}");
    assert!(d.abbrev_entry_evidence("wnjtqbj").unwrap().is_none());
    // 链级证据:`wmjtqbj` 的首词「我们今天」= `wmjt` 是真实词条前缀。
    let chain = d.abbrev_chain_evidence("wmjtqbj", 4, 100).unwrap();
    assert!(chain.is_some(), "wmjt 前缀应有链级证据");
}