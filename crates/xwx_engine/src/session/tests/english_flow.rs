//! 中英混输的会话时序、延迟空格、学习与删除/撤销单测，全部走 `handle`。

use super::*;
use crate::english::{build_synth_bin, EnglishDict, EnglishEngine, SynthWord as W};

fn words() -> Vec<W> {
    vec![
        W { key: "hello", display: "hello", q: 450 },
        W { key: "help", display: "help", q: 520 },
        W { key: "world", display: "world", q: 480 },
        W { key: "python", display: "Python", q: 410 },
        W { key: "you", display: "you", q: 700 },
    ]
}

fn session_with_words(ws: Vec<W>) -> InputSession {
    let dict = EnglishDict::from_bytes(build_synth_bin(&ws)).expect("合成英文词库应可读");
    let engine = ImeEngine::new(None)
        .expect("内存引擎创建失败")
        .with_english(EnglishEngine::new(dict));
    InputSession::new(Arc::new(engine))
}

fn session_with_english() -> InputSession {
    session_with_words(words())
}

fn session_with_both_user_lms() -> InputSession {
    let dict = EnglishDict::from_bytes(build_synth_bin(&words())).expect("合成英文词库应可读");
    let engine = ImeEngine::new(None)
        .expect("内存引擎创建失败")
        .with_user_lm(crate::UserLM::in_memory())
        .with_english(EnglishEngine::new(dict).with_user_lm(crate::UserLM::in_memory()));
    InputSession::new(Arc::new(engine))
}

fn cursor_word(s: &InputSession) -> String {
    s.candidates
        .get(s.cursor)
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

fn candidate_words(s: &InputSession) -> Vec<String> {
    s.candidates.iter().map(|c| c.word.clone()).collect()
}

fn highlight(s: &mut InputSession, word: &str) {
    while cursor_word(s) != word {
        let before = s.cursor;
        s.handle(InputEvent::Down).expect("移动高亮不应失败");
        if s.cursor == before {
            panic!("候选里没有 {word}: {:?}", candidate_words(s));
        }
    }
}

fn commit_text(s: &mut InputSession, event: InputEvent) -> String {
    match s.handle(event).expect("按键不应失败") {
        SessionOutput::Commit(t) => t,
        other => panic!("期望 Commit，实际 {}", other_name(&other)),
    }
}

fn other_name(o: &SessionOutput) -> &'static str {
    match o {
        SessionOutput::Commit(_) => "Commit",
        SessionOutput::Updated(_) => "Updated",
        SessionOutput::CommitAndUpdate { .. } => "CommitAndUpdate",
        SessionOutput::RefreshAndForward(_) => "RefreshAndForward",
        SessionOutput::Noop => "Noop",
    }
}

fn english_unigram(s: &InputSession, key: &str) -> u32 {
    s.engine
        .english()
        .expect("应装配英文")
        .user()
        .snapshot()
        .english_unigram_count(key)
}

#[test]
fn s01_space_number_and_click_all_commit_python_and_clear_input() {
    for pick in 0..3u8 {
        let mut s = session_with_english();
        type_str(&mut s, "pyth");
        highlight(&mut s, "Python");
        let offset = s.cursor - s.page_start();
        let text = match pick {
            0 => commit_text(&mut s, InputEvent::Space),
            1 => commit_text(&mut s, InputEvent::CandidateClicked(offset)),
            _ => commit_text(&mut s, InputEvent::Select(offset as u8 + 1)),
        };
        assert_eq!(text, "Python", "第 {pick} 种选择方式保持规范大小写");
        assert_eq!(s.pinyin, "", "输入必须完全清空");
        assert_eq!(english_unigram(&s, "python"), 1, "计数记在小写 key 上且只学一次");
        assert_eq!(english_unigram(&s, "Python"), 0, "不另立大写键");
    }
}

#[test]
fn s02_enter_raw_does_not_learn_or_add_space() {
    let mut s = session_with_english();
    type_str(&mut s, "pyth");
    assert_eq!(commit_text(&mut s, InputEvent::Enter), "pyth");
    assert_eq!(english_unigram(&s, "python"), 0);
    assert_eq!(english_unigram(&s, "pyth"), 0);
}

