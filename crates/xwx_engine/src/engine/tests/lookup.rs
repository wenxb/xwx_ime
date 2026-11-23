//! crate 级单测:lookup（基础查词、兜底、覆盖范围、回声、起始位置与容错边界）。

use super::*;

#[test]
fn lookup_merges_and_ranks() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 10).expect("插入失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("逆", "ni", 50).expect("插入失败");

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.first().map(|c| c.word.as_str()), Some("你"));
    assert!(cands.iter().any(|c| c.word == "逆"));
}

/// 音节未打完时按已切分前缀给候选，且残余声母不当独立音节做前缀联想。
#[test]
fn mid_syllable_keeps_candidates() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("尼", "ni", 50).expect("插入失败");
    engine.dict.upsert("哈", "ha", 40).expect("插入失败");

    let cands = engine
        .lookup_candidates("nih", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "你"));
    assert!(cands.iter().all(|c| c.word != "哈"), "残余 h 不应联想出「哈」");
}

#[test]
fn first_syllable_incomplete_uses_prefix() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("中", "zhong", 100).expect("插入失败");
    engine.dict.upsert("在", "zai", 90).expect("插入失败");

    for partial in ["z", "zhon"] {
        let cands = engine
            .lookup_candidates(partial, &LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().any(|c| c.word == "中"), "{partial} 应联想出「中」");
    }
}

#[test]
fn complete_pinyin_untouched_by_fallback() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你", "ni", 100).expect("插入失败");
    engine.dict.upsert("你好", "ni hao", 200).expect("插入失败");

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.iter().any(|c| c.word == "你"));
    assert!(cands.iter().all(|c| c.pinyin == "ni"));
}

/// 格边候选只覆盖它自己匹配的那段输入，支撑分段上屏。
#[test]
fn candidate_covered_spans_only_the_input_it_matches() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("我", "wo", 500).expect("插入失败");
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");

    let input = "womenjintian";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.iter().find(|c| c.word == "我们").expect("缺我们").covered, 5);
    assert_eq!(cands.iter().find(|c| c.word == "我").expect("缺我").covered, 2);
    assert!(cands.iter().all(|c| c.covered > 0 && c.covered <= input.len()));
}

/// 回声与兜底覆盖范围契约：无解时返回单条回声，正常命中无回声，部分可解时附带整串逃生口。
#[test]
fn fallback_and_echo_candidates_carry_their_own_coverage() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    let echo = engine
        .lookup_candidates("zzzz", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(echo.len(), 1, "无解时应恰好返回 1 条回声");
    assert_eq!((echo[0].word.as_str(), echo[0].pinyin.as_str(), echo[0].echo, echo[0].freq, echo[0].covered), ("zzzz", "zzzz", true, 0, 4));

    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    let resolved = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(!resolved.is_empty() && resolved.iter().all(|c| !c.echo), "正常命中不应混入回声");

    let cands = engine
        .lookup_candidates("nihaozzzzz", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.iter().find(|c| c.word == "你好").expect("缺你好").covered, 5);
    assert_eq!(cands.iter().find(|c| c.word == "你").expect("缺你").covered, 2);
    assert!(cands.iter().filter(|c| !c.echo).all(|c| c.covered < "nihaozzzzz".len()));
    let escape = cands.iter().find(|c| c.echo).expect("应有原样上屏逃生口");
    assert_eq!((escape.word.as_str(), escape.covered), ("nihaozzzzz", "nihaozzzzz".len()));
}

/// 覆盖停在分隔符 `'` 前的候选一并消费该分隔符，避免残留前导 `'`。
#[test]
fn covered_swallows_the_apostrophe_separator() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    let input = "xi'anha";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    let xian = cands.iter().find(|c| c.word == "西安").expect("xi'anha 应含「西安」");
    assert_eq!(xian.covered, 5, "xian(4) + `'`(1) = 5");
    assert!(xian.covered < input.len());
}

/// 真实词库长串：首位覆盖整串，同时保留只覆盖前缀的分段候选。
#[test]
fn real_dict_long_input_candidates_cover_only_their_own_span() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    let input = "womenjintianqugongyuan";
    let cands = engine
        .lookup_candidates(input, &LmContext::empty())
        .expect("查询失败");
    assert!(!cands.is_empty());
    assert_eq!(cands[0].covered, input.len(), "首位应覆盖整串");
    assert!(cands.iter().any(|c| c.covered < input.len()), "应保留前缀候选");
}

