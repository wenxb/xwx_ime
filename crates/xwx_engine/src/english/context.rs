//! 英文上文的值类型与会话边界。
//!
//! 与 `english_mode`(Ctrl+空格直通)和 `english_run`(大写起头逐字直出)是
//! **三件不同的事**：这里只管「本会话已经实际提交、仍可信的上一个英文词」。

/// 一次英文查询看到的只读上文（查询器不改会话）。
///
/// 只带一个前词：英文 M1 的召回与排序都只用到它。`None` 表示无可信上文
/// （会话一开始、提交过中文、发生过应用操作边界……）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnglishContext<'a> {
    /// 上一个**实际提交**的英文词（规范小写 key）。
    pub previous_word: Option<&'a str>,
}

impl<'a> EnglishContext<'a> {
    /// 无可信英文上文。
    pub fn empty() -> EnglishContext<'static> {
        EnglishContext { previous_word: None }
    }

    /// 带上一个已提交的英文词。
    pub fn with_previous(word: &'a str) -> Self {
        Self { previous_word: Some(word) }
    }

    /// 是否没有可信上文。
    pub fn is_empty(&self) -> bool {
        self.previous_word.is_none()
    }

    /// 上一个可信英文词（只读）。
    pub fn previous_word(&self) -> Option<&'a str> {
        self.previous_word
    }
}
