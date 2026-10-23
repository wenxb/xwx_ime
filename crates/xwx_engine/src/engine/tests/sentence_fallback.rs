//! 非字面中间边不能误用共享末端桶。
use super::*;

fn reference(engine: &ImeEngine, input: &str, lattice: &[Vec<LatticeEdge>]) -> Vec<Candidate> {
    let ctx = LmContext::empty();
    let mut out = engine
        .best_chain_candidates(
            input,
            &ctx,
            lattice,
            SpellingKind::Exact,
            ABBREV_PER_WORD_PENALTY,
        )
        .unwrap();
    if !lattice[0].iter().any(|e| e.end == input.len()) {
        engine
            .append_literal_sentence(input, &ctx, lattice, &mut out)
            .unwrap();
    }
    out
}

fn edge(end: usize, word: &str, pinyin: &str, freq: u64) -> LatticeEdge {
    LatticeEdge {
        end,
        word: word.into(),
        pinyin: pinyin.into(),
        freq,
        fuzzy_penalty: 0.0,
        typo_penalty: 0.0,
        edit_count: 0,
        gap: false,
        abbrev: false,
    }
}

#[test]
fn sentence_nonliteral_prefixes_fall_back_without_losing_paths() {
    let engine = ImeEngine::new(None).unwrap();
    let input = "womennika";
    let mut base = vec![vec![]; input.len() + 1];
    base[0].push(edge(5, "我们", "wo men", 500));
    base[5].push(edge(7, "你", "ni", 100));
    base[7].push(edge(9, "卡", "ka", 10));
    for (word, key, freq) in [
        ("看", "kan", 900),
        ("开", "kai", 800),
        ("考", "kao", 700),
        ("抗", "kang", 600),
    ] {
        base[7].push(edge(9, word, key, freq));
    }
    for variant in 0..5 {
        let mut lattice = base.clone();
        let mut e = edge(7, "啊", "a", 9000);
        match variant {
            0 => e.abbrev = true,
            1 => e.fuzzy_penalty = 0.1,
            2 => {
                e.edit_count = 1;
                e.typo_penalty = 1.0;
            }
            3 => {
                e.gap = true;
                e.word.clear();
                e.pinyin.clear();
                e.edit_count = 1;
            }
            _ => (),
        }
        lattice[5].push(e);
        let got = engine
            .sentence_candidates(input, &LmContext::empty(), &lattice)
            .unwrap();
        let want = reference(&engine, input, &lattice);
        assert_eq!(got, want, "variant={variant}");
    }
}

#[test]
fn sentence_terminal_edits_and_direct_entries_keep_reference_results() {
    let engine = ImeEngine::new(None).unwrap();
    let input = "womennika";
    let mut base = vec![vec![]; input.len() + 1];
    base[0].push(edge(5, "我们", "wo men", 500));
    base[5].push(edge(7, "你", "ni", 100));
    base[7].push(edge(9, "卡", "ka", 10));
    for (word, key) in [("看", "kan"), ("开", "kai"), ("考", "kao"), ("抗", "kang")] {
        base[7].push(edge(9, word, key, 900));
    }
    for variant in 0..6 {
        let mut lattice = base.clone();
        let mut tail = edge(9, "甲", "ka", 9000);
        match variant {
            0 => tail.abbrev = true,
            1 => tail.fuzzy_penalty = 0.1,
            2 => {
                tail.edit_count = 1;
                tail.typo_penalty = 1.0;
            }
            3 => {
                tail.gap = true;
                tail.word.clear();
                tail.pinyin.clear();
                tail.typo_penalty = 1.0;
            }
            4 => tail.edit_count = MAX_TYPO_EDITS + 1,
            _ => lattice[0].push(edge(9, "整词", "wo men ni ka", 10000)),
        }
        lattice[7].push(tail);
        let got = engine
            .sentence_candidates(input, &LmContext::empty(), &lattice)
            .unwrap();
        assert_eq!(
            got,
            reference(&engine, input, &lattice),
            "variant={variant}"
        );
    }
}
