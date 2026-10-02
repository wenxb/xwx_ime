//! 共享的「场景」helper:`ni` 夹具顺序与「敲 ni 取第一页」,被翻页、点选、
//! 屏蔽、焦点几组集成测试共用(它们原在一个文件里,拆开后不再各抄一份)。

use super::*;

/// Fixture order for `ni` (frequency desc, no language model).
pub const NI_ORDER: &[&str] = &[
    "你", "泥", "尼", "逆", "匿", "腻", "拟", "妮", "倪", "霓", "溺", "昵",
];

/// Type `ni` and return the first page's candidates.
pub fn compose_ni(h: &mut Harness) -> Vec<String> {
    h.focus_out().expect("FocusOut 复位");
    h.clear_backlog();
    h.type_ascii("ni");
    let sigs = h.settle_default();
    last_table(&sigs)
        .expect("应弹出候选窗")
        .candidates
}
