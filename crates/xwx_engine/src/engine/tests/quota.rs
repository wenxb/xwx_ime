//! 简拼保底配额只给**缩写读法**（A2 缺陷 1 的定向用例）。
//!
//! 不依赖词库与语言模型，手工构造输入，结果完全确定。

use super::*;
use crate::engine::rank::merge_layer;

/// 简拼层里混着「末尾半音节补全」那批有意的 `Exact` 时，保底配额仍要给
/// `Abbrev` 读法。
///
/// 旧实现按**来源**分层（简拼这条路返回的尾部 N 条一律当简拼层），配额会被
/// `Exact` 白占：这里把 `Exact` 补全的分数设得更高，旧实现截断后一条 `Abbrev`
/// 都不剩——本用例在旧实现上是红的。
#[test]
fn abbrev_quota_survives_exact_completions_in_the_same_layer() {
    let mk = |word: &str, score: f32, spelling: SpellingKind| {
        let mut c = Candidate::new(word, "ha", 1);
        c.score = score;
        c.spelling = spelling;
        c
    };
    // 精确层先占满上限。
    let head: Vec<Candidate> = (0..MAX_CANDIDATES)
        .map(|i| mk(&format!("h{i}"), 100.0, SpellingKind::Exact))
        .collect();
    // 简拼层：ABBREV_QUOTA 条 Exact 补全（分数高，会被优先保留）+ 3 条缩写读法。
    let mut abbrev: Vec<Candidate> = (0..ABBREV_QUOTA)
        .map(|i| mk(&format!("c{i}"), 50.0, SpellingKind::Exact))
        .collect();
    abbrev.extend((0..3).map(|i| mk(&format!("a{i}"), 1.0, SpellingKind::Abbrev)));

    let (_, out) = merge_with_abbrev("ha", head, abbrev);
    let kept = out
        .iter()
        .filter(|c| c.spelling == SpellingKind::Abbrev)
        .count();
    assert_eq!(
        kept, 3,
        "缩写读法应保住 3 条（配额只给 Abbrev，不被 Exact 补全占掉），实际 {:?}",
        out.iter()
            .map(|c| (&c.word, c.spelling))
            .filter(|(_, s)| *s == SpellingKind::Abbrev)
            .collect::<Vec<_>>()
    );
}

/// **容错层也有保底配额**：精确层占满上限时，打错一处的纠错读法（`Fuzzy`）
/// 不能被整表截断掉。
///
/// 旧实现在容错那条路上只做「push 后 sort + truncate」，精确层一满就把这批
/// 纠错读法全切了——这条用例在旧实现上是红的。
#[test]
fn tolerant_layer_keeps_its_quota_when_the_exact_layer_is_full() {
    let mk = |word: &str, score: f32, spelling: SpellingKind| {
        let mut c = Candidate::new(word, "ha", 1);
        c.score = score;
        c.spelling = spelling;
        c
    };
    let head: Vec<Candidate> = (0..MAX_CANDIDATES)
        .map(|i| mk(&format!("h{i}"), 100.0, SpellingKind::Exact))
        .collect();
    // 纠错读法分数天然更低（带编辑惩罚），正是会被截掉的那批。
    let tolerant: Vec<Candidate> = (0..TOLERANT_QUOTA)
        .map(|i| mk(&format!("t{i}"), 1.0, SpellingKind::Fuzzy))
        .collect();

    let (_, out) = merge_layer("ha", head, tolerant, SpellingKind::Fuzzy, TOLERANT_QUOTA);
    let kept = out
        .iter()
        .filter(|c| c.spelling == SpellingKind::Fuzzy)
        .count();
    assert_eq!(
        kept, TOLERANT_QUOTA,
        "容错层应保住 TOLERANT_QUOTA 条纠错读法，实际 {kept} 条"
    );
}
