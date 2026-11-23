//! `lm` 模块单测。

use super::*;

#[test]
fn bigram_prefers_known_pair_and_unknown_is_finite() {
    let mut lm = BigramLM::new();
    lm.add_unigram("中国", 100);
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 50);
    lm.add_bigram("中", "国", 90);

    let prev = Some("中".to_string());
    assert!(lm.score(&prev, "国").0 > lm.score(&prev, "华").0);
    assert!(BigramLM::new().score(&None, "不存在的词").0.is_finite());
}

#[test]
fn state_advances_after_score() {
    let mut lm = BigramLM::new();
    lm.add_unigram("你", 100);
    lm.add_unigram("好", 50);
    lm.add_bigram("你", "好", 80);

    let (s0, next) = lm.score(&lm.initial_state(), "你");
    assert_eq!(next, Some("你".to_string()));
    let (s1, next1) = lm.score(&next, "好");
    assert_eq!(next1, Some("好".to_string()));

    // 句首 unigram P(你) = (100+1)/(150+2)；bigram P(好|你) = (80+1)/(100+2)
    let expected0 = ((100.0_f32 + 1.0) / (150.0_f32 + 2.0)).log10();
    let expected1 = ((80.0_f32 + 1.0) / (100.0_f32 + 2.0)).log10();
    assert!((s0 - expected0).abs() < 1e-6);
    assert!((s1 - expected1).abs() < 1e-6);
}

#[test]
fn lm_store_routes_and_context_hits_bigram() {
    let mut store = LmStore::Bigram(BigramLM::new());
    store
        .load("中国\t100\n中\t100\n国\t50\n中\t国\t90\n".as_bytes())
        .expect("加载失败");

    let state = store.initial_state();
    assert_eq!(state, LmState::Bigram(None));
    let (solo, _) = store.score(&state, "国");
    let (_, advanced) = store.score(&state, "中");
    assert_eq!(advanced, LmState::Bigram(Some("中".to_string())));
    assert!(store.score(&advanced, "国").0 > solo);

    // Bigram 只读 last_word，不受 tail 影响；注入上文后经 LmStore 同样命中 bigram
    let tail_only = LmContext { tail: "中", last_word: None };
    assert_eq!(store.state_from_context(&tail_only), LmState::Bigram(None));
    let ctx = LmContext { tail: "中", last_word: Some("中") };
    assert!(store.score(&store.state_from_context(&ctx), "国").0 > solo);
}

#[test]
fn mixed_state_advances_both_layers() {
    let mut base = BigramLM::new();
    base.add_unigram("今天", 100);
    base.add_bigram("今天", "天气", 90);
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), UserLM::in_memory()));

    let s0 = store.state_from_context(&LmContext {
        tail: "今天",
        last_word: Some("今天"),
    });
    let LmState::Mixed(m) = &s0 else {
        panic!("混合模型应产生 Mixed 状态");
    };
    assert_eq!(m.user_prev.as_deref(), Some("今天"));
    let (score, s1) = store.score(&s0, "天气");
    assert!(score.is_finite());
    let LmState::Mixed(m1) = s1 else {
        panic!("打分后应保持 Mixed 状态");
    };
    assert_eq!(m1.user_prev.as_deref(), Some("天气"));

    // 两层都空时分数有限
    let empty = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), UserLM::in_memory()));
    assert!(empty.score(&empty.initial_state(), "任何词").0.is_finite());
}

#[test]
fn mixed_boost_is_graded_by_count() {
    let user = UserLM::in_memory();
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), user));
    let plain = LmStore::Bigram(BigramLM::new());
    let (base, _) = plain.score(&plain.initial_state(), "可以");
    let LmStore::Mixed(m) = &store else {
        panic!("应为混合模型");
    };

    let mut prev = f32::NEG_INFINITY;
    let mut done = 0u32;
    for target in [1u32, 2, 5, 20] {
        while done < target {
            m.user().learn(None, "可以");
            done += 1;
        }
        let (s, _) = store.score(&store.initial_state(), "可以");
        let expect = (1.0 + target as f32 * USER_COUNT_GAIN_R).log10();
        assert!((s - base - expect).abs() < 1e-4, "target={target}: {}", s - base);
        assert!(s > prev);
        prev = s;
    }
}

