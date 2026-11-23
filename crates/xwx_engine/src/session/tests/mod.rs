//! `session` 单元测试，按主题拆分；共享 helper 定义在本文件。
//!
//! 子模块：`apostrophe`、`basic`、`coin`、`commit`、`commit_segment`、
//! `english_flow`、`paging_delete`、`prediction_flow`、`punct_verbatim`、`recognizer`。

pub(crate) use super::*;
pub(crate) use crate::Dictionary as _;
pub(crate) use crate::{ImeEngine, LmStore};

fn session_with_dict() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine
        .dict
        .upsert("你好", "ni hao", 100)
        .expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    InputSession::new(Arc::new(engine))
}

/// 带「中→国」bigram 的会话：无上文时「过」(20) > 「国」(10)，有上文「中」时「国」排最前。
fn session_with_lm() -> InputSession {
    let mut engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("中", "zhong", 100).expect("插入失败");
    engine.dict.upsert("国", "guo", 10).expect("插入失败");
    engine.dict.upsert("过", "guo", 20).expect("插入失败");
    let LmStore::Bigram(lm) = &mut engine.lm else {
        panic!("默认引擎应为 bigram（测试 helper 用 new(None) 创建）");
    };
    lm.add_unigram("中", 100);
    lm.add_unigram("国", 100);
    lm.add_unigram("过", 100);
    lm.add_bigram("中", "国", 90);
    InputSession::new(Arc::new(engine))
}

/// 分段上屏会话：含「我们」「我」「今天」，输入 `womenjintian` 可分段确认。
fn session_with_segments() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("我", "wo", 500).expect("插入失败");
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");
    InputSession::new(Arc::new(engine))
}

/// 内存引擎 + 内存用户层，预置词条，返回 `(engine, session)`。
fn session_for_coin(entries: &[(&str, &str, u64)]) -> (Arc<ImeEngine>, InputSession) {
    let engine = Arc::new(
        ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    for (w, py, f) in entries {
        engine.dict.upsert(w, py, *f).expect("插入失败");
    }
    let s = InputSession::new(Arc::clone(&engine));
    (engine, s)
}

/// 逐字符输入 `input` 并选中词面等于 `target` 的候选立即上屏。
fn type_and_commit(s: &mut InputSession, input: &str, target: &str) {
    type_str(s, input);
    let cands = s
        .engine
        .lookup_candidates(input, &crate::LmContext::empty())
        .expect("查询失败");
    let idx = cands
        .iter()
        .position(|c| c.word == target)
        .unwrap_or_else(|| panic!("{input} 的候选里应有 {target}"));
    let committed = s.commit_immediately(idx).expect("目标下标应有候选");
    assert_eq!(committed, target, "应上屏 {target}");
}

/// 12 个同音字（`DEFAULT_PAGE_SIZE=9` → 2 页：9 + 3）。
fn session_with_many_candidates() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    for i in 0..12u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine.dict.upsert(&w, "ni", 1000 - i as u64).expect("插入失败");
    }
    InputSession::new(Arc::new(engine))
}

/// 逐字符发送 `InputEvent::Char`，返回最后一次输出。
fn type_str(s: &mut InputSession, input: &str) -> SessionOutput {
    let mut last = SessionOutput::Noop;
    for c in input.chars() {
        last = s.handle(InputEvent::Char(c)).expect("按键失败");
    }
    last
}

/// 断言识别态不变量：串等于原文、处于识别态、唯一候选为原文回声。
fn assert_recognized(s: &InputSession, raw: &str) {
    assert_eq!(s.pinyin(), raw, "拼音串应为原文 {raw:?}");
    assert!(s.recognized.is_some(), "应处于识别态: {raw:?}");
    assert_eq!(s.candidates.len(), 1, "识别态下候选应只有一条(原文回声): {raw:?}");
    let c = &s.candidates[0];
    assert_eq!(c.word, raw, "唯一候选应为原文本身: {raw:?}");
    assert!(c.echo, "识别态候选应为回声形态(上屏不学习): {raw:?}");
}

mod apostrophe;
mod basic;
mod coin;
mod commit;
mod commit_segment;
mod english_flow;
mod paging_delete;
mod prediction_flow;
mod punct_verbatim;
mod recognizer;
