//! 用户自造词条派生索引：O(1) 音节前缀判定 + 按首字母分桶的简拼/混拼召回。
//! 纯内存派生，不写入磁盘，随加载/新增/删除/衰减同步维护。

use std::collections::{HashMap, HashSet};

use super::file::UserTables;

/// 单条自造词条的轻量简拼匹配元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UserEntryItem {
    pub(super) word: String,
    pub(super) pinyin: String,
    pub(super) syl_count: u8,
    pub(super) compact_len: u8,
}

fn insert_prefixes(prefixes: &mut HashSet<String>, pinyin_key: &str) {
    let mut cur = String::with_capacity(pinyin_key.len());
    for (idx, syl) in pinyin_key.split(' ').enumerate() {
        if syl.is_empty() {
            continue;
        }
        if idx > 0 {
            cur.push(' ');
        }
        cur.push_str(syl);
        prefixes.insert(cur.clone());
    }
}

fn insert_abbrev_item(
    buckets: &mut HashMap<u8, Vec<UserEntryItem>>,
    word: &str,
    pinyin_key: &str,
) {
    let Some(&first) = pinyin_key.as_bytes().first().filter(|b| b.is_ascii_lowercase()) else {
        return;
    };
    let syl_count = pinyin_key.split(' ').filter(|s| !s.is_empty()).count();
    if !(2..=8).contains(&syl_count) {
        return;
    }
    let compact_len = pinyin_key.bytes().filter(|&b| b != b' ').count();
    if compact_len > u8::MAX as usize {
        return;
    }
    let list = buckets.entry(first).or_default();
    if list.iter().any(|it| it.word == word && it.pinyin == pinyin_key) {
        return;
    }
    list.push(UserEntryItem {
        word: word.to_string(),
        pinyin: pinyin_key.to_string(),
        syl_count: syl_count as u8,
        compact_len: compact_len as u8,
    });
}

/// 判断 `inp` 是否为 `pinyin_key` 的合法简拼或混合简拼（不含完整全拼本身）。
fn matches_abbrev_or_mixed(inp: &[u8], pinyin_key: &str) -> bool {
    fn rec(inp: &[u8], syls: &[&[u8]]) -> bool {
        if syls.is_empty() {
            return inp.is_empty();
        }
        if inp.len() < syls.len() {
            return false;
        }
        let syl = syls[0];
        let rest = &syls[1..];
        if inp.len() >= syl.len() && inp[..syl.len()] == *syl && rec(&inp[syl.len()..], rest) {
            return true;
        }
        if syl.len() > 2
            && matches!(&syl[..2], b"zh" | b"ch" | b"sh")
            && inp.len() >= 2
            && inp[..2] == syl[..2]
            && rec(&inp[2..], rest)
        {
            return true;
        }
        if syl.len() > 1 && inp[0] == syl[0] && rec(&inp[1..], rest) {
            return true;
        }
        if syl == b"er" && inp[0] == b'r' && rec(&inp[1..], rest) {
            return true;
        }
        if rest.is_empty() && inp.len() > 1 && inp.len() < syl.len() && syl.starts_with(inp) {
            return true;
        }
        false
    }
    let syls: Vec<&[u8]> = pinyin_key
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::as_bytes)
        .collect();
    rec(inp, &syls)
}

impl UserTables {
    /// 新增单条造词后的增量索引更新。
    pub(super) fn entry_on_add(&mut self, word: &str, pinyin_key: &str) {
        insert_prefixes(&mut self.entry_prefixes, pinyin_key);
        insert_abbrev_item(&mut self.entry_by_first, word, pinyin_key);
    }

    /// 全量重建自造词派生索引（加载、删词、衰减后调用）。
    pub(super) fn rebuild_entry_index(&mut self) {
        self.entry_prefixes.clear();
        self.entry_by_first.clear();
        for (key, words) in &self.entries {
            insert_prefixes(&mut self.entry_prefixes, key);
            for w in words {
                insert_abbrev_item(&mut self.entry_by_first, w, key);
            }
        }
    }

    /// 查询与 `input` 匹配的简拼/混拼用户造词条目，返回 `(word, pinyin_key)`。
    pub(super) fn lookup_abbrev_entries(&self, input: &str) -> Vec<(String, String)> {
        let bytes = input.as_bytes();
        let Some(&first) = bytes.first().filter(|b| b.is_ascii_lowercase()) else {
            return Vec::new();
        };
        let Some(bucket) = self.entry_by_first.get(&first) else {
            return Vec::new();
        };
        let n = bytes.len();
        let mut out = Vec::new();
        for item in bucket {
            if n < item.syl_count as usize || n >= item.compact_len as usize {
                continue;
            }
            if matches_abbrev_or_mixed(bytes, &item.pinyin) {
                out.push((item.word.clone(), item.pinyin.clone()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_index_supports_prefixes_and_abbrev_mixed_queries() {
        let mut t = UserTables::default();
        for (w, py) in [
            ("千问三", "qian wen san"),
            ("张三丰", "zhang san feng"),
            ("刘慈欣", "liu ci xin"),
        ] {
            t.entries.entry(py.into()).or_default().push(w.into());
            t.entry_on_add(w, py);
        }

        assert!(t.entry_prefixes.contains("qian"));
        assert!(t.entry_prefixes.contains("qian wen"));
        assert!(t.entry_prefixes.contains("qian wen san"));
        assert!(!t.entry_prefixes.contains("qian w"));

        let words = |q: &str| -> Vec<String> {
            t.lookup_abbrev_entries(q).into_iter().map(|(w, _)| w).collect()
        };
        for q in ["qws", "qianws", "qianwens", "qwensan", "qwens", "qianwensa"] {
            assert_eq!(words(q), vec!["千问三"], "query={q}");
        }
        for q in ["zsf", "zhsf", "zhangsf", "zsanf", "zhsfeng"] {
            assert_eq!(words(q), vec!["张三丰"], "query={q}");
        }
        for q in ["lcx", "liucx", "lcixin"] {
            assert_eq!(words(q), vec!["刘慈欣"], "query={q}");
        }
        // 完整全拼走精确格，不重复经简拼通道产出；少于音节数不匹配。
        assert!(words("qianwensan").is_empty());
        assert!(words("qw").is_empty());

        // 删除后重建不再命中。
        t.entries.remove("qian wen san");
        t.rebuild_entry_index();
        assert!(!t.entry_prefixes.contains("qian wen san"));
        assert!(t.lookup_abbrev_entries("qws").is_empty());
    }
}
