//! `session` 单测:basic(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn typing_produces_updated() {
    let mut s = session_with_dict();
    let out = s
        .handle(InputEvent::Char('n'))
        .expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "n");
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
}

#[test]
fn space_commits_candidate() {
    let mut s = session_with_dict();
    for c in ['n', 'i'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Space).expect("不应失败");
    match out {
        SessionOutput::Commit(text) => {
            assert_eq!(text, "你", "词频最高的 '你' 应被选中");
            assert!(!s.is_active(), "上屏后会话应复位");
        }
        other => panic!("期望 Commit，实际 {other:?}"),
    }
}

#[test]
fn escape_clears() {
    let mut s = session_with_dict();
    s.handle(InputEvent::Char('n')).expect("不应失败");
    let out = s.handle(InputEvent::Escape).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty()),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
}

#[test]
fn left_right_move_caret_within_pinyin() {
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    assert_eq!(s.caret(), 5, "追加输入后 caret 应在串尾");

    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3, "左移两位后 caret 应在 3");

    s.handle(InputEvent::Right).expect("不应失败");
    assert_eq!(s.caret(), 4);

    // 右移到串尾后不再越界;按键仍被吃掉(Updated)
    s.handle(InputEvent::Right).expect("不应失败");
    let out = s.handle(InputEvent::Right).expect("不应失败");
    assert_eq!(s.caret(), 5, "caret 不应越过串尾");
    assert!(matches!(out, SessionOutput::Updated(_)), "串非空时左右键应被吃掉");

    // 左移出界同理
    for _ in 0..6 {
        s.handle(InputEvent::Left).expect("不应失败");
    }
    assert_eq!(s.caret(), 0, "caret 不应越过串首");
    let out = s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 0);
    assert!(matches!(out, SessionOutput::Updated(_)), "串非空时左右键应被吃掉");
}

#[test]
fn left_right_passthrough_when_idle() {
    let mut s = session_with_dict();
    // 拼音串为空:左右键透传给应用(让应用移动自己的光标)
    assert!(matches!(
        s.handle(InputEvent::Left).expect("不应失败"),
        SessionOutput::Noop
    ));
    assert!(matches!(
        s.handle(InputEvent::Right).expect("不应失败"),
        SessionOutput::Noop
    ));
}

#[test]
fn insert_in_middle_refreshes_candidates_for_whole_string() {
    // 输入 nihao,caret 左移 2 位(停在 3,nih|ao),插入 x →
    // 缓冲 nihxao,候选必须基于完整新串 nihxao 而不是 caret 前的 nih。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3);

    let out = s.handle(InputEvent::Char('x')).expect("不应失败");
    assert_eq!(s.pinyin(), "nihxao");
    assert_eq!(s.caret(), 4, "插入后 caret 应越过新字符");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihxao", "preedit 应为完整新串");
            assert_eq!(u.caret, 4);
            assert!(
                u.candidates.iter().all(|c| c.pinyin != "你好"),
                "nihxao 不是合法拼音,不应再出现「你好」候选"
            );
        }
        other => panic!("期望 Updated,实际 {other:?}"),
    }
}

#[test]
fn backspace_deletes_char_before_caret() {
    // 输入 nihao,caret 左移 2 位(停在 3),Backspace 删的是 h → niao。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "niao", "应删掉 caret 前的 h");
    assert_eq!(s.caret(), 2);
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "niao");
            assert_eq!(u.caret, 2);
        }
        other => panic!("期望 Updated,实际 {other:?}"),
    }

    // caret 停在 2,Backspace 只删 caret 前的字符:再删 i、n 后
    // caret 到串首,caret 后的 "ao" 不受影响
    s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "nao");
    assert_eq!(s.caret(), 1);
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "ao");
    assert_eq!(s.caret(), 0);
    match out {
        SessionOutput::Updated(u) => assert!(u.preedit == "ao" && u.caret == 0),
        other => panic!("期望 Updated,实际 {other:?}"),
    }
    // caret 已在串首:吃掉按键,"ao" 不动(串首无前字符可删)
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "ao", "caret 在串首时 Backspace 不应删任何字符");
    assert!(matches!(out, SessionOutput::Updated(_)), "按键应被吃掉");
}

#[test]
fn backspace_to_empty_resets_caret() {
    // caret 在串尾连续删到空:串空后 caret 归 0,再按 Backspace 透传
    let mut s = session_with_dict();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Backspace).expect("不应失败");
    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert!(s.pinyin().is_empty());
    assert_eq!(s.caret(), 0, "删空后 caret 应归 0");
    match out {
        SessionOutput::Updated(u) => assert!(u.preedit.is_empty() && u.caret == 0),
        other => panic!("期望 Updated,实际 {other:?}"),
    }
    // 串已空:Backspace 透传给应用(Noop)
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("不应失败"),
        SessionOutput::Noop
    ));
}

