//! 引擎生命周期边界：焦点、重置/禁用、密码框 `ContentType` 与同连接排队保序。

use std::{process::Command, sync::mpsc, thread, time::Duration};
use zbus::{blocking::MessageIterator, message::Type, Message};

use crate::support::{
    commits, keyval, last_commit, last_preedit, last_table, member_names, shared_fixture, Harness,
    CALL_TIMEOUT, ENGINE_IFACE, ENGINE_PATH, ENGINE_SERVICE,
};

fn compose_ni(h: &mut Harness) {
    h.focus_out().expect("FocusOut 复位");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(last_table(&sigs).is_some(), "前置条件：'ni' 应弹出候选窗");
}

#[test]
fn focus_reset_disable_enable_lifecycle() {
    let mut h = Harness::new("lifecycle-focus");
    h.start_engine(shared_fixture());

    // 空闲时 FocusIn / FocusOut 静默；Enable 仅发 RegisterProperties
    h.clear_backlog();
    h.focus_in().expect("FocusIn");
    assert!(
        h.settle(Duration::from_millis(250), Duration::from_secs(2))
            .is_empty(),
        "空闲 FocusIn 不得发信号"
    );

    h.clear_backlog();
    h.enable().expect("Enable");
    let sigs = h.settle_response(Duration::from_millis(250), Duration::from_secs(2));
    assert_eq!(member_names(&sigs), vec!["RegisterProperties"]);

    h.clear_backlog();
    h.focus_out().expect("空闲 FocusOut");
    assert!(
        h.settle(Duration::from_millis(250), Duration::from_secs(2))
            .is_empty(),
        "空闲 FocusOut 不得发信号"
    );

    // 组合态下 FocusIn 保留输入，空格仍可上屏
    compose_ni(&mut h);
    h.clear_backlog();
    h.focus_in().expect("组合中 FocusIn");
    let sigs = h.settle(Duration::from_millis(250), Duration::from_secs(2));
    assert!(!member_names(&sigs).contains(&"HidePreeditText"));
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("你"));

    // FocusOut / Reset / Disable 均须收起界面并清掉组合串
    for (name, action) in [
        ("FocusOut", Harness::focus_out as fn(&Harness) -> Result<(), String>),
        ("Reset", Harness::reset_session as fn(&Harness) -> Result<(), String>),
        ("Disable", Harness::disable as fn(&Harness) -> Result<(), String>),
    ] {
        compose_ni(&mut h);
        h.clear_backlog();
        action(&h).unwrap_or_else(|e| panic!("{name} 失败: {e}"));
        let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
        let names = member_names(&sigs);
        assert!(
            names.contains(&"HidePreeditText") && names.contains(&"HideLookupTable"),
            "{name} 必须发出 HidePreeditText 和 HideLookupTable: {names:?}"
        );
        h.clear_backlog();
        h.type_ascii("hi");
        let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
        assert_eq!(
            last_preedit(&sigs).as_deref(),
            Some("hi"),
            "{name} 后重打应从空串开始"
        );
    }
}

#[test]
fn m2_prediction_panel_focus_out_hides_without_committing() {
    let mut h = Harness::new("lifecycle-m2-focus-out").with_english_prediction();
    h.start_engine(shared_fixture());

    h.type_ascii("thank");
    h.settle_default();
    h.clear_backlog();
    assert!(h.key(keyval::SPACE, 0));
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    assert!(
        last_table(&sigs).is_some_and(|t| t.candidates.iter().any(|c| c == "you")),
        "前置条件：应已弹出 M2 预测面板"
    );

    h.clear_backlog();
    h.focus_out().expect("FocusOut");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(
        last_commit(&sigs).is_none(),
        "FocusOut 不得提交预测词，实际 {:?}",
        last_commit(&sigs)
    );
    let names = member_names(&sigs);
    assert!(
        names.contains(&"HideLookupTable"),
        "预测面板 FocusOut 必须发出 HideLookupTable: {names:?}"
    );
}

