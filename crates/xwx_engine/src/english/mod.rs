//! 英文通道：只读词库、英文内部排序与（M2 的）下一词数据。
//!
//! 模块边界见 `docs/english-input/02-engine.md` 第 1 节。本模块**不碰**
//! 中文词库、中文 LM 与中文排序器，两者只在 `engine/blend.rs` 的稳定
//! 混排处相遇。

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

use crate::user_lm::UserLM;

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
    /// M2 零输入下一词面板开关（M1 恒 false，前缀补全不受它影响）。
    predict_enabled: bool,
}

impl EnglishEngine {
    /// 用只读词库建英文组件（用户层为纯内存，测试/探针默认）。
    pub fn new(dict: EnglishDict) -> Self {
        Self {
            dict,
            user: UserLM::in_memory(),
            predict_enabled: false,
        }
    }

    /// 换成显式用户实例（生产入口传入可持久化的那个）。
    pub fn with_user_lm(mut self, user: UserLM) -> Self {
        self.user = user;
        self
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
}
