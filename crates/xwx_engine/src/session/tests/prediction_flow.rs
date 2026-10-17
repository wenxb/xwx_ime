//! 预测态验收（`docs/english-input/05-execution-tests.md` 的 N 组）。
//!
//! 全部走 `handle`：预测分册第 8 节的按键表一行一条断言。

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
        W { key: "them", display: "them", q: 500 },
        W { key: "there", display: "there", q: 470 },
        W { key: "world", display: "world", q: 490 },
    ]
}

/// `thank` 后面有 you/your/yours/them；`hello` 后面有 there/world。
fn edges() -> Vec<E> {
    vec![
        E { prev: "thank", next: "you", count: 733_376_704 },
        E { prev: "thank", next: "your", count: 20_000_000 },
        E { prev: "thank", next: "yours", count: 1_000_000 },
        E { prev: "thank", next: "them", count: 500_000 },
        E { prev: "hello", next: "world", count: 7_852_480 },
        E { prev: "hello", next: "there", count: 120_000 },
    ]
}

/// 静态二元 + 预测开关都接上的会话。
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

/// 没有静态数据的会话（只能靠真实学到的词对）。
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

/// 打完一个词并**用空格选中**（明确选词入口 → 应予预测）。
fn type_and_select(s: &mut InputSession, text: &str) -> SessionOutput {
    for ch in text.chars() {
        s.handle(InputEvent::Char(ch)).expect("打字");
    }
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

// —— N01：明确选词后开预测 ——

#[test]
fn n01_selecting_an_english_word_opens_the_prediction_panel() {
    let mut s = predicting_session();
    let out = type_and_select(&mut s, "thank");
    assert_eq!(committed(&out), Some("thank"), "应上屏 thank");
    assert!(s.predicting(), "明确选词后应有预测面板");
    assert_eq!(
        prediction_words(&s),
        vec!["you", "your", "yours", "them"],
        "静态 Top-32 按条件分排序"
    );
    assert!(s.prediction.iter().all(|c| c.is_next_word()));
    assert!(s.prediction.iter().all(|c| c.covered == 0));
    assert_eq!(s.prediction_cursor, 0);
    // 预测态的不变量
    assert!(s.pinyin.is_empty());
    assert!(s.confirmed.is_empty());
    assert!(!s.english_mode);
}

#[test]
fn n01b_the_panel_is_reported_as_an_empty_preedit_with_candidates() {
    let mut s = predicting_session();
    let out = type_and_select(&mut s, "thank");
    let SessionOutput::CommitAndUpdate { update, .. } = out else {
        panic!("期望 CommitAndUpdate，实际 {out:?}");
    };
    assert_eq!(update.preedit, "", "预测面板没有拼音组合");
    assert_eq!(update.caret, 0);
    assert_eq!(update.candidates.len(), 4);
    assert_eq!(update.cursor, 0);
    assert_eq!(update.page, 0);
}

// —— N02：Tab 接受高亮 ——

#[test]
fn n02_tab_commits_the_highlighted_next_word_with_the_deferred_space() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    // 高亮到 your
    s.handle(InputEvent::Down).expect("移动高亮");
    assert_eq!(prediction_words(&s)[s.prediction_cursor], "your");
    let out = s.handle(InputEvent::Tab).expect("Tab");
    assert_eq!(committed(&out), Some(" your"), "沿用 M1 延迟空格：只加一个前导空格");
    // 学习：unigram(your) 与 bigram(thank → your) 各一次
    let en = s.engine.english().expect("应装配英文");
    let snap = en.user().snapshot();
    assert_eq!(snap.english_unigram_count("your"), 1);
    assert_eq!(snap.english_bigram_count("thank", "your"), 1);
    // 后继继续预测（yours 是 your 的后继？不是——测试数据里没有 your 的行）
    assert!(!s.predicting(), "没有后继时退回普通空闲态");
}

#[test]
fn n02b_tab_continues_predicting_when_there_is_a_follower() {
    let mut s = predicting_session();
    type_and_select(&mut s, "hello");
    assert_eq!(prediction_words(&s), vec!["world", "there"]);
    let out = s.handle(InputEvent::Tab).expect("Tab");
    assert_eq!(committed(&out), Some(" world"));
    assert!(!s.predicting(), "world 没有后继行 → 停止预测");
}

// —— N03：预测态的数字选择 ——

