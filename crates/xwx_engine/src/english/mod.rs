//! 英文通道：只读词库、英文内部排序与（M2 的）下一词数据。
//!
//! 模块边界见 `docs/english-input/02-engine.md` 第 1 节。本模块**不碰**
//! 中文词库、中文 LM 与中文排序器，两者只在 `engine/bilingual.rs` 的
//! 稳定混排处相遇。

pub mod dict;
pub mod format;

#[cfg(test)]
mod tests;

pub use dict::{static_score_centi, EnglishDict, EnglishEntryRef};
pub use format::{is_english_key, EnglishError};

/// 英文用户学习的**默认文件名**（release）。
///
/// 与中文用户模型分开：英文统计与中文统计不混用一个文件。
pub const ENGLISH_USER_LM_FILE_NAME: &str = "user_english.txt";
/// debug 构建用的英文用户文件名（与真实用户数据隔离）。
pub const ENGLISH_USER_LM_DEV_FILE_NAME: &str = "user_english.dev.txt";
