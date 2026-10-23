//! 热路径优化的首现、并列与频率证据契约。
use super::*;
use crate::candidate_index::CandidateIndex;

#[test]
fn rescoring_updates_only_first_match_and_never_reorders() {
    let mut first = Candidate::new("甲", "jia", 1);
    first.covered = 1;
    let second = Candidate::new("甲", "jia", 2);
    let other = Candidate::new("乙", "yi", 3);
    let mut ranked = vec![first.clone(), second.clone(), other.clone()];
    let mut index = CandidateIndex::new(&ranked);
    let mut c = Candidate::new("甲", "jia", 100);
    c.score = 4.25;
    c.covered = 3;
    c.spelling = SpellingKind::Abbrev;
    c.matched_len = 7;
    ImeEngine::push_rescored(&mut ranked, &mut index, c.clone(), false);
    assert_eq!(ranked, vec![first.clone(), second.clone(), other.clone()]);
    ImeEngine::push_rescored(&mut ranked, &mut index, c, true);
    first.score = 4.25;
    first.covered = 3;
    first.freq = 100;
    assert_eq!(ranked, vec![first, second, other]);
    let new = Candidate::new("丙", "bing", 4);
    ImeEngine::push_rescored(&mut ranked, &mut index, new.clone(), false);
    ImeEngine::push_rescored(&mut ranked, &mut index, new.clone(), false);
    assert_eq!(ranked.len(), 4);
    assert_eq!(ranked[3], new);
}

#[test]
fn display_fold_keeps_first_on_equal_coverage_and_full_winner_on_larger() {
    let a = Candidate::new("甲", "jia", 1);
    let mut b = a.clone();
    b.score = 12.5;
    b.matched_len = 8;
    let out = crate::engine::recall::fold_display_candidates(vec![a.clone(), b.clone()]);
    assert_eq!(out, vec![a]);
    b.covered = 3;
    let other = Candidate::new("乙", "yi", 1);
    let out = crate::engine::recall::fold_display_candidates(vec![
        Candidate::new("甲", "jia", 1),
        other.clone(),
        b.clone(),
    ]);
    assert_eq!(out, vec![b, other]);
}

#[test]
fn tail_gate_short_circuit_matches_original_segment_budget() {
    let engine = ImeEngine::new(None).unwrap();
    let original = |pinyin: &str| {
        let n = pinyin.len();
        (n >= 2 && !engine.segmenter.segment(&pinyin[..n - 1]).is_empty())
            || (n >= 3
                && matches!(&pinyin[n - 2..], "zh" | "ch" | "sh")
                && !engine.segmenter.segment(&pinyin[..n - 2]).is_empty())
            || (2..n).any(|k| {
                !engine.segmenter.segment(&pinyin[..n - k]).is_empty()
                    && pinyin[n - k..]
                        .bytes()
                        .all(|c| b"bpmfdtnlgkhjqxrzcsyw".contains(&c))
            })
    };
    let mut inputs: Vec<String> = [
        "",
        "n",
        "ni",
        "buyy",
        "yiwanch",
        "daishix",
        "shanghaai",
        "jintqbj",
        "womenjintianquxian",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for n in [6, 12] {
        for tail in ["", "yy", "ae", "ch"] {
            inputs.push(format!("{}{tail}", "xian".repeat(n)));
        }
    }
    for input in inputs {
        assert_eq!(
            engine.ends_one_letter_short(&input),
            original(&input),
            "{input}"
        );
    }
}

#[test]
fn entry_evidence_uses_main_frequency_even_when_candidate_has_nonzero_old_frequency() {
    let Some(engine) = real_bilingual_engine() else {
        return;
    };
    let ev = engine.entry_evidence(&LmContext::empty()).unwrap();
    let key = "ni";
    let entry = engine.dict.lookup(key).unwrap().into_iter().next().unwrap();
    let mut c = entry.clone();
    c.freq = 1;
    c.covered = key.len();
    engine.fill_entry_evidence(key, &mut c, Some(&ev)).unwrap();
    let expected = ((entry.freq.max(1) as f64).log10() - (ev.total as f64).log10()) as f32
        + (engine.lm.score_only(&ev.state, &c.word) - engine.lm.floor_log10()) * engine.lm_weight;
    assert_eq!(c.freq, 1, "非零候选频率保持原字段值");
    assert_eq!(
        c.score.to_bits(),
        expected.to_bits(),
        "打分证据必须来自主词库"
    );
}
