//! D-Bus GVariant 信号解包与常用读取器。

use zvariant::{Array, OwnedValue, Structure, Value};

#[derive(Debug)]
pub struct Signal {
    pub member: String,
    pub(super) value: OwnedValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupTable {
    pub page_size: u32,
    pub cursor: u32,
    pub cursor_visible: bool,
    pub round: bool,
    pub orientation: i32,
    pub candidates: Vec<String>,
    pub labels: usize,
}

fn unwrap_variant(v: &Value<'_>) -> Option<Structure<'static>> {
    let owned: OwnedValue = match v {
        Value::Value(inner) => (**inner).try_to_owned().ok()?,
        other => other.try_to_owned().ok()?,
    };
    Structure::try_from(Value::from(owned)).ok()
}

impl Signal {
    fn body_structure(&self) -> Option<Structure<'static>> {
        Structure::try_from(Value::from(self.value.try_clone().ok()?)).ok()
    }

    /// 解开 IBus 信号外层 variant，取出 `IBusText` / `IBusLookupTable` 结构体。
    fn structure(&self) -> Option<Structure<'static>> {
        let outer = self.body_structure()?;
        let inner = outer
            .fields()
            .first()
            .and_then(|f| match f {
                Value::Value(inner) => (**inner).try_clone().ok(),
                _ => None,
            })
            .and_then(|v| Structure::try_from(v).ok());
        Some(inner.unwrap_or(outer))
    }

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

    pub fn structure_for_test(&self) -> Structure<'static> {
        self.structure().expect("信号负载应为结构体")
    }

    pub fn as_u32_field(&self, idx: usize) -> Option<u32> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<u32>()
            .ok()
    }

    pub fn as_bool_field(&self, idx: usize) -> Option<bool> {
        self.body_structure()?
            .fields()
            .get(idx)?
            .downcast_ref::<bool>()
            .ok()
    }

    pub fn as_text(&self) -> Option<String> {
        let st = self.structure()?;
        st.fields()
            .get(2)?
            .downcast_ref::<&str>()
            .ok()
            .map(str::to_string)
    }

    pub fn as_lookup_table(&self) -> Option<LookupTable> {
        let st = self.structure()?;
        let f = st.fields();
        if f.len() != 9 {
            return None;
        }
        let page_size = f[2].downcast_ref::<u32>().ok()?;
        let cursor = f[3].downcast_ref::<u32>().ok()?;
        let cursor_visible = f[4].downcast_ref::<bool>().ok()?;
        let round = f[5].downcast_ref::<bool>().ok()?;
        let orientation = f[6].downcast_ref::<i32>().ok()?;
        let arr = Array::try_from(f[7].try_clone().ok()?).ok()?;
        let candidates = arr
            .inner()
            .iter()
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
            cursor,
            cursor_visible,
            round,
            orientation,
            candidates,
            labels,
        })
    }
}

pub fn member_names(signals: &[Signal]) -> Vec<&str> {
    signals.iter().map(|s| s.member.as_str()).collect()
}

pub fn last_table(signals: &[Signal]) -> Option<LookupTable> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "UpdateLookupTable")
        .and_then(Signal::as_lookup_table)
}

pub fn last_preedit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "UpdatePreeditText")
        .and_then(Signal::as_text)
}

pub fn last_commit(signals: &[Signal]) -> Option<String> {
    signals
        .iter()
        .rev()
        .find(|s| s.member == "CommitText")
        .and_then(Signal::as_text)
}

pub fn commits(signals: &[Signal]) -> Vec<String> {
    signals
        .iter()
        .filter(|s| s.member == "CommitText")
        .filter_map(Signal::as_text)
        .collect()
}
