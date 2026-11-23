//! `session` 邮箱/网址识别态（recognizer）交互单测。

use super::*;

#[test]
fn email_suppresses_pinyin_candidates_and_commits_raw() {
    // 空闲时 `@` 透传，不进组合。
    let mut s = session_with_dict();
    assert!(matches!(s.handle(InputEvent::Char('@')).expect("不应失败"), SessionOutput::Noop));
    assert!(!s.is_active());

    // `@` 之前是正常拼音态（有真实拼音候选「你」）；`@` 落键后只剩原文回声。
    type_str(&mut s, "ni");
    assert_eq!(s.recognized, None);
    assert!(s.candidates.iter().any(|c| c.word == "你" && !c.echo), "应有真实拼音候选「你」");

    match s.handle(InputEvent::Char('@')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "ni@");
            assert_eq!(u.candidates.len(), 1);
            assert_eq!(u.candidates[0].word, "ni@");
            assert!(u.candidates[0].echo);
        }
        other => panic!("@ 应返回 Updated，实际 {other:?}"),
    }

    // 识别成立后逐字符推进不反悔、不振荡。
    let mut expect = String::from("ni@");
    for c in "gmail.com".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
        expect.push(c);
        assert_recognized(&s, &expect);
    }

    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "ni@gmail.com"));
    assert!(!s.is_active() && s.recognized.is_none());

    // 空格路径经回声候选同样原样上屏。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x.com");
    assert_recognized(&s, "abc@x.com");
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "abc@x.com"
    ));
}

#[test]
fn url_www_dot_not_eaten_by_paging_or_punct() {
    let mut s = session_with_dict();
    type_str(&mut s, "www");
    assert_eq!(s.recognized, None);
    assert!(!s.candidates.is_empty());

    // `.` 落键瞬间走翻页语义，串不变、尚不识别。
    match s.handle(InputEvent::Char('.')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "www");
            assert_eq!(s.recognized, None);
        }
        other => panic!("`.` 翻页应返回 Updated，实际 {other:?}"),
    }

    // 下一个字母到来时补回真实敲过的 `.` 并进入识别态。
    match s.handle(InputEvent::Char('e')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "www.e");
            assert_eq!(u.candidates.len(), 1);
            assert_eq!(u.candidates[0].word, "www.e");
            assert!(u.candidates[0].echo);
        }
        other => panic!("识别应返回 Updated，实际 {other:?}"),
    }
    type_str(&mut s, "xample.com");
    assert_recognized(&s, "www.example.com");
    assert!(matches!(
        s.handle(InputEvent::Enter).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "www.example.com"
    ));
}

#[test]
fn url_dot_is_never_invented_or_resurrected() {
    // 没敲过点：`wwwx` 绝不凭空补成 `www.x`。
    let mut s = session_with_dict();
    type_str(&mut s, "wwwx");
    assert_eq!(s.pinyin(), "wwwx");
    assert_eq!(s.recognized, None);

    // 退格删掉的点不许复活：删完 `e` 与 `.` 后再打 `x` 保持 `wwwx`。
    let mut s = session_with_dict();
    type_str(&mut s, "www.e");
    assert_eq!(s.pinyin(), "www.e");
    s.handle(InputEvent::Backspace).expect("不应失败");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "www");
    type_str(&mut s, "x");
    assert_eq!(s.pinyin(), "wwwx");
    assert_eq!(s.recognized, None);
}

#[test]
fn colon_enters_string_when_scheme_prefix() {
    let mut s = session_with_dict();
    type_str(&mut s, "mailto");
    assert_eq!(s.recognized, None);
    match s.handle(InputEvent::Char(':')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "mailto:");
            assert!(u.candidates.iter().all(|c| c.echo));
        }
        other => panic!("协议前缀后的 `:` 应进串返回 Updated，实际 {other:?}"),
    }
    type_str(&mut s, "a@b.c");
    assert_recognized(&s, "mailto:a@b.c");
}

#[test]
fn mailto_alone_keeps_pinyin_candidates() {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("买", "mai", 100).expect("插入失败");
    let mut s = InputSession::new(Arc::new(engine));
    type_str(&mut s, "mailto");
    assert_eq!(s.recognized, None);
    assert!(s.candidates.iter().any(|c| c.word == "买" && !c.echo));
}