#[test]
fn backspace_at_caret_zero_is_safe_noop() {
    // caret 在串首按 Backspace:串不变、不 panic、按键被吃掉
    // (不透传给应用,否则应用会删掉自己文本框里的字)。
    let mut s = session_with_dict();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 0);

    let out = s.handle(InputEvent::Backspace).expect("不应失败");
    assert_eq!(s.pinyin(), "ni", "caret 在串首时 Backspace 不应删任何字符");
    assert_eq!(s.caret(), 0);
    assert!(
        matches!(out, SessionOutput::Updated(ref u) if u.preedit == "ni"),
        "按键应被吃掉且状态不变: {out:?}"
    );
}

#[test]
fn commit_uses_whole_string_not_prefix_before_caret() {
    // caret 不在串尾时按空格/数字/回车:上屏的都是完整串的结果。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 3);
    // 空格:完整串 nihao 的首选是「你好」,不是前缀 nih 的任何结果
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
        "应上屏完整串的首选「你好」: {out:?}"
    );

    // 数字选词同上
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    let out = s.handle(InputEvent::Select(1)).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "你好"),
        "数字选词应上屏完整串的第 1 个候选「你好」: {out:?}"
    );

    // 回车:完整串原样上屏
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "nihao"),
        "回车应上屏完整串 nihao,不是前缀 nih: {out:?}"
    );
}

#[test]
fn caret_resets_on_commit_and_escape() {
    // 上屏后 caret 归 0
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(_)));
    assert_eq!(s.caret(), 0, "上屏后 caret 应归 0");
    assert!(!s.is_active());

    // Escape 后 caret 归 0
    let mut s = session_with_dict();
    for c in "ni".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    assert_eq!(s.caret(), 1);
    let out = s.handle(InputEvent::Escape).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert!(u.preedit.is_empty());
            assert_eq!(u.caret, 0, "Escape 清空后 caret 应归 0");
        }
        other => panic!("期望 Updated,实际 {other:?}"),
    }
    assert_eq!(s.caret(), 0);
}

#[test]
fn commit_immediately_after_caret_move_still_uses_whole_string() {
    // commit_immediately(面板点击路径)在 caret 移动后也应基于完整串的候选。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    let word = s.commit_immediately(0).expect("下标 0 应有候选");
    assert_eq!(word, "你好", "应提交完整串的首选,不是前缀 nih 的结果");
    assert_eq!(s.caret(), 0, "上屏后 caret 应归 0");
}

#[test]
fn caret_visible_in_session_update() {
    // SessionUpdate.caret 跟随编辑光标,engine.rs 拿它做 preedit 光标。
    let mut s = session_with_dict();
    for c in "nihao".chars() {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    let out = s.handle(InputEvent::Left).expect("不应失败");
    match out {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "nihao");
            assert_eq!(u.caret, 4, "左移后 update 里的 caret 应为 4");
            assert_eq!(u.cursor, 0, "移动编辑光标不应动候选高亮");
        }
        other => panic!("期望 Updated,实际 {other:?}"),
    }
}

#[test]
fn noop_when_idle() {
    let mut s = session_with_dict();
    assert!(matches!(
        s.handle(InputEvent::Space).expect("不应失败"),
        SessionOutput::Noop
    ));
}

#[test]
fn ctrl_space_toggles_chinese_english() {
    let mut s = session_with_dict();
    assert!(!s.english_mode(), "初始应为中文模式");

    // Ctrl+空格：切到英文模式
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode(), "Ctrl+空格后应进入英文模式");

    // 英文模式下字母不进入拼音，直接透传
    let out = s.handle(InputEvent::Char('h')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop));

    // 再按一次 Ctrl+空格：切回中文
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(!s.english_mode(), "再次 Ctrl+空格应回到中文模式");

    // 回到中文后字母恢复进入拼音串
    let out = s.handle(InputEvent::Char('h')).expect("不应失败");
    assert!(matches!(out, SessionOutput::Updated(ref u) if u.preedit == "h"));
}

#[test]
fn toggle_during_composition_commits_pinyin_first() {
    let mut s = session_with_dict();
    for c in ['n', 'i'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 输入到一半切英文：未上屏的拼音原样上屏，不丢字
    let out = s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "ni"),
        "切换为英文时应先把拼音上屏"
    );
    assert!(s.english_mode(), "应已进入英文模式");
    assert!(!s.is_active(), "上屏后会话应复位");
}

#[test]
fn focus_change_keeps_the_mode() {
    let mut s = session_with_dict();
    s.handle(InputEvent::ToggleAscii).expect("切换不应失败");
    assert!(s.english_mode(), "应已进入英文模式");

    let text = s.reset_for_focus_change();
    assert!(text.is_empty(), "没有已确认段时不该有上屏文本");

    // 在输入框里点一下鼠标就会触发 FocusOut/Reset——模式**不能**因此切回中文，
    // 否则英文打一半点一下输入框就变中文，等于没法连续打英文。模式归用户按
    // Ctrl+空格 管；顶栏有中/英指示，不再需要「无声复位」来兜底。
    assert!(s.english_mode(), "焦点切换不应改变中/英模式");
}
