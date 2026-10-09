//! crate 级单测:中英混输的会话时序、学习与边界。
//!
//! 覆盖验收表 S01-S14 与 U01/U02/U06/U07（见
//! `docs/english-input/05-execution-tests.md`）。**全部走 `handle`**：
//! `commit_immediately` 是探针快捷接口，不是确认段时序的证据。

use super::*;
use crate::english::{build_synth_bin, recall::ENGLISH_BLOCK_NAMESPACE, EnglishDict, EnglishEngine, SynthWord as W};

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

/// 中英两条用户层都在的会话（删除/撤销要两边都有载体才测得出来）。
fn session_with_both_user_lms() -> InputSession {
    let dict = EnglishDict::from_bytes(build_synth_bin(&words())).expect("合成英文词库应可读");
    let engine = ImeEngine::new(None)
        .expect("内存引擎创建失败")
        .with_user_lm(crate::UserLM::in_memory())
        .with_english(EnglishEngine::new(dict).with_user_lm(crate::UserLM::in_memory()));
    InputSession::new(Arc::new(engine))
}

/// 逐键打一串小写字母。
fn type_letters(s: &mut InputSession, text: &str) {
    for ch in text.chars() {
        s.handle(InputEvent::Char(ch)).expect("打字不应失败");
    }
}

/// 当前高亮候选的词面。
fn cursor_word(s: &InputSession) -> String {
    s.candidates
        .get(s.cursor)
        .map(|c| c.word.clone())
        .unwrap_or_default()
}

fn candidate_words(s: &InputSession) -> Vec<String> {
    s.candidates.iter().map(|c| c.word.clone()).collect()
}

/// 高亮到指定词面的候选（找不到就报错）。
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

fn block_english(s: &InputSession, key: &str) {
    s.engine
        .english()
        .expect("应装配英文")
        .user()
        .block_candidate(key, ENGLISH_BLOCK_NAMESPACE);
}

// ---------- S01-S02：选择与原文逃逸 ----------

#[test]
fn s01_space_number_and_click_all_commit_python_and_clear_input() {
    for pick in 0..3u8 {
        let mut s = session_with_english();
        type_letters(&mut s, "pyth");
        highlight(&mut s, "Python");
        let offset = s.cursor - s.page_start();
        let text = match pick {
            0 => commit_text(&mut s, InputEvent::Space),
            1 => commit_text(&mut s, InputEvent::CandidateClicked(offset)),
            _ => commit_text(&mut s, InputEvent::Select(offset as u8 + 1)),
        };
        assert_eq!(text, "Python", "第 {pick} 种选择方式");
        assert_eq!(s.pinyin, "", "输入必须完全清空，不残留 on");
        assert_eq!(english_unigram(&s, "python"), 1, "只学一次");
    }
}

#[test]
fn s02_enter_raw_does_not_learn_or_add_space() {
    let mut s = session_with_english();
    type_letters(&mut s, "pyth");
    assert_eq!(commit_text(&mut s, InputEvent::Enter), "pyth", "Enter 严格提交原文");
    assert_eq!(english_unigram(&s, "python"), 0, "原文不学习");
    assert_eq!(english_unigram(&s, "pyth"), 0);
}

// ---------- S03-S06：延迟空格规则 ----------

#[test]
fn s03_second_word_gets_exactly_one_leading_space() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
    type_letters(&mut s, "world");
    assert_eq!(commit_text(&mut s, InputEvent::Space), " world", "第二个英文词前恰好一个空格");
}

#[test]
fn s04_idle_space_passes_through_and_suppresses_the_synthetic_space() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
    assert!(
        matches!(s.handle(InputEvent::Space).expect("空闲空格"), SessionOutput::Noop),
        "空闲空格不消费，交给应用"
    );
    type_letters(&mut s, "world");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "world", "不再补第二个空格");
}

#[test]
fn s05_two_idle_spaces_are_not_collapsed() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    for _ in 0..2 {
        assert!(matches!(
            s.handle(InputEvent::Space).expect("空闲空格"),
            SessionOutput::Noop
        ));
    }
    type_letters(&mut s, "world");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "world");
}

#[test]
fn s06_language_switch_never_adds_a_boundary_space() {
    // 英文 → 中文：不补空格。
    let mut s = session_with_english();
    s.engine.dict.upsert("开发", "kaifa", 100).expect("插入");
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_letters(&mut s, "kaifa");
    let text = commit_text(&mut s, InputEvent::Space);
    assert!(!text.starts_with(' '), "中英之间不自动插空格: {text:?}");

    // 中文 → 英文：同样不补。
    let mut s = session_with_english();
    s.engine.dict.upsert("使用", "shiyong", 100).expect("插入");
    type_letters(&mut s, "shiyong");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "使用");
    type_letters(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello", "中文之后不补空格");
}

// ---------- S07-S09：不学习与一次记账 ----------

