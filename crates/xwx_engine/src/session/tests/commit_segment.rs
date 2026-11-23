//! 分段确认与退回上一段：确认段留在组合里、剩余串继续组合、
//! 退格回退上一次确认、组合结束时一次提交与学习。

use super::*;

#[test]
fn prefix_confirm_keeps_remainder_and_backspace_undoes_then_edits() {
    let mut s = session_with_segments();
    type_str(&mut s, "womenjintian");
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    assert_eq!(s.candidates[idx].covered, 5, "「我们」覆盖 wo men");

    match s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "我们jintian", "确认段 + 剩余串都在组合里");
            assert_eq!(u.caret, 2 + 7, "光标按字符偏移落在剩余串末尾");
            assert!(!u.candidates.is_empty(), "剩余串必须刷新候选");
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "jintian");
    assert!(!s.candidates.is_empty());

    // 第一次退格：回退上一次确认，恢复原输入串。
    match s.handle(InputEvent::Backspace).expect("不应失败") {
        SessionOutput::Updated(u) => {
            assert_eq!(u.preedit, "womenjintian");
            assert_eq!(u.caret, "womenjintian".len());
        }
        other => panic!("期望 Updated，实际 {other:?}"),
    }
    assert_eq!(s.pinyin(), "womenjintian");
    assert!(!s.candidates.is_empty());

    // 第二次退格：无确认段，删末位字母。
    match s.handle(InputEvent::Backspace).expect("不应失败") {
        SessionOutput::Updated(u) => assert_eq!(u.preedit, "womenjintia"),
        other => panic!("期望 Updated，实际 {other:?}"),
    }
}

#[test]
fn confirming_segments_defers_commit_and_learning_until_end() {
    let (engine, mut s) = session_for_coin(&[("我们", "wo men", 300), ("今天", "jin tian", 300)]);
    let user = engine.user_lm().expect("应有用户层");
    let learned = |w: &str| user.counts_with(&user.snapshot(), None, w);
    type_str(&mut s, "womenjintian");

    // ① 确认前段：留在预编辑区、零学习。
    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    let SessionOutput::Updated(u) = &out else {
        panic!("确认段不应上屏: {out:?}");
    };
    assert_eq!(u.preedit, "我们jintian");
    assert_eq!(s.pinyin(), "jintian");
    assert!(s.is_active());
    assert_eq!(learned("我们"), (0, 0), "确认段不学习");

    // ② 退格回退：依然零学习。
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("退格失败"),
        SessionOutput::Updated(_)
    ));
    assert_eq!(s.pinyin(), "womenjintian");
    assert_eq!(learned("我们"), (0, 0), "回退不留学习痕迹");

    // ③ 选到底：整串一次上屏并此时才学习。
    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    let idx = s.candidates.iter().position(|c| c.word == "今天").expect("应有「今天」");
    let out = s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们今天"),
        "整串结束时一次上屏: {out:?}"
    );
    assert!(!s.is_active());
    assert!(
        learned("我们").0 > 0 && learned("今天").0 > 0,
        "上屏时才学习: 我们={:?} 今天={:?}",
        learned("我们"),
        learned("今天")
    );
}

#[test]
fn segmented_commits_never_drop_typed_letters() {
    let mut s = session_with_segments();
    let typed = "womenjintian";
    type_str(&mut s, typed);
    let mut consumed = 0usize;
    for step in 0..8 {
        if s.pinyin().is_empty() {
            break;
        }
        let c = s
            .candidates
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("第 {step} 步：非空输入串必须有候选"));
        let covered = if c.covered == 0 { s.pinyin().len() } else { c.covered };
        s.handle(InputEvent::Space).expect("不应失败");
        consumed += covered;
        assert_eq!(
            consumed + s.pinyin().len(),
            typed.len(),
            "第 {step} 步不应丢失字母"
        );
        assert!(typed.ends_with(s.pinyin()), "剩余串应为原串后缀");
    }
}

#[test]
fn segmented_commits_of_whole_words_do_not_coin_a_phrase() {
    let (engine, mut s) = session_for_coin(&[
        ("我们", "wo men", 300),
        ("我", "wo", 500),
        ("今天", "jin tian", 300),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "womenjintian");

    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    let idx = s.candidates.iter().position(|c| c.word == "今天").expect("应有「今天」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "连续确认多个整词不应造词");
}

#[test]
fn punctuation_after_partial_candidate_commits_uncovered_suffix_and_ends() {
    let mut s = session_with_segments();
    type_str(&mut s, "womenjintian");
    s.cursor = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们jintian，"),
        "标点应提交候选 + 未覆盖后缀 + 标点: {out:?}"
    );
    assert!(!s.is_active(), "标点之后组合结束");
}

#[test]
fn covered_candidate_keeps_the_punctuation_immediately_after_it() {
    let mut s = session_with_segments();
    type_str(&mut s, "womenjintian");
    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");
    s.cursor = s.candidates.iter().position(|c| c.word == "今天").expect("应有「今天」");
    let out = s.handle(InputEvent::Char(',')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们今天，"),
        "整串覆盖时标点紧跟候选: {out:?}"
    );
}

#[test]
fn escape_hatch_commits_the_unexplained_remainder_verbatim() {
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 900)]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "nihaozzzzz");
    assert_eq!(
        s.candidates.first().map(|c| c.word.as_str()),
        Some("你好"),
        "真实候选仍在最前"
    );
    let idx = s
        .candidates
        .iter()
        .position(|c| c.echo && c.word == "nihaozzzzz")
        .expect("应有原样上屏逃生口");
    s.cursor = idx;
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "nihaozzzzz"),
        "逃生口应原样上屏: {out:?}"
    );
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "原样上屏不应造词");
}

#[test]
fn verbatim_mid_composition_still_commits_explicitly_confirmed_segments() {
    let mut s = session_with_segments();
    type_str(&mut s, "womenjintian");
    let idx = s.candidates.iter().position(|c| c.word == "我们").expect("应有「我们」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("不应失败");

    let out = s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Updated(ref u) if u.preedit == "我们jintianG"),
        "大写进组合时已确认段照常显示: {out:?}"
    );
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "我们jintianG"),
        "回车应提交已确认段 + 剩余原样串: {out:?}"
    );
    assert!(!s.is_active());
}

#[test]
fn clicking_an_out_of_range_candidate_is_a_noop() {
    let mut s = session_with_segments();
    type_str(&mut s, "women");
    let before = s.pinyin().to_owned();
    let out = s.handle(InputEvent::CandidateClicked(999)).expect("不应失败");
    assert!(matches!(out, SessionOutput::Noop));
    assert_eq!(s.pinyin(), before);
}

#[test]
fn confirming_a_segment_feeds_the_language_model_window() {
    let (_engine, mut s) = session_for_coin(&[("北京", "bei jing", 3000), ("大学", "da xue", 300)]);
    type_str(&mut s, "beijingdaxue");
    assert!(!s.lm_context().tail.contains("北京"));

    let idx = s.candidates.iter().position(|c| c.word == "北京").expect("应有「北京」");
    s.handle(InputEvent::CandidateClicked(idx)).expect("确认失败");
    assert_eq!(s.pinyin(), "daxue");
    assert!(
        s.lm_context().tail.contains("北京"),
        "确认段应进入 LM 字符级窗口: {:?}",
        s.lm_context().tail
    );
}
