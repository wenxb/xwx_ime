use super::{Candidate, MAX_LOOKUP_RESULTS};

/// 混拼池的 (freq↓, word, pinyin) 全序；调用方已按词/音去重。
pub(super) fn sort_mixed_top(candidates: &mut Vec<Candidate>) {
    let order = |a: &Candidate, b: &Candidate| {
        b.freq
            .cmp(&a.freq)
            .then_with(|| a.word.cmp(&b.word))
            .then_with(|| a.pinyin.cmp(&b.pinyin))
    };
    let limit = MAX_LOOKUP_RESULTS as usize;
    if candidates.len() > limit {
        candidates.select_nth_unstable_by(limit, order);
        candidates.truncate(limit);
    }
    candidates.sort_by(order);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_sort_matches_full_sort_for_empty_small_limit_and_large_pools() {
        for n in [0, 1, 499, 500, 501, 4000] {
            let mut actual: Vec<_> = (0..n)
                .rev()
                .map(|i| {
                    Candidate::new(
                        format!("词{:04}", i / 2),
                        format!("py{}", i % 2),
                        (i % 17) as u64,
                    )
                })
                .collect();
            let mut expected = actual.clone();
            expected.sort_by(|a, b| {
                b.freq
                    .cmp(&a.freq)
                    .then_with(|| a.word.cmp(&b.word))
                    .then_with(|| a.pinyin.cmp(&b.pinyin))
            });
            expected.truncate(MAX_LOOKUP_RESULTS as usize);
            sort_mixed_top(&mut actual);
            assert_eq!(actual, expected, "pool={n}");
        }
    }
}
