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
    /// 输入会话状态机。
    session: tokio::sync::Mutex<InputSession>,
}

impl XwxEngineServer {
    /// 新建引擎服务（共享引擎本体）。
    pub fn new(engine: Arc<ImeEngine>) -> Self {
        Self {
            session: tokio::sync::Mutex::new(InputSession::new(engine)),
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
        match ibus_proto::ibus_text(text) {
            Ok(value) => {
                if let Err(e) = self.commit_text(ctxt, value).await {
                    tracing::warn!(error = %e, "CommitText 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "上屏文本构造失败"),
        }
        self.hide_ui(ctxt).await;
    }

    /// 按会话更新刷新预编辑区与候选窗（GNOME Shell candidate popup 渲染）。
    async fn refresh_ui(&self, ctxt: &zbus::SignalContext<'_>, u: &SessionUpdate) {
        // 拼音串与候选都空了：全部收起
        if u.preedit.is_empty() && u.candidates.is_empty() {
            self.hide_ui(ctxt).await;
            return;
        }

        // 预编辑区：UpdatePreeditText(text, cursor_pos, visible, mode)
        match ibus_proto::ibus_text(&u.preedit) {
            Ok(value) => {
                // preedit 光标跟随会话的编辑光标(caret),不再是串尾:
                // 用户左移光标在中间插字母时,文本光标要停在插入点。
                let cursor = u.caret as u32;
                if let Err(e) = self.update_preedit_text(ctxt, value, cursor, true, 0).await {
                    tracing::warn!(error = %e, "UpdatePreeditText 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "预编辑文本构造失败"),
        }

        // 候选窗：只发标准 UpdateLookupTable，渲染由 GNOME Shell 负责
        let page = u.page_candidates();
        if page.is_empty() {
            let _ = self.hide_lookup_table(ctxt).await;
        } else {
            let words: Vec<String> = page.iter().map(|c| c.word.clone()).collect();
            let cursor_in_page = u
                .cursor
                .saturating_sub(u.page_start())
                .min(u.page_size.saturating_sub(1));
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

    /// 收起预编辑区与候选窗（信号层面）。
    async fn hide_ui(&self, ctxt: &zbus::SignalContext<'_>) {
        if let Err(e) = self.hide_preedit_text(ctxt).await {
            tracing::warn!(error = %e, "HidePreeditText 信号发送失败");
        }
        if let Err(e) = self.hide_lookup_table(ctxt).await {
            tracing::warn!(error = %e, "HideLookupTable 信号发送失败");
        }
    }

    /// 复位会话并收起全部 UI（FocusOut / Reset / Disable 共用）。
    async fn reset_state(&self, ctxt: &zbus::SignalContext<'_>) {
        let was_active = {
            let mut session = self.session.lock().await;
            let active = session.is_active();
            // 焦点切换是造词的真边界：跨应用的上屏片段不该粘成一个词。
            session.reset_for_focus_change();
            active
        };
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

    /// 候选窗点击（由 daemon 转发 `CandidateClicked`）：`index` 是全局下标（跨页）。
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
        let word = self.session.lock().await.commit_at(index as usize);
        if let Some(word) = word {
            self.commit(&ctxt, &word).await;
        }
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
        let path = if n == 1 {
            ENGINE_PATH.to_string()
        } else {
            format!("{ENGINE_PATH}-{n}")
        };
        if n > 1 {
            let server = XwxEngineServer::new(Arc::clone(&self.engine));
            self.conn
                .object_server()
                .at(path.clone(), server)
                .await
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        }
        tracing::info!(name, path, "创建引擎实例");
        OwnedObjectPath::try_from(path)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }
}
