//! `session` 标点映射接线、数字后句点例外与大写 `Verbatim` 组合单测。

use super::*;

#[test]
fn punct_commits_fullwidth_when_idle_and_wiring() {
    let mut s = session_with_dict();
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"));
    assert!(!s.is_active());
    assert_eq!(s.last_word, None, "标点不是词");
    assert_eq!(s.committed_tail, "\u{FF0C}");

    // 代表性会话接线：单字全角、多字映射、`/` 半角例外、`\` 顿号、括引号（全表 22 项见 punctuator.rs）。
    for (input, expected) in [
        ('.', "\u{3002}"),
        ('\\', "\u{3001}"),
        ('/', "/"),
        ('_', "\u{2014}\u{2014}"),
        ('^', "\u{2026}\u{2026}"),
        ('"', "\u{201C}"),
        ('[', "\u{300C}"),
    ] {
        let mut s = session_with_dict();
        let out = s.handle(InputEvent::Char(input)).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == expected),
            "标点 {input:?} 应上屏 {expected:?}，实际 {out:?}"
        );
    }
}

#[test]
fn punct_commits_highlighted_candidate_and_slash_keeps_ascii() {
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你好\u{FF0C}"));
    assert!(!s.is_active());
    assert_eq!(s.last_word.as_deref(), Some("你好"));
    assert_eq!(s.committed_tail, "你好\u{FF0C}");

    // 组合中 `/` 先提交高亮候选再跟半角斜杠。
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    let out = s.handle(InputEvent::Char('/')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你好/"));
}

#[test]
fn punct_with_no_real_candidate_commits_raw_then_fullwidth() {
    let mut s = InputSession::new(Arc::new(ImeEngine::new(None).expect("内存引擎创建失败")));
    type_str(&mut s, "zgr");
    let out = s.handle(InputEvent::Char(';')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "zgr\u{FF1B}"));
    assert!(!s.is_active());
    assert_eq!(s.last_word, None);
}

#[test]
fn punct_passthrough_in_english_mode() {
    let mut s = session_with_dict();
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode());
    for c in [',', '.', ';', '$', '~'] {
        assert!(matches!(
            s.handle(InputEvent::Char(c)).expect("不应失败"),
            SessionOutput::Noop
        ));
    }
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "\u{FF0C}"));
}

#[test]
fn unmapped_ascii_symbols_still_passthrough() {
    for c in ['+', '#', '%', '&', '*', '|'] {
        assert!(fullwidth_punct(c).is_none());
        let mut s = session_with_dict();
        assert!(matches!(s.handle(InputEvent::Char(c)).expect("不应失败"), SessionOutput::Noop));
        assert!(!s.is_active());

        let mut s = session_with_dict();
        type_str(&mut s, "ni");
        assert!(matches!(s.handle(InputEvent::Char(c)).expect("不应失败"), SessionOutput::Noop));
        assert_eq!(s.pinyin(), "ni");
    }
}

#[test]
fn verbatim_composes_capitals_and_words_without_learning() {
    // 空闲大写进组合、不学习、空格原样上屏。
    let (engine, mut s) = session_for_coin(&[]);
    let user = engine.user_lm().expect("应有用户层");
    let before = user.unigram_len();
    match s.handle(InputEvent::Verbatim('G')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "G");
            assert!(u.candidates.iter().any(|c| c.word == "G"));
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert!(s.is_active());
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "G"
    ));
    assert!(!s.is_active() && s.last_word.is_none() && s.committed_tail == "G");
    assert_eq!(user.unigram_len(), before, "大写组合不应产生学习");

    // 连续大写（GPU）与大写起头英文词（Hello）留在同一组合，空格一次上屏。
    let mut s = session_with_dict();
    for (ev, expect) in [
        (InputEvent::Verbatim('G'), "G"),
        (InputEvent::Verbatim('P'), "GP"),
        (InputEvent::Verbatim('U'), "GPU"),
    ] {
        let out = s.handle(ev).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.preedit == expect));
    }
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "GPU"
    ));

    let mut s = session_with_dict();
    assert!(matches!(
        s.handle(InputEvent::Verbatim('H')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "H"
    ));
    for (c, expect) in [('e', "He"), ('l', "Hel"), ('l', "Hell"), ('o', "Hello")] {
        let out = s.handle(InputEvent::Char(c)).expect("不应失败");
        assert!(matches!(out, SessionOutput::Updated(ref u) if u.preedit == expect));
    }
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "Hello"
    ));

    // 无真实候选（空词库）：大写同样进组合并原样上屏。
    let mut s = InputSession::new(Arc::new(ImeEngine::new(None).expect("内存引擎创建失败")));
    type_str(&mut s, "zgr");
    assert!(matches!(
        s.handle(InputEvent::Verbatim('R')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "zgrR"
    ));
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "zgrR"
    ));
    assert_eq!(s.last_word, None);
}

