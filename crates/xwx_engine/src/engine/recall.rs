//! 展示候选折叠：按展示词面 `word` 去重，避免多音词在首屏重复出现，保留更大覆盖范围。

use std::collections::HashMap;

use crate::Candidate;

/// 把展示候选按词面 `word` 折叠：同词保留覆盖更宽者（`covered == 0` 表示整串，
/// 不可被更短覆盖或更低拼写层覆写），并列保留先到（更高排名）者。
pub(super) fn fold_display_candidates(cands: Vec<Candidate>) -> Vec<Candidate> {
    let eff_cov = |cov: usize| if cov == 0 { usize::MAX } else { cov };
    let mut out: Vec<Candidate> = Vec::with_capacity(cands.len());
    let mut index: HashMap<String, usize> = HashMap::with_capacity(cands.len());
    for c in cands {
        match index.get(&c.word) {
            Some(&i) => {
                if eff_cov(c.covered) > eff_cov(out[i].covered)
                    && c.spelling <= out[i].spelling
                {
                    out[i] = c;
                }
            }
            None => {
                index.insert(c.word.clone(), out.len());
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_keeps_first_on_equal_coverage_and_replaces_on_larger() {
        let mut a = Candidate::new("甲", "jia", 1);
        a.covered = 1;
        let mut b = a.clone();
        b.score = 12.5;
        b.matched_len = 8;
        assert_eq!(fold_display_candidates(vec![a.clone(), b.clone()]), vec![a.clone()]);

        b.covered = 3;
        let other = Candidate::new("乙", "yi", 1);
        let out = fold_display_candidates(vec![a, other.clone(), b.clone()]);
        assert_eq!(out, vec![b, other]);

        // covered == 0 表示整串，不得被局部/简拼候选覆写。
        let whole = Candidate::new("丙", "bing", 1);
        let mut abbrev = whole.clone();
        abbrev.covered = 2;
        abbrev.spelling = crate::SpellingKind::Abbrev;
        assert_eq!(
            fold_display_candidates(vec![whole.clone(), abbrev]),
            vec![whole]
        );
    }
}
