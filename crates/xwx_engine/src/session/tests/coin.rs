//! `session` 自动造词（单字组词、造词边界与缓冲上限）单测。

use super::*;

#[test]
fn char_by_char_selection_coins_phrase() {
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 90),
        ("丰", "feng", 80),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    assert_eq!(user.entry_len(), 0);

    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    assert!(s.is_reusable_phrase(), "两个单字片段应够格造词");
    type_and_commit(&mut s, "feng", "丰");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 1, "逐字组词应造出一条用户词");

    // 造词条目经格解码以 Exact 层浮现到首位，拼音 key 为三段拼接。
    let cands = engine
        .lookup_candidates("zhangsanfeng", &crate::LmContext::empty())
        .expect("查询失败");
    let pos = cands
        .iter()
        .position(|c| c.word == "张三丰")
        .expect("造词后应查到「张三丰」");
    assert_eq!(pos, 0, "整词造词候选应排第一");
    assert_eq!(cands[pos].pinyin, "zhang san feng");
    assert_eq!(cands[pos].spelling, crate::SpellingKind::Exact);
    assert!(!cands[pos].echo);
}

#[test]
fn punctuation_idle_and_composing_break_coin_run() {
    // 空闲时标点打断造词缓冲，不跨标点粘词。
    let (engine, mut s) = session_for_coin(&[
        ("你", "ni", 100),
        ("好", "hao", 100),
        ("世", "shi", 100),
        ("界", "jie", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "ni", "你");
    type_and_commit(&mut s, "hao", "好");
    assert!(matches!(
        s.handle(InputEvent::Char(',')).expect("不应失败"),
        SessionOutput::Commit(_)
    ));
    assert_eq!(user.entry_len(), 1);

    type_and_commit(&mut s, "shi", "世");
    type_and_commit(&mut s, "jie", "界");
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert_eq!(user.entry_len(), 2);

    let snap = user.snapshot();
    assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
    assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
    assert!(snap.lookup_entries("ni hao shi jie").is_empty());

    // 组合中打标点同样结算上一段、不跨标点粘词。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    s.handle(InputEvent::Char('f')).expect("按键失败");
    assert!(matches!(
        s.handle(InputEvent::Char(',')).expect("不应失败"),
        SessionOutput::Commit(_)
    ));
    assert_eq!(user.entry_len(), 1);
    assert_eq!(user.snapshot().lookup_entries("zhang san"), &["张三".to_string()]);

    type_and_commit(&mut s, "feng", "丰");
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert!(user.snapshot().lookup_entries("zhang san feng").is_empty());
}

#[test]
fn focus_change_and_mode_toggle_break_coin_run() {
    // 焦点切换（reset_for_focus_change）是造词边界。
    let (engine, mut s) = session_for_coin(&[
        ("你", "ni", 100),
        ("好", "hao", 100),
        ("世", "shi", 100),
        ("界", "jie", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "ni", "你");
    type_and_commit(&mut s, "hao", "好");
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 1);
    type_and_commit(&mut s, "shi", "世");
    type_and_commit(&mut s, "jie", "界");
    s.reset_for_focus_change();
    assert_eq!(user.entry_len(), 2);
    let snap = user.snapshot();
    assert_eq!(snap.lookup_entries("ni hao"), &["你好".to_string()]);
    assert_eq!(snap.lookup_entries("shi jie"), &["世界".to_string()]);
    assert!(snap.lookup_entries("ni hao shi jie").is_empty());

    // 空闲切英文再切回同样打断造词缓冲。
    let (engine, mut s) = session_for_coin(&[
        ("张", "zhang", 100),
        ("三", "san", 100),
        ("丰", "feng", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "zhang", "张");
    type_and_commit(&mut s, "san", "三");
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    s.handle(InputEvent::ToggleAscii).expect("不应失败");
    assert_eq!(user.entry_len(), 1);
    type_and_commit(&mut s, "feng", "丰");
    s.handle(InputEvent::Char('x')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert!(user.snapshot().lookup_entries("zhang san feng").is_empty());
}

#[test]
fn echo_and_verbatim_do_not_coin() {
    let (engine, mut s) = session_for_coin(&[("你", "ni", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_str(&mut s, "zgr");
    s.handle(InputEvent::Space).expect("空格失败");
    s.handle(InputEvent::Verbatim('G')).expect("不应失败");
    s.handle(InputEvent::Char('a')).expect("按键失败");
    s.handle(InputEvent::Enter).expect("回车失败");
    assert_eq!(user.entry_len(), 0, "回声/Verbatim/回车都不应造词");
}

#[test]
fn single_complete_word_commit_does_not_duplicate() {
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "nihao", "你好");
    assert_eq!(user.entry_len(), 0, "单次完整词上屏不应造词");
    assert_eq!(user.unigram_len(), 1, "unigram 学习照常");
}

#[test]
fn multi_char_and_mixed_segments_do_not_coin() {
    // 连续多字词不造句。
    let (engine, mut s) = session_for_coin(&[
        ("当前", "dang qian", 100),
        ("主线", "zhu xian", 100),
        ("进度", "jin du", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "dangqian", "当前");
    type_and_commit(&mut s, "zhuxian", "主线");
    type_and_commit(&mut s, "jindu", "进度");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "连续多字词不应粘成词条");
    assert!(user.snapshot().lookup_entries("dang qian zhu xian jin du").is_empty());
    assert_eq!(user.unigram_len(), 3, "unigram 调频不受影响");

    // 单字与多字词混合同样不造词（要求全部片段为单字）。
    let (engine, mut s) = session_for_coin(&[
        ("设置", "she zhi", 100),
        ("为", "wei", 100),
        ("说着玩的", "shuo zhe wan de", 100),
        ("说", "shuo", 100),
        ("说中文", "shuo zhong wen", 100),
    ]);
    let user = engine.user_lm().expect("应有用户层");
    type_and_commit(&mut s, "shezhi", "设置");
    type_and_commit(&mut s, "wei", "为");
    type_and_commit(&mut s, "shuozhewande", "说着玩的");
    type_and_commit(&mut s, "shuo", "说");
    type_and_commit(&mut s, "shuozhongwen", "说中文");
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "含整词片段的连续上屏不应造词");
    assert!(
        user.snapshot()
            .lookup_entries("she zhi wei shuo zhe wan de shuo shuo zhong wen")
            .is_empty()
    );
}

#[test]
fn reusable_phrase_predicate_table() {
    let (_engine, mut s) = session_for_coin(&[]);

    assert!(!s.is_reusable_phrase(), "空缓冲不够格");
    s.coin_buffer.push(("你".into(), "ni".into()));
    assert!(!s.is_reusable_phrase(), "单片段不够格");
    s.coin_buffer.push(("好".into(), "hao".into()));
    assert!(s.is_reusable_phrase(), "两个单字够格");

    s.coin_buffer.clear();
    s.coin_buffer.push(("你好".into(), "ni hao".into()));
    s.coin_buffer.push(("世界".into(), "shi jie".into()));
    assert!(!s.is_reusable_phrase(), "全多字词不够格");

    s.coin_buffer.push(("张".into(), "zhang".into()));
    assert!(!s.is_reusable_phrase(), "多字 + 单字不够格");

    s.coin_buffer.clear();
    s.coin_buffer.push(("你好".into(), "ni hao".into()));
    s.coin_buffer.push(("啊".into(), "a".into()));
    assert!(!s.is_reusable_phrase(), "顺序不影响判据");

    s.coin_buffer.clear();
    for (w, p) in [("西", "xi"), ("双", "shuang"), ("版", "ban"), ("纳", "na")] {
        s.coin_buffer.push((w.into(), p.into()));
    }
    assert!(s.is_reusable_phrase(), "全单字四片段够格");
}

#[test]
fn non_reusable_run_still_clears_buffer() {
    let (engine, mut s) = session_for_coin(&[("你好", "ni hao", 100), ("世界", "shi jie", 100)]);
    let user = engine.user_lm().expect("应有用户层");

    type_and_commit(&mut s, "nihao", "你好");
    type_and_commit(&mut s, "shijie", "世界");
    assert!(!s.coin_buffer.is_empty());
    s.break_coin_run();
    assert!(s.coin_buffer.is_empty(), "结算后缓冲必须清空");
    assert_eq!(user.entry_len(), 0);
}

#[test]
fn coin_buffer_cap_drops_overlong_run() {
    let engine = Arc::new(
        ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    for i in 0..30u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine.dict.upsert(&w, "a", 100 - i as u64).expect("插入失败");
    }
    let mut s = InputSession::new(Arc::clone(&engine));
    let user = engine.user_lm().expect("应有用户层");

    for _ in 0..(MAX_COIN_CHARS + 2) {
        s.handle(InputEvent::Char('a')).expect("按键失败");
        s.commit_immediately(0).expect("应有候选");
    }
    let buffered: usize = s.coin_buffer.iter().map(|(w, _)| w.chars().count()).sum();
    assert!(buffered <= MAX_COIN_CHARS, "缓冲不应超过上限: {buffered}");
    assert!(user.entry_len() >= 1 || buffered <= 2, "超限应触发结算");

    // 超过 MAX_COIN_CHARS 的纯单字缓冲必须被丢弃、不能造词。
    let (engine, mut s) = session_for_coin(&[("甲", "jia", 100)]);
    let user = engine.user_lm().expect("应有用户层");
    for _ in 0..(MAX_COIN_CHARS + 1) {
        s.coin_buffer.push(("甲".to_string(), "jia".to_string()));
    }
    assert!(!s.is_reusable_phrase());
    s.break_coin_run();
    assert_eq!(user.entry_len(), 0, "超长缓冲必须丢弃");
    assert!(s.coin_buffer.is_empty());
}
