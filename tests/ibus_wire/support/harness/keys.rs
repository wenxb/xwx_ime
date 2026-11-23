//! `Harness` 输入方法：按键、焦点、翻页与点选。

use super::*;

impl Harness {
    pub fn key(&self, keyval: u32, state: u32) -> bool {
        let reply = self
            .call("ProcessKeyEvent", (keyval, 0u32, state))
            .unwrap_or_else(|e| panic!("{e}"));
        reply
            .body()
            .deserialize::<bool>()
            .unwrap_or_else(|e| panic!("ProcessKeyEvent 返回值不是 bool: {e}"))
    }

    pub fn type_ascii(&self, s: &str) -> Vec<bool> {
        s.chars().map(|c| self.key(keyval::ascii(c), 0)).collect()
    }

    pub fn toggle_ascii(&self) -> bool {
        self.key(keyval::SPACE, M_CONTROL)
    }

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

    pub fn candidate_clicked(&self, index: u32) -> Result<(), String> {
        self.call("CandidateClicked", (index, 1u32, 0u32))
            .map(|_| ())
    }
}
