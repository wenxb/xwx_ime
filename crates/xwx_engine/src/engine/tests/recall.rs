//! `RecallHit` 纯结构层（`docs/recall-hit.md` 第 ① 步）。

use super::*;
use crate::engine::recall::{collect_recall_hits, RecallSource};

/// 同词同音、**消费范围不同**的两条是两条召回事实，不能被折叠掉——
/// 分段上屏要靠 `consumed` 区分「这个词最多能吃掉多少输入」。
#[test]
fn same_word_and_pinyin_with_different_consumed_are_two_facts() {
    let mut whole = Candidate::new("北京", "bei jing", 1);
    whole.covered = 8;
    let mut prefix = Candidate::new("北京", "bei jing", 1);
    prefix.covered = 3;

    let hits = collect_recall_hits(&[whole, prefix], RecallSource::Abbrev);
    assert_eq!(hits.len(), 2, "两条事实都应保留");
    assert_eq!(hits[0].consumed, 8);
    assert_eq!(hits[1].consumed, 3);
    assert_ne!(hits[0].consumed, hits[1].consumed);
}

/// 折叠按 `(word, pinyin)`：保留**消费范围最大**的那条，完全重复的去掉。
#[test]
fn folding_keeps_the_largest_consumed() {
    let mut prefix = Candidate::new("北京", "bei jing", 1);
    prefix.covered = 3;
    let mut whole = Candidate::new("北京", "bei jing", 1);
    whole.covered = 8;

    let out = crate::engine::recall::fold_display_candidates(vec![prefix, whole]);
    assert_eq!(out.len(), 1, "同词同音应折叠成一条");
    assert_eq!(out[0].covered, 8, "保留消费范围更大的那条");
}

/// 阶段 4 ②：折叠要**带上来源**——展示投影要靠它决定层序/配额/整串置顶。
#[test]
fn fold_keeps_the_source_of_the_winner() {
    let mk = |word: &str, py: &str, covered: usize| Candidate {
        word: word.to_owned(),
        pinyin: py.to_owned(),
        score: 0.0,
        freq: 0,
        spelling: SpellingKind::Exact,
        echo: false,
        covered,
        matched_len: 0,
    };
    let out = crate::engine::recall::fold_with_sources(vec![
        (mk("出在哪", "chu zai na", 5), crate::engine::recall::RecallSource::Abbrev),
        (
            mk("出在哪", "chu zai na", 8),
            crate::engine::recall::RecallSource::Sentence,
        ),
        (mk("杵在那", "chu zai na", 8), crate::engine::recall::RecallSource::Lattice),
    ]);
    assert_eq!(out.len(), 2, "同词同音折叠成一条");
    let (c, src) = &out[0];
    assert_eq!(
        (c.word.as_str(), c.covered, *src),
        ("出在哪", 8, crate::engine::recall::RecallSource::Sentence),
        "保留消费范围最大的那条，并把它的来源一起留下"
    );
}
