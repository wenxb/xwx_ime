//! M2 零输入预测态验收（N 组），全部走 `handle`。

use super::*;
use crate::english::{
    build_synth_bigram as bigram, build_synth_bin, BigramEdgeSpec as E, EnglishBigram, EnglishDict,
    EnglishEngine, SynthWord as W,
};

fn words() -> Vec<W> {
    vec![
        W { key: "hello", display: "hello", q: 450 },
        W { key: "help", display: "help", q: 520 },
        W { key: "thank", display: "thank", q: 480 },
        W { key: "you", display: "you", q: 700 },
        W { key: "your", display: "your", q: 560 },
        W { key: "yours", display: "yours", q: 400 },
        W { key: "too", display: "too", q: 600 },
        W { key: "them", display: "them", q: 500 },
        W { key: "there", display: "there", q: 470 },
        W { key: "world", display: "world", q: 490 },
    ]
}

/// `thank` → you/your/yours/them；`you` → too（链式预测）；`hello` → world/there。
fn edges() -> Vec<E> {
    vec![
        E { prev: "thank", next: "you", count: 733_376_704 },
        E { prev: "thank", next: "your", count: 20_000_000 },
        E { prev: "thank", next: "yours", count: 1_000_000 },
        E { prev: "thank", next: "them", count: 500_000 },
        E { prev: "you", next: "too", count: 50_000_000 },
        E { prev: "hello", next: "world", count: 7_852_480 },
        E { prev: "hello", next: "there", count: 120_000 },
    ]
}

fn predicting_session() -> InputSession {
    let dict = EnglishDict::from_bytes(build_synth_bin(&words())).expect("合成英文词库");
    let model = EnglishBigram::from_bytes(bigram(&edges())).expect("合成二元模型");
    let engine = ImeEngine::new(None)
        .expect("内存引擎")
        .with_english(
            EnglishEngine::new(dict)
                .with_user_lm(crate::UserLM::in_memory())
                .with_bigram(model)
                .with_predict(true),
        );
    InputSession::new(Arc::new(engine))
}

fn predicting_session_without_static() -> InputSession {
    let dict = EnglishDict::from_bytes(build_synth_bin(&words())).expect("合成英文词库");
    let engine = ImeEngine::new(None)
        .expect("内存引擎")
        .with_english(
            EnglishEngine::new(dict)
                .with_user_lm(crate::UserLM::in_memory())
                .with_predict(true),
        );
    InputSession::new(Arc::new(engine))
}

fn type_and_select(s: &mut InputSession, text: &str) -> SessionOutput {
    type_str(s, text);
    s.handle(InputEvent::Space).expect("选词")
}

fn prediction_words(s: &InputSession) -> Vec<String> {
    s.prediction.iter().map(|c| c.word.clone()).collect()
}

fn committed(out: &SessionOutput) -> Option<&str> {
    match out {
        SessionOutput::Commit(t) => Some(t),
        SessionOutput::CommitAndUpdate { text, .. } => Some(text),
        _ => None,
    }
}

#[test]
fn n01_selecting_an_english_word_opens_the_prediction_panel() {
    let mut s = predicting_session();
    let out = type_and_select(&mut s, "thank");
    let SessionOutput::CommitAndUpdate { text, update } = out else {
        panic!("期望 CommitAndUpdate，实际 {out:?}");
    };
    assert_eq!(text, "thank");
    assert_eq!(update.preedit, "", "预测面板没有拼音组合");
    assert_eq!((update.caret, update.cursor, update.page, update.page_start()), (0, 0, 0, 0));
    assert_eq!(update.candidates.len(), 4);
    assert_eq!(update.page_candidates().len(), 4);

    assert!(s.predicting());
    assert_eq!(prediction_words(&s), vec!["you", "your", "yours", "them"]);
    assert!(s.prediction.iter().all(|c| c.is_next_word() && c.covered == 0));
    assert_eq!(s.prediction_cursor, 0);
    assert!(s.pinyin.is_empty() && s.confirmed.is_empty() && !s.english_mode);

    // 展示不学习、不改上文、不消耗前导空格。
    let snap = s.engine.english().expect("英文").user().snapshot();
    for k in ["you", "your", "yours", "them"] {
        assert_eq!(snap.english_unigram_count(k), 0, "{k} 不该因为展示而被学习");
    }
    assert_eq!(s.english.previous_word(), Some("thank"));
    assert!(s.english.needs_space(), "前导空格仍待输出");
}

