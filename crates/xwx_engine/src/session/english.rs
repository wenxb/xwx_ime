//! 英文会话上文：本会话**已实际提交**、仍可信的上一个英文词。
//!
//! 与另外两件事严格分开（会话分册第 1 节）：
//! - [`InputSession::english_mode`]（Ctrl+空格直通）是模式；
//! - 这里只是「上一个提交过的英文词」，用来做下一词的召回与打分。
//!
//! 复用它们当英文上文会让两者的边界互相污染（模式切换会抹掉上文），
//! 所以是独立状态。

use crate::english::EnglishContext;

/// 英文上文 + 「之后是否已经透传过空格」。
///
/// `separator_seen` 只服务一件事：提交新英文候选时要不要补一个前导空格
/// （会话分册第 5 节的固定规则，不让实现自行发挥）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnglishRunContext {
    /// 已提交词的规范小写 key。
    last_word: Option<String>,
    /// 之后已经透传过 ASCII 空格。
    separator_seen: bool,
}

impl EnglishRunContext {
    /// 空上文。
    pub fn new() -> Self {
        Self::default()
    }

    /// 交给查询器的**只读**视图（查询器不改会话）。
    pub fn context(&self) -> EnglishContext<'_> {
        match self.last_word.as_deref() {
            Some(w) => EnglishContext::with_previous(w),
            None => EnglishContext::empty(),
        }
    }

    /// 上一个已提交英文词。
    pub fn previous_word(&self) -> Option<&str> {
        self.last_word.as_deref()
    }

    /// 是否已经透传过空格。
    pub fn separator_seen(&self) -> bool {
        self.separator_seen
    }

    /// 是否需要在下一个英文词前补一个空格。
    ///
    /// `needs_space = last_word.is_some() && !separator_seen`：用户已经
    /// 显式敲过空格时不重复补，也不回删用户已输入的空格。
    pub fn needs_space(&self) -> bool {
        self.last_word.is_some() && !self.separator_seen
    }

    /// 提交了一个英文词：上文换成它，并清掉「已透传空格」标记
    /// （应用里的最后一个字符又是新词末尾）。
    pub fn on_word_committed(&mut self, key: &str) {
        self.last_word = Some(key.to_string());
        self.separator_seen = false;
    }

    /// 空闲时透传了一个 ASCII 空格：保留上一词，只标记住「已隔开」。
    ///
    /// 没有上一词时是纯透传，不改状态。
    pub fn on_passthrough_space(&mut self) {
        if self.last_word.is_some() {
            self.separator_seen = true;
        }
    }

    /// 语义边界：清空英文上文。
    ///
    /// 由原文提交、中文候选、标点、识别态、Escape、模式切换、焦点切换
    /// 以及空闲时交给应用的按键调用。**`reset()` 不调用它**——否则
    /// 刚提交英文后的组合收尾就会把下一词需要的上文抹掉。
    pub fn clear(&mut self) {
        self.last_word = None;
        self.separator_seen = false;
    }
}
