use super::rank::{conditional_base, user_counts, EnglishHit};
use super::recall::{completion_allowed, recall_english, ENGLISH_BLOCK_NAMESPACE};
use super::*;
use std::collections::BTreeMap;

// 保留优化前的逐源读取/逐词 exact 行为作 oracle。
fn oracle(
    dict: &EnglishDict,
    input: &str,
    ctx: &EnglishContext<'_>,
    snap: &crate::user_lm::UserLmSnapshot,
    model: &EnglishBigram,
) -> Vec<EnglishHit> {
    let mut keys = vec![input.to_owned()];
    keys.extend(
        dict.prefix_top(input)
            .unwrap()
            .iter()
            .map(|e| e.key.to_owned()),
    );
    keys.extend(snap.english_prefix_top(input).iter().take(32).cloned());
    if let Some(prev) = ctx.previous_word {
        keys.extend(
            snap.english_next_top(prev)
                .iter()
                .take(32)
                .filter(|k| k.starts_with(input))
                .cloned(),
        );
        if let Some(row) = model.row(prev).unwrap() {
            keys.extend(
                row.iter(model)
                    .take(32)
                    .filter(|e| e.next.starts_with(input))
                    .map(|e| e.next.to_owned()),
            );
        }
    }
    let mut merged = BTreeMap::new();
    for key in keys {
        if merged.contains_key(&key) || (!completion_allowed(input, ctx) && key != input) {
            continue;
        }
        let Some(e) = dict.exact(&key).unwrap() else {
            continue;
        };
        let (uni, bi) = user_counts(snap, ctx, e.key);
        let static_base = ctx
            .previous_word
            .and_then(|p| model.row(p).unwrap())
            .and_then(|r| {
                let count = r
                    .iter(model)
                    .find(|x| x.next == e.key)
                    .map(|x| x.count)
                    .unwrap_or(0);
                conditional_base(e.zipf_centi, count, r.observed_total)
            });
        merged.insert(
            key.clone(),
            EnglishHit {
                key,
                display: e.display.into(),
                zipf_centi: e.zipf_centi,
                match_kind: if e.key == input {
                    EnglishMatchKind::Exact
                } else {
                    EnglishMatchKind::Completion
                },
                user_unigram: uni,
                user_bigram: bi,
                static_base,
            },
        );
    }
    merged.retain(|k, _| !snap.is_blocked(ENGLISH_BLOCK_NAMESPACE, k));
    merged.into_values().collect()
}

#[test]
fn optimized_recall_matches_old_sources_and_scores_bit_for_bit() {
    let words = [
        SynthWord {
            key: "a",
            display: "a",
            q: 700,
        },
        SynthWord {
            key: "he",
            display: "he",
            q: 600,
        },
        SynthWord {
            key: "hello",
            display: "Hello",
            q: 450,
        },
        SynthWord {
            key: "help",
            display: "help",
            q: 520,
        },
        SynthWord {
            key: "python",
            display: "Python",
            q: 410,
        },
    ];
    let dict = EnglishDict::from_bytes(build_synth_bin(&words)).unwrap();
    let model = EnglishBigram::from_bytes(build_synth_bigram(&[
        BigramEdgeSpec {
            prev: "say",
            next: "hello",
            count: 100,
        },
        BigramEdgeSpec {
            prev: "say",
            next: "help",
            count: 50,
        },
        BigramEdgeSpec {
            prev: "say",
            next: "removed",
            count: 10,
        },
    ]))
    .unwrap();
    for learned in [false, true] {
        let user = UserLM::in_memory();
        if learned {
            user.learn(Some("say"), "python");
            user.learn(Some("say"), "removed");
            user.block_candidate("help", ENGLISH_BLOCK_NAMESPACE);
        }
        let snap = user.snapshot();
        for ctx in [
            EnglishContext::empty(),
            EnglishContext::with_previous("say"),
            EnglishContext::with_previous("absent"),
        ] {
            for input in [
                "", "a", "h", "he", "hel", "hello", "py", "pyt", "removed", "missing",
            ] {
                let actual = recall_english(&dict, input, &ctx, &snap, Some(&model)).unwrap();
                let expected = oracle(&dict, input, &ctx, &snap, &model);
                assert_eq!(actual.len(), expected.len());
                for (a, b) in actual.iter().zip(&expected) {
                    assert_eq!(format!("{a:?}"), format!("{b:?}"), "{input} {ctx:?}");
                    assert_eq!(
                        a.static_base.map(f32::to_bits),
                        b.static_base.map(f32::to_bits)
                    );
                }
            }
        }
    }
}