#[test]
fn s07_query_paging_highlight_and_escape_never_learn() {
    let mut s = session_with_english();
    type_letters(&mut s, "pyth");
    s.handle(InputEvent::Down).expect("移动高亮");
    s.handle(InputEvent::PageDown).expect("翻页");
    s.handle(InputEvent::Up).expect("移动高亮");
    assert_eq!(english_unigram(&s, "python"), 0, "查询与导航不学习");
    s.handle(InputEvent::Escape).expect("取消");
    assert_eq!(english_unigram(&s, "python"), 0, "取消不学习");
    assert_eq!(english_unigram(&s, "hello"), 0);
}

#[test]
fn s08_chinese_confirmed_segment_blanks_the_english_context() {
    let mut s = session_with_english();
    s.engine.dict.upsert("开发", "kai fa", 100).expect("插入");
    s.engine.dict.upsert("放", "fang", 90).expect("插入");
    // 先提交一个英文词，让上文可信。
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    assert_eq!(s.english_run_context().previous_word(), Some("hello"));
    // 打中文并确认一段（方案 b：候选只覆盖前缀 → 留在 confirmed 里）。
    type_letters(&mut s, "kaifang");
    highlight(&mut s, "开发");
    assert!(
        matches!(s.handle(InputEvent::Space).expect("确认一段"), SessionOutput::Updated(_)),
        "只覆盖前缀时应留确认段"
    );
    assert!(!s.confirmed.is_empty(), "应有确认段");
    assert_eq!(
        s.english_context().previous_word(),
        None,
        "有中文确认段时英文上文必须为空"
    );
    // 撤回确认：上下文按实际剩余确认段重算。
    s.handle(InputEvent::Backspace).expect("撤回确认");
    assert_eq!(
        s.english_context().previous_word(),
        Some("hello"),
        "撤回后英文上文恢复"
    );
    assert_eq!(english_unigram(&s, "hello"), 1, "撤回不产生新学习");
}

#[test]
fn s09_english_never_enters_the_chinese_coin_buffer() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    assert!(s.coin_buffer.is_empty(), "英文不得进中文造词缓冲");
}

// ---------- S12-S13：组合内导航与退格 ----------

#[test]
fn s12_navigation_inside_composition_keeps_the_english_context() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_letters(&mut s, "wor");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"), "打字不清上文");
    s.handle(InputEvent::Left).expect("组合内移动光标");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"), "组合内导航不清上文");
    s.handle(InputEvent::Right).expect("组合内移动光标");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"));
    s.handle(InputEvent::Down).expect("移动高亮");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"));
}

#[test]
fn s13_backspace_inside_composition_keeps_context_and_idle_backspace_clears_it() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    type_letters(&mut s, "wor");
    s.handle(InputEvent::Backspace).expect("组合内退格");
    assert_eq!(s.pinyin, "wo");
    assert_eq!(s.english_run_context().previous_word(), Some("hello"), "组合内退格不清上文");
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
fn s14_page_and_click_use_the_same_cursor_after_paging() {
    let mut s = session_with_english();
    // 英文展示上限是 8 条，单靠英文凑不出第二页——混入中文候选把首页撑满。
    for i in 0..20 {
        let word: &'static str = Box::leak(format!("词{i}").into_boxed_str());
        s.engine.dict.upsert(word, "hel", 1000 - i).expect("插入");
    }
    type_letters(&mut s, "hel");
    assert!(s.candidates.len() > s.page_size, "必须有第二页");
    let first = cursor_word(&s);
    s.handle(InputEvent::PageDown).expect("翻页");
    let paged = cursor_word(&s);
    assert_ne!(first, paged, "翻页应换到下一页首项");
    assert_eq!(commit_text(&mut s, InputEvent::Space), paged, "空格上屏的就是当前高亮");
}

// ---------- U01/U02/U06/U07 ----------

#[test]
fn u01_english_learn_does_not_touch_the_chinese_user_lm() {
    let dict = EnglishDict::from_bytes(build_synth_bin(&words())).expect("词库");
    let engine = ImeEngine::new(None)
        .expect("引擎")
        .with_user_lm(crate::UserLM::in_memory())
        .with_english(EnglishEngine::new(dict));
    let mut s = InputSession::new(Arc::new(engine));
    type_letters(&mut s, "hello");
    commit_text(&mut s, InputEvent::Space);
    let zh = s.engine.user_lm().expect("应有中文用户层");
    assert_eq!(zh.unigram_len(), 0, "英文不得进中文一元计数");
    assert_eq!(zh.entry_len(), 0, "英文不得进中文造词条目");
    assert_eq!(english_unigram(&s, "hello"), 1, "只进英文计数");
}

#[test]
fn u02_zero_counts_add_nothing_and_learning_never_lowers_the_score() {
    let mut s = session_with_english();
    type_letters(&mut s, "pyth");
    let score_of = |s: &InputSession| {
        s.candidates
            .iter()
            .find(|c| c.english_key() == Some("python"))
            .map(|c| c.score)
            .expect("应召回 Python")
    };
    let before = score_of(&s);
    for _ in 0..5 {
        s.engine.learn_english(None, "python");
    }
    s.handle(InputEvent::Escape).expect("清掉上一次组合");
    type_letters(&mut s, "pyth");
    let after = score_of(&s);
    assert!(after > before, "学过之后分数只能更高: {before} → {after}");
}

