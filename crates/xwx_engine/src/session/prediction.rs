//! M2：**零输入下一词预测态**（`CandidateMode::Prediction`）。
//!
//! 与组合态严格分开：预测列表不叫 `candidates`、不参与 `composing()`、
//! 不走 `commit_candidate` 的 `covered` 语义。见
//! `docs/english-input/04-prediction.md` 第 6、7、8 节。

use super::*;
use crate::english::EnglishContext;

/// 预测态处理一个事件的结果。
pub(super) enum PredictionStep {
    /// 预测态已经给出完整输出，**立即返回**，不再走普通处理。
    Handled(SessionOutput),
    /// 预测已关闭：调用方继续把同一个事件走一次普通处理。
    Closed,
}

impl InputSession {
    /// 是否处于预测态（有下一词候选就算）。
    pub fn predicting(&self) -> bool {
        !self.prediction.is_empty()
    }

    /// 清掉预测面板（不动英文上文、不发任何信号）。
    pub(super) fn clear_prediction(&mut self) {
        self.prediction = Arc::new(Vec::new());
        self.prediction_cursor = 0;
    }

    /// **明确选择英文词之后**才调用（会话分册第 6 节）。
    ///
    /// 标点 / Enter 原文 / Verbatim 大写 / 模式切换 / 焦点清算都**不**走到
    /// 这里，所以它们不会弹出预测面板。返回面板是否真的开起来了。
    pub(super) fn maybe_start_prediction(&mut self) -> bool {
        self.clear_prediction();
        let Some(en) = self.engine.english() else {
            return false;
        };
        if !en.predict_enabled() {
            return false;
        }
        // 预测态的不变量（第 7 节）：任何一条不满足就退回普通空闲态，
        // 绝不维持一个空的 Prediction 状态。
        if !self.pinyin.is_empty()
            || !self.confirmed.is_empty()
            || self.caret != 0
            || self.recognized.is_some()
            || self.english_mode
            || self.english_run
        {
            return false;
        }
        let Some(prev) = self.english.previous_word().map(str::to_string) else {
            return false;
        };
        let ctx = EnglishContext::with_previous(&prev);
        let Ok(list) = en.next_candidates(&ctx) else {
            return false; // 读失败只是没有面板，不影响已提交的文本
        };
        if list.is_empty() || list.iter().any(|c| !c.is_next_word()) {
            return false;
        }
        self.prediction = Arc::new(list);
        self.prediction_cursor = 0;
        true
    }

    /// 高亮的下一词。
    fn highlighted_prediction(&self) -> Option<Candidate> {
        self.prediction.get(self.prediction_cursor).cloned()
    }

    /// 提交一条下一词候选：上屏 + 学习（复用英文提交路径）。
    ///
    /// 返回 `(上屏文本, 是否还能继续预测)`。
    fn commit_prediction(&mut self, c: &Candidate) -> (String, bool) {
        // `prediction_cursor` 与候选表在提交期间都不再使用，先清掉面板，
        // 让 `record_english_commit` 在干净的空闲态上跑（前导空格、学习、
        // 上屏尾部都按英文词的既有规则）。
        let text = self.record_english_commit(c);
        let more = self.maybe_start_prediction();
        let _ = &text;
        (text, more)
    }

