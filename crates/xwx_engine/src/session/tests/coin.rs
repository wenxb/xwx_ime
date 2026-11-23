//! `session` 分段自动造词（同一次预编辑分段选词即刻生效、长度边界与防误造）单测。

use super::*;

fn pick_word(s: &mut InputSession, word: &str) -> SessionOutput {
    let idx = s
        .candidates
        .iter()
        .position(|c| c.word == word)
        .unwrap_or_else(|| {
            let shown: Vec<&str> = s.candidates.iter().map(|c| c.word.as_str()).collect();
            panic!("候选里应有「{word}」，实际: {shown:?}");
        });
    s.cursor = idx;
    s.handle(InputEvent::Space).expect("空格选词不应失败")
}

#[test]
fn single_preedit_segmented_selection_coins_immediately() {
    // ① 逐字分段选出（张三丰）：整串结束立刻生效，无需标点或焦点切换。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 90),
        ("丰", "feng", 80),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    assert_eq!(user.entry_len(), 0);

    type_str(&mut s, "zhangsanfeng");
    assert!(matches!(pick_word(&mut s, "张"), SessionOutput::Updated(_)));
    assert!(matches!(pick_word(&mut s, "三"), SessionOutput::Updated(_)));
    assert_eq!(user.entry_len(), 0, "未完成整串前不得提前造词");
    assert!(matches!(
        pick_word(&mut s, "丰"),
        SessionOutput::Commit(ref t) if t == "张三丰"
    ));
    assert_eq!(user.entry_len(), 1, "同预编辑分段选完应即刻造词");

    let cands = engine
        .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
        .expect("查询失败");
    assert_eq!(cands[0].word, "张三丰", "下次打全拼应直接排第一");
    assert_eq!(cands[0].pinyin, "zhang san feng");
    assert_eq!(cands[0].spelling, crate::SpellingKind::Exact);

    // ② 单字 + 多字混合片段（刘 + 慈欣）与短多字拼接（万象 + 模型）同样即时造词。
    let (engine, mut s) = session_for_coin(&[
        ("刘", "liu", 100),
        ("慈欣", "ci xin", 90),
        ("万象", "wan xiang", 100),
        ("模型", "mo xing", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "liucixin");
    assert!(matches!(pick_word(&mut s, "刘"), SessionOutput::Updated(_)));
    assert!(matches!(
        pick_word(&mut s, "慈欣"),
        SessionOutput::Commit(ref t) if t == "刘慈欣"
    ));
    assert_eq!(
        user.snapshot().lookup_entries("liu ci xin"),
        &["刘慈欣".to_string()]
    );

    type_str(&mut s, "wanxiangmoxing");
    assert!(matches!(pick_word(&mut s, "万象"), SessionOutput::Updated(_)));
    assert!(matches!(
        pick_word(&mut s, "模型"),
        SessionOutput::Commit(ref t) if t == "万象模型"
    ));
    assert_eq!(
        user.snapshot().lookup_entries("wan xiang mo xing"),
        &["万象模型".to_string()]
    );
}

#[test]
fn backspace_correction_mid_preedit_coins_final_segments_only() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("散", "san", 120),
        ("三", "san", 90),
        ("丰", "feng", 80),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_str(&mut s, "zhangsanfeng");
    assert!(matches!(pick_word(&mut s, "张"), SessionOutput::Updated(_)));
    assert!(matches!(pick_word(&mut s, "散"), SessionOutput::Updated(_)));
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("退格不应失败"),
        SessionOutput::Updated(_)
    ));
    assert!(matches!(pick_word(&mut s, "三"), SessionOutput::Updated(_)));
    assert!(matches!(
        pick_word(&mut s, "丰"),
        SessionOutput::Commit(ref t) if t == "张三丰"
    ));
    assert_eq!(user.entry_len(), 1);
    assert_eq!(
        user.snapshot().lookup_entries("zhang san feng"),
        &["张三丰".to_string()]
    );
}

#[test]
fn cross_preedit_separate_commits_do_not_glue_into_phrase() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    type_and_commit(&mut s, "feng", "丰");
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 0, "跨预编辑独立上屏不得粘成自造词");
    assert_eq!(user.unigram_len(), 3, "单字调频学习不受影响");
}

#[test]
fn abort_echo_verbatim_and_overlong_segments_do_not_coin() {
    // Esc / 回车 / 焦点切出打断未完成分段时不造词。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_str(&mut s, "zhangsanfeng");
    pick_word(&mut s, "张");
    pick_word(&mut s, "三");
    s.handle(InputEvent::Enter).expect("回车不应失败");
    assert_eq!(user.entry_len(), 0, "回车原文收尾不应造词");

    type_str(&mut s, "zhangsanfeng");
    pick_word(&mut s, "张");
    pick_word(&mut s, "三");
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 0, "焦点切出未选完不应造词");

    // 回声 / Verbatim 不造词。
    type_str(&mut s, "zgr");
    s.handle(InputEvent::Space).expect("空格失败");
    s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    s.handle(InputEvent::Char('a')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert_eq!(user.entry_len(), 0);

    // 单次完整词上屏不重复造词。
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100)]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "nihao", "你好");
    assert_eq!(user.entry_len(), 0);
    assert_eq!(user.unigram_len(), 1);
}

#[test]
fn reusable_phrase_entry_predicate_table() {
    let cand = |w: &str, py: &str| Candidate::new(w.to_string(), py.to_string(), 100);
    let c1 = cand("张", "zhang");
    let c2 = cand("三", "san");
    let c3 = cand("丰", "feng");
    let c_multi_a = cand("万象", "wan xiang");
    let c_multi_b = cand("模型", "mo xing");
    let c_multi_c = cand("引擎", "yin qing");
    let c_three = cand("大语言", "da yu yan");

    assert!(InputSession::reusable_phrase_entry(&[]).is_none());
    assert!(InputSession::reusable_phrase_entry(&[&c1]).is_none());
    assert_eq!(
        InputSession::reusable_phrase_entry(&[&c1, &c2, &c3]),
        Some(("张三丰".into(), "zhang san feng".into()))
    );
    assert_eq!(
        InputSession::reusable_phrase_entry(&[&c_multi_a, &c_multi_b]),
        Some(("万象模型".into(), "wan xiang mo xing".into()))
    );
    assert_eq!(
        InputSession::reusable_phrase_entry(&[&c_three, &c_multi_b]),
        Some(("大语言模型".into(), "da yu yan mo xing".into()))
    );
    // 纯多字超过 MAX_COIN_MULTI_WORD_CHARS (5) 拒绝。
    assert!(InputSession::reusable_phrase_entry(&[&c_multi_a, &c_multi_b, &c_multi_c]).is_none());
    // 含单字 8 字允许、9 字拒绝。
    let a = cand("甲", "jia");
    let eight = vec![&a; MAX_COIN_CHARS];
    assert!(InputSession::reusable_phrase_entry(&eight).is_some());
    let nine = vec![&a; MAX_COIN_CHARS + 1];
    assert!(InputSession::reusable_phrase_entry(&nine).is_none());

    // 非汉字 / 回声 / 音节不匹配拒绝。
    let mut echo = cand("张", "zhang");
    echo.echo = true;
    assert!(InputSession::reusable_phrase_entry(&[&echo, &c2]).is_none());
    let ascii = cand("AI", "ai");
    assert!(InputSession::reusable_phrase_entry(&[&ascii, &c2]).is_none());
    let bad_py = cand("张三", "zhang");
    assert!(InputSession::reusable_phrase_entry(&[&bad_py, &c3]).is_none());
}
