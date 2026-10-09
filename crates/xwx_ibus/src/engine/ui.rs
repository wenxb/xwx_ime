//! UI 信号辅助：上屏、预编辑区/候选窗刷新与收起。
//!
//! 从 `engine.rs` 拆出：这些函数只发信号、不碰会话状态，与按键翻译分开。

use xwx_engine::SessionUpdate;

use crate::{ibus_proto, XwxEngineServer};

    /// 上屏：发 `CommitText` 信号 + 收起预编辑区/候选窗/面板。
    pub(super) async fn commit(server: &XwxEngineServer, ctxt: &zbus::SignalContext<'_>, text: &str) {
        commit_text_only(server, ctxt, text).await;
        hide_ui(server, ctxt).await;
    }

    /// 只发 `CommitText`(不收起界面)——分段上屏用:上屏一段之后,
    /// 预编辑区与候选窗要继续显示剩余输入串。
    pub(super) async fn commit_text_only(server: &XwxEngineServer, ctxt: &zbus::SignalContext<'_>, text: &str) {
        match ibus_proto::ibus_text(text) {
            Ok(value) => {
                if let Err(e) = server.commit_text(ctxt, value).await {
                    tracing::warn!(error = %e, "CommitText 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "上屏文本构造失败"),
        }
    }

    /// 按会话更新刷新预编辑区与候选窗（GNOME Shell candidate popup 渲染）。
    pub(super) async fn refresh_ui(server: &XwxEngineServer, ctxt: &zbus::SignalContext<'_>, u: &SessionUpdate) {
        // 拼音串与候选都空了：全部收起
        if u.preedit.is_empty() && u.candidates.is_empty() {
            hide_ui(server, ctxt).await;
            return;
        }

        // **区分「内容变化」与「仅光标变化」**（P2 第 3 项）：先跟上次发出去的状态比。
        let caret = u.caret as u32;
        let page = u.page_candidates();
        let hidden_table = page.is_empty();
        // `cursor` 与 `page` 同源（`page = cursor / page_size`），这里就是取模。
        let cursor_in_page = u.cursor - u.page_start();
        let send_preedit = {
            let mut last = server.last_ui.lock().await;
            let same = last
                .as_ref()
                .is_some_and(|(p, c)| p == &u.preedit && *c == caret);
            *last = Some((u.preedit.clone(), caret));
            !same
        };

        // 预编辑区：UpdatePreeditText(text, cursor_pos, visible, mode)
        // 文本与插入点都没变就跳过（纯光标移动/翻页时不重发）。
        if send_preedit {
            match ibus_proto::ibus_text(&u.preedit) {
                Ok(value) => {
                    // preedit 光标跟随会话的编辑光标(caret),不再是串尾:
                    // 用户左移光标在中间插字母时,文本光标要停在插入点。
                    // `visible` 必须取「preedit 非空」：预测面板的 preedit
                    // 恒为空，不能给它显示一个空的活动组合。
                    let visible = !u.preedit.is_empty();
                    if let Err(e) = server
                        .update_preedit_text(ctxt, value, caret, visible, 0)
                        .await
                    {
                        tracing::warn!(error = %e, "UpdatePreeditText 信号发送失败");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "预编辑文本构造失败"),
            }
        }

        // 候选窗：只发标准 UpdateLookupTable，渲染由 GNOME Shell 负责。
        //
        // **只发当前页 + 页内 cursor**。这里刻意不走
        // 「发整表 + 绝对 cursor」那条路：实测整表构造+序列化
        // 9 条 = 146µs / 500 条 = 7.5ms（`ibus_lookup_table` 后转
        // `OwnedValue`，每键一次），单这一项就吃掉 12ms 热路径预算的一半以上。
        // 页内方案下两种面板口径给出**同一个点击下标**：
        //   面板按 `page = cursor/page_size` 算 → 页内 cursor < page_size ⇒ page=0
        //   ⇒ 点击回传 `0*page_size + 按钮序号` = 按钮序号（页内下标）；
        // 若面板把 cursor 当页内下标（也是 0..page_size-1），结论相同。
        // 于是「面板显示的那条」与「会话收到的那条」永远同源——
        // 会话把页内下标映射回全局（`page_start + index`，与数字选词同一
        // 条映射），而页码由唯一权威的全局 cursor 推导（见 `InputSession::page`）。
        // 旧实现的缺陷不在页内方案本身，而在**映射缺失**：会话把页内下标
        // 当成全局下标，第 2 页点第 1 条会上屏第 1 页第 1 条。
        // **候选表每次都发**：既有保证「翻页、空操作也要刷新面板」（tests/paging.rs
        // 钉着它）——面板状态与引擎快照不是一回事，省这一次信号换来的风险不值。
        // P2 第 3 项真正省下的是上面那条 preedit：文本与插入点没变就不重发。
        {
            if hidden_table {
                let _ = server.hide_lookup_table(ctxt).await;
            } else {
                let words: Vec<String> = page.iter().map(|c| c.word.clone()).collect();
                match ibus_proto::ibus_lookup_table(&words, cursor_in_page, u.page_size) {
                    Ok(table) => {
                        if let Err(e) = server.update_lookup_table(ctxt, table, true).await {
                            tracing::warn!(error = %e, "UpdateLookupTable 信号发送失败");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "候选表构造失败"),
                }
            }
        }
    }

    /// 收起预编辑区与候选窗（信号层面）。
    pub(super) async fn hide_ui(server: &XwxEngineServer, ctxt: &zbus::SignalContext<'_>) {
        // 面板收起后「上次发出去的状态」不再有效（下次露面必须整份重发），
        // 否则焦点切回来时会因为「和上次一样」而一条信号都不发、面板空白。
        *server.last_ui.lock().await = None;
        if let Err(e) = server.hide_preedit_text(ctxt).await {
            tracing::warn!(error = %e, "HidePreeditText 信号发送失败");
        }
        if let Err(e) = server.hide_lookup_table(ctxt).await {
            tracing::warn!(error = %e, "HideLookupTable 信号发送失败");
        }
    }