    /// 预测态的按键分发表（第 8 节，逐行实现，测试与它一一对应）。
    pub(super) fn handle_prediction(&mut self, event: &InputEvent) -> PredictionStep {
        match event {
            // 普通字母：退出预测面板、**保留**英文上文，同一个事件进普通组合查询
            InputEvent::Char(_) | InputEvent::Verbatim(_) => {
                self.clear_prediction();
                PredictionStep::Closed
            }
            // Space：关闭预测、保留上一词并标记显式空格，原空格交给应用
            InputEvent::Space => {
                self.clear_prediction();
                self.english.on_passthrough_space();
                PredictionStep::Handled(SessionOutput::Noop)
            }
            // Enter：关闭预测、清空英文上文，回车交给应用
            InputEvent::Enter => {
                self.close_and_clear();
                PredictionStep::Handled(SessionOutput::Noop)
            }
            // Backspace/Left/Right：关闭预测、清空英文上文，应用正常编辑
            // （前向 Delete 没有会话事件，由 IBus 层按同一条规则处理。）
            InputEvent::Backspace | InputEvent::Left | InputEvent::Right => {
                self.close_and_clear();
                PredictionStep::Handled(SessionOutput::Noop)
            }
            // Escape：关闭预测并清空英文上文（**消费**该键）
            InputEvent::Escape => {
                self.close_and_clear();
                PredictionStep::Handled(SessionOutput::Updated(self.update()))
            }
            // 数字：有效序号提交；`0` 或超出候选数的数字关闭并透传
            InputEvent::Select(idx) => {
                let n = usize::from(*idx);
                if n == 0 || n > self.prediction.len() {
                    self.close_and_clear();
                    PredictionStep::Handled(SessionOutput::Noop)
                } else {
                    let c = self.prediction[n - 1].clone();
                    PredictionStep::Handled(self.after_prediction_commit(&c))
                }
            }
            // 鼠标选择有效候选
            InputEvent::CandidateClicked(idx) => {
                let n = *idx;
                match self.prediction.get(n).cloned() {
                    Some(c) => PredictionStep::Handled(self.after_prediction_commit(&c)),
                    None => {
                        self.clear_prediction();
                        PredictionStep::Handled(SessionOutput::Noop)
                    }
                }
            }
            // Tab：接受当前高亮的下一词
            InputEvent::Tab => match self.highlighted_prediction() {
                Some(c) => PredictionStep::Handled(self.after_prediction_commit(&c)),
                None => {
                    self.clear_prediction();
                    PredictionStep::Handled(SessionOutput::Noop)
                }
            },
            // Up/Down：移动高亮，不查询、不学习
            InputEvent::Up | InputEvent::Down | InputEvent::PageUp | InputEvent::PageDown => {
                self.move_prediction_cursor(event);
                PredictionStep::Handled(SessionOutput::Updated(self.update()))
            }
            // Ctrl+Delete：屏蔽该英文目标并重算预测
            InputEvent::DeleteCandidate => {
                if let Some(c) = self.highlighted_prediction() {
                    if let Some(key) = c.english_key().map(str::to_string) {
                        self.engine.delete_english_candidate(&key);
                    }
                }
                self.maybe_start_prediction();
                if self.predicting() {
                    PredictionStep::Handled(SessionOutput::Updated(self.update()))
                } else {
                    self.clear_prediction();
                    PredictionStep::Handled(SessionOutput::Updated(self.update()))
                }
            }
            // Ctrl+Shift+Delete：撤销最近一次真实删除；有上文时可重新生成预测
            InputEvent::UnblockCandidate => {
                let undone = self.engine.unblock_last_candidate();
                self.maybe_start_prediction();
                match (undone, self.predicting()) {
                    (true, _) => {
                        PredictionStep::Handled(SessionOutput::Updated(self.update()))
                    }
                    (false, true) => {
                        PredictionStep::Handled(SessionOutput::Updated(self.update()))
                    }
                    // 没有可撤操作：按键交给应用（不制造空视图）
                    (false, false) => {
                        self.clear_prediction();
                        PredictionStep::Handled(SessionOutput::Noop)
                    }
                }
            }
            // Ctrl+空格：清预测/上文，切纯英文直通（消费）
            InputEvent::ToggleAscii => {
                self.close_and_clear();
                PredictionStep::Handled(SessionOutput::Updated(self.update()))
            }
        }
    }

    /// 预测提交后的输出：上屏文本 + 面板。
    ///
    /// `Commit` → `CommitAndUpdate`（IBus 提交文本后刷新面板，**不能**
    /// 调会 `hide_ui` 的 `commit`）；没有后继预测就退回普通 `Commit`。
    fn after_prediction_commit(&mut self, c: &Candidate) -> SessionOutput {
        let (text, more) = self.commit_prediction(c);
        if more {
            SessionOutput::CommitAndUpdate {
                text,
                update: self.update(),
            }
        } else {
            SessionOutput::Commit(text)
        }
    }

    /// 关闭面板并清空英文上文。
    fn close_and_clear(&mut self) {
        self.clear_prediction();
        self.english.clear();
    }

    /// 预测面板里的 Up/Down/PageUp/PageDown。
    fn move_prediction_cursor(&mut self, event: &InputEvent) {
        let n = self.prediction.len();
        if n == 0 {
            return;
        }
        match event {
            InputEvent::Up => {
                self.prediction_cursor = self.prediction_cursor.saturating_sub(1);
            }
            InputEvent::Down => {
                self.prediction_cursor = (self.prediction_cursor + 1).min(n - 1);
            }
            InputEvent::PageUp => {
                // 不足一页时**钳位**，不整页跳空
                self.prediction_cursor = self.prediction_cursor.saturating_sub(self.page_size);
            }
            InputEvent::PageDown => {
                self.prediction_cursor = (self.prediction_cursor + self.page_size).min(n - 1);
            }
            _ => {}
        }
    }
}