#[test]
fn s03_sequence_adds_single_space_records_counts_and_clears_on_focus() {
    let mut s = session_with_english();
    type_str(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
    type_str(&mut s, "wor");
    assert_eq!(commit_text(&mut s, InputEvent::Space), " world", "第二个英文词前恰好一个空格");

    let snap = s.engine.english().expect("英文").user().snapshot();
    assert_eq!(snap.english_unigram_count("hello"), 1);
    assert_eq!(snap.english_unigram_count("world"), 1);
    assert_eq!(snap.english_bigram_count("hello", "world"), 1);
    assert_eq!(s.english_run_context().previous_word(), Some("world"));

    // 焦点切换：清上文，不撤销已提交统计。
    s.reset_for_focus_change();
    assert_eq!(s.english_run_context().previous_word(), None);
    let after = s.engine.english().expect("英文").user().snapshot();
    assert_eq!(after.english_unigram_count("hello"), 1, "已提交统计不撤销");
}

#[test]
fn s04_idle_spaces_passthrough_and_suppress_synthetic_space() {
    for idle_spaces in [1, 2] {
        let mut s = session_with_english();
        type_str(&mut s, "hello");
        assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
        for _ in 0..idle_spaces {
            assert!(
                matches!(s.handle(InputEvent::Space).expect("空闲空格"), SessionOutput::Noop),
                "空闲空格应原样交给应用"
            );
        }
        type_str(&mut s, "world");
        assert_eq!(
            commit_text(&mut s, InputEvent::Space),
            "world",
            "{idle_spaces} 个空闲空格后不再补合成空格"
        );
    }
}

#[test]
fn s06_language_switch_never_adds_a_boundary_space() {
    let mut s = session_with_english();
    s.engine.dict.upsert("开发", "kaifa", 100).expect("插入");
    type_str(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_str(&mut s, "kaifa");
    let text = commit_text(&mut s, InputEvent::Space);
    assert!(!text.starts_with(' '), "英→中不自动插空格: {text:?}");

    let mut s = session_with_english();
    s.engine.dict.upsert("使用", "shiyong", 100).expect("插入");
    type_str(&mut s, "shiyong");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "使用");
    type_str(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello", "中→英不补空格");
}

#[test]
fn partial_chinese_readings_stay_first_before_learning_even_after_english() {
    for (input, word, pinyin) in [("rug", "如果", "ruguo"), ("bir", "比如", "biru")] {
        for after_english in [false, true] {
            let mut ws = words();
            ws.extend([
                W { key: "rug", display: "rug", q: 400 },
                W { key: "rugby", display: "rugby", q: 450 },
                W { key: "rugged", display: "rugged", q: 420 },
                W { key: "bir", display: "bir", q: 350 },
                W { key: "bird", display: "bird", q: 450 },
                W { key: "birth", display: "birth", q: 420 },
            ]);
            let mut s = session_with_words(ws);
            s.engine.dict.upsert(word, pinyin, 900).expect("插入中文词条");
            if after_english {
                type_str(&mut s, "hello");
                assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
            }
            type_str(&mut s, input);
            assert_eq!(english_unigram(&s, input), 0);
            assert_eq!(s.candidates[0].word, word, "{input}, after_english={after_english}");
            assert!(s.candidates.iter().any(|c| c.english_key() == Some(input)));
            assert_eq!(commit_text(&mut s, InputEvent::Space), word);
            assert_eq!(s.english_run_context().previous_word(), None);
        }
    }
}

#[test]
fn s07_query_paging_highlight_and_escape_never_learn() {
    let mut s = session_with_english();
    type_str(&mut s, "pyth");
    s.handle(InputEvent::Down).expect("移动高亮");
    s.handle(InputEvent::PageDown).expect("翻页");
    s.handle(InputEvent::Up).expect("移动高亮");
    assert_eq!(english_unigram(&s, "python"), 0);
    s.handle(InputEvent::Escape).expect("取消");
    assert_eq!(english_unigram(&s, "python"), 0);
    assert_eq!(english_unigram(&s, "hello"), 0);
}

#[test]
fn s08_chinese_confirmed_segment_blanks_the_english_context() {
    let mut s = session_with_english();
    s.engine.dict.upsert("开发", "kai fa", 100).expect("插入");
    s.engine.dict.upsert("放", "fang", 90).expect("插入");
    type_str(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    assert_eq!(s.english_run_context().previous_word(), Some("hello"));

    type_str(&mut s, "kaifang");
    highlight(&mut s, "开发");
    assert!(matches!(
        s.handle(InputEvent::Space).expect("确认一段"),
        SessionOutput::Updated(_)
    ));
    assert!(!s.confirmed.is_empty());
    assert_eq!(s.english_context().previous_word(), None, "有中文确认段时英文上文为空");

    s.handle(InputEvent::Backspace).expect("撤回确认");
    assert_eq!(s.english_context().previous_word(), Some("hello"), "撤回后英文上文恢复");
    assert_eq!(english_unigram(&s, "hello"), 1, "撤回不产生新学习");
}

#[test]
fn s12_composition_navigation_and_backspace_keep_context_until_idle_backspace() {
    let mut s = session_with_english();
    type_str(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_str(&mut s, "wor");

    for ev in [InputEvent::Left, InputEvent::Right, InputEvent::Down] {
        s.handle(ev).expect("组合内导航");
        assert_eq!(s.english_run_context().previous_word(), Some("hello"));
    }

    s.handle(InputEvent::Backspace).expect("组合内退格");
    assert_eq!(s.pinyin, "wo");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"));
    s.handle(InputEvent::Backspace).expect("退格");
    s.handle(InputEvent::Backspace).expect("退格");
    assert_eq!(s.pinyin, "");
    assert!(matches!(
        s.handle(InputEvent::Backspace).expect("空闲退格"),
        SessionOutput::Noop
    ));
    assert_eq!(s.english_run_context().previous_word(), None, "空闲退格清上文");
}

#[test]
fn s15_ke_after_english_keeps_chinese_first() {
    let mut ws = words();
    ws.push(W { key: "keep", display: "keep", q: 520 });
    ws.push(W { key: "key", display: "key", q: 540 });
    let mut s = session_with_words(ws);
    s.engine.dict.upsert("可", "ke", 900).expect("插入");
    type_str(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_str(&mut s, "ke");
    assert_eq!(cursor_word(&s), "可", "ke 首选应是中文: {:?}", candidate_words(&s));
    assert!(s.candidates.get(1).is_some_and(Candidate::is_english), "英文仍在第 2 位");
}

#[test]
fn u01_english_learn_does_not_touch_the_chinese_user_lm_or_coin_buffer() {
    let mut s = session_with_both_user_lms();
    type_str(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    let zh = s.engine.user_lm().expect("应有中文用户层");
    assert_eq!(zh.unigram_len(), 0, "英文不得进中文一元计数");
    assert_eq!(zh.entry_len(), 0, "英文不得进中文造词条目");
    assert!(s.coin_buffer.is_empty(), "英文不得进中文造词缓冲");
    assert_eq!(english_unigram(&s, "hello"), 1, "只进英文计数");
}

#[test]
fn u07_blocking_python_hides_it_from_prefix_exact_and_later_queries() {
    let mut s = session_with_both_user_lms();
    // 先选词学习一次，验证删除既清计数又屏蔽后续查询。
    type_str(&mut s, "pyth");
    highlight(&mut s, "Python");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "Python");
    assert_eq!(english_unigram(&s, "python"), 1);

    type_str(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删候选");
    assert_eq!(english_unigram(&s, "python"), 0, "屏蔽同时清掉一元计数");
    assert!(
        !s.candidates.iter().any(|c| c.english_key() == Some("python")),
        "删除后当场从候选消失"
    );
    for input in ["pyth", "pytho", "python"] {
        s.handle(InputEvent::Escape).expect("清组合");
        type_str(&mut s, input);
        assert!(
            !s.candidates.iter().any(|c| c.english_key() == Some("python")),
            "屏蔽后 {input} 不得再召回 python"
        );
    }
}

#[test]
fn u08_undo_reverts_most_recent_delete_across_languages_and_ignores_repeats() {
    // ① 先删中文、再删英文 → 撤销只恢复英文，中文仍屏蔽。
    let mut s = session_with_both_user_lms();
    s.engine.dict.upsert("你好", "ni hao", 100).expect("插入");
    s.engine.dict.upsert("泥好", "ni hao", 90).expect("插入");
    type_str(&mut s, "nihao");
    highlight(&mut s, "你好");
    s.handle(InputEvent::DeleteCandidate).expect("删中文");
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");

    assert!(s.engine.unblock_last_candidate(), "应撤回英文（最后一次）");
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "pyth");
    assert!(s.candidates.iter().any(|c| c.english_key() == Some("python")));
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "nihao");
    assert!(!s.candidates.iter().any(|c| c.word == "你好"));

    // ② 先删英文、再删中文 → 撤销只恢复中文，英文仍屏蔽。
    let mut s = session_with_both_user_lms();
    s.engine.dict.upsert("你好", "ni hao", 100).expect("插入");
    type_str(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "nihao");
    highlight(&mut s, "你好");
    s.handle(InputEvent::DeleteCandidate).expect("删中文");

    assert!(s.engine.unblock_last_candidate(), "应撤回中文（最后一次）");
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "nihao");
    assert!(s.candidates.iter().any(|c| c.word == "你好"));
    s.handle(InputEvent::Escape).expect("清组合");
    type_str(&mut s, "pyth");
    assert!(!s.candidates.iter().any(|c| c.english_key() == Some("python")));

    // ③ 重复删除同一词不覆盖撤销槽位。
    let mut s = session_with_both_user_lms();
    type_str(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");
    s.engine.delete_english_candidate("python");
    assert!(s.engine.unblock_last_candidate(), "重复删除不得清空槽位");
    assert!(!s.engine.unblock_last_candidate(), "撤过之后没有第二条");
}
