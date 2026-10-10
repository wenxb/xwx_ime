//! # xwx_ibus：IBus 协议适配层
//!
//! 把 [`xwx_engine`] 的输入会话接到 `org.freedesktop.IBus.Engine` 上：
//!
//! 1. [`ibus_proto`]：IBus 线上格式（GVariant 序列化）、按键翻译、
//!    component XML / RegisterComponent 的字段来源；
//! 2. [`engine`]：`org.freedesktop.IBus.Engine` 与
//!    `org.freedesktop.IBus.Factory` 两个接口的 zbus 实现。
//!
//! 候选窗渲染完全交给 GNOME Shell / IBus 自带的 candidate popup，
//! 本 crate 只负责发标准信号（`UpdateLookupTable` /
//! `UpdatePreeditText` / `CommitText` 等）。
//!
//! 进程入口在 `xwx` bin crate 里。

pub mod engine;
pub mod factory;
pub mod ibus_proto;
pub mod stall;

pub use engine::XwxEngineServer;
pub use factory::EngineFactory;
