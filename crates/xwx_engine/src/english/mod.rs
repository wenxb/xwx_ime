//! 英文通道：只读词库、英文内部排序与（M2 的）下一词数据。
//!
//! 模块边界见 `docs/english-input/02-engine.md` 第 1 节。本模块**不碰**
//! 中文词库、中文 LM 与中文排序器，两者只在 `engine/blend.rs` 的稳定
//! 混排处相遇。

pub mod bigram;
pub mod context;
pub mod dict;
pub mod format;
pub mod rank;
pub mod recall;

#[cfg(test)]
mod synth;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use synth::{alpha_key, build as build_synth_bin, W as SynthWord};

use crate::candidate::{Candidate, EnglishMatchKind};
use crate::user_lm::UserLM;

use self::rank::{conditional_base, user_counts, EnglishHit, RankedEnglish};

/// 零输入预测面板最多展示的下一词数。
pub const ENGLISH_NEXT_LIMIT: usize = 5;

pub use bigram::{BigramEdge, BigramRow, EnglishBigram};
pub use context::EnglishContext;
pub use dict::{static_score_centi, EnglishDict, EnglishEntryRef};
pub use format::{is_english_key, EnglishError};
pub use rank::ENGLISH_DISPLAY_LIMIT;
pub(crate) use rank::rank_english;
pub(crate) use recall::recall_english;

/// 英文用户学习的**默认文件名**（release）。
///
/// 与中文用户模型分开：英文统计与中文统计不混用一个文件。
pub const ENGLISH_USER_LM_FILE_NAME: &str = "user_english.txt";
/// debug 构建用的英文用户文件名（与真实用户数据隔离）。
pub const ENGLISH_USER_LM_DEV_FILE_NAME: &str = "user_english.dev.txt";

/// 英文组件：只读词库 + **独立**英文用户实例（+ M2 的可选二元数据）。
///
/// 设计边界（引擎分册第 1 节）：不引入通用插件框架，也不把英文用户实例
/// 装进中文 `MixedLM`——两者的计数混在一起会让「学英文」改变中文候选。
#[derive(Debug)]
pub struct EnglishEngine {
    dict: EnglishDict,
    user: UserLM,
    /// M2 零输入下一词面板开关（前缀补全不受它影响）。
    predict_enabled: bool,
    /// M2 可选静态二元模型（`None` = 没有静态数据，只剩用户词对）。
    bigram: Option<EnglishBigram>,
}

impl EnglishEngine {
    /// 用只读词库建英文组件（用户层为纯内存，测试/探针默认）。
    pub fn new(dict: EnglishDict) -> Self {
        Self {
            dict,
            user: UserLM::in_memory(),
            predict_enabled: false,
            bigram: None,
        }
    }

    /// 换成显式用户实例（生产入口传入可持久化的那个）。
    pub fn with_user_lm(mut self, user: UserLM) -> Self {
        self.user = user;
        self
    }

    /// 接上静态二元模型（M2）。加载失败由装配层决定是否降级，不在这里吞错。
    pub fn with_bigram(mut self, model: EnglishBigram) -> Self {
        self.bigram = Some(model);
        self
    }

    /// 静态二元模型句柄。
    pub fn bigram(&self) -> Option<&EnglishBigram> {
        self.bigram.as_ref()
    }

    /// 开启 M2 零输入下一词面板。
    pub fn with_predict(mut self, enabled: bool) -> Self {
        self.predict_enabled = enabled;
        self
    }

    /// 只读词库。
    pub fn dict(&self) -> &EnglishDict {
        &self.dict
    }

    /// 英文用户实例。
    pub fn user(&self) -> &UserLM {
        &self.user
    }

    /// M2 预测面板是否开启。
    pub fn predict_enabled(&self) -> bool {
        self.predict_enabled
    }

    /// **零输入下一词召回**（M2）。
    ///
    /// 与 `lookup_composition` 是两条独立入口：这里**不接收 input**，也不
    /// 复用空串组合查询（会话分册/预测分册第 1、6 节）。候选池 = 静态上一词
    /// Top-32 ∪ 用户上一词 Top-32（去重前上界 64）；按完整 key 去重、过滤
    /// 词库里已不存在的词与全局英文屏蔽词，再按条件排序，`extra = 0`，
    /// 最多 [`ENGLISH_NEXT_LIMIT`] 条，全部是 `NextWord`。
    ///
    /// 两种来源都没有命中时返回**空 Vec**（调用方据此不展示面板，
    /// 绝不用全局高频词硬填）。展示不学习、不改英文上文、不生成前导空格。
    pub fn next_candidates(
        &self,
        ctx: &EnglishContext<'_>,
    ) -> Result<Vec<Candidate>, EnglishError> {
        let Some(prev) = ctx.previous_word else {
            return Ok(Vec::new());
        };
        // 一次预测取**一代**新快照，整个计算同一代。
        let snapshot = self.user.snapshot();
        let row = match &self.bigram {
            Some(model) => model.row(prev)?,
            None => None,
        };
        let mut merged: std::collections::BTreeMap<String, EnglishHit> =
            std::collections::BTreeMap::new();
        let mut push = |key: &str,
                        merged: &mut std::collections::BTreeMap<String, EnglishHit>|
         -> Result<(), EnglishError> {
            if merged.contains_key(key) {
                return Ok(());
            }
            let Some(e) = self.dict.exact(key)? else {
                return Ok(()); // 升级后已移除的词不凭旧计数复活
            };
            let (uni, bi) = user_counts(&snapshot, ctx, e.key);
            let static_base = match (&self.bigram, row) {
                (Some(model), Some(r)) => {
                    let c = r
                        .iter(model)
                        .find(|x| x.next == e.key)
                        .map(|x| x.count)
                        .unwrap_or(0);
                    conditional_base(e.zipf_centi, c, r.observed_total)
                }
                _ => None,
            };
            merged.insert(
                e.key.to_string(),
                EnglishHit {
                    key: e.key.to_string(),
                    display: e.display.to_string(),
                    zipf_centi: e.zipf_centi,
                    // 零输入没有输入串，所以是 NextWord（covered 恒 0）。
                    match_kind: EnglishMatchKind::NextWord,
                    user_unigram: uni,
                    user_bigram: bi,
                    static_base,
                },
            );
            Ok(())
        };
        if let Some((model, r)) = self.bigram.as_ref().zip(row) {
            for e in r.iter(model) {
                push(e.next, &mut merged)?;
            }
        }
        for key in snapshot.english_next_top(prev) {
            push(key, &mut merged)?;
        }
        merged.retain(|key, _| !snapshot.is_blocked(recall::ENGLISH_BLOCK_NAMESPACE, key));

        // extra = 0：零输入没有「补全差多少」这回事。
        let ranked: RankedEnglish = rank::rank_english(merged.into_values().collect(), 0, ctx);
        let mut out = ranked.candidates(0);
        out.truncate(ENGLISH_NEXT_LIMIT);
        Ok(out)
    }
}
