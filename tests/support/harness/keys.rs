//! `Harness` 的输入侧:按键、焦点、翻页、点选。

use super::*;

impl Harness {
    // —— the `ProcessKeyEvent` contract: bool out, so it is asserted, not ignored ——

    /// Send one key press and return the engine's "did I consume it" bool.
    ///
    /// `state` is the IBus modifier mask; `keycode` is passed as 0 (the engine
    /// documents that it does not use it, `engine.rs:164`).
    pub fn key(&self, keyval: u32, state: u32) -> bool {
        match self.key_checked(keyval, state) {
            Ok(v) => v,
            Err(e) => panic!("{e}"),
        }
    }

    pub fn key_checked(&self, keyval: u32, state: u32) -> Result<bool, String> {
        let reply = self.call("ProcessKeyEvent", (keyval, 0u32, state))?;
        let v: bool = reply
            .body()
            .deserialize()
            .map_err(|e| format!("ProcessKeyEvent 返回值不是 bool: {e}"))?;
        Ok(v)
    }

    /// Type an ASCII string as individual key presses (no modifiers).
    pub fn type_ascii(&self, s: &str) -> Vec<bool> {
        s.chars()
            .map(|c| self.key(keyval::ascii(c), 0))
            .collect()
    }

    /// Ctrl+Space (keyval 0x20, `IBUS_CONTROL_MASK` = 1<<2, no other
    /// modifiers). Must return `true` — the application must not see it.
    pub fn toggle_ascii(&self) -> bool {
        self.key(keyval::SPACE, M_CONTROL)
    }

    // —— panel / lifecycle methods (void returns) ——

    pub fn focus_in(&self) -> Result<(), String> {
        self.call("FocusIn", ()).map(|_| ())
    }
    pub fn focus_out(&self) -> Result<(), String> {
        self.call("FocusOut", ()).map(|_| ())
    }
    pub fn reset_session(&self) -> Result<(), String> {
        self.call("Reset", ()).map(|_| ())
    }
    pub fn enable(&self) -> Result<(), String> {
        self.call("Enable", ()).map(|_| ())
    }
    pub fn disable(&self) -> Result<(), String> {
        self.call("Disable", ()).map(|_| ())
    }
    pub fn page_up(&self) -> Result<(), String> {
        self.call("PageUp", ()).map(|_| ())
    }
    pub fn page_down(&self) -> Result<(), String> {
        self.call("PageDown", ()).map(|_| ())
    }
    pub fn cursor_up(&self) -> Result<(), String> {
        self.call("CursorUp", ()).map(|_| ())
    }
    pub fn cursor_down(&self) -> Result<(), String> {
        self.call("CursorDown", ()).map(|_| ())
    }

    /// `CandidateClicked(index, button, state)` — `index` is the **global**
    /// candidate index (across pages), per `engine.rs:294`.
    pub fn candidate_clicked(&self, index: u32) -> Result<(), String> {
        self.call("CandidateClicked", (index, 1u32, 0u32)).map(|_| ())
    }
}
