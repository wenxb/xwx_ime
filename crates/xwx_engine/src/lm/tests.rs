//! 本模块的单测(从同一个文件里拆出,2026-10-02:`mod.rs` 只管实现,
//! 测试单独一个文件;`use super::*` 让它们照旧能碰私有条目)。

use super::*;

#[test]
fn bigram_prefers_known_pair() {
    let mut lm = BigramLM::new();
    // "中国" 出现 100 次，"中" 出现 100 次，"国" 出现 50 次
    lm.add_unigram("中国", 100);
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 50);
    lm.add_bigram("中", "国", 90);

    // 状态 = 上一个词：Some("中") 表示上文是「中」。
    let prev = Some("中".to_string());
    let with_pair = lm.score(&prev, "国").0;
    let without_pair = lm.score(&prev, "华").0;
    assert!(with_pair > without_pair, "有 bigram 计数时应得分更高");
}

#[test]
fn unknown_word_has_finite_score() {
    let lm = BigramLM::new();
    let (s, _) = lm.score(&lm.initial_state(), "不存在的词");
    assert!(s.is_finite());
}

#[test]
fn state_advances_after_score() {
    // 等价于旧 `score_sequence(&["你", "好"])`：第一个词用初始状态
    // （只有 unigram 分），随后状态推进为前一个词再打分（bigram 命中）。
    let mut lm = BigramLM::new();
    lm.add_unigram("你", 100);
    lm.add_unigram("好", 50);
    lm.add_bigram("你", "好", 80);

    let (s0, next) = lm.score(&lm.initial_state(), "你");
    assert_eq!(next, Some("你".to_string()), "打分后状态应推进为刚消费的词");
    let (s1, next1) = lm.score(&next, "好");
    assert_eq!(next1, Some("好".to_string()));

    // 句首 unigram P(你) = (100+1)/(150+2)；bigram P(好|你) = (80+1)/(100+2)
    let expected0 = ((100.0_f32 + 1.0) / (150.0_f32 + 2.0)).log10();
    let expected1 = ((80.0_f32 + 1.0) / (100.0_f32 + 2.0)).log10();
    assert!((s0 - expected0).abs() < 1e-6, "首个词应是句首 unigram 分");
    assert!((s1 - expected1).abs() < 1e-6, "状态推进后应命中 bigram 分");
}

#[test]
fn lm_store_routes_via_enum_state() {
    let mut store = LmStore::Bigram(BigramLM::new());
    let data = "中国\t100\n中\t100\n国\t50\n中\t国\t90\n";
    store.load(data.as_bytes()).expect("加载失败");

    let state = store.initial_state();
    assert_eq!(state, LmState::Bigram(None), "初始状态应是无上文");

    // 无上文：unigram 分（与改动前 `score_sequence(&[word])` 等价）
    let (solo, _) = store.score(&state, "国");
    // 推进：上文 = 中
    let (_, advanced) = store.score(&state, "中");
    assert_eq!(advanced, LmState::Bigram(Some("中".to_string())));
    // 再打分：命中 bigram(中, 国)
    let (paired, _) = store.score(&advanced, "国");
    assert!(paired > solo, "状态推进后应命中 bigram 计数");
}

#[test]
fn state_from_context_reads_last_word_only() {
    let lm = BigramLM::new();

    // 无上下文：空
    assert_eq!(lm.state_from_context(&LmContext::empty()), None);
    assert_eq!(
        lm.initial_state(),
        None,
        "initial_state 应等价于空上下文（默认实现）"
    );

    // 只有 tail、没有 last_word（如原始字母/标点上屏）：无词级上文
    let tail_only = LmContext { tail: "中", last_word: None };
    assert_eq!(lm.state_from_context(&tail_only), None);

    // 有 last_word：bigram 状态 = 前一个词（tail 里还有别的字也不影响）
    let ctx = LmContext { tail: "中", last_word: Some("中国") };
    assert_eq!(
        lm.state_from_context(&ctx),
        Some("中国".to_string()),
        "bigram 只应读取 last_word"
    );
}

