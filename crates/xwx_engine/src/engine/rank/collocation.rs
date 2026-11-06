//! 整串简拼填充词条：仅在原排序键和词频相同时用内部搭配打破平局。
use super::*;

pub(super) fn break_ties(
    input: &str,
    ranked: &[Candidate],
    keys: &[PlainKey],
    order: &mut [usize],
    lm: &WanxiangLM,
) {
    let eligible = |i: usize| {
        let c = &ranked[i];
        c.spelling == SpellingKind::Abbrev
            && c.freq == FILLER_FREQ_TIER
            && c.covered == input.len()
            && c.score.is_finite()
            && !c.echo
            && !c.is_english()
    };
    let mut peers = Vec::new();
    let mut start = 0;
    while start < order.len() {
        let first = order[start];
        let mut end = start + 1;
        while end < order.len()
            && keys[first] == keys[order[end]]
            && ranked[first].freq == ranked[order[end]].freq
        {
            end += 1;
        }
        let group = &mut order[start..end];
        if group.iter().filter(|&&i| eligible(i)).take(2).count() == 2 {
            peers.clear();
            peers.extend(
                group
                    .iter()
                    .copied()
                    .filter(|&i| eligible(i))
                    .map(|i| (i, ScoreDesc(lm.internal_collocation_score(&ranked[i].word)))),
            );
            // 稳定排序保留搭配分相同的词面顺序；不挪动不符合条件的候选位。
            peers.sort_by_key(|&(_, score)| score);
            let mut replacements = peers.iter();
            for slot in group.iter_mut().filter(|i| eligible(**i)) {
                *slot = replacements.next().unwrap().0;
            }
        }
        start = end;
    }
}