#[test]
fn bare_domain_stays_pinyin_per_ruling() {
    let mut s = session_with_dict();
    type_str(&mut s, "example");
    assert_eq!(s.recognized, None);
    assert!(matches!(
        s.handle(InputEvent::Char('.')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "example"
    ));
    assert_eq!(s.recognized, None);
    type_str(&mut s, "com");
    assert_eq!(s.pinyin(), "examplecom");
    assert_eq!(s.recognized, None);
}

#[test]
fn recognized_samples_end_to_end_commit_identity() {
    for input in [
        "zhangsan@gmail.com",
        "www.example.com",
        "http://x.com",
        "https://x.com",
        "mailto:a@b.c",
        "ftp.x.com",
        "file:/x",
    ] {
        let mut s = session_with_dict();
        let mut committed = String::new();
        for ch in input.chars() {
            if let SessionOutput::Commit(t) = s.handle(InputEvent::Char(ch)).expect("按键失败") {
                committed.push_str(&t);
            }
        }
        assert_recognized(&s, input);
        if let SessionOutput::Commit(t) = s.handle(InputEvent::Enter).expect("回车失败") {
            committed.push_str(&t);
        }
        assert_eq!(committed, input, "端到端上屏应等于原串 {input:?}");
    }
}

#[test]
fn backspace_and_caret_editing_in_recognized_state() {
    // 删掉 `@` 退出识别态并恢复拼音候选；删光复位。
    let mut s = session_with_dict();
    type_str(&mut s, "ni@");
    assert_recognized(&s, "ni@");
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "ni"
    ));
    assert_eq!(s.recognized, None);
    assert!(s.candidates.iter().any(|c| c.word == "你" && !c.echo));
    s.handle(InputEvent::Backspace).expect("不应失败");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert!(!s.is_active() && s.recognized.is_none());

    // 邮箱末尾回删仍命中时保持识别态。
    let mut s = session_with_dict();
    type_str(&mut s, "a@b.co");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_recognized(&s, "a@b.c");

    // caret 中段插入/退格按完整串重判。
    let mut s = session_with_dict();
    type_str(&mut s, "a@b.com");
    for _ in 0..6 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 1);
    s.handle(InputEvent::Char('x')).expect("不应失败");
    assert_recognized(&s, "ax@b.com");
    assert_eq!(s.caret(), 2);

    let mut s = session_with_dict();
    type_str(&mut s, "a@b");
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!((s.pinyin(), s.recognized), ("ab", None));
}

#[test]
fn escape_and_ascii_toggle_clear_recognition() {
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x");
    assert!(s.recognized.is_some());
    assert!(matches!(
        s.handle(InputEvent::Escape).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit.is_empty()
    ));
    assert!(!s.is_active() && s.recognized.is_none());

    // 中途切英文原样上屏并清空识别态，切回中文无残留。
    let mut s = session_with_dict();
    type_str(&mut s, "abc@x");
    assert!(matches!(
        s.handle(InputEvent::ToggleAscii).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "abc@x"
    ));
    assert!(s.english_mode() && s.recognized.is_none());
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    type_str(&mut s, "ni");
    assert_eq!(s.recognized, None);
}

#[test]
fn recognized_commit_does_not_learn_and_does_not_coin() {
    let (engine, mut s) = session_for_coin(&[("你", "ni", 100)]);
    let user = engine.user_lm().expect("应有用户层");
    let before_uni = user.unigram_len();

    type_str(&mut s, "a@b.c");
    assert!(s.recognized.is_some());
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "a@b.c"
    ));
    assert_eq!(user.unigram_len(), before_uni, "识别态上屏不应学习");
    assert_eq!(s.last_word, None);

    type_str(&mut s, "ni");
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Commit(ref t) if t == "你"
    ));
    assert_eq!(user.entry_len(), 0, "识别态原文不应参与造词");
}

#[test]
fn digits_and_capitals_extend_recognized_url() {
    let mut s = session_with_dict();
    type_str(&mut s, "http://x");
    assert!(s.recognized.is_some());
    assert!(matches!(s.handle(InputEvent::Select(1)).expect("不应失败"), SessionOutput::Updated(_)));
    assert!(matches!(s.handle(InputEvent::Select(0)).expect("不应失败"), SessionOutput::Updated(_)));
    assert_eq!(s.pinyin(), "http://x10");
    type_str(&mut s, ".com/");
    assert!(matches!(s.handle(InputEvent::Verbatim('A')).expect("不应失败"), SessionOutput::Updated(_)));
    assert_recognized(&s, "http://x10.com/A");
}