#[test]
fn n02_tab_commits_the_highlighted_next_word_with_the_deferred_space() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    s.handle(InputEvent::Down).expect("移动高亮");
    assert_eq!(prediction_words(&s)[s.prediction_cursor], "your");
    let out = s.handle(InputEvent::Tab).expect("Tab");
    assert_eq!(committed(&out), Some(" your"), "沿用延迟空格：只加一个前导空格");
    let snap = s.engine.english().expect("应装配英文").user().snapshot();
    assert_eq!(snap.english_unigram_count("your"), 1);
    assert_eq!(snap.english_bigram_count("thank", "your"), 1);
    assert!(!s.predicting(), "your 没有后继时退回空闲态");
}

#[test]
fn n02b_tab_continues_predicting_when_there_is_a_follower() {
    // 有后继：thank → you → too，Tab 提交 " you" 后继续预测 ["too"]。
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    assert_eq!(prediction_words(&s)[0], "you");
    match s.handle(InputEvent::Tab).expect("Tab") {
        SessionOutput::CommitAndUpdate { text, update } => {
            assert_eq!(text, " you");
            assert_eq!(update.preedit, "");
            assert_eq!(update.candidates.len(), 1);
        }
        other => panic!("有后继时 Tab 应返回 CommitAndUpdate，实际 {other:?}"),
    }
    assert!(s.predicting(), "you 有后继 too → 应继续预测");
    assert_eq!(prediction_words(&s), vec!["too"]);

    // 无后继：hello → world，Tab 提交 " world" 后结束预测。
    let mut s = predicting_session();
    type_and_select(&mut s, "hello");
    assert_eq!(prediction_words(&s), vec!["world", "there"]);
    match s.handle(InputEvent::Tab).expect("Tab") {
        SessionOutput::Commit(text) => assert_eq!(text, " world"),
        other => panic!("无后继时 Tab 应返回 Commit，实际 {other:?}"),
    }
    assert!(!s.predicting(), "world 没有后继 → 停止预测");
}

#[test]
fn n03_digit_selects_and_out_of_range_digit_passes_through() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Select(2)).expect("数字选词");
    assert_eq!(committed(&out), Some(" your"));
    assert!(!s.predicting(), "your 没有后继 → 不再预测");

    for digit in [0u8, 5u8] {
        let mut s = predicting_session();
        type_and_select(&mut s, "thank");
        let out = s.handle(InputEvent::Select(digit)).expect("越界数字");
        assert!(matches!(out, SessionOutput::Noop), "数字 {digit} 应关闭预测并透传");
        assert!(!s.predicting());
        assert!(s.english.previous_word().is_none(), "越界数字要清掉英文上文");
    }
}

#[test]
fn n04_space_closes_the_panel_keeps_context_and_forwards_one_space() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Space).expect("空格");
    assert!(matches!(out, SessionOutput::Noop), "空格应透传给应用: {out:?}");
    assert!(!s.predicting());
    assert_eq!(s.english.previous_word(), Some("thank"));
    let en = s.engine.english().expect("英文");
    assert_eq!(en.user().snapshot().english_bigram_count("thank", "you"), 0);
}

#[test]
fn n05_enter_backspace_and_arrows_close_and_clear_context_but_escape_consumes() {
    for event in [
        InputEvent::Enter,
        InputEvent::Backspace,
        InputEvent::Left,
        InputEvent::Right,
    ] {
        let mut s = predicting_session();
        type_and_select(&mut s, "thank");
        let out = s.handle(event).expect("按键");
        assert!(matches!(out, SessionOutput::Noop), "{event:?} 应透传: {out:?}");
        assert!(!s.predicting());
        assert!(s.english.previous_word().is_none());
    }
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Escape).expect("Esc");
    assert!(matches!(out, SessionOutput::Updated(_)), "Esc 应被消费: {out:?}");
    assert!(!s.predicting());
    assert!(s.english.previous_word().is_none());
}

#[test]
fn n06_a_letter_closes_the_panel_but_keeps_the_context_for_completion() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Char('y')).expect("字母");
    assert!(matches!(out, SessionOutput::Updated(_)), "实际 {out:?}");
    assert!(!s.predicting());
    assert_eq!(s.pinyin, "y");
    assert_eq!(s.english.previous_word(), Some("thank"));
}

