//! crate 级单测(原为 `lib.rs` 里 2522 行的内联模块,2026-10-02 拆成按主题
//! 的七个文件)。本文件放共享 helper。

use super::*;
use crate::engine::rank::{
    effective_layer, leading_abbrev_input, pure_abbrev_input, tail_only, unexplained_letters,
    unreachable_tail,
};

/// 测试内的排序首键(与 [`sort_key`] 的 bucket 字段同式):
/// 候选终点处的不可避免未解释字母数(u_min 桶)。
/// 断言「(bucket, 层序) 整体非降」比旧的「(未解释字母数, 层序)
/// 非降」更贴近排序键本身——bucket 才是首键;`unexplained_letters`
/// 只是罚分因子,不再是键。
fn sort_bucket(inp: &str, c: &Candidate) -> usize {
    if c.spelling == SpellingKind::Abbrev {
        return 0;
    }
    let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
    if cp_len > inp.len() {
        0 // 补全词必以输入开头,u=0,桶恒 0
    } else if tail_only(inp, c) && inp.len() - cp_len >= MAX_TYPO_GAP {
        // 「吃掉了整段前缀」的读法(`zmgai` → 该):桶 = 被跳过的字母数。
        // 判据与生产**共用** `tail_only` 与 `MAX_TYPO_GAP`,不另写一份。
        unexplained_letters(inp, c)
    } else {
        unreachable_tail(inp, None)[cp_len]
    }
}

/// 测试内的排序第二键(**有效层序**),与 [`sort_key`] 的 `spelling` 字段同式。
///
/// 判据与生产**共用**（[`effective_layer`] + 两个类别判据），不另写一份——
/// 两份判据迟早漂移，而漂移的症状是「测试与实现各说各话」。
/// `reachable` 用 `unreachable_tail(inp, None)[0] == 0` 近似生产侧的
/// `lattice_reachable`：两者只在「某音节的词条在词库里一个都没有」时可能分歧，
/// 而断言用到的输入都不是这种形状；真分歧的后果是断言**变红**，不会静默放过。
fn sort_layer(inp: &str, c: &Candidate, seg: &Segmenter, has_abbrev: bool) -> SpellingKind {
    effective_layer(
        c.spelling,
        c.freq,
        c.covered,
        inp.len(),
        entry_tier_exception(inp, seg, has_abbrev),
    )
}

/// 层序收编的输入类别判据：纯缩写 ∪ 开头即缩写（与 `lookup.rs` 同源）。
/// 生产侧的门槛还要 `!reachable`，此处由 [`sort_layer`] 传入。
fn entry_tier_exception(inp: &str, seg: &Segmenter, has_abbrev: bool) -> bool {
    if unreachable_tail(inp, None)[0] == 0 {
        return false; // 精确格可达 ⇒ 走的是 `sort_candidates(.., false)` 那条路
    }
    pure_abbrev_input(inp, has_abbrev) || leading_abbrev_input(seg, inp, has_abbrev)
}

/// 万象 gram 的常规位置（与 indexed_mixed 的查找规则一致）。
fn gram_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("XWX_LM_PATH") {
        let p = std::path::PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = std::path::PathBuf::from(home).join(".local/share/xwx/wanxiang-lts-zh-hans.gram");
    p.is_file().then_some(p)
}

/// 真实数据的生产同款装配（真实词库 + 简拼副索引 + 真实英文词库 +
/// 万象 gram）；缺件时返回 None，用例自行跳过。
fn real_bilingual_engine() -> Option<ImeEngine> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let dict = root.join("dict.bin");
    let abbrev = root.join("abbrev.bin");
    if !dict.is_file() || !abbrev.is_file() {
        return None;
    }
    let mut engine = match gram_path() {
        Some(gram) => ImeEngine::new_with_lm(Some(&dict), Some(&gram)).ok()?,
        None => ImeEngine::new(Some(&dict)).ok()?,
    };
    engine.dict = engine.dict.with_abbrev(&abbrev);
    let graph = root.join("syllable_graph.bin");
    if graph.is_file() {
        engine.dict = engine.dict.with_syllable_graph(&graph);
    }
    let english = root.join("english.bin");
    if !english.is_file() {
        return None;
    }
    let en = crate::english::EnglishDict::open(&english).ok()?;
    Some(engine.with_english(crate::english::EnglishEngine::new(en)))
}

mod input;
mod lookup;
mod beam_lm;
mod fuzzy;
mod abbrev;
mod abbrev_quota;
mod indexed_mixed;
mod abbrev_correct;
mod personalization;
mod completion;
mod prewarm;
mod query_snapshot;
mod dag_reachability;
mod typo_budget;
mod typo_repeat;
mod typo_window;
mod tail_completion;
mod exact_first;
mod projection_filler;
mod recall;
mod sentence_gate;
mod bilingual;
mod blend_matrix;
mod blocklist;
