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
