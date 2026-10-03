//! `www.` 的「被翻页吃掉的那个点」只能凭**真实敲过的点**补回。
//!
//! 旧实现靠串形状猜(`www`/`ftp` 开头 + 串内无点 + 补点后命中 url),
//! 于是没敲点的 `wwwx` 也被补成 `www.x`——凭空造出用户没输入的字符;
//! 删掉的点也会在下一个字母到来时被重新补回。
//!
//! 真实词库用例:词库缺失时跳过(与 `prefix_mixed.rs` 同口径)。

use std::path::Path;

use super::*;

fn real_session() -> Option<InputSession> {
    let dict_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !dict_path.is_file() {
        return None;
    }
    let engine = ImeEngine::new(Some(&dict_path)).expect("词库加载失败");
    Some(InputSession::new(Arc::new(engine)))
}

fn type_str(s: &mut InputSession, text: &str) {
    for ch in text.chars() {
        s.handle(InputEvent::Char(ch)).expect("不应失败");
    }
}

/// 没敲过点:串必须逐字节保持原样,不许冒出 `.`。
#[test]
fn typing_www_without_a_dot_never_invents_one() {
    let Some(mut s) = real_session() else {
        return;
    };
    type_str(&mut s, "wwwx");
    assert_eq!(s.pinyin(), "wwwx", "查询刷新不许修改原始输入");
    assert_eq!(s.recognized, None, "没敲点就不该进 URL 识别态");
}

/// 真敲过 `www.` 再打字母:点补回串里,进识别态(既有能力不回退)。
#[test]
fn a_really_typed_dot_is_restored() {
    let Some(mut s) = real_session() else {
        return;
    };
    type_str(&mut s, "www");
    type_str(&mut s, ".");
    type_str(&mut s, "e");
    assert_eq!(s.pinyin(), "www.e", "真敲过的点应被补回");
    assert!(s.recognized.is_some(), "`www.e` 应进 URL 识别态");
}

/// 删掉的点不许复活:退格删点后再打字母,串里不能又出现 `.`。
#[test]
fn a_deleted_dot_does_not_come_back() {
    let Some(mut s) = real_session() else {
        return;
    };
    type_str(&mut s, "www");
    type_str(&mut s, ".");
    type_str(&mut s, "e");
    assert_eq!(s.pinyin(), "www.e");

    s.handle(InputEvent::Backspace).expect("不应失败"); // 删 e
    s.handle(InputEvent::Backspace).expect("不应失败"); // 删 .
    assert_eq!(s.pinyin(), "www");
    type_str(&mut s, "x");
    assert_eq!(s.pinyin(), "wwwx", "删掉的点不许被重新补回");
    assert_eq!(s.recognized, None);
}