#[test]
fn password_content_type_forwards_keys_and_hides_composition() {
    let mut h = Harness::new("lifecycle-password");
    h.start_engine(shared_fixture());

    // 对照：普通框消费字母
    h.set_content_type(0, 0).expect("ContentType=(0,0)");
    assert!(h.key(keyval::ascii('n'), 0), "普通框 'n' 应被消费");
    h.focus_out().expect("FocusOut");

    // 密码框 (8, 6144)：透传所有键、状态粘性、不拦截 Ctrl+Space
    h.set_content_type(8, 6144).expect("ContentType=(8,6144)");
    for kv in [
        keyval::ascii('n'),
        keyval::ascii('1'),
        keyval::ascii('/'),
        keyval::SPACE,
        keyval::RETURN,
        keyval::ascii('a'),
        keyval::ascii('a'),
    ] {
        assert!(!h.key(kv, 0), "密码框按键 {kv:#x} 必须透传");
    }
    assert!(!h.toggle_ascii(), "密码框 Ctrl+Space 必须透传且不切模式");

    // 回到普通框：恢复中文组合且模式未被改写
    h.set_content_type(0, 0).expect("ContentType=(0,0)");
    h.clear_backlog();
    assert!(h.key(keyval::ascii('n'), 0), "离开密码框后应恢复组合");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_preedit(&sigs).as_deref(), Some("n"));

    // 组合态直接切入密码框：下一次按键透传并收起候选窗、绝不上屏
    h.type_ascii("i");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert!(last_table(&sigs).is_some(), "前置条件：组合态候选窗已打开");
    h.clear_backlog();

    h.set_content_type(8, 6144).expect("ContentType=(8,6144)");
    assert!(!h.key(keyval::ascii('x'), 0), "密码框按键必须透传");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    let names = member_names(&sigs);
    assert!(names.contains(&"HideLookupTable"), "应收起候选窗: {names:?}");
    assert!(names.contains(&"HidePreeditText"), "应收起预编辑: {names:?}");
    assert!(commits(&sigs).is_empty(), "切进密码框不得提交未确认拼音");

    h.set_content_type(0, 0).expect("ContentType=(0,0)");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(5));
    assert_eq!(last_preedit(&sigs).as_deref(), Some("ni"));
}

#[test]
fn password_content_type_after_focus_out_emits_no_extra_hides() {
    let mut h = Harness::new("lifecycle-password-daemon-order");
    h.start_engine(shared_fixture());

    h.set_content_type(0, 0).expect("ContentType=(0,0)");
    h.type_ascii("ni");
    h.settle_default();
    h.clear_backlog();

    // daemon 真实顺序：先 FocusOut 清空并收窗，再设 ContentType
    h.focus_out().expect("FocusOut");
    let fo_sigs = h.settle_response(Duration::from_millis(300), Duration::from_secs(2));
    assert!(member_names(&fo_sigs).contains(&"HideLookupTable"));
    h.clear_backlog();

    h.set_content_type(8, 6144).expect("ContentType=(8,6144)");
    let sigs1 = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    h.clear_backlog();

    assert!(!h.key(keyval::ascii('x'), 0));
    let sigs2 = h.settle(Duration::from_millis(300), Duration::from_secs(2));
    let mut names = member_names(&sigs1);
    names.extend(member_names(&sigs2));
    assert!(
        !names.contains(&"HideLookupTable") && !names.contains(&"HidePreeditText"),
        "FocusOut 已清会话，后续不得补发隐藏信号: {names:?}"
    );
    assert!(commits(&sigs1).is_empty() && commits(&sigs2).is_empty());
}

// —— 同连接排队输入保序（SIGSTOP/SIGCONT 守卫 Ordered<T> 分发）——

fn raw_call<B: serde::Serialize + zvariant::DynamicType>(method: &str, body: B) -> Message {
    Message::method(ENGINE_PATH, method)
        .unwrap()
        .destination(ENGINE_SERVICE)
        .unwrap()
        .interface(ENGINE_IFACE)
        .unwrap()
        .build(&body)
        .unwrap()
}

fn raw_key(ch: u32) -> Message {
    raw_call("ProcessKeyEvent", (ch, 0u32, 0u32))
}

fn raw_letters(text: &str) -> Vec<Message> {
    text.chars().map(|ch| raw_key(ch as u32)).collect()
}

fn raw_content_type(purpose: u32) -> Message {
    Message::method(ENGINE_PATH, "Set")
        .unwrap()
        .destination(ENGINE_SERVICE)
        .unwrap()
        .interface("org.freedesktop.DBus.Properties")
        .unwrap()
        .build(&(
            ENGINE_IFACE,
            "ContentType",
            zvariant::Value::from((purpose, 0u32)),
        ))
        .unwrap()
}

struct PausedEngine(u32);

impl PausedEngine {
    fn new(h: &Harness) -> Self {
        let guard = Self(h.engine_pid());
        guard.signal("-STOP");
        guard
    }

