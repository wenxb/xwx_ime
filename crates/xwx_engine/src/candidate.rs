//! 候选词（candidate）数据结构。
//!
//! 候选词是输入法引擎的核心数据单元：一次按键处理的结果会产出一组候选词，
//! 上屏的也是候选词。`Candidate` 同时承担两层含义：
//!
//! 1. **词典条目**：`dict.rs` 从词库查出的原始条目（此时 `score` 尚未计算，为 0.0）；
//! 2. **引擎排序结果**：`lib.rs` 综合词频 + 语言模型打分后的最终候选
//!    （此时 `score` 是最终得分，`freq` 保留原始词频供调试/回退使用）。

use serde::{Deserialize, Serialize};

/// 一个候选词。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// 候选词文本，如 "你好"。
    pub word: String,
    /// 对应的拼音串。多音节时用空格分隔，如 "ni hao"。
    ///
    /// 约定与 `data/README.md` 中词库的 `pinyin` 字段格式一致。
    pub pinyin: String,
    /// 最终得分（越大越靠前）。由引擎综合词频与语言模型打分得出；
    /// 词典直接产出的条目此字段为 0.0。
    pub score: f32,
    /// 原始词频（词典中记录的静态频率），用于展示与打分的回退基准。
    pub freq: u64,
    /// 该候选是否来自含模糊拼音边的解码路径(分层排序用)。
    ///
    /// 精确匹配为 `false`;只要到达该候选的最优路径含任一模糊边
    /// (一处不精确则整体不精确)即为 `true`。词典直接产出的条目与
    /// `fallback_candidates` 联想路径(不经过 `expand_fuzzy`)恒为
    /// `false`,因此 `fuzzy_enabled = false` 时全部候选都在精确层,
    /// 分层退化为无操作。
    ///
    /// 序列化向后兼容:旧数据没有此字段,反序列化按 `false`(精确)
    /// 处理——这正是字段引入前的语义,旧候选天然全在精确层。
    #[serde(default)]
    pub fuzzy: bool,
}

impl Candidate {
    /// 构造一个新候选词（`score` 初始为 0.0，由引擎后续打分）。
    pub fn new(word: impl Into<String>, pinyin: impl Into<String>, freq: u64) -> Self {
        Self {
            word: word.into(),
            pinyin: pinyin.into(),
            score: 0.0,
            freq,
            fuzzy: false, // 词典条目是精确匹配;分层信息由引擎回填
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let c = Candidate::new("你好", "ni hao", 42);
        let json = serde_json::to_string(&c).expect("序列化不应失败");
        let back: Candidate = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(c, back);
    }

    #[test]
    fn serde_legacy_without_fuzzy_defaults_exact() {
        // 字段引入前的旧格式没有 `fuzzy`:反序列化应回退为 false(精确层),
        // 与引入前的语义一致(旧候选全是精确匹配)。
        let legacy = r#"{"word":"你好","pinyin":"ni hao","score":-1.5,"freq":42}"#;
        let c: Candidate = serde_json::from_str(legacy).expect("旧格式反序列化不应失败");
        assert!(!c.fuzzy, "缺省 fuzzy 应为 false(精确层)");
        assert_eq!(c.word, "你好");
        assert_eq!(c.freq, 42);
    }
}
