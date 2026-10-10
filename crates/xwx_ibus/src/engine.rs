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

use std::sync::Arc;

use xwx_engine::{ImeEngine, InputEvent, InputSession, SessionOutput};

use zbus::interface;

use crate::ibus_proto;

mod ui;

/// Factory 接口已拆到 [`crate::factory`]，这里转出以保持原有导入路径。
pub use crate::factory::EngineFactory;

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
    /// **上一次上报给面板的中/英模式**：模式没变就不重发 `UpdateProperty`。
    last_mode: tokio::sync::Mutex<Option<bool>>,
}

impl XwxEngineServer {
    /// 新建引擎服务（共享引擎本体）。
    pub fn new(engine: Arc<ImeEngine>) -> Self {
        Self {
            session: tokio::sync::Mutex::new(InputSession::new(engine)),
            last_ui: tokio::sync::Mutex::new(None),
            last_mode: tokio::sync::Mutex::new(None),
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
                ui::commit(self, ctxt, &text).await;
                true
            }
            SessionOutput::Updated(u) => {
                ui::refresh_ui(self, ctxt, &u).await;
                true
            }
            // 上屏**然后**刷新预测面板：必须用 `commit_text_only`，
            // 不能调会 hide_ui 的 `commit`——否则刚算好的下一词面板
            // 会被自己藏掉（预测分册第 9 节）。
            SessionOutput::CommitAndUpdate { text, update } => {
                ui::commit_text_only(self, ctxt, &text).await;
                ui::refresh_ui(self, ctxt, &update).await;
                true
            }
            // 只刷新/隐藏界面，**不提交任何字符**：返回 false，按键照常
            // 交给应用（绝不能 CommitText 一个空格再返回 false）。
            SessionOutput::RefreshAndForward(u) => {
                ui::refresh_ui(self, ctxt, &u).await;
                false
            }
        }
    }

    /// 中/英属性在面板上的三件套：`(symbol, label, state)`。
    ///
    /// `symbol` 是面板上显示的那个字（GNOME 顶栏与输入源并排显示），`label` 进菜单。
    fn mode_property_parts(english: bool) -> (&'static str, &'static str, u32) {
        if english {
            ("英", "English", ibus_proto::IBUS_PROP_STATE_CHECKED)
        } else {
            ("中", "中文", ibus_proto::IBUS_PROP_STATE_UNCHECKED)
        }
    }

    /// 发 `UpdateProperty`（单个属性）。
    async fn send_update_property(&self, ctxt: &zbus::SignalContext<'_>, english: bool) {
        let (symbol, label, state) = Self::mode_property_parts(english);
        match ibus_proto::ibus_property(ibus_proto::PROP_KEY_MODE, symbol, label, state) {
            Ok(value) => {
                if let Err(e) = self.update_property(ctxt, value).await {
                    tracing::warn!(error = %e, "UpdateProperty 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "中英属性构造失败"),
        }
    }

    /// 把当前中/英模式同步给面板：**只在模式真的变了**时发信号。
    ///
    /// 面板据此显示 中/英。此前没有任何反馈：用户按了 Ctrl+空格 之后
    /// 只能靠「打出来是字母还是候选」猜自己在哪个模式。
    async fn sync_mode_property(&self, ctxt: &zbus::SignalContext<'_>) {
        let english = self.session.lock().await.english_mode();
        let changed = {
            let mut last = self.last_mode.lock().await;
            // `None` = 面板还没收到过 `RegisterProperties`：只记下来，不发信号，
            // 否则空闲的 FocusOut 也会往面板推属性（面板还没这一项）。
            let registered = last.is_some();
            let changed = *last != Some(english);
            *last = Some(english);
            registered && changed
        };
        if changed {
            self.send_update_property(ctxt, english).await;
        }
    }

    /// 首次注册属性表（`RegisterProperties`）：面板收到后菜单里才有这一项。
    async fn register_mode_property(&self, ctxt: &zbus::SignalContext<'_>) {
        let english = self.session.lock().await.english_mode();
        let _ = self.last_mode.lock().await.replace(english);
        let (symbol, label, state) = Self::mode_property_parts(english);
        match ibus_proto::ibus_prop_list(ibus_proto::PROP_KEY_MODE, symbol, label, state) {
            Ok(value) => {
                if let Err(e) = self.register_properties(ctxt, value).await {
                    tracing::warn!(error = %e, "RegisterProperties 信号发送失败");
                }
            }
            Err(e) => tracing::warn!(error = %e, "中英属性表构造失败"),
        }
    }

    /// 英文上下文失效（IBus 层钩子）：只清英文上文与延迟空格状态——
    /// **不碰**中文组合（会话分册第 2 节）。用于那些在进入会话之前就
    /// `return false` 的事件。
    ///
    /// M2 起同时收掉预测面板，并**返回**「刚才是否有面板」：有就让调用方
    /// 发一次隐藏信号，没有就是纯 no-op（预测分册第 10.3 节）。释放事件
    /// 由调用方先区分开，不会走到这里。
    async fn invalidate_english_context(&self) -> bool {
        let mut session = self.session.lock().await;
        let had_prediction = session.predicting();
        session.clear_english_context();
        had_prediction
    }

    async fn reset_state(&self, ctxt: &zbus::SignalContext<'_>) {
        let (was_active, pending) = {
            let mut session = self.session.lock().await;
            // 预测态 pinyin 为空，旧的 `is_active()` 会返回 false——
            // 那会让刚弹出的预测面板在焦点切换后僵在屏幕上。
            let active = session.has_visible_ui();
            // 焦点切换是造词的真边界：跨应用的上屏片段不该粘成一个词。
            // 返回值是「已确认但还没交给应用」的段(方案 b)——用户明确选过
            // 的内容不能因为切窗口就丢掉,这里补一次 CommitText。
            let pending = session.reset_for_focus_change();
            (active, pending)
        };
        if !pending.is_empty() {
            ui::commit_text_only(self, ctxt, &pending).await;
        }
        if was_active {
            ui::hide_ui(self, ctxt).await;
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
            // 模式变了就把新状态推给面板（顶栏的 中/英 靠这条信号）。
            self.sync_mode_property(&ctxt).await;
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
        //
        // 两者对**英文上下文**的处理相反（会话分册第 2 节最后两行）：
        // - 按键**释放**不清：连续打英文时每个字母都会来一次 release，
        //   清一下就等于每个字母都毁掉上文（下一词召回与延迟空格全失效）；
        // - 应用**快捷键**（按下）要清：它可能在进入会话之前就透传了，
        //   而「按了应用快捷键」是明确的操作边界。
        let is_release = state & ibus_proto::IBUS_RELEASE_MASK != 0;
        let is_modifier_combo = state
            & (ibus_proto::IBUS_CONTROL_MASK
                | ibus_proto::IBUS_MOD1_MASK
                | ibus_proto::IBUS_SUPER_MASK
                | ibus_proto::IBUS_HYPER_MASK
                | ibus_proto::IBUS_META_MASK)
            != 0;
        if is_release || is_modifier_combo {
            if !is_release && self.invalidate_english_context().await {
                ui::hide_ui(self, &ctxt).await;
            }
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
            // 直通应用的原始按键同样是英文链的边界（应用自己决定它是什么）。
            if self.invalidate_english_context().await {
                ui::hide_ui(self, &ctxt).await;
            }
            return false;
        }

        let Some(event) = ibus_proto::translate_keyval(keyval, state) else {
            // 翻译层不认识的按键会原样透传给应用——同样是边界
            // （Shift+Tab 走的就是这条路：它必须原样交给应用，
            // 绝不能折成「接受预测」）。
            if self.invalidate_english_context().await {
                ui::hide_ui(self, &ctxt).await;
            }
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
        self.sync_mode_property(&ctxt).await;
    }

    /// IBus 显式复位请求（切换引擎/清空输入时由 daemon 调用）。
    #[zbus(name = "Reset")]
    async fn reset(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.reset_state(&ctxt).await;
        self.sync_mode_property(&ctxt).await;
    }

    /// 引擎被启用：把中/英属性注册给面板。
    #[zbus(name = "Enable")]
    async fn enable(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.register_mode_property(&ctxt).await;
    }

    /// 面板点击属性（中/英切换）：`name` 是属性 key。
    ///
    /// 面板把本属性当**普通菜单项**（`IBUS_PROP_TYPE_NORMAL`），传来的 `state`
    /// 恒为 0，所以这里按当前模式**取反**——读 `state` 的话点一下只能切到
    /// 「英文关」，再也回不到英文。只认自己的属性；模式确实要变时才走
    /// Ctrl+空格 那条路（会话里可能还有未上屏的拼音，交给它按原有语义处理），
    /// 随后同步回面板。
    #[zbus(name = "PropertyActivate")]
    async fn property_activate(
        &self,
        #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>,
        name: String,
        state: u32,
    ) {
        if name != ibus_proto::PROP_KEY_MODE {
            return;
        }
        let _ = state; // NORMAL 属性恒为 0，只作占位（见本方法的文档）
        let want_english = !self.session.lock().await.english_mode();
        if self.session.lock().await.english_mode() == want_english {
            return;
        }
        self.handle_key(&ctxt, InputEvent::ToggleAscii).await;
        self.sync_mode_property(&ctxt).await;
    }

    /// 属性显示/隐藏：本输入法只有一个常显属性，无需处理；
    /// 实现出来是为了 daemon 调用时不报 UnknownMethod。
    #[zbus(name = "PropertyShow")]
    async fn property_show(&self, name: String) {
        let _ = name;
    }

    #[zbus(name = "PropertyHide")]
    async fn property_hide(&self, name: String) {
        let _ = name;
    }

    /// 引擎被禁用：清空未上屏的输入。
    #[zbus(name = "Disable")]
    async fn disable(&self, #[zbus(signal_context)] ctxt: zbus::SignalContext<'_>) {
        self.reset_state(&ctxt).await;
        self.sync_mode_property(&ctxt).await;
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

    /// 注册属性表：`RegisterProperties(v)`。
    #[zbus(signal)]
    async fn register_properties(
        &self,
        ctxt: &zbus::SignalContext<'_>,
        props: zvariant::OwnedValue,
    ) -> zbus::Result<()>;

    /// 更新单个属性：`UpdateProperty(v)`。
    #[zbus(signal)]
    async fn update_property(
        &self,
        ctxt: &zbus::SignalContext<'_>,
        prop: zvariant::OwnedValue,
    ) -> zbus::Result<()>;

    /// 收起候选窗：`HideLookupTable()`。
    #[zbus(signal)]
    async fn hide_lookup_table(&self, ctxt: &zbus::SignalContext<'_>) -> zbus::Result<()>;
}