    fn signal(&self, signal: &str) {
        assert!(Command::new("kill")
            .args([signal, &self.0.to_string()])
            .status()
            .expect("signal private engine")
            .success());
    }
}

impl Drop for PausedEngine {
    fn drop(&mut self) {
        self.signal("-CONT");
    }
}

fn burst(h: &mut Harness, messages: Vec<Message>) -> Vec<Message> {
    let connection = zbus::blocking::connection::Builder::address(h.address())
        .unwrap()
        .build()
        .unwrap();
    let replies = MessageIterator::from(&connection);
    let serials: Vec<_> = messages
        .iter()
        .map(|m| m.primary_header().serial_num())
        .collect();
    let expected = serials.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut received = Vec::new();
        for reply in replies {
            let reply = reply.expect("burst reply");
            if reply
                .header()
                .reply_serial()
                .is_some_and(|s| expected.contains(&s))
            {
                received.push(reply);
                if received.len() == expected.len() {
                    tx.send(received).unwrap();
                    return;
                }
            }
        }
    });
    let paused = PausedEngine::new(h);
    for message in &messages {
        connection
            .send(message)
            .expect("enqueue key on private bus");
    }
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetId",
            &(),
        )
        .unwrap();
    drop(paused);
    let replies = rx
        .recv_timeout(CALL_TIMEOUT)
        .expect("queued input must finish");
    assert!(replies
        .iter()
        .all(|m| m.message_type() == Type::MethodReturn));
    assert_eq!(
        replies
            .iter()
            .map(|m| m.header().reply_serial().unwrap())
            .collect::<Vec<_>>(),
        serials,
        "replies must follow the input order"
    );
    replies
}

fn start_empty_engine(label: &str) -> Harness {
    let mut h = Harness::new(label);
    let absent = h.root.join("absent-dict.bin");
    h.start_engine_with(&absent, None, CALL_TIMEOUT).unwrap();
    h.clear_backlog();
    h
}

#[test]
fn queued_keys_keep_preedit_commit_and_reply_order() {
    let mut h = start_empty_engine("queued-keys");
    let mut messages = raw_letters("abcdefgh");
    messages.push(raw_key(keyval::RETURN));
    messages.extend(raw_letters("nihaoshijie"));
    messages.push(raw_key(keyval::RETURN));
    let replies = burst(&mut h, messages);
    assert!(replies
        .iter()
        .all(|m| m.body().deserialize::<bool>().unwrap()));
    let signals = h.settle_default();
    assert_eq!(commits(&signals), ["abcdefgh", "nihaoshijie"]);
    let preedits: Vec<_> = signals
        .iter()
        .filter(|s| s.member == "UpdatePreeditText")
        .map(|s| s.as_text().unwrap())
        .collect();
    let expected: Vec<_> = ["abcdefgh", "nihaoshijie"]
        .into_iter()
        .flat_map(|s| (1..=s.len()).map(move |end| s[..end].to_owned()))
        .collect();
    assert_eq!(preedits, expected);
}

#[test]
fn queued_reset_focus_and_disable_separate_compositions() {
    let mut h = start_empty_engine("queued-boundaries");
    let mut messages = Vec::new();
    for boundary in ["Reset", "FocusOut", "Disable"] {
        messages.extend(raw_letters("discard"));
        messages.push(raw_call(boundary, ()));
        messages.extend(raw_letters("keep"));
        messages.push(raw_key(keyval::RETURN));
    }
    burst(&mut h, messages);
    assert_eq!(commits(&h.settle_default()), ["keep", "keep", "keep"]);
}

#[test]
fn queued_content_type_applies_at_the_password_boundary() {
    let mut h = start_empty_engine("queued-password");
    let mut messages = raw_letters("discard");
    messages.push(raw_content_type(8));
    messages.push(raw_key('x' as u32));
    messages.push(raw_key(keyval::RETURN));
    messages.push(raw_content_type(0));
    messages.extend(raw_letters("safe"));
    messages.push(raw_key(keyval::RETURN));
    let replies = burst(&mut h, messages);
    assert!(!replies[8].body().deserialize::<bool>().unwrap());
    assert!(!replies[9].body().deserialize::<bool>().unwrap());
    let signals = h.settle_default();
    assert_eq!(commits(&signals), ["safe"]);
    assert!(!signals
        .iter()
        .filter(|s| s.member == "UpdatePreeditText")
        .any(|s| s.as_text().is_some_and(|s| s.contains('x'))));
}
