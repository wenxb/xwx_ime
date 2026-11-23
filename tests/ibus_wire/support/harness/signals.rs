//! `Harness` 信号监控与静默期采集。

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::MessageIterator;
use zbus::message::Type as MsgType;
use zbus::MatchRule;
use zvariant::{Structure, Value};

use super::*;

impl Harness {
    pub(super) fn ensure_monitor(&mut self) {
        if self.monitor.is_some() {
            return;
        }
        let conn = zbus::blocking::connection::Builder::address(self.address.as_str())
            .expect("解析私有总线地址")
            .build()
            .expect("连接总线以捕获信号");
        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .interface(ENGINE_IFACE)
            .expect("interface 名合法")
            .build();
        let iter = MessageIterator::for_match_rule(rule, &conn, Some(4096))
            .expect("注册信号 match rule 失败");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for m in iter.flatten() {
                let Some(member) = m.header().member().map(|m| m.to_string()) else {
                    continue;
                };
                // HidePreeditText / HideLookupTable 空体无法反序列化为 Structure，用占位值保留
                let value = match m.body().deserialize::<Structure<'_>>() {
                    Ok(st) => match Value::Structure(st).try_to_owned() {
                        Ok(v) => v,
                        Err(_) => continue,
                    },
                    Err(_) => match Value::U8(0u8).try_to_owned() {
                        Ok(v) => v,
                        Err(_) => continue,
                    },
                };
                if tx.send(Signal { member, value }).is_err() {
                    break;
                }
            }
        });
        thread::sleep(Duration::from_millis(150));
        self.monitor = Some(conn);
        self.rx = Some(rx);
    }

    fn drain(&mut self) -> Vec<Signal> {
        self.ensure_monitor();
        let mut out = Vec::new();
        while let Some(s) = self.try_recv() {
            out.push(s);
        }
        out
    }

    fn try_recv(&mut self) -> Option<Signal> {
        self.rx.as_ref()?.try_recv().ok()
    }

    fn recv_timeout(&mut self, d: Duration) -> Option<Signal> {
        self.rx.as_ref()?.recv_timeout(d).ok()
    }

    /// 按固定静默窗口采集信号（适用于断言「不该有信号」或排干残留）。
    pub fn settle(&mut self, gap: Duration, max: Duration) -> Vec<Signal> {
        self.ensure_monitor();
        let start = Instant::now();
        let mut out = Vec::new();
        loop {
            let elapsed = start.elapsed();
            if elapsed >= max {
                break;
            }
            match self.recv_timeout(gap.min(max - elapsed)) {
                Some(s) => out.push(s),
                None => break,
            }
        }
        out
    }

    pub fn settle_default(&mut self) -> Vec<Signal> {
        self.settle(Duration::from_millis(300), Duration::from_secs(5))
    }

    /// 先等第一条信号（上限 `max`），再按 `gap` 静默收尾，避免高负载下空批次误红。
    pub fn settle_response(&mut self, gap: Duration, max: Duration) -> Vec<Signal> {
        self.ensure_monitor();
        let deadline = Instant::now() + max;
        let mut out = Vec::new();
        match self.recv_timeout(max) {
            Some(s) => out.push(s),
            None => return out,
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.recv_timeout(gap.min(remaining)) {
                Some(s) => out.push(s),
                None => break,
            }
        }
        out
    }

    pub fn wait_signal(&mut self, member: &str, timeout: Duration) -> Result<Signal, String> {
        self.ensure_monitor();
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                seen.extend(self.drain());
                return Err(format!(
                    "等待信号 {member} 超时（{timeout:?}）；已见: {:?}",
                    member_names(&seen)
                ));
            }
            match self.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Some(s) if s.member == member => return Ok(s),
                Some(s) => seen.push(s),
                None => continue,
            }
        }
    }

    pub fn clear_backlog(&mut self) {
        let _ = self.drain();
    }
}