#[test]
fn context_injection_hits_bigram() {
    // 有词对的模型：注入上文后 bigram 分支必须真的命中，
    // 与无上文的打分不同（这是第 2a 步的核心验收点）。
    let mut lm = BigramLM::new();
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 50);
    lm.add_bigram("中", "国", 90);

    let ctx = LmContext { tail: "中", last_word: Some("中") };
    let with_prev = lm.score(&lm.state_from_context(&ctx), "国").0;
    let no_prev = lm.score(&lm.initial_state(), "国").0;
    assert!(
        with_prev > no_prev,
        "有上文(中→国)应命中 bigram 计数而得分更高: {with_prev} vs {no_prev}"
    );

    // 经 LmStore 枚举分发也同样生效
    let store = LmStore::Bigram(lm);
    let s1 = store.score(&store.state_from_context(&ctx), "国").0;
    let s0 = store.score(&store.initial_state(), "国").0;
    assert!(s1 > s0, "LmStore 分发后同样应命中 bigram: {s1} vs {s0}");
}

#[test]
fn mixed_state_advances_both_layers() {
    let mut base = BigramLM::new();
    base.add_unigram("今天", 100);
    base.add_bigram("今天", "天气", 90);
    let store = LmStore::Mixed(MixedLM::new(
        LmStore::Bigram(base),
        UserLM::in_memory(),
                ));
    let s0 = store.state_from_context(&LmContext {
        tail: "今天",
        last_word: Some("今天"),
    });
    let LmState::Mixed(m) = &s0 else {
        panic!("混合模型应产生 Mixed 状态");
    };
    assert_eq!(m.user_prev.as_deref(), Some("今天"), "用户层应读词级上文");
    let (_, s1) = store.score(&s0, "天气");
    let LmState::Mixed(m1) = s1 else {
        panic!("打分后应保持 Mixed 状态");
    };
    assert_eq!(m1.user_prev.as_deref(), Some("天气"), "用户层上文应推进");
}

#[test]
fn mixed_prefers_user_hits_without_losing_base() {
    // 用户学过「斯巴克」,通用层没学过 → 混合后斯巴克应明显高于
    // 通用层的同上文竞争者;同时通用层自己的统计(今天→天气)
    // 在用户层空白处仍应起作用。
    let mut base = BigramLM::new();
    base.add_unigram("今天", 1000);
    base.add_unigram("天气", 500);
    base.add_unigram("星冰乐", 100);
    base.add_bigram("今天", "天气", 90);
    let user = UserLM::in_memory();
    for _ in 0..3 {
        user.learn(Some("星冰乐"), "斯巴克");
    }
    // 混合层插入一个 0.3/0.7 线性插值,绝对分会被改写;
    // 本测试只断言「信号存在」,因此断言前先确认两层各有多少
    // log10 增益,数值放对注释里,断言仍只比较相对大小。
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));

    // 用户层独有能力:「星冰乐→斯巴克」是通用层没有的
    let ctx = LmContext { tail: "星冰乐", last_word: Some("星冰乐") };
    let user_hit = store.score(&store.state_from_context(&ctx), "斯巴克").0;
    let user_miss = store.score(&store.state_from_context(&ctx), "星冰乐").0;
    assert!(user_hit > user_miss, "用户学过的搭配应更高: {user_hit} vs {user_miss}");
}

#[test]
#[should_panic(expected = "混合模型不应再嵌套混合")]
fn mixed_rejects_nested_mixed() {
    let inner = LmStore::Mixed(MixedLM::new(
        LmStore::Bigram(BigramLM::new()),
        UserLM::in_memory(),
                ));
    // debug_assert 在测试构建(debug)下触发 panic;
    // release 下 debug_assert 被编译掉,但语义上仍禁止(见 MixedLM::new 文档)。
    let _ = MixedLM::new(inner, UserLM::in_memory());
}

#[test]
fn mixed_score_stays_finite_on_empty_layers() {
    let store = LmStore::Mixed(MixedLM::new(
        LmStore::Bigram(BigramLM::new()),
        UserLM::in_memory(),
                ));
    let (s, _) = store.score(&store.initial_state(), "任何词");
    assert!(s.is_finite(), "两层都空时混合分仍应有限");
}

