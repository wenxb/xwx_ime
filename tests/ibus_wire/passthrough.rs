//! `ProcessKeyEvent` 透传/消费边界与跨 IBus 上下文失效契约。

use std::time::Duration;

use crate::support::{
    keyval, last_commit, last_table, member_names, shared_fixture, Harness, M_CONTROL, M_LOCK,
    M_RELEASE, M_SHIFT,
};

#[test]
fn english_mode_early_return_emits_no_ui_and_toggles_back() {
    let mut h = Harness::new("passthrough-en");
    h.start_engine(shared_fixture());

    // 对照：中文模式消费字母
    assert!(h.key(keyval::ascii('n'), 0), "中文模式 'n' 应被消费");
    h.focus_out().expect("FocusOut");

    // 切到英文模式：字母、Shift、CapsLock、标点一律透传且零 UI/上屏信号
    assert!(h.toggle_ascii(), "Ctrl+Space 应被消费");
    h.clear_backlog();

    for c in "nihao".chars() {
        assert!(!h.key(keyval::ascii(c), 0), "英文模式 '{c}' 必须透传");
    }
    assert!(!h.key(keyval::ascii('g'), M_SHIFT), "英文模式 Shift+G 必须透传");
    assert!(!h.key(keyval::ascii('g'), M_LOCK), "英文模式 CapsLock+g 必须透传");
    assert!(!h.key(keyval::ascii(','), 0), "英文模式 ',' 必须透传");

    let sigs = h.settle_default();
    let names = member_names(&sigs);
    assert!(
        !names.contains(&"UpdatePreeditText")
            && !names.contains(&"UpdateLookupTable")
            && !names.contains(&"CommitText"),
        "英文透传不得发出预编辑/候选窗/上屏信号: {names:?}"
    );

    // 切回中文：同一键恢复消费
    assert!(h.toggle_ascii(), "切回中文的 Ctrl+Space 应被消费");
    assert!(h.key(keyval::ascii('n'), 0), "切回中文后 'n' 应恢复消费");
}

#[test]
fn modifier_chords_and_key_release_filter() {
    let mut h = Harness::new("passthrough-mods");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    for (kv, state, label) in [
        (keyval::ascii('c'), M_CONTROL, "Ctrl+C"),
        (keyval::ascii('v'), M_CONTROL, "Ctrl+V"),
        (keyval::ascii('n'), M_CONTROL, "Ctrl+N"),
        (keyval::ascii('n'), 1 << 3, "Alt+n"),
        (keyval::ascii('n'), M_CONTROL | M_SHIFT, "Ctrl+Shift+N"),
        (keyval::ascii('n'), 1 << 26, "Super+n"),
        (keyval::SPACE, M_CONTROL | M_SHIFT, "Ctrl+Shift+Space"),
        (keyval::DELETE, M_CONTROL | M_SHIFT, "空闲 Ctrl+Shift+Delete"),
    ] {
        assert!(!h.key(kv, state), "{label} 必须透传");
    }

    for (kv, state) in [
        (keyval::ascii('n'), M_RELEASE),
        (keyval::SPACE, M_RELEASE),
        (keyval::ascii(','), M_RELEASE),
        (keyval::SPACE, M_RELEASE | M_CONTROL),
    ] {
        assert!(!h.key(kv, state), "释放事件 {kv:#x} 必须透传");
    }

    assert!(h.key(keyval::SPACE, M_CONTROL), "Ctrl+Space 按下应被消费");
}

#[test]
fn ctrl_delete_gate_follows_composition_state() {
    let mut h = Harness::new("passthrough-ctrl-del");
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    assert!(!h.key(keyval::DELETE, M_CONTROL), "空闲时 Ctrl+Delete 必须透传");

    h.type_ascii("ni");
    h.settle_default();
    assert!(h.key(keyval::DELETE, M_CONTROL), "组合中 Ctrl+Delete 应被消费");
    assert!(h.key(keyval::DELETE, M_CONTROL), "仍有候选时应继续消费");

    h.focus_out().expect("FocusOut");
    assert!(
        !h.key(keyval::DELETE, M_CONTROL),
        "FocusOut 后 Ctrl+Delete 必须恢复透传"
    );
}

#[test]
fn passthrough_context_invalidation_across_ibus_boundary() {
    let mut h = Harness::new("passthrough-ctx").with_english_prediction();
    h.start_engine(shared_fixture());
    h.focus_out().expect("FocusOut");

    // 1. M_RELEASE 保留数字句点上下文：1 + release + '.' -> "."，下一次 '.' -> "。"
    h.clear_backlog();
    assert!(!h.key(keyval::ascii('1'), 0));
    assert!(!h.key(keyval::ascii('1'), M_RELEASE));
    assert!(h.key(keyval::ascii('.'), 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("."));

    h.clear_backlog();
    assert!(h.key(keyval::ascii('.'), 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("。"));

    // 2. 快捷键、未识别键、Backspace 与 FocusOut 均清空数字句点上下文
    for (kv, state, label) in [
        (keyval::ascii('a'), M_CONTROL, "Ctrl+a"),
        (keyval::HOME, 0, "Home"),
        (keyval::BACKSPACE, 0, "Backspace"),
    ] {
        h.clear_backlog();
        assert!(!h.key(keyval::ascii('1'), 0));
        assert!(!h.key(kv, state), "{label} 应透传");
        assert!(h.key(keyval::ascii('.'), 0));
        let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
        assert_eq!(last_commit(&sigs).as_deref(), Some("。"), "{label} 应清数字句点");
    }

    h.clear_backlog();
    assert!(!h.key(keyval::ascii('1'), 0));
    h.focus_out().expect("FocusOut");
    assert!(h.key(keyval::ascii('.'), 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("。"), "FocusOut 应清数字句点");

    // 3. M_RELEASE 保留英文连续输入上下文：hello + release + world -> " world"
    h.clear_backlog();
    h.type_ascii("hello");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("hello"));
    for c in "world".chars() {
        assert!(!h.key(keyval::ascii(c), M_RELEASE));
    }
    h.type_ascii("world");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some(" world"),
        "按键释放不得打断英文上文"
    );

    // 4. M2 预测态下 Shift+Tab 透传并收起面板，不接受预测
    h.focus_out().expect("FocusOut");
    h.type_ascii("thank");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let table = last_table(&sigs).expect("应弹出预测面板");
    assert_eq!(table.candidates.first().map(String::as_str), Some("you"));

    h.clear_backlog();
    assert!(!h.key(keyval::TAB, M_SHIFT), "Shift+Tab 必须透传");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(last_commit(&sigs).is_none(), "Shift+Tab 不得提交预测词");
    assert!(
        member_names(&sigs).contains(&"HideLookupTable"),
        "Shift+Tab 应同时收起预测面板: {:?}",
        member_names(&sigs)
    );
}
