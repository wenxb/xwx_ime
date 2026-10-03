//! XwxEngineServer：`org.freedesktop.IBus.Engine` 接口的 zbus 实现。
//!
//! 职责：
//! 1. 把 IBus 的按键/焦点事件翻译成引擎的 [`InputEvent`]；
//! 2. 把引擎的 [`SessionOutput`] 翻译回标准 IBus 信号：上屏文本
//!    （`CommitText`）、预编辑区（`UpdatePreeditText`）、候选窗
//!    （`UpdateLookupTable`）。候选窗渲染完全交给 GNOME Shell / IBus
//!    自带的 candidate popup，本引擎不再自绘窗口；用户对候选窗的操作
//!    （上下移动、翻页、点击）由 daemon 折回调用本接口的
//!    `CursorUp/Down`、`PageUp/Down`、`CandidateClicked` 方法；
//! 3. 实现 Factory 接口（[`EngineFactory`]），响应 ibus-daemon 的
//!    `CreateEngine` 调用，按需创建引擎实例。
//!
//! 每个引擎实例持有一个独立的 [`InputSession`]（不同输入焦点互不干扰），
//! 引擎本体（词库/模型）由 `Arc<ImeEngine>` 共享。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use xwx_engine::{ImeEngine, InputEvent, InputSession, SessionOutput, SessionUpdate};
use zbus::interface;
use zvariant::OwnedObjectPath;

use crate::ibus_proto::{self, ENGINE_NAME, ENGINE_PATH};

/// 引擎服务：一个输入焦点（IBus 上下文）对应一个实例。
///
/// zbus 要求接口类型 `Send + Sync`：`ImeEngine` 经 `Mutex<Connection>`
/// 变成 `Sync`（见 xwx_engine 的 dict.rs），会话放在 `tokio::sync::Mutex`
/// 里保证同一实例的按键串行处理。
pub struct XwxEngineServer {
    /// **上一次发出去的预编辑文本**（P2 第 3 项）：文本与插入点都没变就不重发
/// `UpdatePreeditText`（纯光标移动/翻页时）。候选表每次都发（见 `refresh_ui`）。
    last_ui: tokio::sync::Mutex<Option<(String, u32)>>,
    /// 输入会话状态机。
    session: tokio::sync::Mutex<InputSession>,
}

impl XwxEngineServer {
    /// 新建引擎服务（共享引擎本体）。
    pub fn new(engine: Arc<ImeEngine>) -> Self {
        Self {
            session: tokio::sync::Mutex::new(InputSession::new(engine)),
            last_ui: tokio::sync::Mutex::new(None),
        }
    }

