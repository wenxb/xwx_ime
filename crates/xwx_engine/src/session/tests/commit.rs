//! `session` 单测:commit(从 `session/tests.rs` 拆出,2026-10-02)。

use super::*;

#[test]
fn commit_word_learns_but_commit_raw_does_not() {
    // 端到端学习钩子:引擎挂用户层后,候选词上屏应学习、
    // 原始字母/标点上屏不应学习(后者不是词)。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    engine.dict.upsert("测试", "ce shi", 10).expect("插入失败");
    let mut s = InputSession::new(Arc::clone(&engine));

    for ch in "ceshi".chars() {
        s.handle(InputEvent::Char(ch)).expect("按键失败");
    }
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
        "候选词上屏应产生计数,没学过的词不应有计数: {learned:?} vs {unknown:?}"
    );

    let before = user.unigram_len();
    s.handle(InputEvent::Char('a')).expect("按键失败");
    let out = s.handle(InputEvent::Enter).expect("回车原样上屏失败");
    assert!(matches!(out, SessionOutput::Commit(ref t) if t == "a"));
    assert_eq!(
        user.unigram_len(),
        before,
        "commit_raw 原始字母上屏不应产生学习"
    );
}

#[test]
fn echo_candidate_commit_does_not_learn() {
    // 回声候选(词库解不出的原始输入串)上屏不应被当成词学习——
    // 它不是词,语义等同 commit_raw(对应 librime raw 不进 memory)。
    // 引擎挂内存用户层,断言上屏回声候选后 unigram_len() 不变。
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    // 空词库:输入 "zgr" 解不出任何真实候选,只会产出回声候选
    let mut s = InputSession::new(Arc::clone(&engine));

    for ch in "zgr".chars() {
        s.handle(InputEvent::Char(ch)).expect("按键失败");
    }
    let user = engine.user_lm().expect("引擎应有用户层");
    let before = user.unigram_len();

    // 空格选中唯一候选(即回声候选)上屏
    let out = s.handle(InputEvent::Space).expect("空格选词失败");
    assert!(
        matches!(out, SessionOutput::Commit(ref t) if t == "zgr"),
        "回声候选上屏应提交原始输入串: {out:?}"
    );
    assert_eq!(
        user.unigram_len(),
        before,
        "回声候选上屏不应产生学习(等同 commit_raw)"
    );
    assert_eq!(s.last_word, None, "回声候选上屏后 last_word 应为 None");
}

#[test]
fn commit_immediately_commits_global_index() {
    let mut s = session_with_dict();
    for c in ['n', 'i', 'h', 'a', 'o'] {
        s.handle(InputEvent::Char(c)).expect("不应失败");
    }
    // 第 2b 步起候选按「整条路径」打分：
    // 路径[你好]（一个词，-700）压过路径[你,好]（两个词，-1250），
    // 所以首选是整串解码出的「你好」，而不是逐字拆开的「你」——
    // 这正是 beam search 解码要的可见行为（短语整体概率 > 逐字累加）。
    // 次选仍是单字候选：你(-1250) > 好(-1250)，可以只上屏一个字。
    let word = s.commit_immediately(0).expect("下标 0 应有候选");
    assert_eq!(word, "你好");
    assert!(!s.is_active(), "上屏后会话应复位");
    assert!(s.commit_immediately(0).is_none(), "复位后不应再取到候选");
}
