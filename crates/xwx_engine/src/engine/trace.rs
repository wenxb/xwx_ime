//! 召回诊断（阶段 0）：把「首次丢失在哪里」变成可观测，而不是靠猜。
//!
//! 只在 `XWX_TRACE=1` 时记录；关闭时每个记录点只是一次 `OnceLock` 读，
//! **不增加词典查询、不改变任何候选与顺序**（阶段 0 的验收要求）。
//! 用法见 `examples/recall_trace.rs`。
//!
//! 记录的是**判据与停止原因**，不是分数明细：先回答「哪一道门槛/哪一次
//! 提前停止把它挡掉了」，再决定要不要更细的探针。

use std::cell::RefCell;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("XWX_TRACE").is_some())
}

thread_local! {
    static NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// 开始一次查询：清掉上一条输入留下的记录。
pub(crate) fn begin() {
    if enabled() {
        NOTES.with(|n| n.borrow_mut().clear());
    }
}

/// 记一条诊断。参数是闭包，关闭时连格式化都不做。
pub(crate) fn note(msg: impl FnOnce() -> String) {
    if enabled() {
        NOTES.with(|n| n.borrow_mut().push(msg()));
    }
}

/// 取出本次查询的全部诊断（关闭时为空）。
pub fn take() -> Vec<String> {
    if enabled() {
        NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
    } else {
        Vec::new()
    }
}
