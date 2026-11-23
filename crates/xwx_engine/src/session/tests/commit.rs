//! `session` 上屏、学习、上下文窗口与 `commit_immediately` 单测。

use super::*;

#[test]
fn commit_word_learns_but_commit_raw_does_not() {
    let (engine, mut s) = session_for_coin(&[("测试", "ce shi", 10)]);
    type_str(&mut s, "ceshi");
    let out = s.handle(InputEvent::Space).expect("空格选词失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "测试"),
        "应上屏候选词「测试」: {out:?}"
    );
    let user = engine.user_lm().expect("引擎应有用户层");
    let snap = user.snapshot();
    let learned = user.counts_with(&snap, None, "测试");
    let unknown = user.counts_with(&snap, None, "没学过的词");
    assert!(
        learned.0 > 0 && unknown == (0, 0),
        "候选词上屏应产生计数: {learned:?} vs {unknown:?}"
    );

    let before = user.unigram_len();
    s.handle(InputEvent::Char('a')).expect("按键失败");
    let out = s.handle(InputEvent::Enter).expect("回车原样上屏失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "a"));
    assert_eq!(user.unigram_len(), before, "原始字母上屏不应产生学习");
}

#[test]
fn echo_candidate_commit_does_not_learn() {
    let (engine, mut s) = session_for_coin(&[]);
    type_str(&mut s, "zgr");
    let user = engine.user_lm().expect("引擎应有用户层");
    let before = user.unigram_len();

    let out = s.handle(InputEvent::Space).expect("空格选词失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "zgr"),
        "回声候选上屏应提交原文: {out:?}"
    );
    assert_eq!(user.unigram_len(), before, "回声候选上屏不应学习");
    assert_eq!(s.last_word, None, "回声候选上屏后 last_word 应为 None");
}

#[test]
fn commit_immediately_commits_refreshes_and_learns_at_once() {
    // 整串提交：caret 移动后仍按整串取全局下标，上屏后复位且 caret 归 0。
    let mut s = session_with_dict();
    type_str(&mut s, "nihao");
    s.handle(InputEvent::Left).expect("不应失败");
    s.handle(InputEvent::Left).expect("不应失败");
    let word = s.commit_immediately(0).expect("下标 0 应有候选");
    assert_eq!(word, "你好");
    assert!(!s.is_active() && s.caret() == 0);
    assert!(s.commit_immediately(0).is_none(), "复位后不应再取到候选");

    // 前缀提交：立刻返回文本、立刻刷新剩余串候选、立刻学习（与 handle 的延迟语义对照）。
    let (engine, mut s) = session_for_coin(&[("我们", "wo men", 300), ("今天", "jin tian", 300)]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "womenjintian");
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == "我们")
        .expect("候选里应有「我们」");
    let text = s.commit_immediately(idx).expect("应有候选");
    assert_eq!(text, "我们");
    assert_eq!(s.pinyin(), "jintian");
    let expected = s
        .engine
        .lookup_candidates("jintian", &s.lm_context())
        .expect("查询失败");
    assert_eq!(
        s.candidates.first().map(|c| c.word.as_str()),
        expected.first().map(|c| c.word.as_str()),
        "剩余串候选应同步刷新"
    );
    assert!(
        user.counts_with(&user.snapshot(), None, "我们").0 > 0,
        "commit_immediately 应立刻学习"
    );
}

#[test]
fn committed_text_feeds_next_candidates() {
    let mut s = session_with_lm();
    type_str(&mut s, "zhong");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "中"));
    assert_eq!(s.last_word.as_deref(), Some("中"));
    assert_eq!(s.committed_tail, "中");

    type_str(&mut s, "guo");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "国"),
        "上文「中」应让「国」压过词频更高的「过」"
    );
    assert_eq!(s.committed_tail, "中国");
}

#[test]
fn raw_commit_resets_last_word_but_keeps_tail() {
    let mut s = session_with_dict();
    type_str(&mut s, "ni");
    let out = s.handle(InputEvent::Space).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "你"));
    assert_eq!(s.last_word.as_deref(), Some("你"));

    type_str(&mut s, "hao");
    let out = s.handle(InputEvent::Enter).expect("不应失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "hao"));
    assert_eq!(s.last_word, None, "原始字母上屏后 last_word 应为 None");
    assert_eq!(s.committed_tail, "你hao");
}

#[test]
fn committed_tail_is_bounded_on_char_boundary() {
    let mut s = session_with_dict();

    s.commit_raw("一二三四五六七八九十一二三四五六七八九十".to_string());
    assert_eq!(s.committed_tail, "五六七八九十一二三四五六七八九十");
    assert_eq!(s.committed_tail.chars().count(), COMMITTED_TAIL_LIMIT);

    // 4 字节 emoji 截断必须按 char 边界进行。
    s.commit_raw("😀".repeat(20));
    s.commit_raw("尾".to_string());
    assert_eq!(s.committed_tail.chars().count(), COMMITTED_TAIL_LIMIT);
    assert_eq!(s.committed_tail.chars().last(), Some('尾'));
    assert!(std::str::from_utf8(s.committed_tail.as_bytes()).is_ok());
}
