//! `RecallHit`：一条**召回事实**（谁、以什么拼写、消费了多长一段、花了几次编辑）。
//!
//! 与展示候选 [`Candidate`] 分开存放：四条召回路径各自产出事实，折叠成候选是
//! 后续步骤（见 `docs/recall-hit.md`）。**当前只有简拼路径产出、且不参与任何
//! 排序与截断**——这一步的验收在于「同词同音、消费范围不同的两条事实都还在」。

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
