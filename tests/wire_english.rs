//! 英文通道走到 D-Bus 线上：提交文本、前导空格、原文逃生与关闭语义。
//!
//! 单元测试证明会话状态机正确；这一层证明**线上看到的东西**也是对的
//! （`CommitText` 的文本、候选表里的词面、`ProcessKeyEvent` 的 bool 返回值）。
//! 英文通道是可选装配，所以这里显式用 `XWX_ENGLISH*` 接线——同时这也反证
//! 了「不接线就整体关闭」。

mod support;

use std::path::{Path, PathBuf};

use support::{keyval, last_commit, last_preedit, last_table, Harness, M_CONTROL, M_RELEASE};

/// 预测面板专用的词库与二元表（静态 `thank → you` 等）。
fn prediction_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/english")
        .join(name)
}

fn start_prediction_harness(label: &str) -> Harness {
    let mut h = Harness::new(label)
        .with_env("XWX_ENGLISH", "1")
        .with_env(
            "XWX_ENGLISH_DICT_PATH",
            english_fixture().to_string_lossy().to_string(),
        )
        .with_env(
            "XWX_ENGLISH_BIGRAM_PATH",
            prediction_fixture("bigram.bin").to_string_lossy().to_string(),
        )
        .with_env("XWX_ENGLISH_PREDICT", "1");
    h.start_engine(support::shared_fixture());
    h
}

/// 打完一个词并用空格选中，返回信号。
fn select_word(h: &mut Harness, word: &str) -> Vec<support::Signal> {
    h.type_ascii(word);
    h.settle_default();
    h.key(keyval::SPACE, 0);
    h.settle_default()
}

/// 测试用英文词库：`scripts/build_english_dict.py --entries` 的真实产物。
fn english_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/english/words.bin")
}

