//! 英文通道走到 D-Bus 线上：提交文本、前导空格、原文逃生与关闭语义。
//!
//! 单元测试证明会话状态机正确；这一层证明**线上看到的东西**也是对的
//! （`CommitText` 的文本、候选表里的词面、`ProcessKeyEvent` 的 bool 返回值）。
//! 英文通道是可选装配，所以这里显式用 `XWX_ENGLISH*` 接线——同时这也反证
//! 了「不接线就整体关闭」。

mod support;

use std::path::{Path, PathBuf};

use support::{keyval, last_commit, last_preedit, last_table, Harness, M_CONTROL, M_RELEASE};

/// 测试用英文词库：`scripts/build_english_dict.py --entries` 的真实产物。
fn english_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/english/words.bin")
}

fn start_english_harness(label: &str) -> Harness {
    let mut h = Harness::new(label).with_env(
        "XWX_ENGLISH_DICT_PATH",
        english_fixture().to_string_lossy().to_string(),
    );
    h.start_engine(support::shared_fixture());
    h
}

/// 敲一串小写字母，返回这一串产生的信号。
fn type_word(h: &mut Harness, word: &str) -> Vec<support::Signal> {
    h.type_ascii(word);
    h.settle_default()
}

/// 从一批信号里取候选词面（线上看到的那一份）。
fn cands_of(sigs: &[support::Signal]) -> Vec<String> {
    last_table(sigs).map(|t| t.candidates).unwrap_or_default()
}

#[test]
fn english_prefix_shows_english_candidates_on_the_wire() {
    let h = &mut start_english_harness("english-wire-prefix");
    let sigs = type_word(h, "pyth");
    let cands = cands_of(&sigs);
    assert!(
        cands.iter().any(|c| c == "Python"),
        "线上候选里应看到规范大小写的 Python，实际 {cands:?}"
    );
}

#[test]
fn english_selection_commits_the_display_form() {
    let h = &mut start_english_harness("english-wire-commit");
    let sigs = type_word(h, "hello");
    let cands = cands_of(&sigs);
    assert!(cands.contains(&"hello".to_string()), "应看到 hello: {cands:?}");
    h.key(keyval::SPACE, 0);
    let sigs = h.settle_default();
    assert_eq!(last_commit(&sigs).as_deref(), Some("hello"), "空格应上屏 hello");
    assert!(
        last_preedit(&sigs).as_deref().unwrap_or("").is_empty(),
        "上屏后预编辑区应清空"
    );
}

#[test]
fn second_english_word_arrives_with_one_leading_space() {
    let h = &mut start_english_harness("english-wire-space");
    type_word(h, "hello");
    h.key(keyval::SPACE, 0);
    assert_eq!(last_commit(&h.settle_default()).as_deref(), Some("hello"));

    let _ = type_word(h, "world");
    h.key(keyval::SPACE, 0);
    assert_eq!(
        last_commit(&h.settle_default()).as_deref(),
        Some(" world"),
        "第二个英文词前恰好一个空格（线上文本）"
    );
}

#[test]
fn enter_commits_the_raw_letters_not_a_completion() {
    let h = &mut start_english_harness("english-wire-enter");
    let _ = type_word(h, "pyth");
    h.key(keyval::RETURN, 0);
    assert_eq!(
        last_commit(&h.settle_default()).as_deref(),
        Some("pyth"),
        "Enter 是原文逃生通道：不补全、不加空格"
    );
}

#[test]
fn key_release_inside_an_english_run_does_not_break_the_next_word_pair() {
    // S12 的线上形态：release 事件透传但不失效英文上文，所以第二个词
    // 仍然拿到前导空格（上文还在）。
    let h = &mut start_english_harness("english-wire-release");
    type_word(h, "hello");
    h.key(keyval::SPACE, 0);
    h.settle_default();
    // 每个字母都发一次 release（daemon 的真实行为）。
    for c in "world".chars() {
        h.key(keyval::ascii(c), M_RELEASE);
    }
    h.settle_default();
    let _ = type_word(h, "world");
    h.key(keyval::SPACE, 0);
    assert_eq!(
        last_commit(&h.settle_default()).as_deref(),
        Some(" world"),
        "release 不该打断英文上文"
    );
}

#[test]
fn english_mode_passthrough_does_not_commit() {
    // Ctrl+空格进英文模式后，字母原样透传（返回 false），输入法不上屏。
    let h = &mut start_english_harness("english-wire-english-mode");
    assert!(h.key(keyval::SPACE, M_CONTROL), "Ctrl+空格应被消费");
    h.settle_default();
    assert!(!h.key(keyval::ascii('h'), 0), "英文模式下字母必须透传给应用");
    let sigs = h.settle_default();
    assert_eq!(last_commit(&sigs), None, "透传不该产生 CommitText");
}

#[test]
fn english_master_switch_off_never_shows_english_candidates() {
    // XWX_ENGLISH=0 ⇒ 英文整体关闭（连词库都不打开）。
    let mut h = Harness::new("english-wire-switch-off")
        .with_env("XWX_ENGLISH", "0")
        .with_env(
            "XWX_ENGLISH_DICT_PATH",
            english_fixture().to_string_lossy().to_string(),
        );
    h.start_engine(support::shared_fixture());
    let sigs = type_word(&mut h, "pyth");
    let cands = cands_of(&sigs);
    assert!(
        !cands.iter().any(|c| c == "Python"),
        "总开关关闭时不该出现英文候选，实际 {cands:?}"
    );
}

#[test]
fn missing_english_dict_degrades_without_breaking_chinese() {
    // 词库缺失 ⇒ 只关英文并告警，中文路径照常。用一个不存在的路径。
    let mut h = Harness::new("english-wire-missing-dict").with_env(
        "XWX_ENGLISH_DICT_PATH",
        "/nonexistent/english.bin".to_string(),
    );
    h.start_engine(support::shared_fixture());
    let sigs = type_word(&mut h, "pyth");
    let cands = cands_of(&sigs);
    assert!(!cands.iter().any(|c| c == "Python"), "缺词库时没有英文候选");
    // 中文仍然可用：夹具里的词照常召回。
    for _ in 0..4 {
        h.key(keyval::BACKSPACE, 0);
    }
    h.settle_default();
    let sigs = type_word(&mut h, "nihao");
    let cands = cands_of(&sigs);
    assert!(
        !cands.is_empty(),
        "英文降级不得牵连中文候选，实际 {cands:?}"
    );
}

#[test]
fn idle_english_key_keeps_letters_away_from_the_engine() {
    // 空闲时的普通字母是组合输入，应被输入法消费（返回 true）。
    let h = &mut start_english_harness("english-wire-consumed");
    assert!(h.key(keyval::ascii('a'), 0), "字母应被输入法消费");
    assert!(
        !h.settle_default().is_empty(),
        "消费掉的字母至少要发一次候选信号"
    );
}
