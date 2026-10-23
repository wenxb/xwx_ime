//! 随机正向格的共享搜索不能改路径或 f32 分数。
use super::*;

fn path_keys(paths: &[BeamPath]) -> Vec<(u32, Vec<(String, String)>)> {
    paths
        .iter()
        .map(|p| {
            (
                p.score.to_bits(),
                p.words
                    .iter()
                    .map(|w| (w.word.clone(), w.pinyin.clone()))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn shared_terminal_search_matches_two_decoders_on_random_lattices() {
    let mut engine = ImeEngine::new(None).unwrap();
    let user = crate::UserLM::in_memory();
    user.learn(Some("甲"), "乙");
    engine = engine.with_user_lm(user);
    let mut seed = 0x005e_ed45_u64;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((seed >> 32) as usize) % n
    };
    let words = ["甲", "乙", "丙", "丁"];
    for _ in 0..100 {
        let len = 3 + next(6);
        let input = "a".repeat(len);
        let mut lattice = vec![vec![]; len + 1];
        for (pos, bucket) in lattice.iter_mut().enumerate().take(len) {
            for _ in 0..1 + next(10) {
                let end = pos + 1 + next(len - pos);
                let letters = end - pos + usize::from(end == len && next(2) == 0);
                bucket.push(LatticeEdge {
                    end,
                    word: words[next(words.len())].into(),
                    pinyin: "a".repeat(letters),
                    freq: next(4) as u64,
                    fuzzy_penalty: 0.0,
                    typo_penalty: 0.0,
                    edit_count: 0,
                    gap: false,
                    abbrev: false,
                });
            }
        }
        let literal: Vec<Vec<LatticeEdge>> = lattice
            .iter()
            .enumerate()
            .map(|(pos, es)| {
                es.iter()
                    .filter(|e| ImeEngine::edge_spells_span(e, pos))
                    .cloned()
                    .collect()
            })
            .collect();
        for k in [0, 1, 4, 16] {
            let ctx = LmContext {
                tail: "甲",
                last_word: Some("甲"),
            };
            let (normal, exact) = engine
                .sentence_beam(&input, &ctx, k, &lattice, ABBREV_PER_WORD_PENALTY)
                .unwrap();
            let want = engine
                .decode_beam_with_lattice_penalized(
                    &input,
                    &ctx,
                    k,
                    &lattice,
                    ABBREV_PER_WORD_PENALTY,
                )
                .unwrap();
            let want_exact = engine
                .decode_beam_with_lattice_penalized(
                    &input,
                    &ctx,
                    k,
                    &literal,
                    ABBREV_PER_WORD_PENALTY,
                )
                .unwrap();
            assert_eq!(path_keys(&normal), path_keys(&want));
            assert_eq!(path_keys(&exact), path_keys(&want_exact));
        }
    }
}