#[test]
fn n06b_an_unknown_key_that_passes_through_reports_the_panel_hiding() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    // 无映射字符 `+` 先关预测态再落回普通透传 Noop → 必须包装成 RefreshAndForward。
    let out = s.handle(InputEvent::Char('+')).expect("透传字符");
    let SessionOutput::RefreshAndForward(u) = out else {
        panic!("预测关闭后透传应返回 RefreshAndForward，实际 {out:?}");
    };
    assert!(u.preedit.is_empty() && u.candidates.is_empty(), "应通知 UI 隐藏空面板");
    assert!(!s.predicting());
    assert!(s.english.previous_word().is_none());
}

#[test]
fn n07_toggle_ascii_clears_everything_and_returns_an_empty_view() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let SessionOutput::Updated(u) = s.handle(InputEvent::ToggleAscii).expect("Ctrl+空格") else {
        panic!("ToggleAscii 必须返回空视图 Updated");
    };
    assert!(u.preedit.is_empty() && u.candidates.is_empty());
    assert!(!s.predicting() && s.english.previous_word().is_none() && s.english_mode());
}

#[test]
fn n09_no_sources_means_no_panel_and_no_global_filler() {
    let mut s = predicting_session();
    type_and_select(&mut s, "hello");
    s.handle(InputEvent::Select(1)).expect("选 world");
    assert!(!s.predicting(), "两种来源都没有时不得展示面板");

    // 无静态数据时，真实学过的词对仍能预测。
    let mut s = predicting_session_without_static();
    s.engine
        .english()
        .expect("英文")
        .user()
        .learn(Some("thank"), "you");
    type_and_select(&mut s, "thank");
    assert!(s.predicting());
    assert_eq!(prediction_words(&s), vec!["you"]);
}

#[test]
fn n10_ctrl_delete_blocks_the_predicted_target_and_recomputes() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    assert_eq!(prediction_words(&s).first().map(String::as_str), Some("you"));
    assert!(matches!(
        s.handle(InputEvent::DeleteCandidate).expect("Ctrl+Delete"),
        SessionOutput::Updated(_)
    ));
    assert!(!prediction_words(&s).contains(&"you".to_string()));
    assert!(s.predicting(), "还有其他下一词，面板保留");

    assert!(matches!(
        s.handle(InputEvent::UnblockCandidate).expect("撤销"),
        SessionOutput::Updated(_)
    ));
    assert_eq!(prediction_words(&s).first().map(String::as_str), Some("you"));
}

#[test]
fn n11_tab_is_noop_outside_prediction() {
    let mut s = predicting_session();
    assert!(matches!(s.handle(InputEvent::Tab).expect("Tab"), SessionOutput::Noop));
    type_str(&mut s, "than");
    assert!(matches!(s.handle(InputEvent::Tab).expect("Tab"), SessionOutput::Noop));
    assert_eq!(s.pinyin, "than");
}

#[test]
fn n12_focus_reset_drops_the_panel_without_committing_it() {
    let mut s = predicting_session();
    assert!(!s.is_active() && !s.has_visible_ui());
    type_and_select(&mut s, "thank");
    assert!(!s.is_active(), "预测态 pinyin 为空");
    assert!(s.has_visible_ui() && s.predicting());

    let pending = s.reset_for_focus_change();
    assert!(pending.is_empty(), "预测项不是确认段，不得提交");
    assert!(!s.predicting() && !s.has_visible_ui() && s.english.previous_word().is_none());
}

#[test]
fn n13_moving_the_highlight_and_paging_clamps() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 1);
    s.handle(InputEvent::Down).expect("Down");
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 3);
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 3, "越界钳位不绕回");

    s.handle(InputEvent::PageDown).expect("PageDown");
    assert_eq!(s.prediction_cursor, 3);
    s.handle(InputEvent::PageUp).expect("PageUp");
    assert_eq!(s.prediction_cursor, 0);
    s.handle(InputEvent::PageUp).expect("PageUp");
    assert_eq!(s.prediction_cursor, 0);

    let en = s.engine.english().expect("英文");
    assert_eq!(en.user().snapshot().english_unigram_count("you"), 0);
}
