//! 信号解包:`zbus` 收到的是 GVariant 的 `Structure`,这里把它变成
//! 「成员名 + 字段」并给出测试要的取值(`last_commit` / `lookup_tables` …)。

use zvariant::Array;
use zvariant::OwnedValue;
use zvariant::Structure;
use zvariant::Value;

/// One captured D-Bus signal.
#[derive(Debug)]
pub struct Signal {
    pub member: String,
    pub value: OwnedValue,
}

/// Decoded `IBusLookupTable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupTable {
    /// `page_size` (field 2).
    pub page_size: u32,
    /// `cursor` **within the page** (field 3) — NOT a global candidate index.
    pub cursor_in_page: u32,
    pub cursor_visible: bool,
    pub round: bool,
    pub orientation: i32,
    /// Candidate words of this page, in display order (field 7, `av`).
    pub candidates: Vec<String>,
    /// Number of label entries (field 8); the engine leaves it empty so the
    /// panel auto-numbers 1..9.
    pub labels: usize,
}

/// Get a structure out of a value that may be a **variant wrapping** one.
///
/// IBus puts emitted objects behind a variant everywhere: the signal body's
/// field 0 is a `v`, and every element of the candidate `av` is a `v`. A bare
/// `Structure::try_from(value)` fails on those, which silently produced empty
/// text/candidate lists.
fn unwrap_variant(v: &Value<'_>) -> Option<Structure<'static>> {
    let owned: OwnedValue = match v {
        Value::Value(inner) => (**inner).try_to_owned().ok()?,
        other => other.try_to_owned().ok()?,
    };
    Structure::try_from(Value::from(owned)).ok()
}

impl Signal {
    /// The signal's **full wire body**, e.g. `(vubu)` for `UpdatePreeditText`.
    ///
    /// Field indices here are the signal's own arguments: for
    /// `UpdatePreeditText(v, u, b, u)` index 1 is `cursor_pos`, 2 `visible`,
    /// 3 `mode`. Use this for those; use [`Self::structure`] for the payload
    /// object itself.
    fn body_structure(&self) -> Option<Structure<'static>> {
        Structure::try_from(Value::from(self.value.try_clone().ok()?)).ok()
    }

    /// The signal's **payload** structure (the `IBusText` / `IBusLookupTable`).
    ///
    /// IBus wraps the emitted object in a variant: the wire body of
    /// `UpdatePreeditText` is `(vubu)` and of `UpdateLookupTable` is `(vb)`,
    /// where field 0 is a `v` holding the `IBusText` / `IBusLookupTable`
    /// structure. Callers want that *inner* object — its signature is the
    /// `(sa{sv}sv)` / `(sa{sv}uubbiavav)` the assertions pin, and its field
    /// indices are the ones `as_text` / `as_lookup_table` read.
    ///
    /// So unwrap the variant here, once, instead of at every accessor. (Reading
    /// field 2 of the *outer* body used to yield `Bool(true)` and made every
    /// text assertion come back `None` while the engine was emitting correctly.)
    fn structure(&self) -> Option<Structure<'static>> {
        let outer = self.body_structure()?;
        if let Some(Value::Value(inner)) = outer.fields().first() {
            if let Ok(v) = (**inner).try_clone() {
                if let Ok(inner_st) = Structure::try_from(v) {
                    return Some(inner_st);
                }
            }
        }
        Some(outer)
    }

    /// Top-level string fields, in wire order.
    ///
    /// For an `IBusText` this is `["IBusText", <text>]`; for an
    /// `IBusLookupTable` it is `["IBusLookupTable"]` (candidates live in the
    /// nested `av`). Calling this out explicitly matters: a naive text scan of
    /// the wire bytes also picks up the literal `"IBusText"` /
    /// `"IBusAttrList"` type names, which is exactly the kind of false
    /// observation this suite must avoid.
    pub fn strings(&self) -> Vec<String> {
        self.structure()
            .map(|st| {
                st.fields()
                    .iter()
                    .filter_map(|f| f.downcast_ref::<&str>().ok().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Structure view for signature assertions (public so tests can pin the
    /// exact wire signature, not just the decoded fields).
    pub fn structure_for_test(&self) -> Structure<'static> {
        self.structure().expect("信号负载应为结构体")
    }

    /// Read a `u32` field of the signal's **body** by wire index.
    ///
    /// Body, not payload: `cursor_pos` / `mode` are arguments of the signal
    /// itself (`UpdatePreeditText(v, u, b, u)`), not fields of the `IBusText`.
    pub fn as_u32_field(&self, idx: usize) -> Option<u32> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<u32>()
            .ok()
    }

    /// Read a `bool` field of the signal's **body** by wire index
    /// (`visible` for both `UpdatePreeditText` and `UpdateLookupTable`).
    pub fn as_bool_field(&self, idx: usize) -> Option<bool> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<bool>()
            .ok()
    }

    /// `IBusText` payload (field 2). `None` for signals with no text.
    pub fn as_text(&self) -> Option<String> {
        let st = self.structure()?;
        st.fields()
            .get(2)?
            .downcast_ref::<&str>()
            .ok()
            .map(str::to_string)
    }

    /// `IBusLookupTable` payload, or `None` for other signals.
    pub fn as_lookup_table(&self) -> Option<LookupTable> {
        let st = self.structure()?;
        let f = st.fields();
        if f.len() != 9 {
            return None;
        }
        let page_size = f[2].downcast_ref::<u32>().ok()?;
        let cursor_in_page = f[3].downcast_ref::<u32>().ok()?;
        let cursor_visible = f[4].downcast_ref::<bool>().ok()?;
        let round = f[5].downcast_ref::<bool>().ok()?;
        let orientation = f[6].downcast_ref::<i32>().ok()?;
        let arr = Array::try_from(f[7].try_clone().ok()?).ok()?;
        let candidates = arr
            .inner()
            .iter()
            // Each element is an `IBusText` wrapped in a variant (`av`), so it
            // must be unwrapped before it is a structure — this is why the
            // candidate list used to come back empty while the engine was
            // sending nine correct words.
            .filter_map(|v| {
                let text = unwrap_variant(v)?;
                text.fields()
                    .get(2)?
                    .downcast_ref::<&str>()
                    .ok()
                    .map(str::to_string)
            })
            .collect();
        let labels = Array::try_from(f[8].try_clone().ok()?)
            .ok()?
            .inner()
            .len();
        Some(LookupTable {
            page_size,
            cursor_in_page,
            cursor_visible,
            round,
            orientation,
            candidates,
            labels,
        })
    }
}

/// Convenience readers over a captured batch of signals.
/// `OwnedValue` is not `Clone`, so a signal is cloned field-by-field.
pub fn clone_signal(s: &Signal) -> Signal {
    Signal {
        member: s.member.clone(),
        value: s.value.try_clone().expect("克隆信号负载"),
    }
}

pub fn member_names(signals: &[Signal]) -> Vec<&str> {
    signals.iter().map(|s| s.member.as_str()).collect()
}

/// `last_commit` reachable from an inherent method body above the free
/// functions' definition order.
pub(crate) fn support_last_commit(signals: &[Signal]) -> Option<String> {
    last_commit(signals)
}

/// Every candidate page the engine sent, in order.
pub fn lookup_tables(signals: &[Signal]) -> Vec<LookupTable> {
    signals
        .iter()
        .filter(|s| s.member == "UpdateLookupTable")
        .filter_map(|s| s.as_lookup_table())
        .collect()
}

/// The last `UpdateLookupTable` payload in the batch.
pub fn last_table(signals: &[Signal]) -> Option<LookupTable> {
    lookup_tables(signals).pop()
}

/// The last `UpdatePreeditText` text in the batch.
pub fn last_preedit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "UpdatePreeditText")
        .and_then(|s| s.as_text())
}

/// The last `CommitText` text in the batch.
pub fn last_commit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "CommitText")
        .and_then(|s| s.as_text())
}

/// All `CommitText` texts in the batch, in order.
pub fn commits(signals: &[Signal]) -> Vec<String> {
    signals
        .iter()
        .filter(|s| s.member == "CommitText")
        .filter_map(|s| s.as_text())
        .collect()
}
