//! `'` 音节分隔符（强制切分点）的消歧、编辑与原文保真单测。
//!
//! 核心判据：分隔符不得落在音节内部；兜底路径同样遵守分隔符；回声必须来自原文。

use super::*;

#[test]
fn apostrophe_disambiguates_xian() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 50).expect("插入失败");
    let cands = engine
        .lookup_candidates("xi'an", &crate::LmContext::empty())
        .expect("查询失败");
    let c = cands
        .iter()
        .find(|c| c.word == "西安")
        .expect("xi'an 应消歧出「西安」");
    assert_eq!(c.pinyin, "xi an", "词典 key 不含 `'`");
    assert!(
        !cands.iter().any(|c| c.word == "先" && c.pinyin == "xian"),
        "xi'an 不应召回跨分隔符的单音节「先」"
    );
}

#[test]
fn apostrophe_entry_caret_and_backspace() {
    // 空闲时 `'` 透传；组合中 `'` 进入拼音串。
    let mut s = session_with_dict();
    assert!(
        matches!(s.handle(InputEvent::Char('\'')).expect("不应失败"), SessionOutput::Noop),
        "空闲时 ' 应透传"
    );
    type_str(&mut s, "xi");
    match s.handle(InputEvent::Char('\'')).expect("不应失败") {
        SessionOutput::Updated(u) => assert_eq!(u.preedit, "xi'"),
        other => panic!("拼音串非空时 ' 应进串，实际 {other:?}"),
    }

    // caret 可跨过 `'`，Backspace 可删掉 `'` 前字符或 `'` 本身。
    type_str(&mut s, "an");
    assert_eq!((s.pinyin(), s.caret()), ("xi'an", 5));
    for _ in 0..3 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 2);
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "x'an");

    let mut s = session_with_dict();
    type_str(&mut s, "x'a");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 2);
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "xa", "Backspace 应能删掉 `'`");
}

#[test]
fn apostrophe_edge_cases_do_not_panic() {
    for input in ["''", "an'", "'a", "a''n", "'''"] {
        let mut s = session_with_dict();
        type_str(&mut s, input);
    }
}

#[test]
fn fallback_never_crosses_separator_and_keeps_tail_editable() {
    let engine = Arc::new(ImeEngine::new(None).expect("内存引擎创建失败"));
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 900).expect("插入失败");

    // 兜底路径不跨分隔符，且覆盖范围按原文字节计（`xi'an` = 5）。
    let cands = engine
        .lookup_candidates("xi'anq", &crate::LmContext::empty())
        .expect("查询失败");
    assert!(
        !cands.iter().any(|c| c.pinyin == "xian"),
        "兜底不得召回跨分隔符的「先」"
    );
    let xian = cands.iter().find(|c| c.word == "西安").expect("应有「西安」");
    assert_eq!(xian.covered, 5, "覆盖范围应按原文字节计算");

    // 空格确认边界内读法「西安」，尾巴 `q` 留在组合里。
    let mut s = InputSession::new(engine);
    type_str(&mut s, "xi'anq");
    match s.handle(InputEvent::Space).expect("不应失败") {
        SessionOutput::Updated(u) => assert_eq!(u.preedit, "西安q"),
        other => panic!("只覆盖前缀时应返回 Updated，实际 {other:?}"),
    }
}

#[test]
fn every_pinyin_candidate_respects_the_separator() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("西安", "xi an", 100).expect("插入失败");
    engine.dict.upsert("先", "xian", 900).expect("插入失败");
    engine.dict.upsert("岸", "an", 80).expect("插入失败");
    engine.dict.upsert("洗", "xi", 70).expect("插入失败");

    for input in ["xi'an", "xi'anq", "'xian", "xian'", "xi''an", "xi'a"] {
        let marks: Vec<usize> = input
            .char_indices()
            .filter(|(_, c)| *c == '\'')
            .map(|(i, _)| input[..i].chars().filter(|c| *c != '\'').count())
            .collect();
        let cands = engine
            .lookup_candidates(input, &crate::LmContext::empty())
            .unwrap_or_else(|e| panic!("{input} 查询失败: {e}"));
        for c in cands.iter().filter(|c| !c.echo) {
            assert!(
                !c.pinyin.contains('\''),
                "Candidate.pinyin 不应含 `'`: {:?}",
                c.pinyin
            );
            let mut pos = 0usize;
            let mut edges = vec![0usize];
            for syl in c.pinyin.split(' ') {
                pos += syl.len();
                edges.push(pos);
            }
            for m in &marks {
                if *m < pos {
                    assert!(
                        edges.contains(m),
                        "{input} 的候选 {} (key {:?}) 穿过了位置 {m} 的分隔符; 边界 {edges:?}",
                        c.word,
                        c.pinyin
                    );
                }
            }
        }
    }
}

#[test]
fn echo_and_enter_keep_the_raw_separator_input() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    let cands = engine
        .lookup_candidates("zz'zz", &crate::LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands.len(), 1);
    assert!(cands[0].echo && cands[0].word == "zz'zz");

    for ev in [InputEvent::Space, InputEvent::Enter] {
        let mut s = InputSession::new(Arc::new(ImeEngine::new(None).expect("内存引擎创建失败")));
        type_str(&mut s, "zz'zz");
        let out = s.handle(ev).expect("不应失败");
        assert!(
            matches!(&out, SessionOutput::Commit(t) if t == "zz'zz"),
            "{ev:?} 应上屏原文 zz'zz: {out:?}"
        );
    }
}
