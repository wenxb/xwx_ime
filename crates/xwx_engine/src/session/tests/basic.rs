//! `session` 基础编辑、光标移动与中英模式切换单测。

use super::*;

#[test]
fn basic_typing_space_commit_and_escape() {
    let mut s = session_with_dict();
    match s.handle(InputEvent::Char('n')).expect("不应失败") {
        SessionOutput::Updated(u) => assert_eq!(u.preedit, "n"),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    s.handle(InputEvent::Char('i')).expect("不应失败");
    match s.handle(InputEvent::Space).expect("不应失败") {
        SessionOutput::Commit(text) => {
            assert_eq!(text, "你", "词频最高的「你」应被选中");
            assert!(!s.is_active(), "上屏后会话应复位");
        }
        other => panic!("期望 Commit，实际 {other:?}"),
    }

    s.handle(InputEvent::Char('n')).expect("不应失败");
    match s.handle(InputEvent::Escape).expect("不应失败") {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty()),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
}

#[test]
fn idle_keys_passthrough() {
    let mut s = session_with_dict();
    for ev in [InputEvent::Space, InputEvent::Left, InputEvent::Right] {
        assert!(
            matches!(s.handle(ev).expect("不应失败"), SessionOutput::Noop),
            "空闲时 {ev:?} 应透传"
        );
    }
}

#[test]
fn left_right_move_caret_within_pinyin() {
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    assert_eq!(s.caret(), 5, "追加输入后 caret 应在串尾");

    // 移动编辑光标更新 update.caret 且不动候选高亮。
    match s.handle(InputEvent::Left).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihao");
            assert_eq!(u.caret, 4);
            assert_eq!(u.cursor, 0, "移动编辑光标不应动候选高亮");
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3);

    s.handle(InputEvent::Right).expect("不应失败");
    assert_eq!(s.caret(), 4);

    // 边界钳位且按键仍被吃掉。
    s.handle(InputEvent::Right).expect("不应失败");
    let out = s.handle(InputEvent::Right).expect("不应失败");
    assert_eq!(s.caret(), 5, "caret 不应越过串尾");
    assert!(matches!(out, SessionOutput::Updated(_)));

    for _ in 0..6 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 0, "caret 不应越过串首");
    let out = s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 0);
    assert!(matches!(out, SessionOutput::Updated(_)));
}

#[test]
fn insert_in_middle_refreshes_candidates_for_whole_string() {
    // 正向：打 `niho` 后左移光标补 `a` → 整串 `nihao` 重查出「你好」，空格上屏整串。
    let mut s = session_with_dict();
    type_str(&mut s, "niho");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3);
    match s.handle(InputEvent::Char('a')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihao");
            assert_eq!(u.caret, 4);
            assert!(
                u.candidates.iter().any(|c| c.word == "你好"),
                "中段补字后候选应按整串重查: {:?}",
                u.candidates.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
            );
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert!(
        matches!(s.handle(InputEvent::Space).expect("不应失败"), SessionOutput::Commit(ref w) if w == "你好"),
        "中段插入后空格应上屏整串「你好」"
    );

    // 反向：中段插入超出容错预算的字母（`nihqqao`），整串重查不再产出「你好」。
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Char('q')).expect("不应失败");
    match s.handle(InputEvent::Char('q')).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihqqao");
            assert_eq!(u.caret, 5);
            assert!(
                u.candidates.iter().all(|c| c.word != "你好"),
                "nihqqao 不应再出现「你好」"
            );
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
}

#[test]
fn backspace_deletes_char_before_caret() {
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");

    match s.handle(InputEvent::Backspace).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "niao");
            assert_eq!(u.caret, 2);
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "niao");

    // 继续删至 caret == 0，caret 后的 "ao" 不受影响。
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!((s.pinyin(), s.caret()), ("nao", 1));
    match s.handle(InputEvent::Backspace).expect("不应失败") {
        SessionOutput::Updated(u) => assert!(u.preedit == "ao" && u.caret == 0),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    // caret 在串首时吃掉按键、不动内容。
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!((s.pinyin(), s.caret()), ("ao", 0));
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.preedit == "ao"));
}

#[test]
fn backspace_to_empty_resets_caret() {
    let mut s = session_with_dict();
    type_str(&mut s, "ni");
    s.handle(InputEvent::Backspace).expect("不应失败");
    match s.handle(InputEvent::Backspace).expect("不应失败") {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty() && u.caret == 0),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert!(s.pinyin().is_empty() && s.caret() == 0);
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("不应失败"),
        SessionOutput::Noop
    ));
}

#[test]
fn commit_and_escape_use_whole_string_and_reset_caret() {
    // caret 不在串尾时，空格/数字/回车均基于完整串上屏且 caret 归 0。
    for (event, expected) in [
        (InputEvent::Space, "你好"),
        (InputEvent::Select(1), "你好"),
        (InputEvent::Enter, "nihao"),
    ] {
        let mut s = session_with_dict();
        type_str(&mut s, "nihao");
        s.handle(InputEvent::Left).expect("不应失败");
        s.handle(InputEvent::Left).expect("不应失败");
        assert_eq!(s.caret(), 3);
        let out = s.handle(event).expect("不应失败");
        assert!(
            matches!(out, SessionOutput::Commit(ref t) if t == expected),
            "{event:?} 应上屏 {expected:?}，实际 {out:?}"
        );
        assert_eq!(s.caret(), 0);
        assert!(!s.is_active());
    }

    // Escape 清空后 caret 归 0。
    let mut s = session_with_dict();
    type_str(&mut s, "ni");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 1);
    match s.handle(InputEvent::Escape).expect("不应失败") {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty() && u.caret == 0),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert_eq!(s.caret(), 0);
}

#[test]
fn ctrl_space_toggles_chinese_english_and_commits_active_pinyin() {
    let mut s = session_with_dict();
    assert!(!s.english_mode(), "初始应为中文模式");

    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode());
    assert!(matches!(
        s.handle(InputEvent::Char('h')).expect("不应失败"),
        SessionOutput::Noop
    ));

    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(!s.english_mode());
    assert!(matches!(
        s.handle(InputEvent::Char('h')).expect("不应失败"),
        SessionOutput::Updated(ref u) if u.preedit == "h"
    ));

    // 组合中途切英文：未上屏拼音原样上屏，不丢字。
    let mut s = session_with_dict();
    type_str(&mut s, "ni");
    let out = s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "ni"),
        "切换为英文时应先把拼音上屏: {out:?}"
    );
    assert!(s.english_mode() && !s.is_active());
}

#[test]
fn focus_change_keeps_the_mode() {
    let mut s = session_with_dict();
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode());

    let text = s.reset_for_focus_change();
    assert!(text.is_empty(), "没有已确认段时不该有上屏文本");
    assert!(s.english_mode(), "焦点切换不应改变中/英模式");
}