#[test]
fn mixed_boost_independent_of_corpus_size() {
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), UserLM::in_memory()));
    let LmStore::Mixed(m) = &store else {
        panic!("应为混合模型");
    };
    m.user().learn(None, "可以");
    let (small, _) = store.score(&store.initial_state(), "可以");

    for i in 0..20 {
        m.user().learn(None, &format!("无关噪声词{i}"));
    }
    let (big, _) = store.score(&store.initial_state(), "可以");
    assert_eq!(small, big, "同一计数的增益不得随语料规模变化");
}

#[test]
fn mixed_bigram_evidence_adds_on_top() {
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), UserLM::in_memory()));
    let LmStore::Mixed(m) = &store else {
        panic!("应为混合模型");
    };
    for _ in 0..3 {
        m.user().learn(Some("星冰乐"), "斯巴克");
    }
    let ctx = store.state_from_context(&LmContext {
        tail: "",
        last_word: Some("星冰乐"),
    });
    let (with_prev, _) = store.score(&ctx, "斯巴克");
    let (no_prev, _) = store.score(&store.initial_state(), "斯巴克");
    assert!((with_prev - no_prev - count_gain(3)).abs() < 1e-4);
}

#[test]
fn mixed_no_evidence_falls_back_to_base_exactly() {
    let mut base = BigramLM::new();
    base.add_unigram("今天", 1000);
    base.add_unigram("天气", 500);
    let mut plain = BigramLM::new();
    plain.add_unigram("今天", 1000);
    plain.add_unigram("天气", 500);
    let user = UserLM::in_memory();
    user.learn(None, "斯巴克");
    let mixed = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));
    let plain = LmStore::Bigram(plain);

    for word in ["天气", "没见过的词"] {
        let (m, _) = mixed.score(&mixed.initial_state(), word);
        let (b, _) = plain.score(&plain.initial_state(), word);
        assert_eq!(m, b, "{word} 无个人证据时应与通用层逐位相同");
    }
    let (learned, _) = mixed.score(&mixed.initial_state(), "斯巴克");
    let (b, _) = plain.score(&plain.initial_state(), "斯巴克");
    assert!(learned > b);
}

#[test]
fn mixed_unpaired_unigram_boost_is_damped_under_mismatched_context() {
    let mut base = BigramLM::new();
    base.add_unigram("今天", 100);
    base.add_unigram("斯巴克", 100);
    let user = UserLM::in_memory();
    user.learn(None, "斯巴克");
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));

    let (standalone, _) = store.score(&store.initial_state(), "斯巴克");
    let ctx = store.state_from_context(&LmContext {
        tail: "今天",
        last_word: Some("今天"),
    });
    let (unpaired, _) = store.score(&ctx, "斯巴克");
    assert!(unpaired < standalone, "无二元证据时的一元增益应被折减: {unpaired} vs {standalone}");
}

#[test]
fn score_only_matches_score_bit_for_bit() {
    let words = ["你好", "可以", "天气", "西安", "斯巴克", "龘", "不存在的词"];

    let mut base = BigramLM::new();
    base.add_unigram("你", 100);
    base.add_unigram("好", 50);
    base.add_unigram("可以", 30);
    base.add_bigram("你", "好", 40);
    for state in [base.initial_state(), Some("你".to_string())] {
        for word in words {
            assert_eq!(
                base.score_only(&state, word).to_bits(),
                base.score(&state, word).0.to_bits(),
                "bigram: state={state:?} word={word}"
            );
        }
    }

    let user = UserLM::in_memory();
    for word in ["你好", "可以", "斯巴克"] {
        user.learn(None, word);
    }
    user.learn(Some("你"), "好");
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));
    for state in [
        store.initial_state(),
        store.state_from_context(&LmContext {
            tail: "你",
            last_word: Some("你"),
        }),
    ] {
        for word in words {
            assert_eq!(
                store.score_only(&state, word).to_bits(),
                store.score(&state, word).0.to_bits(),
                "mixed: word={word}"
            );
        }
    }
}
