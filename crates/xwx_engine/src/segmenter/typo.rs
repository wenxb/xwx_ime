//! 容错解码用的「音节级 ≤1 次编辑」索引：输入窗口 → 可达的规范音节 + 代价。

use std::collections::HashMap;
use std::sync::LazyLock;

use super::SYLLABLES;

pub(crate) struct TypoSyllables {
    /// 窗口 → (规范音节, 代价, 是否替换类读法)。
    map: HashMap<String, Vec<(&'static str, f32, bool)>>,
}

const TYPO_P_SUBST_DEFAULT: f64 = 0.01;
const TYPO_P_DIALECT: f64 = 0.05;
const TYPO_P_INSERT_DEFAULT: f64 = 0.005;
const TYPO_P_DELETE_DEFAULT: f64 = 0.004;
const TYPO_P_TRANSPOSE: f64 = 0.03;
const TYPO_P_ADJACENT: f64 = 0.05;

const QWERTY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

pub(crate) fn is_qwerty_neighbor(a: char, b: char) -> bool {
    for row in QWERTY_ROWS {
        if let (Some(i), Some(j)) = (row.find(a), row.find(b)) {
            return i.abs_diff(j) == 1;
        }
    }
    false
}

pub(crate) fn qwerty_row_neighbors(c: char) -> (Option<char>, Option<char>) {
    for row in QWERTY_ROWS {
        if let Some(i) = row.find(c) {
            let left = i.checked_sub(1).and_then(|j| row.chars().nth(j));
            let right = row.chars().nth(i + 1);
            return (left, right);
        }
    }
    (None, None)
}

pub(crate) fn penalty(p: f64) -> f32 {
    -((p.log10()) as f32)
}

pub(crate) fn confusion_pair_penalty(a: char, b: char) -> Option<f32> {
    for pair in crate::corrector::COMMON_CONFUSION_PAIRS {
        let (mut x, mut y) = (pair.a.chars(), pair.b.chars());
        if let (Some(x), Some(y), None, None) = (x.next(), y.next(), x.next(), y.next()) {
            if (x, y) == (a, b) || (x, y) == (b, a) {
                return Some(penalty(pair.weight.max(0.001) as f64).max(0.1));
            }
        }
    }
    None
}

fn substitution_penalty(a: char, b: char) -> f32 {
    if let Some(p) = confusion_pair_penalty(a, b) {
        return p;
    }
    if is_qwerty_neighbor(a, b) {
        return penalty(TYPO_P_ADJACENT);
    }
    penalty(TYPO_P_SUBST_DEFAULT)
}

fn insertion_penalty(inserted: char, prev: Option<char>) -> f32 {
    let dialect = matches!((prev, inserted), (Some('z' | 'c' | 's'), 'h') | (Some('n'), 'g'));
    if dialect {
        penalty(TYPO_P_DIALECT)
    } else {
        penalty(TYPO_P_INSERT_DEFAULT)
    }
}

pub(crate) fn gap_cost(len: usize) -> f32 {
    penalty(TYPO_P_DELETE_DEFAULT) * len as f32
}

pub(crate) fn deletion_penalty(deleted: char, prev: Option<char>) -> f32 {
    let dialect = matches!((prev, deleted), (Some('z' | 'c' | 's'), 'h') | (Some('n'), 'g'));
    if dialect {
        penalty(TYPO_P_DIALECT)
    } else {
        penalty(TYPO_P_DELETE_DEFAULT)
    }
}

pub(crate) fn typo_penalty_adjacent() -> f32 {
    penalty(TYPO_P_ADJACENT)
}
pub(crate) fn typo_penalty_transpose() -> f32 {
    penalty(TYPO_P_TRANSPOSE)
}
pub(crate) fn typo_penalty_default_sub() -> f32 {
    penalty(TYPO_P_SUBST_DEFAULT)
}

static TYPO_INSTANCE: LazyLock<TypoSyllables> = LazyLock::new(TypoSyllables::build);

pub(crate) fn prepare_typo_table() {
    let _ = &*TYPO_INSTANCE;
}

impl TypoSyllables {
    fn build() -> Self {
        let mut map: HashMap<String, Vec<(&'static str, f32, bool)>> = HashMap::new();
        let mut push = |window: &[u8], syl: &'static str, cost: f32, is_subst: bool| {
            let s = std::str::from_utf8(window).expect("音节表仅含 ASCII");
            if let Some(slot) = map.get_mut(s) {
                match slot.iter_mut().find(|(existing, _, _)| *existing == syl) {
                    Some((_, c, sub)) => {
                        if cost < *c {
                            *c = cost;
                            *sub = is_subst;
                        }
                    }
                    None => slot.push((syl, cost, is_subst)),
                }
            } else {
                map.insert(s.to_owned(), vec![(syl, cost, is_subst)]);
            }
        };
        let mut buf = [0u8; 8];
        for &syl in SYLLABLES {
            let bytes = syl.as_bytes();
            let len = bytes.len();
            // 替换
            for (i, &orig) in bytes.iter().enumerate() {
                buf[..len].copy_from_slice(bytes);
                for b in b'a'..=b'z' {
                    if b == orig {
                        continue;
                    }
                    buf[i] = b;
                    push(
                        &buf[..len],
                        syl,
                        substitution_penalty(orig as char, b as char),
                        true,
                    );
                }
            }
            // 插入（用户漏打了一个字母）
            for i in 0..=len {
                buf[..i].copy_from_slice(&bytes[..i]);
                buf[i + 1..=len].copy_from_slice(&bytes[i..]);
                let prev = i.checked_sub(1).map(|j| bytes[j] as char);
                for b in b'a'..=b'z' {
                    buf[i] = b;
                    push(&buf[..=len], syl, insertion_penalty(b as char, prev), false);
                }
            }
            // 删除（用户多打了一个字母）
            if len > 0 {
                for i in 0..len {
                    buf[..i].copy_from_slice(&bytes[..i]);
                    buf[i..len - 1].copy_from_slice(&bytes[i + 1..]);
                    let prev = i.checked_sub(1).map(|j| bytes[j] as char);
                    push(
                        &buf[..len - 1],
                        syl,
                        deletion_penalty(bytes[i] as char, prev),
                        false,
                    );
                }
            }
            // 调换相邻两字母
            for i in 0..len.saturating_sub(1) {
                buf[..len].copy_from_slice(bytes);
                buf.swap(i, i + 1);
                push(&buf[..len], syl, penalty(TYPO_P_TRANSPOSE), false);
            }
        }
        for v in map.values_mut() {
            v.shrink_to_fit();
        }
        map.shrink_to_fit();
        Self { map }
    }

    pub(crate) fn lookup(window: &str) -> &'static [(&'static str, f32, bool)] {
        TYPO_INSTANCE
            .map
            .get(window)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}