    /// 处理一个按键事件，返回是否被本输入法消费。
    async fn handle_key(&self, ctxt: &zbus::SignalContext<'_>, event: InputEvent) -> bool {
        let output = {
            let mut session = self.session.lock().await;
            match session.handle(event) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(error = %e, "会话处理按键失败，透传给应用");
                    return false;
                }
            }
        };
        match output {
            SessionOutput::Noop => false,
            SessionOutput::Commit(text) => {
                self.commit(ctxt, &text).await;
                true
            }
            SessionOutput::Updated(u) => {
                self.refresh_ui(ctxt, &u).await;
                true
            }
        }
    }

    /// 上屏：发 `CommitText` 信号 + 收起预编辑区/候选窗/面板。
    async fn commit(&self, ctxt: &zbus::SignalContext<'_>, text: &str) {
        self.commit_text_only(ctxt, text).await;
        self.hide_ui(ctxt).await;
    }

    /// 只发 `CommitText`(不收起界面)——分段上屏用:上屏一段之后,
    /// 预编辑区与候选窗要继续显示剩余输入串。
    async fn commit_text_only(&self, ctxt: &zbus::SignalContext<'_>, text: &str) {
        match ibus_proto::ibus_text(text) {
            Ok(value) => {
                if let Err(e) = self.commit_text(ctxt, value).await {
                    tracing::warn!(error = %e, "CommitText 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "上屏文本构造失败"),
        }
    }

    /// 按会话更新刷新预编辑区与候选窗（GNOME Shell candidate popup 渲染）。
    async fn refresh_ui(&self, ctxt: &zbus::SignalContext<'_>, u: &SessionUpdate) {
        // 拼音串与候选都空了：全部收起
        if u.preedit.is_empty() && u.candidates.is_empty() {
            self.hide_ui(ctxt).await;
            return;
        }

        // **区分「内容变化」与「仅光标变化」**（P2 第 3 项）：先跟上次发出去的状态比。
        let caret = u.caret as u32;
        let page = u.page_candidates();
        let hidden_table = page.is_empty();
        // `cursor` 与 `page` 同源（`page = cursor / page_size`），这里就是取模。
        let cursor_in_page = u.cursor - u.page_start();
        let send_preedit = {
            let mut last = self.last_ui.lock().await;
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
                    if let Err(e) = self.update_preedit_text(ctxt, value, caret, true, 0).await {
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
                let _ = self.hide_lookup_table(ctxt).await;
            } else {
                let words: Vec<String> = page.iter().map(|c| c.word.clone()).collect();
                match ibus_proto::ibus_lookup_table(&words, cursor_in_page, u.page_size) {
                    Ok(table) => {
                        if let Err(e) = self.update_lookup_table(ctxt, table, true).await {
                            tracing::warn!(error = %e, "UpdateLookupTable 信号发送失败");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "候选表构造失败"),
                }
            }
        }
    }

    /// 收起预编辑区与候选窗（信号层面）。
    async fn hide_ui(&self, ctxt: &zbus::SignalContext<'_>) {
        // 面板收起后「上次发出去的状态」不再有效（下次露面必须整份重发），
        // 否则焦点切回来时会因为「和上次一样」而一条信号都不发、面板空白。
        *self.last_ui.lock().await = None;
        if let Err(e) = self.hide_preedit_text(ctxt).await {
            tracing::warn!(error = %e, "HidePreeditText 信号发送失败");
        }
        if let Err(e) = self.hide_lookup_table(ctxt).await {
            tracing::warn!(error = %e, "HideLookupTable 信号发送失败");
        }
    }

    /// 复位会话并收起全部 UI（FocusOut / Reset / Disable 共用）。
    async fn reset_state(&self, ctxt: &zbus::SignalContext<'_>) {
        let (was_active, pending) = {
            let mut session = self.session.lock().await;
            let active = session.is_active();
            // 焦点切换是造词的真边界：跨应用的上屏片段不该粘成一个词。
            // 返回值是「已确认但还没交给应用」的段(方案 b)——用户明确选过
            // 的内容不能因为切窗口就丢掉,这里补一次 CommitText。
            let pending = session.reset_for_focus_change();
            (active, pending)
        };
        if !pending.is_empty() {
            self.commit_text_only(ctxt, &pending).await;
        }
        if was_active {
            self.hide_ui(ctxt).await;
        }
    }
}

/// `org.freedesktop.IBus.Engine` 接口实现。
///
/// 方法名在 D-Bus 上默认按 PascalCase 暴露（`ProcessKeyEvent` 等），
/// 与 IBus 的 introspection XML 一致。
#[interface(name = "org.freedesktop.IBus.Engine")]
impl XwxEngineServer {
    /// IBus 引擎主入口：处理一次按键事件。
    ///
    /// 返回 `true` 表示按键被本输入法消费；`false` 时 daemon 会把
    /// 按键透传给应用。
    ///
    /// - **Ctrl+空格**：中英文切换（走引擎自己的逻辑，消费该按键，
    ///   不再转发给应用）。带 Ctrl/Alt/Super/Meta 的其它组合键是应用
    ///   快捷键，照常透传；
    /// - 除空格外的释放事件透传；
    /// - `keycode` 暂不使用（后续可做按键布局映射）。
    #[zbus(name = "ProcessKeyEvent")]
    async fn process_key_event(
        &self,
        #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>,
        keyval: u32,
        keycode: u32,
        state: u32,
    ) -> bool {
        tracing::trace!(keyval, keycode, state, "收到按键事件");

        // —— Ctrl+空格：切换中英文 ——
        // 放在修饰键透传过滤**之前**：普通 Ctrl 组合键会直接被滤掉，
        // 这里要把 Ctrl+空格单拎出来拦截。只认纯 Ctrl+Space，
        // Ctrl+Shift+Space / CapsLock 下的 Ctrl+Space 仍是应用场景。
        let is_ctrl_space = (keyval == ibus_proto::keyval::SPACE
            || keyval == 0xff00) /* KP_Space */
            && state & ibus_proto::IBUS_RELEASE_MASK == 0
            && state & ibus_proto::IBUS_CONTROL_MASK != 0
            && state
                & (ibus_proto::IBUS_SHIFT_MASK
                    | ibus_proto::IBUS_LOCK_MASK
                    | ibus_proto::IBUS_MOD1_MASK
                    | ibus_proto::IBUS_SUPER_MASK
                    | ibus_proto::IBUS_HYPER_MASK
                    | ibus_proto::IBUS_META_MASK)
                == 0;
        if is_ctrl_space {
            self.handle_key(&ctxt, InputEvent::ToggleAscii).await;
            return true; // 消费掉，应用不再收到 Ctrl+空格
        }

        // —— Ctrl+Delete:删当前高亮候选 ——
        // 与 Ctrl+空格同一理由放在修饰键透传过滤**之前**:普通 Ctrl 组合
        // 键会被下面的过滤器整体透传,这里把 Ctrl+Delete 单拎出来拦截。
        // 只认纯 Ctrl+Delete(Ctrl+Shift+Delete 等仍是应用快捷键);
        // 会话层在无候选时对它返回 Noop,这里随之返回 false 透传给应用
        // (应用的「删词」快捷键不被吃掉)。
        let is_ctrl_delete = keyval == ibus_proto::keyval::DELETE
            && state & ibus_proto::IBUS_RELEASE_MASK == 0
            && state & ibus_proto::IBUS_CONTROL_MASK != 0
            && state
                & (ibus_proto::IBUS_SHIFT_MASK
                    | ibus_proto::IBUS_LOCK_MASK
                    | ibus_proto::IBUS_MOD1_MASK
                    | ibus_proto::IBUS_SUPER_MASK
                    | ibus_proto::IBUS_HYPER_MASK
                    | ibus_proto::IBUS_META_MASK)
                == 0;
        if is_ctrl_delete {
            return self.handle_key(&ctxt, InputEvent::DeleteCandidate).await;
        }

        // —— Ctrl+Shift+Delete:撤销最近一次屏蔽 ——
        // 与 Ctrl+Delete 同一位置拦截（都在修饰键透传过滤之前）；区别是**要求**
        // Shift：纯 Ctrl+Delete 是「删当前高亮候选」，加 Shift 才是「撤回上一条」。
        // 会话层在没得撤时返回 Noop，这里随之返回 false 透传给应用。
        let is_ctrl_shift_delete = keyval == ibus_proto::keyval::DELETE
            && state & ibus_proto::IBUS_RELEASE_MASK == 0
            && state & ibus_proto::IBUS_CONTROL_MASK != 0
            && state & ibus_proto::IBUS_SHIFT_MASK != 0
            && state
                & (ibus_proto::IBUS_LOCK_MASK
                    | ibus_proto::IBUS_MOD1_MASK
                    | ibus_proto::IBUS_SUPER_MASK
                    | ibus_proto::IBUS_HYPER_MASK
                    | ibus_proto::IBUS_META_MASK)
                == 0;
        if is_ctrl_shift_delete {
            return self.handle_key(&ctxt, InputEvent::UnblockCandidate).await;
        }

        // —— 其余：释放事件与 Ctrl/Alt/Super/Hyper/Meta 组合键透传给应用 ——
        if state
            & (ibus_proto::IBUS_RELEASE_MASK
                | ibus_proto::IBUS_CONTROL_MASK
                | ibus_proto::IBUS_MOD1_MASK
                | ibus_proto::IBUS_SUPER_MASK
                | ibus_proto::IBUS_HYPER_MASK
                | ibus_proto::IBUS_META_MASK)
            != 0
        {
            return false;
        }
        // —— 英文模式:按键直接透传给应用 ——
        // 不进翻译层的关键原因:翻译层会把字母按 Shift/CapsLock 归一
        // (Shift+G → Verbatim('G'),CapsLock+Shift+G → Char('g')),
        // 而英文模式下应用需要看到**原始按键**——Shift+G 上屏大写、
        // CapsLock+Shift+g 上屏小写。透传(返回 false)让 daemon 把
        // 原始 keyval+state 原样交给应用,一个字都不动。
        let in_english = self.session.lock().await.english_mode();
        if in_english {
            return false;
        }

        let Some(event) = ibus_proto::translate_keyval(keyval, state) else {
            return false;
        };
        self.handle_key(&ctxt, event).await
    }

    /// 焦点进入应用。TODO: 记录焦点上下文（用户词频学习等）。
    #[zbus(name = "FocusIn")]
    async fn focus_in(&self) {}

    /// 焦点离开应用：清空未上屏的输入。
    #[zbus(name = "FocusOut")]
    async fn focus_out(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.reset_state(&ctxt).await;
    }

    /// IBus 显式复位请求（切换引擎/清空输入时由 daemon 调用）。
    #[zbus(name = "Reset")]
    async fn reset(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.reset_state(&ctxt).await;
    }

    /// 引擎被启用。TODO: 加载用户词频、模糊音配置。
    #[zbus(name = "Enable")]
    async fn enable(&self) {}

    /// 引擎被禁用：清空未上屏的输入。
    #[zbus(name = "Disable")]
    async fn disable(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.reset_state(&ctxt).await;
    }

    /// 候选窗翻页（daemon 转发面板按键）。
    #[zbus(name = "PageUp")]
    async fn page_up(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.handle_key(&ctxt, InputEvent::PageUp).await;
    }

    #[zbus(name = "PageDown")]
    async fn page_down(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.handle_key(&ctxt, InputEvent::PageDown).await;
    }

    /// 候选高亮上移（daemon 转发面板按键）。
    #[zbus(name = "CursorUp")]
    async fn cursor_up(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.handle_key(&ctxt, InputEvent::Up).await;
    }

    #[zbus(name = "CursorDown")]
    async fn cursor_down(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.handle_key(&ctxt, InputEvent::Down).await;
    }

    /// 候选窗点击（由 daemon 转发 `CandidateClicked`）：`index` 是**页内**下标。
    ///
    /// 我们发出去的 `UpdateLookupTable` 只含当前页、`cursor` 也是页内下标
    /// （见 `refresh_ui`），面板因此按页内坐标系回传；会话负责映射回全局
    /// 。旧注释写的「全局下标（跨页）」正是当初那个缺口。
    ///
    /// TODO: `button`/`state` 目前忽略，后续支持右键翻页、Shift+点击等。
    #[zbus(name = "CandidateClicked")]
    async fn candidate_clicked(
        &self,
        #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>,
        index: u32,
        button: u32,
        state: u32,
    ) {
        let _ = (button, state);
        // 走会话事件的统一出口(而不是直接 `commit_immediately`):点击与空格/数字
        // 共用同一条分段上屏路径,剩余串继续组合的界面刷新也一样。
        self.handle_key(&ctxt, InputEvent::CandidateClicked(index as usize))
            .await;
    }

    // —— 以下为信号声明（无函数体，zbus 会生成发射器）——

    /// 上屏文本：`CommitText(v)`。
    #[zbus(signal)]
    async fn commit_text(&self, ctxt: &zbus::SignalContext<'_>, text: zvariant::OwnedValue)
        -> zbus::Result<()>;

    /// 刷新预编辑区：`UpdatePreeditText(v, u, b, u)`。
    #[zbus(signal)]
    async fn update_preedit_text(
        &self,
        ctxt: &zbus::SignalContext<'_>,
        text: zvariant::OwnedValue,
        cursor_pos: u32,
        visible: bool,
        mode: u32,
    ) -> zbus::Result<()>;

    /// 收起预编辑区：`HidePreeditText()`。
    #[zbus(signal)]
    async fn hide_preedit_text(&self, ctxt: &zbus::SignalContext<'_>) -> zbus::Result<()>;

    /// 刷新候选窗：`UpdateLookupTable(v, b)`。
    #[zbus(signal)]
    async fn update_lookup_table(
        &self,
        ctxt: &zbus::SignalContext<'_>,
        table: zvariant::OwnedValue,
        visible: bool,
    ) -> zbus::Result<()>;

    /// 收起候选窗：`HideLookupTable()`。
    #[zbus(signal)]
    async fn hide_lookup_table(&self, ctxt: &zbus::SignalContext<'_>) -> zbus::Result<()>;
}

/// Factory 服务：daemon 调用 `CreateEngine` 时按需创建引擎实例。
///
/// 路径约定：首个实例使用规范路径 [`ENGINE_PATH`]（main.rs 启动时已
/// 预导出，这里直接复用），后续实例带序号后缀，避免与已导出对象冲突。
pub struct EngineFactory {
    /// 共享的引擎本体。
    engine: Arc<ImeEngine>,
    /// 用于导出新引擎对象的连接。
    conn: zbus::Connection,
    /// 已创建的引擎实例计数（首个复用启动时预导出的实例）。
    seq: AtomicU32,
}

impl EngineFactory {
    /// 新建工厂。
    pub fn new(engine: Arc<ImeEngine>, conn: zbus::Connection) -> Self {
        Self {
            engine,
            seq: AtomicU32::new(0),
            conn,
        }
    }
}

/// `org.freedesktop.IBus.Factory` 接口实现。
#[interface(name = "org.freedesktop.IBus.Factory")]
impl EngineFactory {
    /// 创建（或复用）一个引擎实例，返回其对象路径：`CreateEngine(s) -> (o)`。
    #[zbus(name = "CreateEngine")]
    async fn create_engine(&self, name: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        if name != ENGINE_NAME {
            return Err(zbus::fdo::Error::InvalidArgs(format!("未知引擎: {name}")));
        }
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // **先构造并校验路径,再注册**:注册之后才发现路径非法,对象表里会
        // 留下一个永远不应答的路径。非法源头只有一个字符——D-Bus 对象路径
        // 的分段只允许 `[A-Za-z0-9_]`,旧实现的 `{ENGINE_PATH}-{n}`(连字符)
        // 会被 zvariant 拒绝,于是**第二次 CreateEngine 必定失败**
        // 。
        let path = OwnedObjectPath::try_from(engine_object_path(n))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        if n > 1 {
            let server = XwxEngineServer::new(Arc::clone(&self.engine));
            self.conn
                .object_server()
                .at(path.clone(), server)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        tracing::info!(name, path = %path, "创建引擎实例");
        Ok(path)
    }
}

/// 第 `n` 个引擎实例的对象路径(`n = 1` 即预导出的那个)。
///
/// 单独成函数是为了能被单测钉住:序号部分必须是合法路径分段
/// (`[A-Za-z0-9_]`),不能用连字符。
pub(crate) fn engine_object_path(n: u32) -> String {
    if n <= 1 {
        ENGINE_PATH.to_string()
    } else {
        format!("{ENGINE_PATH}_{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第 2 个及之后的实例路径必须能被 D-Bus 接受:
    /// 旧实现用连字符(`.../xwx-2`),zvariant 直接拒绝,于是第二次
    /// `CreateEngine` 必定失败。
    #[test]
    fn later_engine_instances_use_valid_object_paths() {
        assert_eq!(engine_object_path(1), ENGINE_PATH);
        assert_eq!(engine_object_path(2), format!("{ENGINE_PATH}_2"));
        for n in 2..=4 {
            let p = engine_object_path(n);
            assert!(!p.contains('-'), "对象路径分段不许有连字符: {p}");
            assert!(
                OwnedObjectPath::try_from(p.as_str()).is_ok(),
                "第 {n} 个实例的路径必须合法: {p}"
            );
        }
        // 旧公式的失败本身也要钉住:这条断言如果过了,说明 zvariant 放宽了
        // 校验(或者有人把修复改回去了)。
        assert!(
            OwnedObjectPath::try_from(format!("{ENGINE_PATH}-2").as_str()).is_err(),
            "旧公式 `xwx-2` 应被 D-Bus 拒绝"
        );
    }
}
