//! 同一连接积压的按键、焦点和输入框属性必须保持接收顺序。
mod support;

use std::{process::Command, sync::mpsc, thread};
use support::{keyval, Harness, Signal, CALL_TIMEOUT, ENGINE_IFACE, ENGINE_PATH, ENGINE_SERVICE};
use zbus::{blocking::MessageIterator, message::Type, Message};

fn call<B: serde::Serialize + zvariant::DynamicType>(method: &str, body: B) -> Message {
    Message::method(ENGINE_PATH, method)
        .unwrap()
        .destination(ENGINE_SERVICE)
        .unwrap()
        .interface(ENGINE_IFACE)
        .unwrap()
        .build(&body)
        .unwrap()
}

fn key(ch: u32) -> Message {
    call("ProcessKeyEvent", (ch, 0u32, 0u32))
}

fn letters(text: &str) -> Vec<Message> {
    text.chars().map(|ch| key(ch as u32)).collect()
}

fn content_type(purpose: u32) -> Message {
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
        let guard = Self(h.engine_pid_for_test());
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
    // 同连接的总线回复证明前面的消息已交给 daemon，不依赖固定睡眠。
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

fn commits(signals: &[Signal]) -> Vec<String> {
    signals
        .iter()
        .filter(|s| s.member == "CommitText")
        .map(|s| s.as_text().expect("commit text"))
        .collect()
}

fn start(label: &str) -> Harness {
    let mut h = Harness::new(label);
    let absent = h.root.join("absent-dict.bin");
    h.start_engine_with(&absent, None, CALL_TIMEOUT).unwrap();
    h.clear_backlog();
    h
}

#[test]
fn queued_keys_keep_preedit_commit_and_reply_order() {
    let h = &mut start("queued-keys");
    let mut messages = letters("abcdefgh");
    messages.push(key(keyval::RETURN));
    messages.extend(letters("nihaoshijie"));
    messages.push(key(keyval::RETURN));
    let replies = burst(h, messages);
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
    let h = &mut start("queued-boundaries");
    let mut messages = Vec::new();
    for boundary in ["Reset", "FocusOut", "Disable"] {
        messages.extend(letters("discard"));
        messages.push(call(boundary, ()));
        messages.extend(letters("keep"));
        messages.push(key(keyval::RETURN));
    }
    burst(h, messages);
    assert_eq!(commits(&h.settle_default()), ["keep", "keep", "keep"]);
}

#[test]
fn queued_content_type_applies_at_the_password_boundary() {
    let h = &mut start("queued-password");
    let mut messages = letters("discard");
    messages.push(content_type(8));
    messages.push(key('x' as u32));
    messages.push(key(keyval::RETURN));
    messages.push(content_type(0));
    messages.extend(letters("safe"));
    messages.push(key(keyval::RETURN));
    let replies = burst(h, messages);
    assert!(!replies[8].body().deserialize::<bool>().unwrap());
    assert!(!replies[9].body().deserialize::<bool>().unwrap());
    let signals = h.settle_default();
    assert_eq!(commits(&signals), ["safe"]);
    assert!(!signals
        .iter()
        .filter(|s| s.member == "UpdatePreeditText")
        .any(|s| s.as_text().is_some_and(|s| s.contains('x'))));
}
