use super::*;

fn collocation_engine() -> Option<ImeEngine> {
    let engine = real_bilingual_engine()?;
    engine.lm.collocation_model()?;
    Some(engine.with_user_lm(UserLM::in_memory()))
}

fn filler(word: &str) -> Candidate {
    Candidate {
        score: -8.0,
        spelling: SpellingKind::Abbrev,
        covered: 4,
        ..Candidate::new(word, "li mian qu", FILLER_FREQ_TIER)
    }
}

#[test]
fn limq_phrase_is_visible_and_commits_in_a_real_session() {
    let Some(engine) = collocation_engine() else {
        return;
    };
    let engine = Arc::new(engine);
    let mut session = engine.create_session();
    for ch in "limq".chars() {
        session.handle(InputEvent::Char(ch)).unwrap();
    }
    let rank = session
        .candidates()
        .iter()
        .position(|c| c.word == "里面去")
        .unwrap();
    assert!(rank < 9, "常见短语应在首屏，实际第 {} 名", rank + 1);
    let output = session
        .handle(InputEvent::Select((rank + 1) as u8))
        .unwrap();
    assert!(matches!(&output, SessionOutput::Commit(word) if word == "里面去"));
}

#[test]
fn internal_collocation_breaks_a_filler_tie_without_changing_scores() {
    let Some(engine) = collocation_engine() else {
        return;
    };
    let mut candidates = [filler("李明启"), filler("里面去"), filler("里面请")];
    rank::sort_abbrev_candidates("limq", &mut candidates, &engine.lm);
    assert_eq!(candidates[0].word, "里面去");
    assert_eq!(candidates[1].word, "里面请");
    assert!(candidates
        .iter()
        .all(|c| c.score == -8.0 && c.freq == FILLER_FREQ_TIER));
}

#[test]
fn collocations_cannot_override_primary_score_frequency_layer_or_coverage() {
    let Some(engine) = collocation_engine() else {
        return;
    };
    for boundary in 0..6 {
        let mut name = filler("李明启");
        let mut phrase = filler("里面去");
        match boundary {
            0 => name.score = -7.999999,
            1 => name.freq = FILLER_FREQ_TIER + 1,
            2 => phrase.freq = FILLER_FREQ_TIER - 1,
            3 => name.spelling = SpellingKind::Exact,
            4 => phrase.covered = 3,
            5 => phrase.freq = 0,
            _ => unreachable!(),
        }
        let mut candidates = [phrase, name];
        rank::sort_abbrev_candidates("limq", &mut candidates, &engine.lm);
        assert_eq!(candidates[0].word, "李明启", "boundary={boundary}");
    }
}

#[test]
fn tied_partial_candidates_keep_their_slots_between_whole_candidates() {
    let Some(engine) = collocation_engine() else {
        return;
    };
    let mut partial = filler("里面");
    partial.covered = 3;
    let mut candidates = [filler("里面去"), partial, filler("李明启")];
    rank::sort_abbrev_candidates("limq", &mut candidates, &engine.lm);
    assert_eq!(candidates[0].word, "里面去");
    assert_eq!(candidates[1].word, "里面");
    assert_eq!(candidates[2].word, "李明启");
}

#[test]
fn no_character_model_keeps_filler_ties_in_text_order() {
    let lm = LmStore::Bigram(BigramLM::new());
    let learned = LmStore::Mixed(MixedLM::new(
        LmStore::Bigram(BigramLM::new()),
        UserLM::in_memory(),
    ));
    for lm in [&lm, &learned] {
        let mut candidates = [filler("里面去"), filler("李明启")];
        rank::sort_abbrev_candidates("limq", &mut candidates, lm);
        assert_eq!(candidates[0].word, "李明启");
    }
}