/// 候选全部起始于位置 0，且整串短语覆盖优先于词频更高的前缀单字。
#[test]
fn candidates_all_start_at_position_zero() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 100).expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    engine.dict.upsert("号", "hao", 120).expect("插入失败");
    engine.dict.upsert("毫", "hao", 80).expect("插入失败");

    let cands = engine
        .lookup_candidates("nihao", &LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands[0].word, "你好", "覆盖整串的短语应排首位");
    let pos_ni = cands.iter().position(|c| c.word == "你").expect("应含单字「你」");
    assert!(cands[0].score > cands[pos_ni].score, "整串分应高于前缀单字");
    for w in ["好", "号", "毫"] {
        assert!(!cands.iter().any(|c| c.word == w), "不应召回后段音节「{w}」");
    }
    assert!(cands.iter().all(|c| c.pinyin == "ni" || c.pinyin.starts_with("ni ")));
}

/// 兜底路径同样满足「精确候选起始于位置 0」。
#[test]
fn fallback_candidates_all_start_at_position_zero() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    let expected: [(&str, &[&str]); 2] = [
        ("zhongguor", &["zhong"]),
        ("qiax", &["qi", "qia"]),
    ];
    for (input, want_firsts) in expected {
        let k = (1..=input.len())
            .rev()
            .find(|&k| !engine.segmenter.segment(&input[..k]).is_empty())
            .expect("应有可切前缀");
        let allowed_firsts: std::collections::HashSet<String> = engine
            .segmenter
            .segment(&input[..k])
            .iter()
            .filter_map(|seg| seg.first().cloned())
            .collect();
        let want: std::collections::HashSet<String> =
            want_firsts.iter().map(|s| s.to_string()).collect();
        assert_eq!(allowed_firsts, want);

        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty());
        for c in &cands {
            if c.echo || c.spelling == SpellingKind::Abbrev || c.spelling == SpellingKind::Fuzzy {
                continue;
            }
            let first_syl = c.pinyin.split(' ').next().expect("候选拼音非空");
            assert!(
                allowed_firsts.contains(first_syl),
                "{input} 精确候选「{}」({}) 首音节非法",
                c.word,
                c.pinyin
            );
        }
    }
}

/// 候选数不受 `BEAM_WIDTH` 钳死（来源是 `lattice[0]` 全量）。
#[test]
fn candidate_count_not_clamped_by_beam_width() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for i in 0..20u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine.dict.upsert(&w, "ni", 1000 - i as u64).expect("插入失败");
    }

    let cands = engine
        .lookup_candidates("ni", &LmContext::empty())
        .expect("查询失败");
    assert!(cands.len() > BEAM_WIDTH);
    assert_eq!(cands.len(), 20);
}

/// 单音节一处打错（调换/多打/叠字）仍可容错召回目标词。
#[test]
fn typo_pinyin_still_yields_the_intended_word() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("你好", "ni hao", 900).expect("插入失败");
    engine.dict.upsert("你", "ni", 500).expect("插入失败");
    engine.dict.upsert("什么", "shen me", 800).expect("插入失败");
    engine.dict.upsert("深", "shen", 400).expect("插入失败");

    for (input, want) in [
        ("nihoa", "你好"),
        ("niqhao", "你好"),
        ("qnihao", "你好"),
        ("shenem", "什么"),
        ("nihaoo", "你好"),
    ] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(cands.iter().any(|c| c.word == want), "{input} 应容错召回「{want}」");
    }
}

/// 打对的输入不触发容错层（默认关模糊音下不出现 `Fuzzy` 候选）。
#[test]
fn tolerant_pass_leaves_exactly_typed_inputs_alone() {
    let Some(engine) = real_dict_engine() else {
        return;
    };
    for input in ["nihao", "womenjintianqugongyuan", "zhongguor", "wsm", "zgr", "nih"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("查询失败");
        assert!(!cands.is_empty(), "{input} 应有候选");
        assert!(
            cands.iter().all(|c| c.spelling != SpellingKind::Fuzzy),
            "{input} 不应出现容错/模糊层候选"
        );
    }
}

/// 非 ASCII 输入返回原文回声而不 panic；空串返回空。
#[test]
fn non_ascii_input_returns_an_echo_instead_of_panicking() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for input in ["你", "你好", "🎉", "a中b", "ni你hao", "\u{0301}"] {
        let cands = engine
            .lookup_candidates(input, &LmContext::empty())
            .expect("非 ASCII 不应报错");
        assert!(cands.iter().any(|c| c.echo && c.word == input), "{input:?} 应回回声");
    }
    assert!(engine.lookup_candidates("", &LmContext::empty()).unwrap().is_empty());
    assert!(!engine.lookup_candidates("nihao", &LmContext::empty()).unwrap().is_empty());
}
