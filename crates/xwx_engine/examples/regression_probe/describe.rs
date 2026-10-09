//! 按键与输出的文本化：只负责把 `InputEvent`/`SessionOutput` 变成
//! 逐字节稳定的单行描述（对拍用的就是这些行）。
//!
//! 从 `regression_probe.rs` 拆出：那边已经接近 600 行上限。

use xwx_engine::{InputEvent, SessionOutput};

pub(super) fn describe(key: &InputEvent) -> String {
    match key {
        InputEvent::Char(c) => format!("key Char({c})"),
        InputEvent::Verbatim(c) => format!("key Verbatim({c})"),
        InputEvent::Select(n) => format!("key Select({n})"),
        InputEvent::CandidateClicked(i) => format!("key CandidateClicked({i})"),
        InputEvent::Backspace => "key Backspace".to_owned(),
        InputEvent::Enter => "key Enter".to_owned(),
        InputEvent::Space => "key Space".to_owned(),
        InputEvent::Escape => "key Escape".to_owned(),
        InputEvent::Up => "key Up".to_owned(),
        InputEvent::Down => "key Down".to_owned(),
        InputEvent::Left => "key Left".to_owned(),
        InputEvent::Right => "key Right".to_owned(),
        InputEvent::PageUp => "key PageUp".to_owned(),
        InputEvent::PageDown => "key PageDown".to_owned(),
        InputEvent::DeleteCandidate => "key DeleteCandidate".to_owned(),
        InputEvent::UnblockCandidate => "key UnblockCandidate".to_owned(),
        InputEvent::ToggleAscii => "key ToggleAscii".to_owned(),
        InputEvent::Tab => "key Tab".to_owned(),
    }
}

pub(super) fn describe_out(out: &SessionOutput) -> String {
    match out {
        SessionOutput::Commit(text) => format!("Commit({text:?})"),
        SessionOutput::Updated(u) => {
            let top3: Vec<&str> = u.candidates.iter().take(3).map(|c| c.word.as_str()).collect();
            format!(
                "Updated {{ preedit={:?} cursor={} page={}/{} n={} top3={top3:?} }}",
                u.preedit,
                u.cursor,
                u.page,
                u.page_size,
                u.candidates.len()
            )
        }
        SessionOutput::CommitAndUpdate { text, update } => format!(
            "CommitAndUpdate({text:?}, n={})",
            update.candidates.len()
        ),
        SessionOutput::RefreshAndForward(u) => format!(
            "RefreshAndForward {{ preedit={:?} n={} }}",
            u.preedit,
            u.candidates.len()
        ),
        SessionOutput::Noop => "Noop".to_owned(),
    }
}

// ---------------------------------------------------------------- 身份记录