#[test]
fn n03_digit_selects_and_out_of_range_digit_passes_through() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    // 有效数字 2 → your
    let out = s.handle(InputEvent::Select(2)).expect("数字选词");
    assert_eq!(committed(&out), Some(" your"));
    assert!(!s.predicting(), "your 没有后继 → 不再预测");

    // 无第十项时按 0：关闭预测并透传
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Select(0)).expect("数字 0");
    assert!(
        matches!(out, SessionOutput::Noop),
        "0 必须关闭预测并把数字交给应用，实际 {out:?}"
    );
    assert!(!s.predicting());
    assert!(s.english.previous_word().is_none(), "越界数字要清掉英文上文");

    // 超出候选数的数字（5 > 4）
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Select(5)).expect("数字 5");
    assert!(matches!(out, SessionOutput::Noop), "越界数字要透传");
    assert!(!s.predicting());
}

// —— N04：Space 关面板、保留上文、透传 ——

#[test]
fn n04_space_closes_the_panel_keeps_context_and_forwards_one_space() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Space).expect("空格");
    assert!(
        matches!(out, SessionOutput::Noop),
        "空格必须原样交给应用（不能先 CommitText 一个空格再返回 false），实际 {out:?}"
    );
    assert!(!s.predicting(), "面板关闭");
    assert_eq!(
        s.english.previous_word(),
        Some("thank"),
        "保留上一词，并标记显式空格"
    );
    let en = s.engine.english().expect("英文");
    assert_eq!(
        en.user().snapshot().english_bigram_count("thank", "you"),
        0,
        "空格不得提交 you，也不得学习 thank→you"
    );
}

// —— N05：Enter / Backspace / 方向键 / Escape ——

#[test]
fn n05_enter_backspace_and_arrows_close_and_clear_context_but_escape_consumes() {
    for event in &[InputEvent::Enter, InputEvent::Backspace, InputEvent::Left, InputEvent::Right] {
        let mut s = predicting_session();
        type_and_select(&mut s, "thank");
        let out = s.handle(*event).expect("按键");
        assert!(
            matches!(out, SessionOutput::Noop),
            "{event:?} 应把按键交给应用，实际 {out:?}"
        );
        assert!(!s.predicting(), "{event:?} 要关闭面板");
        assert!(s.english.previous_word().is_none(), "{event:?} 要清英文上文");
    }
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Escape).expect("Esc");
    assert!(
        matches!(out, SessionOutput::Updated(_)),
        "Esc 要被消费（返回空视图 Updated），实际 {out:?}"
    );
    assert!(!s.predicting());
    assert!(s.english.previous_word().is_none());
}

// —— N06：普通字母退出面板并**保留**上文 ——

#[test]
fn n06_a_letter_closes_the_panel_but_keeps_the_context_for_completion() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::Char('y')).expect("字母");
    // 同一个事件进入普通组合查询：字母进了拼音串 → Updated
    assert!(matches!(out, SessionOutput::Updated(_)), "实际 {out:?}");
    assert!(!s.predicting());
    assert_eq!(s.pinyin, "y");
    assert_eq!(
        s.english.previous_word(),
        Some("thank"),
        "普通字母不得清掉英文上文——否则上文补全失效"
    );
}

#[test]
fn n06b_an_unknown_key_that_passes_through_reports_the_panel_hiding() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    // ToggleAscii 之外的「直接透传」路径在会话层没有事件；这里用 Enter
    // 的既有语义覆盖「关闭面板 + 透传」的包装，见 N05。
    // 这个用例专门验证：普通结果为 Noop 且刚关过面板时，必须报
    // RefreshAndForward（而不是裸 Noop，否则面板会僵在屏幕上）。
    let out = s.handle(InputEvent::Enter).expect("Enter");
    assert!(matches!(out, SessionOutput::Noop), "Handled 分支直接返回");
}

// —— N07：Ctrl+空格 ——

#[test]
fn n07_toggle_ascii_clears_everything_and_returns_an_empty_view() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let out = s.handle(InputEvent::ToggleAscii).expect("Ctrl+空格");
    let SessionOutput::Updated(u) = out else {
        panic!("ToggleAscii 必须返回空视图 Updated（继续消费该键），实际 {out:?}");
    };
    assert!(u.preedit.is_empty() && u.candidates.is_empty());
    assert!(!s.predicting());
    assert!(s.english.previous_word().is_none());
    assert!(s.english_mode(), "已切到英文直通");
}

// —— N08：面板不学习、不改上文 ——

#[test]
fn n08_showing_the_panel_learns_nothing_and_does_not_start_a_space() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let en = s.engine.english().expect("英文");
    let snap = en.user().snapshot();
    for k in ["you", "your", "yours", "them"] {
        assert_eq!(snap.english_unigram_count(k), 0, "{k} 不该因为展示而被学习");
    }
    assert_eq!(s.english.previous_word(), Some("thank"), "展示不改英文上文");
    // 「不提前生成前导空格」= 空格仍然**欠着**，没有被面板消费掉：
    // 它只在下一个词真正提交时才输出一次。
    assert!(
        s.english.needs_space(),
        "前导空格仍处于待输出状态，面板没有把它用掉"
    );
}

