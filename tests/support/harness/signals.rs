//! `Harness` 的信号侧:监控线程、静默期收尾、等待某个信号。

use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use zbus::blocking::MessageIterator;
use zbus::message::Type as MsgType;
use zbus::MatchRule;
use zvariant::Structure;
use zvariant::Value;

use super::*;

impl Harness {
    // —— signals ——

    pub(super) fn ensure_monitor(&mut self) {
        if self.monitor.is_some() {
            return;
        }
        let conn = zbus::blocking::connection::Builder::address(self.address.as_str())
            .expect("解析私有总线地址")
            .build()
            .expect("连接总线以捕获信号");
        // Scoped to the engine interface so method *replies* on the same
        // connection (the clock would otherwise never go quiet) are not mixed
        // into the stream.
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
                // Deserialize the body as a **`Structure`**, not an
                // `OwnedValue`.
                //
                // `Body::deserialize::<OwnedValue>()` expects the body to be a
                // single value; every IBus signal body is a multi-field struct
                // (`UpdatePreeditText` is `(vubu)`, `UpdateLookupTable` is
                // `(vbbu)`), so that deserialization FAILS and the signal was
                // silently discarded — leaving `sigs` empty and every signal
                // assertion reporting `已见: []` even though the engine had
                // emitted the signal correctly. `Structure` matches the struct
                // signature, and `try_to_owned` gives the `'static` value the
                // `Signal` keeps.
                let body = m.body();
                // `HidePreeditText()` / `HideLookupTable()` carry an **empty**
                // body, so deserializing a `Structure` fails. Keep those signals
                // with a body-less sentinel — tests assert that the hide signal
                // was emitted, and dropping it made them report `实际: []`.
                let value = match body.deserialize::<Structure<'_>>() {
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
        // The match rule reaches the bus asynchronously; give it a moment
        // before the first action so no signal is missed.
        thread::sleep(Duration::from_millis(150));
        self.monitor = Some(conn);
        self.rx = Some(rx);
    }

    /// Non-blocking: everything queued right now.
    pub fn drain(&mut self) -> Vec<Signal> {
        self.ensure_monitor();
        let mut out = Vec::new();
        while let Some(s) = self.try_recv() {
            out.push(s);
        }
        out
    }

    fn try_recv(&mut self) -> Option<Signal> {
        let rx = self.rx.as_ref()?;
        rx.try_recv().ok()
    }