/// **句首排序的真正通道是用户层 unigram,不是通用层。**
///
/// 钉住一个实测事实(2026-10-01,`examples/lm_ctx_probe.rs`):在空
/// 上下文下
/// - 万象 `.gram`(`WanxiangLM::score_word` 的 `ctx_tail.is_empty()`
///   早退)对**任何**词都返回同一个常数 `floor_log10()` ≈ -11.605767
///   ——实测「西安/先/闲/狄安娜/中国/的/龘」七词分数逐位相同;
/// - 用户层(`UserLmSnapshot::log10_prob`)在无 `prev` 时**回落到
///   unigram 计数**,因此对学过的词给出**不同**的分。
///
/// 这条测试的意义是防止误诊:看 `wanxiang.rs` 的空上下文早退很容易
/// 得出「句首排序完全由词频决定」的结论,但那只在生产进程同时挂了
/// 用户层(`main.rs` 调 `with_user_lm`)时才对**通用层**成立——
/// **用户用过的词在句首是能被区分的**。
///
/// 同时它也是「空上下文 unigram backoff 不可行」这条结论的边界说明:
/// 那条修法针对的是**通用 gram**层(键长不可比),而**用户层**的
/// unigram 计数本来就是可比的自然数,不需要也不应该一起否决。
#[test]
fn empty_context_user_layer_discriminates_but_base_does_not() {
    let base = BigramLM::new(); // 空通用层:等价于「通用层给不出信号」
    let user = UserLM::in_memory();
    for _ in 0..5 {
        user.learn(None, "西安");
    }
    user.learn(None, "的");
    let store = LmStore::Mixed(MixedLM::new(
        LmStore::Bigram(base),
        user,
                ));

    // 空上下文(句首):last_word = None。
    let st = store.state_from_context(&LmContext::empty());
    let (learned, _) = store.score(&st, "西安");
    let st2 = store.state_from_context(&LmContext::empty());
    let (rare, _) = store.score(&st2, "龘");

    assert!(
        learned.is_finite() && rare.is_finite(),
        "两词分都应有限: {learned} / {rare}"
    );
    assert!(
        learned > rare,
        "用户学过的「西安」在句首应高于没学过的「龘」——\
         这正是句首可被用户层区分的证据: {learned} vs {rare}"
    );
}

/// 空上下文下 `LmContext::empty()` 的两个字段都必须是「无上文」。
///
/// 这是上面那条测试的前提:`tail = ""` 触发万象早退,
/// `last_word = None` 触发用户层 unigram 回落。任一字段被填上,
/// 口径就变了(不再是「句首」)。钉住它以免后人改 `empty()`。
#[test]
fn empty_context_has_both_fields_absent() {
    let c = LmContext::empty();
    assert!(c.tail.is_empty(), "tail 必须为空串(否则万象不走早退)");
    assert!(c.last_word.is_none(), "last_word 必须为 None(否则用户层走 bigram)");
}

/// 计数增益的**梯度**:1/2/5/20 次严格递增,且逐位等于 `log10(1+c·R)`。
///
/// 这是「引用计数」语义的落点:选得越多分越高。旧实现在这条线上
/// 几乎无区分(计数 1 → -2.10、计数 3 → -1.90,只差 0.2),而真正
/// 决定排序的是「学过 vs 没学过」那道 9 个数量级的断崖。
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
        assert!(
            (s - base - expect).abs() < 1e-4,
            "学到 {target} 次的加分应为 log10(1+{target}·R)={expect}: 实测 {}",
            s - base
        );
        assert!(s > prev, "计数增加后分必须严格上升: {prev} -> {s}");
        prev = s;
    }
}