fn start_english_harness(label: &str) -> Harness {
    let mut h = Harness::new(label)
        .with_env("XWX_ENGLISH", "1")
        .with_env(
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
fn i05_sigint_and_sigterm_both_save_the_english_tail_counts() {
    // 英文尾部计数必须和中文一样在退出时落盘：commit 一个英文词（dirty=1，
    // 远低于 64 的批量阈值），然后 SIGTERM，检查英文用户文件里真的有它。
    let h = &mut start_english_harness("english-wire-flush");
    let sigs = type_word(h, "hello");
    assert!(cands_of(&sigs).contains(&"hello".to_string()));
    h.key(keyval::SPACE, 0);
    h.settle_default();
    // 用 SIGTERM 走 `sigterm.recv()` 分支（Ctrl+C 分支由既有 hermeticity 测试覆盖）。
    let pid = h.engine_pid_for_test().to_string();
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(800));

    let user_file = h.root.join("cfg/user_english.txt");
    let text = std::fs::read_to_string(&user_file).unwrap_or_else(|e| {
        panic!("英文用户文件应已落盘 {}: {e}", user_file.display())
    });
    assert!(
        text.contains("hello"),
        "SIGTERM 后英文用户文件里应有 hello：\n{text}"
    );
}

#[test]
fn i03_bad_english_data_still_lets_chinese_through() {
    // 坏数据（这里是「存在但不是合法 english.bin」）只关闭英文通道。
    let bad = tempfile::NamedTempFile::new().expect("临时文件");
    std::fs::write(bad.path(), b"not an english.bin at all").expect("写坏数据");
    let mut h = Harness::new("english-wire-bad-data").with_env(
        "XWX_ENGLISH_DICT_PATH",
        bad.path().to_string_lossy().to_string(),
    );
    h.start_engine(support::shared_fixture());
    let sigs = type_word(&mut h, "nihao");
    assert!(
        !cands_of(&sigs).is_empty(),
        "坏英文数据不得影响中文候选"
    );
    assert!(
        !cands_of(&sigs).iter().any(|c| c == "hello"),
        "坏数据下不该有英文候选"
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

// ---------- M2：零输入下一词面板（预测分册第 8、11 节）----------

#[test]
fn m2_selecting_thank_shows_a_prediction_panel_with_empty_preedit() {
    let h = &mut start_prediction_harness("english-wire-predict-panel");
    let sigs = select_word(h, "thank");
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    // 面板：空 preedit + 下一词候选
    assert_eq!(last_preedit(&sigs).as_deref(), Some(""), "预测态没有拼音组合");
    let cands = cands_of(&sigs);
    assert!(cands.contains(&"you".to_string()), "面板应显示 you: {cands:?}");
    assert!(
        cands.first().map(String::as_str) == Some("you"),
        "静态 thank→you 应排第一: {cands:?}"
    );
}

#[test]
fn m2_tab_commits_the_highlighted_next_word_with_one_leading_space() {
    let h = &mut start_prediction_harness("english-wire-predict-tab");
    select_word(h, "thank");
    let consumed = h.key(keyval::TAB, 0);
    assert!(consumed, "Tab 接受下一词必须被消费");
    let sigs = h.settle_default();
    assert_eq!(
        last_commit(&sigs).as_deref(),
        Some(" you"),
        "延迟空格：只加一个前导空格"
    );
}

#[test]
fn m2_space_on_the_panel_is_forwarded_as_exactly_one_space() {
    let h = &mut start_prediction_harness("english-wire-predict-space");
    select_word(h, "thank");
    let consumed = h.key(keyval::SPACE, 0);
    assert!(!consumed, "面板上的空格必须交给应用（返回 false）");
    let sigs = h.settle_default();
    assert!(
        last_commit(&sigs).is_none(),
        "空格不得触发任何提交（否则应用会收到两个空格），实际 {:?}",
        last_commit(&sigs)
    );
}

#[test]
fn m2_shift_tab_passes_through_instead_of_accepting_the_prediction() {
    let h = &mut start_prediction_harness("english-wire-predict-shifttab");
    select_word(h, "thank");
    let consumed = h.key(keyval::TAB, support::M_SHIFT);
    assert!(!consumed, "Shift+Tab 必须原样透传给应用");
    let sigs = h.settle_default();
    assert!(
        last_commit(&sigs).is_none(),
        "Shift+Tab 不得接受预测，实际提交了 {:?}",
        last_commit(&sigs)
    );
}

#[test]
fn m2_predict_switch_off_means_no_panel_at_all() {
    let mut h = Harness::new("english-wire-predict-off")
        .with_env("XWX_ENGLISH", "1")
        .with_env(
            "XWX_ENGLISH_DICT_PATH",
            english_fixture().to_string_lossy().to_string(),
        )
        .with_env(
            "XWX_ENGLISH_BIGRAM_PATH",
            prediction_fixture("bigram.bin").to_string_lossy().to_string(),
        )
        .with_env("XWX_ENGLISH_PREDICT", "0");
    h.start_engine(support::shared_fixture());
    let sigs = select_word(&mut h, "thank");
    assert_eq!(last_commit(&sigs).as_deref(), Some("thank"));
    // 关掉预测面板之后，空 preedit 的候选表要么没有、要么为空
    let cands = last_table(&sigs).map(|t| t.candidates).unwrap_or_default();
    assert!(cands.is_empty(), "关闭预测后不得出现下一词面板: {cands:?}");
}

#[test]
fn m2_typing_a_letter_after_the_panel_returns_to_normal_composition() {
    let h = &mut start_prediction_harness("english-wire-predict-letter");
    select_word(h, "thank");
    let consumed = h.key(keyval::ascii('y'), 0);
    assert!(consumed, "字母要被输入法接住");
    let sigs = h.settle_default();
    let preedit = last_preedit(&sigs).unwrap_or_default();
    assert_eq!(preedit, "y", "字母回到普通组合查询");
}

#[test]
fn m2_focus_out_drops_the_panel_without_committing_it() {
    let h = &mut start_prediction_harness("english-wire-predict-focus");
    select_word(h, "thank");
    h.focus_out().expect("FocusOut");
    let sigs = h.settle_default();
    assert!(
        last_commit(&sigs).is_none(),
        "焦点清算不得提交当前高亮的 you，实际 {:?}",
        last_commit(&sigs)
    );
}

#[test]
fn m2_broken_bigram_only_disables_the_static_source() {
    // 坏二元文件：英文补全与用户学习照常，只是没有静态条件排序
    let mut h = Harness::new("english-wire-predict-badbigram")
        .with_env("XWX_ENGLISH", "1")
        .with_env(
            "XWX_ENGLISH_DICT_PATH",
            english_fixture().to_string_lossy().to_string(),
        )
        .with_env(
            "XWX_ENGLISH_BIGRAM_PATH",
            prediction_fixture("words.bin").to_string_lossy().to_string(),
        )
        .with_env("XWX_ENGLISH_PREDICT", "1");
    h.start_engine(support::shared_fixture());
    let sigs = type_word(&mut h, "pyth");
    let cands = cands_of(&sigs);
    assert!(
        cands.iter().any(|c| c == "Python"),
        "坏二元文件不得影响前缀补全: {cands:?}"
    );
}