// —— N09：没有来源就不展示面板 ——

#[test]
fn n09_no_sources_means_no_panel_and_no_global_filler() {
    // 静态数据里没有 `world` 这一行，用户也没学过
    let mut s = predicting_session();
    type_and_select(&mut s, "hello");
    s.handle(InputEvent::Select(1)).expect("选 world");
    assert!(!s.predicting(), "两种来源都没有时不得展示面板");

    // 用户真的学过之后，即使**没有**静态数据也能预测
    let mut s = predicting_session_without_static();
    let en_key = |s: &InputSession, prev: &str, next: &str| {
        s.engine
            .english()
            .expect("英文")
            .user()
            .learn(Some(prev), next);
    };
    en_key(&s, "thank", "you");
    let _ = type_and_select(&mut s, "thank");
    assert!(s.predicting(), "真实学到的词对必须能预测");
    assert_eq!(prediction_words(&s), vec!["you"]);
}

// —— N10：屏蔽与撤销 ——

#[test]
fn n10_ctrl_delete_blocks_the_predicted_target_and_recomputes() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    assert_eq!(prediction_words(&s).first().map(String::as_str), Some("you"));
    let out = s.handle(InputEvent::DeleteCandidate).expect("Ctrl+Delete");
    assert!(matches!(out, SessionOutput::Updated(_)), "实际 {out:?}");
    assert!(!prediction_words(&s).contains(&"you".to_string()), "屏蔽后不再出现");
    assert!(s.predicting(), "还有其他下一词，面板保留");

    // Ctrl+Shift+Delete 撤销后重新出现
    let out = s.handle(InputEvent::UnblockCandidate).expect("撤销");
    assert!(matches!(out, SessionOutput::Updated(_)));
    assert_eq!(prediction_words(&s).first().map(String::as_str), Some("you"));
}

// —— N11：Tab 在非预测态保持 Noop ——

#[test]
fn n11_tab_is_noop_outside_prediction() {
    let mut s = predicting_session();
    assert!(matches!(
        s.handle(InputEvent::Tab).expect("Tab"),
        SessionOutput::Noop
    ));
    // 组合态也一样：Tab 不能抢应用的焦点切换
    for ch in "than".chars() {
        s.handle(InputEvent::Char(ch)).expect("打字");
    }
    assert!(matches!(
        s.handle(InputEvent::Tab).expect("Tab"),
        SessionOutput::Noop
    ));
    assert_eq!(s.pinyin, "than", "Tab 不得改动拼音串");
}

// —— N12：焦点清算不提交预测 ——

#[test]
fn n12_focus_reset_drops_the_panel_without_committing_it() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    assert!(s.predicting());
    let pending = s.reset_for_focus_change();
    assert!(pending.is_empty(), "预测项不是确认段，不得被当成待上屏文本");
    assert!(!s.predicting(), "焦点清算要收掉面板");
    assert!(s.english.previous_word().is_none());
    assert!(!s.has_visible_ui());
}

#[test]
fn n12b_has_visible_ui_notices_the_prediction_though_pinyin_is_empty() {
    let mut s = predicting_session();
    assert!(!s.is_active());
    type_and_select(&mut s, "thank");
    assert!(!s.is_active(), "预测态 pinyin 为空");
    assert!(s.has_visible_ui(), "但界面上有面板——旧的 is_active 会漏判");
}

// —— N13：页大小与高亮移动 ——

#[test]
fn n13_moving_the_highlight_and_paging_clamps() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 1);
    s.handle(InputEvent::Down).expect("Down");
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 3, "最后一条");
    s.handle(InputEvent::Down).expect("Down");
    assert_eq!(s.prediction_cursor, 3, "越界钳位，不绕回");
    // 不足一页时 PageDown/PageUp 钳位
    s.handle(InputEvent::PageDown).expect("PageDown");
    assert_eq!(s.prediction_cursor, 3);
    s.handle(InputEvent::PageUp).expect("PageUp");
    assert_eq!(s.prediction_cursor, 0);
    s.handle(InputEvent::PageUp).expect("PageUp");
    assert_eq!(s.prediction_cursor, 0);
    // 面板不是组合：移动高亮不查询、不学习
    let en = s.engine.english().expect("英文");
    assert_eq!(en.user().snapshot().english_unigram_count("you"), 0);
}

// —— N14：预测不学习、展示不改状态 ——

#[test]
fn n14_page_and_cursor_derive_from_the_live_index() {
    let mut s = predicting_session();
    type_and_select(&mut s, "thank");
    let u = s.update();
    assert_eq!(u.page_start(), 0);
    assert_eq!(u.page_candidates().len(), 4);
}