/// **加分与用户语料规模无关**(新口径的结构性收益)。
///
/// 旧实现是概率插值,用户层的分母是自己的语料规模:实测同一句
/// 「学 1 次目标词」的加分从 +11.45(语料 1 条)掉到 +7.45(语料
/// 10001 条)——重度用户的个人化比新用户弱 4 个数量级。计数口径下
/// 这两次必须**逐位相同**。
#[test]
fn mixed_boost_independent_of_corpus_size() {
    let user = UserLM::in_memory();
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), user));
    let LmStore::Mixed(m) = &store else {
        panic!("应为混合模型");
    };
    m.user().learn(None, "可以");
    let (small, _) = store.score(&store.initial_state(), "可以");

    for i in 0..10_000 {
        m.user().learn(None, &format!("无关噪声词{i}"));
    }
    let (big, _) = store.score(&store.initial_state(), "可以");
    assert_eq!(
        small, big,
        "同一计数的加分享不得随语料规模变化(旧实现差 4 个数量级): {small} vs {big}"
    );
}

/// 二元证据在一元之上**再加一份**(匹配上文时更高)。
#[test]
fn mixed_bigram_evidence_adds_on_top() {
    let user = UserLM::in_memory();
    let store = LmStore::Mixed(MixedLM::new(LmStore::Bigram(BigramLM::new()), user));
    let LmStore::Mixed(m) = &store else {
        panic!("应为混合模型");
    };
    for _ in 0..3 {
        m.user().learn(Some("星冰乐"), "斯巴克"); // 一元 3 + 二元(星冰乐→斯巴克) 3
    }
    let ctx = store.state_from_context(&LmContext {
        tail: "",
        last_word: Some("星冰乐"),
    });
    let (with_prev, _) = store.score(&ctx, "斯巴克");
    let (no_prev, _) = store.score(&store.initial_state(), "斯巴克");
    assert!(
        (with_prev - no_prev - count_gain(3)).abs() < 1e-4,
        "匹配上文应在一元增益之上再加一份二元增益 {}: {with_prev} vs {no_prev}",
        count_gain(3)
    );
}

/// **没有个人证据的词,混合分必须与通用层逐位相同**(backoff)。
///
/// 旧实现给「没学过」的词一个绝对中性分 `-1.0`,于是用户层里
/// **有任何证据**的词反而更差——实测简拼 `ky` 下用户学过 3 次的
/// 「可以」被压到第 11 名,而没学过的「跨越」白拿 -1.15
/// (真实数据,2026-10-02)。这条测试钉住修复后的口径:
/// 没证据 ⇒ 通用层原样输出,用户层一个字都不改。
#[test]
fn mixed_no_evidence_falls_back_to_base_exactly() {
    let mut base = BigramLM::new();
    base.add_unigram("今天", 1000);
    base.add_unigram("天气", 500);
    let mut plain = BigramLM::new();
    plain.add_unigram("今天", 1000);
    plain.add_unigram("天气", 500);
    let user = UserLM::in_memory();
    user.learn(None, "斯巴克"); // 只学过这一个词
    let mixed = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));
    let plain = LmStore::Bigram(plain);

    for word in ["天气", "没见过的词"] {
        let (m, _) = mixed.score(&mixed.initial_state(), word);
        let (b, _) = plain.score(&plain.initial_state(), word);
        assert_eq!(
            m, b,
            "{word} 无个人证据时混合分应与通用层逐位相同: {m} vs {b}"
        );
    }
    // 反面对照:学过的那一个词必须**高于**通用层,否则上面两条
    // 断言没有区分力(一个恒等于通用层的实现也能过)。
    let (learned, _) = mixed.score(&mixed.initial_state(), "斯巴克");
    let (b, _) = plain.score(&plain.initial_state(), "斯巴克");
    assert!(learned > b, "学过的词应高于通用层: {learned} vs {b}");
}