#[test]
fn verbatim_mid_composition_space_commits_candidate_and_enter_keeps_raw() {
    // 组合中途大写保留拼音前缀候选，空格上屏「候选 + 原样尾巴」。
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    match s.handle(InputEvent::Verbatim('G')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihaoG");
            assert!(u.candidates.iter().any(|c| c.word == "你好"));
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "你好G"
    ));
    assert!(!s.is_active());

    // 未确认的高亮候选不替用户提交：回车原样上屏全部已敲字母。
    let mut s = session_with_segments();
    type_str(&mut s, "womenjintian");
    s.cursor = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    assert!(matches!(
        s.handle(InputEvent::Verbatim('G')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "womenjintianG"
    ));
    assert!(matches!(
        s.handle(InputEvent::Enter).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "womenjintianG"
    ));
    assert!(!s.is_active());
}

#[test]
fn numeric_period_window_and_boundaries() {
    let mut s = session_with_dict();

    // 3 秒内紧跟透传数字的 `.` 保持半角。
    assert!(matches!(s.handle(InputEvent::Select(1)).unwrap(), SessionOutput::Noop));
    if let Some(past) = Instant::now().checked_sub(Duration::from_millis(2500)) {
        s.last_digit_at = Some(past);
    }
    assert!(matches!(s.handle(InputEvent::Char('.')).unwrap(), SessionOutput::Commit(ref t) if t == "."));

    // 超过 3 秒恢复全角句号。
    s.handle(InputEvent::Select(1)).unwrap();
    if let Some(expired) = Instant::now().checked_sub(Duration::from_millis(3100)) {
        s.last_digit_at = Some(expired);
        assert!(matches!(
            s.handle(InputEvent::Char('.')).unwrap(),
            SessionOutput::Commit(ref t) if t == "。"
        ));
    }

    // 连续输入数字会刷新计时窗口。
    s.handle(InputEvent::Char('1')).unwrap();
    if let Some(expired) = Instant::now().checked_sub(Duration::from_secs(4)) {
        s.last_digit_at = Some(expired);
    }
    s.handle(InputEvent::Char('0')).unwrap();
    assert!(matches!(s.handle(InputEvent::Char('.')).unwrap(), SessionOutput::Commit(ref t) if t == "."));

    // 中间夹非数字键（空格）后不再触发半角句点。
    s.handle(InputEvent::Select(3)).unwrap();
    s.handle(InputEvent::Space).unwrap();
    assert!(matches!(s.handle(InputEvent::Char('.')).unwrap(), SessionOutput::Commit(ref t) if t == "。"));

    // 数字选词不算输入数字，紧随其后的 `.` 仍出全角句号。
    type_str(&mut s, "nihao");
    assert!(matches!(s.handle(InputEvent::Select(1)).unwrap(), SessionOutput::Commit(ref t) if t == "你好"));
    assert!(matches!(s.handle(InputEvent::Char('.')).unwrap(), SessionOutput::Commit(ref t) if t == "。"));
}
