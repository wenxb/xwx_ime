//! 隔离私有 D-Bus 集成测试夹具与信号采集工具。

mod fixture;
mod harness;
mod signal;

pub use fixture::{english_fixture, shared_fixture, Fixture};
pub use harness::Harness;
pub use signal::{
    commits, last_commit, last_preedit, last_table, member_names, LookupTable, Signal,
};

use std::time::Duration;

pub const ENGINE_SERVICE: &str = "org.freedesktop.IBus.Engine.xwx";
pub const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/xwx";
pub const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";

pub const M_SHIFT: u32 = 1 << 0;
pub const M_LOCK: u32 = 1 << 1;
pub const M_CONTROL: u32 = 1 << 2;
pub const M_RELEASE: u32 = 1 << 30;

pub mod keyval {
    pub const SPACE: u32 = 0x20;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const BACKSPACE: u32 = 0xff08;
    pub const HOME: u32 = 0xff50;
    pub const DELETE: u32 = 0xffff;

    pub fn ascii(c: char) -> u32 {
        c as u32
    }
}

pub const NAME_TIMEOUT: Duration = Duration::from_secs(30);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

fn msg<T: std::fmt::Display>(e: T) -> String {
    e.to_string()
}