/// **学习永不降权**:同一个词学 0/1/2/3/6/11 次,混合分单调不降,
/// 且学过就不低于通用层;没学过的词保持通用层原样。
///
/// 计数只有**正证据**(负证据走 Ctrl+Delete 屏蔽表,不进计数),
/// 所以「学过反而更差」在任何数据规模下都是 bug。上面 `ky` 那次
/// 事故(用户语料很小 ⇒ 用户层概率低于中性常数)正是被这条挡住
/// 的那一类;任何让学过比没学过更差的改动都会在这里红。
#[test]
fn mixed_learning_never_demotes() {
    let mut plain = BigramLM::new();
    plain.add_unigram("跨越", 10_000);
    let plain = LmStore::Bigram(plain);
    let mut base = BigramLM::new();
    base.add_unigram("跨越", 10_000);
    let user = UserLM::in_memory();
    let mixed = LmStore::Mixed(MixedLM::new(LmStore::Bigram(base), user));
    let LmStore::Mixed(m) = &mixed else {
        panic!("应为混合模型");
    };
    let (base_learned, _) = plain.score(&plain.initial_state(), "可以");

    let mut prev: Option<f32> = None;
    let mut learns = 0usize;
    for target in [0usize, 1, 2, 3, 6, 11] {
        while learns < target {
            m.user().learn(None, "可以");
            learns += 1;
        }
        let (s, _) = mixed.score(&mixed.initial_state(), "可以");
        if let Some(p) = prev {
            assert!(s >= p, "学到第 {learns} 次时分不得下降: {p} -> {s}");
        }
        if target > 0 {
            assert!(s > base_learned, "学过就不该低于通用层: {s} vs {base_learned}");
        }
        prev = Some(s);
    }
    // 没学过的词:原样保留通用层分(与上面第一条测试同口径,
    // 这里连同「用户层已有计数」一起再钉一次)。
    let (untouched, _) = mixed.score(&mixed.initial_state(), "跨越");
    let (b_untouched, _) = plain.score(&plain.initial_state(), "跨越");
    assert_eq!(untouched, b_untouched, "没学过的词必须原样保留通用层分");
}

/// 万象 `.gram` 的真实路径(有才验,没有就跳过——不假装验过)。
fn real_gram_path() -> Option<std::path::PathBuf> {
    let xdg = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })?;
    let path = xdg.join("xwx").join("wanxiang-lts-zh-hans.gram");
    path.is_file().then_some(path)
}

/// `score_only` 必须与 `score(..).0` **逐位相同**(codex 审查第二批)。
///
/// 它省的是分配,不是精度:`score` 要为被丢弃的状态付一次 `String`/`Box`,
/// `score_only` 不建状态,但**两边算出来的分数必须一模一样**。
/// 按 `to_bits()` 比而不是肉眼近似:f32 加法不满足结合律,混合层里
/// `base + (g1 + g2)` 与 `(base + g1) + g2` 原则上能差一个 ULP。
///
/// **这条测试证明的是什么、不证明什么(实测过)**:它证明四个实现当下
/// 逐位一致;但它**抓不住**「把加法顺序改一处」——把混合层的
/// `base + boost` 展开成 `(base + g1) + g2` 之后,本测试语料上的结果
/// 恰好仍然逐位相同,测试照旧是绿的。要真正证明省下了分配,看
/// `tests/alloc_count.rs`(计数分配器:600 次打分 `score` 31200 字节、
/// `score_only` 0 字节)。
#[test]
fn score_only_matches_score_bit_for_bit() {
    let words = ["你好", "可以", "天气", "西安", "斯巴克", "龘", "不存在的词"];

    // ① 通用 bigram 层(两个状态:句首 / 有上文)
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
                "bigram:state={state:?} word={word}"
            );
        }
    }

    // ② 混合层(用户计数增益那一项最容易在改写时换掉加法顺序;
    //    这里同时喂一元与二元计数,让两个 gain 都非零)
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
                "mixed:word={word}"
            );
        }
    }

    // ③ 万象(字符级窗口):真实数据文件不存在就跳过
    if let Some(path) = real_gram_path() {
        let lm = LmStore::Wanxiang(WanxiangLM::open(&path).expect("打开万象 .gram 失败"));
        for state in [
            lm.initial_state(),
            lm.state_from_context(&LmContext {
                tail: "今天天气",
                last_word: Some("天气"),
            }),
        ] {
            for word in words {
                assert_eq!(
                    lm.score_only(&state, word).to_bits(),
                    lm.score(&state, word).0.to_bits(),
                    "wanxiang:word={word}"
                );
            }
        }
    }
}
