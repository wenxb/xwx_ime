//! crate 级单测:abbrev。

use super::*;

const ABBREV_QUOTA: usize = 5;

/// 校验同桶内有效层序非降（`Exact` → `Fuzzy` → `Abbrev`），且桶内原始 `Abbrev`
/// 只允许在「被收编的整串条目级简拼」时出现在 `Fuzzy` 之前。
fn assert_bucket_layer_order(inp: &str, cands: &[Candidate], seg: &Segmenter, has_abbrev: bool) {
    let keys: Vec<(usize, SpellingKind)> = cands
        .iter()
        .map(|c| (sort_bucket(inp, c), sort_layer(inp, c, seg, has_abbrev)))
        .collect();
    assert!(
        keys.windows(2).all(|w| w[0] <= w[1]),
        "{inp} (u_min 桶, 有效层序) 应整体非降: {:?}",
        cands
            .iter()
            .map(|c| (
                &c.word,
                c.spelling,
                sort_bucket(inp, c),
                sort_layer(inp, c, seg, has_abbrev),
            ))
            .collect::<Vec<_>>()
    );
    let exc = entry_tier_exception(inp, seg, has_abbrev);
    for w in cands.windows(2) {
        if sort_bucket(inp, &w[0]) != sort_bucket(inp, &w[1]) {
            continue;
        }
        if w[0].spelling == SpellingKind::Abbrev && w[1].spelling == SpellingKind::Fuzzy {
            assert!(
                exc && w[0].covered >= inp.len(),
                "{inp} 桶内 Abbrev→Fuzzy 交错只允许覆盖整串的简拼候选: \
                 {:?}(freq={}, covered={}) 后接 {:?}(Fuzzy)",
                w[0].word,
                w[0].freq,
                w[0].covered,
                w[1].word
            );
        }
    }
}

/// 分层排序：(u_min 桶, 层序) 整体非降，同桶内精确层按分降序、简拼层不越位。
#[test]
fn real_dict_abbrev_layer_never_outranks_exact() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }

    let cands = engine
        .lookup_candidates("beijdx", &LmContext::empty())
        .expect("查询失败");
    assert!(
        cands.iter().any(|c| c.spelling == SpellingKind::Exact),
        "beijdx 应有精确层候选"
    );
    assert!(
        cands.iter().any(|c| c.spelling == SpellingKind::Abbrev),
        "beijdx 应有简拼层候选"
    );
    let pair_keys: Vec<(usize, SpellingKind)> = cands
        .iter()
        .map(|c| (sort_bucket("beijdx", c), c.spelling))
        .collect();
    assert!(
        pair_keys.windows(2).all(|w| w[0] <= w[1]),
        "(u_min 桶, 层序) 应整体非降: {:?}",
        cands
            .iter()
            .map(|c| (&c.word, c.spelling, sort_bucket("beijdx", c)))
            .collect::<Vec<_>>()
    );
    let exact_scores: Vec<f32> = cands
        .iter()
        .filter(|c| c.spelling == SpellingKind::Exact && sort_bucket("beijdx", c) == 3)
        .map(|c| c.score)
        .collect();
    assert!(
        exact_scores.windows(2).all(|w| w[0] >= w[1]),
        "同桶精确层内仍按分降序: {exact_scores:?}"
    );
}

/// `lue`/`nue` 经主切分层归一到 `lve`/`nve`；`lu`/`nu` 不合并到 `lv`/`nv`。
#[test]
fn real_dict_ue_input_reaches_the_v_spelling() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let ctx = LmContext::empty();

    for (input, word) in [("hulue", "忽略"), ("lue", "略"), ("nuedai", "虐待")] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let pos = cands.iter().position(|c| c.word == word).unwrap_or_else(|| {
            panic!(
                "`{input}` 必须能打出「{word}」: 前 5 = {:?}",
                cands.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
            )
        });
        assert_eq!(cands[pos].spelling, SpellingKind::Exact, "`{input}` 应走主切分层");
        assert!(pos < 10, "`{input}` 的「{word}」应在前 10: 第 {} 名", pos + 1);
    }

    let cands = engine.lookup_candidates("nuren", &ctx).expect("查询失败");
    assert!(!cands.iter().any(|c| c.word == "女人"), "`nuren` 不得混进「女人」");
}

/// 一字一字母边界：候选字数不超过输入字母数，且不误杀正常简拼目标词。
#[test]
fn real_dict_candidates_never_exceed_one_character_per_letter() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let ctx = LmContext::empty();

    for input in ["wsm", "zgr", "ky", "zho", "sh", "zh", "xian", "nih", "w"] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let over: Vec<_> = cands
            .iter()
            .filter(|c| c.word.chars().count() > input.len())
            .map(|c| (&c.word, &c.pinyin))
            .collect();
        assert!(over.is_empty(), "`{input}` 出现字数 > 字母数的候选: {over:?}");
    }

    for (input, word) in [
        ("wsm", "为什么"),
        ("zgr", "中国人"),
        ("zgr", "这个人"),
        ("ky", "可以"),
        ("zho", "中"),
        ("xian", "西安"),
    ] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        assert!(cands.iter().any(|c| c.word == word), "`{input}` 应给出「{word}」");
    }
}

/// 末尾半音节补全读法进入第一页前列。
#[test]
fn real_dict_partial_syllable_completions_reach_the_first_page() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let ctx = LmContext::empty();
    for (input, want, max_rank) in [
        ("beijin", "北京", 3),
        ("niha", "你好", 3),
        ("meiyo", "没有", 2),
        ("zhongguor", "中国人", 3),
        ("zhongg", "中国", 3),
    ] {
        let cands = engine.lookup_candidates(input, &ctx).expect("查询失败");
        let rank = cands.iter().position(|c| c.word == want).map(|i| i + 1);
        assert!(
            matches!(rank, Some(r) if r <= max_rank),
            "`{input}` 的「{want}」应在前 {max_rank} 名(实测 {rank:?}): {:?}",
            cands.iter().take(10).map(|c| &c.word).collect::<Vec<_>>()
        );
    }
}

