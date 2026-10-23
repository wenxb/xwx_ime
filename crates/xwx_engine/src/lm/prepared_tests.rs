use super::*;

#[test]
fn prepared_scorer_preserves_mixed_boost_order_and_snapshot() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/wanxiang-lts-zh-hans.gram");
    if !path.is_file() {
        return;
    }
    let user = UserLM::in_memory();
    user.learn(Some("今天"), "北京");
    let mut base = WanxiangLM::open(&path).unwrap();
    base.config.collocation_max_length = 6;
    let mut lm = LmStore::Mixed(MixedLM::new(LmStore::Wanxiang(base), user));
    for max in [6, 7] {
        if let LmStore::Mixed(mixed) = &mut lm {
            if let LmStore::Wanxiang(base) = mixed.base.as_mut() {
                base.config.collocation_max_length = max;
            }
        }
        for ctx in [
            LmContext::empty(),
            LmContext {
                tail: "我们今天",
                last_word: Some("今天"),
            },
        ] {
            let state = lm.state_from_context(&ctx);
            let scorer = lm.prepare(&state);
            if let LmStore::Mixed(mixed) = &lm {
                mixed.user().learn(Some("今天"), "北京");
            }
            for word in ["", "北京", "你好", "满额", "hello", "🙂"] {
                assert_eq!(
                    scorer.score(word).to_bits(),
                    lm.score_only(&state, word).to_bits()
                );
            }
        }
    }
}
