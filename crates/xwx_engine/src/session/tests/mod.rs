//! `session` 的单测(原为 `session.rs` 里 2538 行的内联模块)。
//!
//! 按主题分六个文件,共享 helper 都在本文件里:
//! `basic.rs`、`commit.rs`、`punct_verbatim.rs`、`coin.rs`、`paging_delete.rs`、`recognizer.rs`。
//!
//! 子模块用 `use super::*` 拿到 helper;`session` 的类型经由本模块的
//! `pub(crate) use super::*` 继续可见。

pub(crate) use super::*;
pub(crate) use crate::Dictionary as _;
pub(crate) use crate::{ImeEngine, LmStore};

fn session_with_dict() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // 手工塞几条词（内存词库）
    engine
        .dict
        .upsert("你好", "ni hao", 100)
        .expect("插入失败");
    engine.dict.upsert("你", "ni", 200).expect("插入失败");
    engine.dict.upsert("好", "hao", 150).expect("插入失败");
    InputSession::new(Arc::new(engine))
}

/// 带「中→国」词对的会话：上文注入后 bigram 分要能改变候选排序。
///
/// 词频：过(20) > 国(10)，无上文时排前面的是「过」；有上文「中」
/// 时 bigram(中,国) 分应把「国」抬到最前。
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

/// 分段上屏用的会话:词库里有「我们」「今天」两个整词与它们的单字,
/// 输入 `womenjintian` 整串可解(格边候选覆盖各自那一段)。
fn session_with_segments() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    engine.dict.upsert("我们", "wo men", 300).expect("插入失败");
    engine.dict.upsert("我", "wo", 500).expect("插入失败");
    engine.dict.upsert("今天", "jin tian", 300).expect("插入失败");
    InputSession::new(Arc::new(engine))
}

/// 造词测试的引擎装配:内存引擎 + 内存用户层,词库塞入
/// 指定的单字/词(供逐字选择),返回 (engine, session)。
fn session_for_coin(entries: &[(&str, &str, u64)]) -> (Arc<ImeEngine>, InputSession) {
    let engine = Arc::new(
        crate::ImeEngine::new(None)
            .expect("内存引擎创建失败")
            .with_user_lm(crate::UserLM::in_memory()),
    );
    for (w, py, f) in entries {
        engine.dict.upsert(w, py, *f).expect("插入失败");
    }
    let s = InputSession::new(Arc::clone(&engine));
    (engine, s)
}

/// 逐个打出 `input` 并选中「按词精确匹配目标」的候选上屏。
///
/// 候选列表里同拼音可能有多个字,按 word 精确定位再上屏
/// (模拟用户逐字选择)。目标词必须在候选里,否则 panic。
fn type_and_commit(s: &mut InputSession, input: &str, target: &str) {
    for ch in input.chars() {
        s.handle(InputEvent::Char(ch)).expect("按键失败");
    }
    // 用引擎直接查候选列表定位目标词的全局下标(与会话内部
    // 一致:同一引擎、同一输入、空上下文),再 commit_at 上屏
    // (等价于用户翻到该候选并选中)。
    let cands = s
        .engine
        .lookup_candidates(input, &crate::LmContext::empty())
        .expect("查询失败");
    let idx = cands
        .iter()
        .position(|c| c.word == target)
        .unwrap_or_else(|| panic!("{input} 的候选里应有 {target}"));
    let committed = s.commit_at(idx).expect("目标下标应有候选");
    assert_eq!(committed, target, "应上屏 {target}");
}

/// 造一个「候选超过一页」的会话:同一拼音塞 page_size+3 个同音字,
/// 保证至少有 2 页可翻。
fn session_with_many_candidates() -> InputSession {
    let engine = ImeEngine::new(None).expect("内存引擎创建失败");
    // DEFAULT_PAGE_SIZE=9,塞 12 个同音字 → 2 页(9 + 3)
    for i in 0..12u32 {
        let w = char::from_u32(0x4E00 + i).unwrap().to_string();
        engine.dict.upsert(&w, "ni", 1000 - i as u64).expect("插入失败");
    }
    InputSession::new(Arc::new(engine))
}

/// 往会话里逐字符打 `input`,返回最后一次的输出。
fn type_str(s: &mut InputSession, input: &str) -> SessionOutput {
    let mut last = SessionOutput::Noop;
    for c in input.chars() {
        last = s.handle(InputEvent::Char(c)).expect("按键失败");
    }
    last
}

/// 断言识别态的三条不变量:串等于原文、候选恰一条(原文回声)、
/// 候选窗里没有任何拼音候选(词 ≠ 原文的都不能有)。
fn assert_recognized(s: &InputSession, raw: &str) {
    assert_eq!(s.pinyin(), raw, "拼音串应为原文 {raw:?}");
    assert!(s.recognized.is_some(), "应处于识别态: {raw:?}");
    assert_eq!(s.candidates.len(), 1, "识别态下候选应只有一条(原文回声): {raw:?}");
    let c = &s.candidates[0];
    assert_eq!(c.word, raw, "唯一候选应为原文本身: {raw:?}");
    assert!(c.echo, "识别态候选应为回声形态(上屏不学习): {raw:?}");
}

mod basic;
mod commit;
mod punct_verbatim;
mod coin;
mod paging_delete;
mod prefix_mixed;
mod recognizer;