/// 跨来源去重：候选列表中同一 `(word, pinyin)` 只出现一次。
#[test]
fn real_dict_abbrev_no_duplicates_across_sources() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }

    for input in ["zgr", "zhongg", "nihao"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let mut seen = std::collections::HashSet::new();
        for c in &cands {
            assert!(
                seen.insert((c.word.as_str(), c.pinyin.as_str())),
                "{input} 的 (word, pinyin) 不得跨层重复: ({}, {})",
                c.word,
                c.pinyin
            );
        }
    }
}

/// 副索引缺失时优雅退化：全拼照常、纯首字母退化为回声、`lookup_mixed` 仍靠静态表命中。
#[test]
fn engine_without_abbrev_index_degrades_gracefully() {
    let dict_path = data_dir().join("dict.bin");
    if !dict_path.is_file() {
        return;
    }
    let engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    assert!(!engine.dict.has_abbrev(), "未装配副索引");

    let nihao = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert!(nihao.iter().any(|c| c.word == "你好"), "全拼应照常");

    let zgr = engine
        .lookup_candidates("zgr", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(zgr.len(), 1, "无副索引时 zgr 应只剩回声: {zgr:?}");
    assert!(zgr[0].echo && zgr[0].spelling != SpellingKind::Abbrev);

    let DictStore::Mmap(dict) = &engine.dict else {
        panic!("dict.bin 应加载为 Mmap 词库");
    };
    let mixed = dict.lookup_mixed("zhongguor").expect("混合简拼查询失败");
    assert!(
        mixed.iter().any(|c| c.word == "中国人"),
        "无副索引时 lookup_mixed 仍应命中中国人"
    );
}

/// 双字母声母（zh/ch/sh）的整体声母简拼写法命中副索引，且折叠校验拦住声母不符的词。
#[test]
fn real_dict_two_letter_initial_abbreviations_hit_the_side_index() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    if !engine.dict.has_abbrev() {
        return;
    }
    let DictStore::Mmap(dict) = &engine.dict else {
        return;
    };

    for (input, want) in [
        ("shh", "时候"),
        ("zhh", "只会"),
        ("chh", "称号"),
        ("zhzhzhzh", "重中之重"),
        ("ch", "策划"),
    ] {
        let got = dict.lookup_abbrev(input).expect("简拼查询失败");
        assert!(
            got.iter().any(|c| c.word == want),
            "`{input}` 应命中「{want}」: {:?}",
            got.iter().take(5).map(|c| (&c.word, c.freq)).collect::<Vec<_>>()
        );
    }
    for (input, bad) in [("chh", "策划"), ("sh", "所"), ("sh", "四"), ("zh", "在")] {
        let got = dict.lookup_abbrev(input).expect("简拼查询失败");
        assert!(
            !got.iter().any(|c| c.word == bad),
            "`{input}` 不该给出「{bad}」"
        );
    }
}

/// 满页时简拼层保底、混拼目标词排名不回退、可达全拼不产简拼层。
#[test]
fn abbrev_layer_survives_full_page_and_mixed_targets_rank_high() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    let has_abbrev = engine.dict.has_abbrev();
    let seg = Segmenter::new();

    for (input, want) in [("beijdx", "北京大学"), ("zhonggr", "中国人")] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().any(|c| c.word == want), "{input} 应含 {want}");
    }

    for (input, want, rank_ceiling) in [
        ("nih", "你好", 4usize),
        ("renm", "人们", 2usize),
        ("zhongguor", "中国人", 1usize),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        let pos = cands
            .iter()
            .position(|c| c.word == want)
            .unwrap_or_else(|| panic!("{input} 应含 {want}"));
        assert!(
            pos < rank_ceiling,
            "{input} 的 {want} 排第 {} 名，超出上限 {rank_ceiling}",
            pos + 1
        );
        assert_eq!(cands[pos].spelling, SpellingKind::Abbrev);
        assert_bucket_layer_order(input, &cands, &seg, has_abbrev);
    }

    for input in ["zh", "sh"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert_eq!(cands.len(), MAX_CANDIDATES, "{input} 应装满");
        let n_abbrev = cands
            .iter()
            .filter(|c| c.spelling == SpellingKind::Abbrev)
            .count();
        assert!(n_abbrev >= ABBREV_QUOTA, "{input} 简拼层不足保底: {n_abbrev}");
        assert_bucket_layer_order(input, &cands, &seg, has_abbrev);
    }

    let xian = engine
        .lookup_candidates("xian", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(
        xian.iter().filter(|c| c.spelling == SpellingKind::Abbrev).count(),
        0,
        "可达输入 xian 不应出现简拼层"
    );
    assert_bucket_layer_order("xian", &xian, &seg, has_abbrev);
}

/// 末尾单字母尾巴补全到词库真实整串键（`daishix` → 待实现）。
#[test]
fn single_letter_tail_completion_reaches_the_real_key() {
    let Some(engine) = real_full_engine() else {
        return;
    };
    let cands = engine
        .lookup_candidates("daishix", &LmContext::empty())
        .expect("查询失败");
    let top3: Vec<&str> = cands.iter().take(3).map(|c| c.word.as_str()).collect();
    assert!(top3.contains(&"待实现"), "前 3 应含「待实现」，实际: {top3:?}");
}
