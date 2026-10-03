//! 识别态（邮箱/网址）的回声候选构造。
//!
//! 从 `input.rs` 拆出：单文件须 ≤600 行，这段是自成一体的纯构造。

use super::*;

impl InputSession {
    /// 识别态：候选 = 原文本身（回声形态）。
    ///
    /// 借回声候选的既有语义：它是「词库解不出的原始输入串」，上屏按
    /// `commit_raw` 处理——邮箱/网址同样不是词、不该进 UserLM/造词；
    /// 覆盖整串（剩余串按定义为空）。
    pub(super) fn recognized_echo_candidate(pinyin: &str) -> Candidate {
        Candidate {
            word: pinyin.to_string(),
            pinyin: pinyin.to_string(),
            score: 0.0,
            freq: 0,
            spelling: crate::SpellingKind::Exact,
            echo: true,
            covered: pinyin.len(),
        }
    }
}