    fn recv_timeout(&mut self, d: Duration) -> Option<Signal> {
        let rx = self.rx.as_ref()?;
        match rx.recv_timeout(d) {
            Ok(s) => Some(s),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Collect signals until the stream has been quiet for `gap`, or until
    /// `max` elapses (whichever comes first). Always bounded.
    ///
    /// Quiet-gap collection is the right shape here because the engine emits
    /// `UpdatePreeditText` and `UpdateLookupTable` as *two* async signals per
    /// keystroke with no ordering guarantee we should rely on (zbus dispatches
    /// each on its own task).
    pub fn settle(&mut self, gap: Duration, max: Duration) -> Vec<Signal> {
        self.ensure_monitor();
        let start = Instant::now();
        let mut out = Vec::new();
        loop {
            let elapsed = start.elapsed();
            if elapsed >= max {
                break;
            }
            let wait = gap.min(max - elapsed);
            match self.recv_timeout(wait) {
                Some(s) => out.push(s),
                None => break,
            }
        }
        self.backlog.extend(out.iter().map(clone_signal));
        out
    }

    /// Default settle budget: 300 ms of silence, 5 s ceiling.
    pub fn settle_default(&mut self) -> Vec<Signal> {
        self.settle(Duration::from_millis(300), Duration::from_secs(5))
    }

    /// 收「**某个动作引发的这一批**信号」:先等**第一条**信号(上限 `max`),
    /// 之后再按 `gap` 静默收尾;`max` 内一条都没来才返回空。
    ///
    /// 为什么不直接用 [`Harness::settle`]:那个在「一开始就没信号」时也立刻
    /// 返回**空批次**(`recv_timeout(gap)` 一次超时就 break)。而引擎的
    /// `UpdatePreeditText`/`UpdateLookupTable` 是**异步**信号——同步的
    /// `ProcessKeyEvent` 已经返回,信号仍在路上。只要调度被挤压(整套跑、
    /// 机器有负载),`settle` 就会拿到空批次,于是「断言这批里有 X」的测试
    /// 偶发变红(实测过一次:同一套件连跑,某 target 整片失败,重跑全绿)。
    ///
    /// 断言「**不该**有信号」时仍用 [`Harness::settle`]:那种情形必须靠固定
    /// 窗口结束——「永远等不到」本身就可能是正确结果,不能去等第一条。
    pub fn settle_response(&mut self, gap: Duration, max: Duration) -> Vec<Signal> {
        self.ensure_monitor();
        let deadline = Instant::now() + max;
        let mut out = Vec::new();
        // 先等第一条:整批信号的「开头」可能比 gap 晚得多。
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
        self.backlog.extend(out.iter().map(clone_signal));
        out
    }

    /// Wait until a signal with `member` arrives (bounded).
    pub fn wait_signal(&mut self, member: &str, timeout: Duration) -> Result<Signal, String> {
        self.ensure_monitor();
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "等待信号 {member} 超时（{:?}）；已见: {:?}",
                    timeout,
                    member_names(&self.drain())
                ));
            }
            match self.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Some(s) if s.member == member => return Ok(s),
                Some(s) => self.backlog.push_back(s),
                None => continue,
            }
        }
    }

    /// Record everything observed on the bus, cleared. Returns the batch.
    pub fn take_backlog(&mut self) -> Vec<Signal> {
        self.drain();
        let out: Vec<Signal> = self.backlog.iter().map(clone_signal).collect();
        self.backlog.clear();
        out
    }

    /// Wait until the engine sends a `CommitText` whose text equals `expect`
    /// (bounded). Returns the full batch captured while waiting.
    ///
    /// Matching on the *content* rather than on "a commit happened" is the
    /// point: a commit of the wrong text is a failure, not a pass.
    pub fn wait_commit(
        &mut self,
        expect: &str,
        timeout: Duration,
    ) -> Result<Vec<Signal>, String> {
        self.ensure_monitor();
        let deadline = Instant::now() + timeout;
        let mut batch: Vec<Signal> = Vec::new();
        loop {
            if batch
                .iter()
                .any(|s| s.member == "CommitText" && s.as_text().as_deref() == Some(expect))
            {
                return Ok(batch);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let seen: Vec<String> = batch
                    .iter()
                    .filter(|s| s.member == "CommitText")
                    .map(|s| s.as_text().unwrap_or_default())
                    .collect();
                return Err(format!(
                    "等待 CommitText({expect:?}) 超时；已见的 CommitText: {seen:?}；\
                     全部信号: {:?}",
                    member_names(&batch)
                ));
            }
            match self.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Some(s) => batch.push(s),
                None => continue,
            }
        }
    }

    /// Type a full pinyin string and press space, returning the batch and the
    /// committed text (if any).
    pub fn type_and_commit(&mut self, pinyin: &str, timeout: Duration) -> (Vec<Signal>, Option<String>) {
        self.type_ascii(pinyin);
        let _ = self.settle(Duration::from_millis(80), timeout);
        self.clear_backlog();
        let consumed = self.key(keyval::SPACE, 0);
        assert!(consumed, "空格在有拼音串时必须被消费(true)");
        let batch = self.settle_default();
        let committed = support_last_commit(&batch);
        (batch, committed)
    }

    /// Everything captured so far (settled batches + wait results).
    pub fn backlog(&self) -> Vec<Signal> {
        self.backlog.iter().map(clone_signal).collect()
    }

    pub fn clear_backlog(&mut self) {
        self.backlog.clear();
        let _ = self.drain();
    }
}
