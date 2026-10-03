//! 屏蔽 → 撤销：引擎层闭环（Ctrl+Delete / Ctrl+Shift+Delete 的引擎侧）。
//!
//! 会话与 IBus 两层只是把按键映射到这两次调用（`session/input.rs`、
//! `crates/xwx_ibus/src/engine.rs`），闭环在这里钉住；会话层那次
//! `refresh()` 让候选回来的行为另有用例覆盖。

use super::*;

#[test]
fn delete_then_unblock_last_round_trips() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
    if !path.is_file() {
        return; // 依赖真实词库；缺数据由 tests/required_data.rs 报红
    }
    // 必须有用户层：屏蔽与撤销都发生在用户层，没有它两个动作都是空操作。
    let engine = ImeEngine::new(Some(&path))
        .expect("词库加载")
        .with_user_lm(crate::UserLM::in_memory());

    engine.delete_candidate("北京", "bei jing");
    assert!(
        engine
            .user_lm()
            .expect("已挂用户层")
            .snapshot()
            .is_blocked("bei jing", "北京"),
        "Ctrl+Delete 之后该 (拼音, 词) 应被屏蔽"
    );

    assert!(engine.unblock_last_candidate(), "应能撤到最近一条");
    assert!(
        !engine
            .user_lm()
            .expect("已挂用户层")
            .snapshot()
            .is_blocked("bei jing", "北京"),
        "撤销之后不该再被拦"
    );
    assert!(
        !engine.unblock_last_candidate(),
        "槽位已空，再撤返回 false（按键侧随之透传给应用）"
    );
}
