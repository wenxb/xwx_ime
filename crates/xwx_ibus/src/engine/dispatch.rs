//! Engine 与 Properties 都在 zbus 接收循环中完成，不另起并发任务。
use std::collections::HashMap;

use zbus::{
    names::{InterfaceName, MemberName},
    object_server::{DispatchResult, Interface, SignalContext},
    Connection, Message, ObjectServer,
};
use zvariant::{OwnedValue, Value};

use super::XwxEngineServer;

impl XwxEngineServer {
    /// 导出引擎与有序属性接口；必须在占用服务名或返回对象路径之前完成。
    pub async fn register(self, conn: &Connection, path: &str) -> zbus::Result<()> {
        let server = conn.object_server();
        server.at(path, Ordered(self)).await?;
        // zbus 自带 Properties 默认并发，不能让密码框状态越过之前的按键。
        server.remove::<zbus::fdo::Properties, _>(path).await?;
        server.at(path, Ordered(zbus::fdo::Properties)).await?;
        Ok(())
    }
}

// 保留 zbus 对属性查找、错误和 introspection 的处理，只关闭并发分发。
struct Ordered<T>(T);

#[zbus::export::async_trait::async_trait]
impl<T: Interface> Interface for Ordered<T> {
    fn name() -> InterfaceName<'static> {
        T::name()
    }

    fn spawn_tasks_for_methods(&self) -> bool {
        false
    }

    async fn get(&self, name: &str) -> Option<zbus::fdo::Result<OwnedValue>> {
        self.0.get(name).await
    }

    async fn get_all(&self) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        self.0.get_all().await
    }

    fn set<'call>(
        &'call self,
        name: &'call str,
        value: &'call Value<'_>,
        ctxt: &'call SignalContext<'_>,
    ) -> DispatchResult<'call> {
        self.0.set(name, value, ctxt)
    }

    async fn set_mut(
        &mut self,
        name: &str,
        value: &Value<'_>,
        ctxt: &SignalContext<'_>,
    ) -> Option<zbus::fdo::Result<()>> {
        self.0.set_mut(name, value, ctxt).await
    }

    fn call<'call>(
        &'call self,
        server: &'call ObjectServer,
        conn: &'call Connection,
        message: &'call Message,
        name: MemberName<'call>,
    ) -> DispatchResult<'call> {
        self.0.call(server, conn, message, name)
    }

    fn call_mut<'call>(
        &'call mut self,
        server: &'call ObjectServer,
        conn: &'call Connection,
        message: &'call Message,
        name: MemberName<'call>,
    ) -> DispatchResult<'call> {
        self.0.call_mut(server, conn, message, name)
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        self.0.introspect_to_writer(writer, level);
    }
}