#[test]
fn u06_python_and_capitalized_share_one_counter_but_display_keeps_case() {
    let mut s = session_with_english();
    type_letters(&mut s, "pyth");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "Python", "显示仍是规范大小写");
    assert_eq!(english_unigram(&s, "python"), 1, "计数记在完整小写 key 上");
    assert_eq!(english_unigram(&s, "Python"), 0, "不另立大写键");
}

#[test]
fn u07_blocking_python_hides_it_from_prefix_exact_and_later_queries() {
    let mut s = session_with_both_user_lms();
    type_letters(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删候选");
    assert_eq!(english_unigram(&s, "python"), 0, "屏蔽同时清掉一元计数");
    for input in ["pyth", "pytho", "python"] {
        let mut t = session_with_english();
        block_english(&t, "python");
        type_letters(&mut t, input);
        assert!(
            !t.candidates.iter().any(|c| c.english_key() == Some("python")),
            "屏蔽后 {input} 不得再召回 python"
        );
    }
}

#[test]
fn u08_undo_reverts_the_actually_most_recent_delete_not_the_current_language() {
    // 中文 → 英文交错删除后，撤销的是**真正最近**的那一次。
    let mut s = session_with_both_user_lms();
    s.engine.dict.upsert("你好", "ni hao", 100).expect("插入");
    s.engine.dict.upsert("泥好", "ni hao", 90).expect("插入");

    type_letters(&mut s, "nihao");
    highlight(&mut s, "你好");
    s.handle(InputEvent::DeleteCandidate).expect("删中文");
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");

    assert!(s.engine.unblock_last_candidate(), "应撤回英文那一条（最后一次）");
    // 英文回来
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "pyth");
    assert!(
        s.candidates.iter().any(|c| c.english_key() == Some("python")),
        "撤销后 Python 必须回来"
    );
    // 中文那条仍在屏蔽表里（撤销只撤一条，不按当前高亮猜语言）。
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "nihao");
    assert!(
        !s.candidates.iter().any(|c| c.word == "你好"),
        "撤销英文不该顺带把中文那条也放了"
    );
}

#[test]
fn u08_undo_prefers_english_when_english_was_deleted_last() {
    let mut s = session_with_both_user_lms();
    s.engine.dict.upsert("你好", "ni hao", 100).expect("插入");
    type_letters(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "nihao");
    highlight(&mut s, "你好");
    s.handle(InputEvent::DeleteCandidate).expect("删中文");

    assert!(s.engine.unblock_last_candidate(), "应撤回中文那一条（最后一次）");
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "nihao");
    assert!(s.candidates.iter().any(|c| c.word == "你好"), "中文应回来");
    s.handle(InputEvent::Escape).expect("清组合");
    type_letters(&mut s, "pyth");
    assert!(
        !s.candidates.iter().any(|c| c.english_key() == Some("python")),
        "英文那条仍应在屏蔽表里"
    );
}

#[test]
fn u08_repeated_delete_does_not_overwrite_the_undo_slot() {
    // 「删除未实际发生」时不得腾空撤销槽位。
    let mut s = session_with_both_user_lms();
    type_letters(&mut s, "pyth");
    highlight(&mut s, "Python");
    s.handle(InputEvent::DeleteCandidate).expect("删英文");
    // 再删一次同一个词：它已不在候选里，手里没有可删的候选——直接问引擎。
    s.engine.delete_english_candidate("python");
    assert!(s.engine.unblock_last_candidate(), "槽位不该被重复删除清空");
    assert!(!s.engine.unblock_last_candidate(), "撤过之后没有第二条");
}

// ---------- 完整时序（会话分册第 10 节） ----------

#[test]
fn full_sequence_records_exactly_one_unigram_and_one_bigram() {
    let mut s = session_with_english();
    type_letters(&mut s, "hello");
    assert_eq!(commit_text(&mut s, InputEvent::Space), "hello");
    type_letters(&mut s, "wor");
    assert_eq!(commit_text(&mut s, InputEvent::Space), " world");
    let snap = s
        .engine
        .english()
        .expect("英文")
        .user()
        .snapshot();
    assert_eq!(snap.english_unigram_count("hello"), 1);
    assert_eq!(snap.english_unigram_count("world"), 1);
    assert_eq!(snap.english_bigram_count("hello", "world"), 1);
    assert_eq!(s.english_run_context().previous_word(), Some("world"));
    // 焦点切换：上文清空，但已提交统计不撤销。
    s.reset_for_focus_change();
    assert_eq!(s.english_run_context().previous_word(), None);
    assert_eq!(snap.english_unigram_count("hello"), 1, "已提交统计不撤销");
}
