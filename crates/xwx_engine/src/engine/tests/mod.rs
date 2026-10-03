//! crate 级单测(原为 `lib.rs` 里 2522 行的内联模块,2026-10-02 拆成按主题
//! 的七个文件)。本文件放共享 helper。

use super::*;
use crate::engine::rank::unreachable_tail;

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
    } else {
        unreachable_tail(inp, None)[cp_len]
    }
}

mod input;
mod lookup;
mod beam_lm;
mod fuzzy;
mod abbrev;
mod abbrev_quota;
mod personalization;
mod completion;
mod prewarm;
mod query_snapshot;
mod dag_reachability;
mod typo_budget;
mod typo_repeat;
mod tail_completion;
mod projection_filler;
mod quota;
mod recall;
mod sentence_gate;
mod blocklist;
