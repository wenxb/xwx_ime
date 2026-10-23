//! 共享末端桶必须与两次独立 beam 的路径、并列次序和分数逐位一致。
use super::*;

fn assert_paths_equal(got: &[BeamPath], want: &[BeamPath]) {
    assert_eq!(got.len(), want.len());
    for (got, want) in got.iter().zip(want) {
        assert_eq!(got.score.to_bits(), want.score.to_bits());
        let words = |p: &BeamPath| {
            p.words
                .iter()
                .map(|w| (w.word.clone(), w.pinyin.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(words(got), words(want));
    }
}

#[test]
fn shared_sentence_terminal_bucket_matches_independent_decodes() {
    let engine = ImeEngine::new(None).unwrap();
    for (word, key, freq) in [
        ("我", "wo", 500),
        ("窝", "wo", 500),
        ("们", "men", 200),
        ("我们", "wo men", 600),
        ("窝门", "wo men", 600),
        ("卡", "ka", 10),
        ("咖", "ka", 10),
        ("看", "kan", 900),
        ("开", "kai", 800),
        ("考", "kao", 700),
        ("抗", "kang", 600),
    ] {
        engine.dict.upsert(word, key, freq).unwrap();
    }
    let mut engines = vec![engine];
    if let Some(real) = real_bilingual_engine() {
        engines.push(real);
    }
    if let Some(real) = real_bilingual_engine() {
        let user = crate::UserLM::in_memory();
        user.learn(Some("今天"), "卡");
        engines.push(real.with_user_lm(user));
    }
    for engine in &engines {
        for input in ["womenka", "haihuika", "womenjintianquxian", "nihaobeijin"] {
            let lattice = engine.build_lattice(input, None, None).unwrap();
            assert!(lattice
                .iter()
                .enumerate()
                .all(|(pos, edges)| edges.iter().all(|e| {
                    e.end == input.len()
                        || (!e.gap
                            && !e.abbrev
                            && e.fuzzy_penalty == 0.0
                            && e.edit_count == 0
                            && ImeEngine::edge_spells_span(e, pos))
                })));
            let literal: Vec<Vec<LatticeEdge>> = lattice
                .iter()
                .enumerate()
                .map(|(pos, edges)| {
                    edges
                        .iter()
                        .filter(|e| {
                            !e.gap
                                && !e.abbrev
                                && e.fuzzy_penalty == 0.0
                                && e.edit_count == 0
                                && ImeEngine::edge_spells_span(e, pos)
                        })
                        .cloned()
                        .collect()
                })
                .collect();
            for ctx in [
                LmContext::empty(),
                LmContext {
                    tail: "我们今天",
                    last_word: Some("今天"),
                },
            ] {
                for k in [0, 1, 4, 16] {
                    for penalty in [0.0, ABBREV_PER_WORD_PENALTY] {
                        let (normal, exact) = engine
                            .sentence_beam(input, &ctx, k, &lattice, penalty)
                            .unwrap();
                        let want = engine
                            .decode_beam_with_lattice_penalized(input, &ctx, k, &lattice, penalty)
                            .unwrap();
                        let want_exact = engine
                            .decode_beam_with_lattice_penalized(input, &ctx, k, &literal, penalty)
                            .unwrap();
                        assert_paths_equal(&normal, &want);
                        assert_paths_equal(&exact, &want_exact);
                    }
                }
            }
        }
    }
}
