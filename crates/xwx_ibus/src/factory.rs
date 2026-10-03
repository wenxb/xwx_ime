//! `org.freedesktop.IBus.Factory` 接口：daemon 按需创建引擎实例。
//!
//! 从 `engine.rs` 拆出来（AGENTS.md：单文件 ≤600 行）。方法仍以
//! [`EngineFactory`] 的固有实现暴露，接口名与路径约定不变。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use xwx_engine::ImeEngine;
use zbus::interface;
use zvariant::OwnedObjectPath;

use crate::engine::XwxEngineServer;
use crate::ibus_proto::{ENGINE_NAME, ENGINE_PATH};

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
