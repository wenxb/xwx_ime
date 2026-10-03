//! `RecallHit`：一条**召回事实**（谁、以什么拼写、消费了多长一段、花了几次编辑）。
//!
//! 与展示候选 [`Candidate`] 分开存放：四条召回路径各自产出事实，折叠成候选是
//! 后续步骤（见 `docs/recall-hit.md`）。**当前只有简拼路径产出、且不参与任何
//! 排序与截断**——这一步的验收在于「同词同音、消费范围不同的两条事实都还在」。

use std::collections::HashMap;

use crate::{Candidate, SpellingKind};

/// 候选是从哪条路被召回的。
///
/// ① 步只落地结构：目前只有 `Abbrev` 被构造，其余分支在 ② ③ 步接入。
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecallSource {
    /// 精确格子的路径解码。
    Lattice,
    /// 简拼层（首字母副索引 + 混拼 + 整串词串）。
    Abbrev,
    /// 容错纠错读法。
    Tolerant,
    /// 整句候选。
    Sentence,
}

/// 一条召回事实。
///
/// ① 步只落地结构，`spelling` / `edits` / `source` 在 ② ③ 步被消费。
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecallHit {
    pub(super) source: RecallSource,
    pub(super) spelling: SpellingKind,
    /// 消费的输入字节数（干净串坐标），即展示候选的 `covered`。
    pub(super) consumed: usize,
    /// 这条路径上的编辑次数（容错路径大于 0）。
    pub(super) edits: u8,
}

/// 把一条召回路径产出的候选记成事实。
///
/// **不去重**：同词同音、消费范围不同的两条是两条事实——分段上屏要靠 `consumed`
/// 区分，折叠成展示候选是后面的事。
#[cfg(test)]
pub(super) fn collect_recall_hits(cands: &[Candidate], source: RecallSource) -> Vec<RecallHit> {
    cands
        .iter()
        .map(|c| RecallHit {
            source,
            spelling: c.spelling,
            consumed: c.covered,
            edits: 0,
        })
        .collect()
}

/// 把展示候选按 `(word, pinyin)` 折叠。
///
/// 召回事实层**不去重**（同词同音、消费范围不同是两条事实，见
/// [`collect_recall_hits`]），折叠只发生在展示这一侧，并且保留**消费范围最大**
/// 的那条——分段上屏要的是「这个词最多能吃掉多少输入」。
///
/// 反例（实测）：把去重键加宽成 `(word, pinyin, covered)` 会让同一个词在候选窗里
/// 出现两次（v1 对拍重复 word 由 4 条涨到 10 条）。
/// 把「候选 + 它的召回来源」按 `(word, pinyin)` 折叠。
///
/// 规则与 [`fold_display_candidates`] 完全一致（保留消费范围最大的那条、
/// 并列时保留先到的），额外**保留来源**：阶段 4 的展示投影要靠
/// `RecallHit::source` 决定层序、配额与整串置顶，而不是靠「排序后第一条
/// 覆盖整串的候选」这种位置性判断。
///
/// 目前还没有调用方（下一步在 `lookup.rs` 接上），先把规则与来源一起钉住。
#[allow(dead_code)]
pub(super) fn fold_with_sources(
    cands: Vec<(Candidate, RecallSource)>,
) -> Vec<(Candidate, RecallSource)> {
    let mut out: Vec<(Candidate, RecallSource)> = Vec::with_capacity(cands.len());
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    for (c, src) in cands {
        match index.get(&(c.word.clone(), c.pinyin.clone())) {
            Some(&i) => {
                // 同词同音：保留消费范围更大的那条（并列时保留先到的，稳定），
                // 来源跟着胜出的那条走。
                if c.covered > out[i].0.covered {
                    out[i] = (c, src);
                }
            }
            None => {
                index.insert((c.word.clone(), c.pinyin.clone()), out.len());
                out.push((c, src));
            }
        }
    }
    out
}

pub(super) fn fold_display_candidates(cands: Vec<Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::with_capacity(cands.len());
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    for c in cands {
        match index.get(&(c.word.clone(), c.pinyin.clone())) {
            Some(&i) => {
                // 同词同音：保留消费范围更大的那条（并列时保留先到的，稳定）。
                if c.covered > out[i].covered {
                    out[i] = c;
                }
            }
            None => {
                index.insert((c.word.clone(), c.pinyin.clone()), out.len());
                out.push(c);
            }
        }
    }
    out
}
