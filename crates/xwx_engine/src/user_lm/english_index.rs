//! 英文用户派生索引：前缀 Top-32 与下一词 Top-32。
//!
//! **为什么不复用中文通道**：中文候选池里没有英文词，只给静态 Top-32 内的
//! 英文词加分等于永远不召回长尾——用户刚学过的词必须能自己走进召回池。
//!
//! 两条索引都只存**规范小写 key**（1～48 个 ASCII 字母），不存指向另一代
//! 快照的引用，也**不写入用户文件**（纯派生，加载后按计数重建）。
//! 排序口径：计数降序、并列按 key 升序。
//!
//! 增量只在「计数增加」时做（其他词没变，不需要全表重建）；删除、全局衰减
//! 与加载完成一律全量重建，避免删除后缺位或衰减后并列顺序错误。

use std::collections::HashMap;

use super::file::UserTables;
use crate::english::is_english_key;

/// 每个桶最多保留的 key 数。
pub(super) const ENGLISH_INDEX_TOP: usize = 32;

/// 把一个词并入某个桶，再按 (计数降序, key 升序) 重排并截断。
///
/// `counts` 是**同代**的计数字典（前缀桶用一元计数，下一词桶用该 prev 的
/// 二元计数）——增量更新读的是当前表，不是旧快照。
fn bump(
    index: &mut HashMap<String, Vec<String>>,
    bucket: &str,
    word: &str,
    counts: &HashMap<String, u64>,
) {
    let list = index.entry(bucket.to_string()).or_default();
    if !list.iter().any(|w| w == word) {
        list.push(word.to_string());
    }
    let score = |w: &String| counts.get(w).copied().unwrap_or(0);
    list.sort_by(|a, b| score(b).cmp(&score(a)).then_with(|| a.cmp(b)));
    list.truncate(ENGLISH_INDEX_TOP);
}

/// 按 (计数降序, key 升序) 排好的候选序列（只保留合法英文键）。
fn ranked<'a>(counts: impl Iterator<Item = (&'a String, u64)>) -> Vec<&'a String> {
    let mut v: Vec<(&String, u64)> = counts.filter(|(w, _)| is_english_key(w)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().map(|(w, _)| w).collect()
}

impl UserTables {
    /// 计数**增加**后的增量更新（只改这个词所在的桶）。
    pub(super) fn english_on_learn(&mut self, prev: Option<&str>, word: &str) {
        if !is_english_key(word) {
            return;
        }
        let UserTables {
            unigrams,
            bigrams,
            english_prefix_top,
            english_next_top,
            ..
        } = self;
        for end in 1..=word.len() {
            bump(english_prefix_top, &word[..end], word, unigrams);
        }
        let Some(prev) = prev.filter(|p| is_english_key(p)) else {
            return;
        };
        let Some(inner) = bigrams.get(prev) else {
            return;
        };
        bump(english_next_top, prev, word, inner);
    }

    /// 全量重建（删除、全局衰减、加载完成）。
    pub(super) fn rebuild_english_index(&mut self) {
        let UserTables {
            unigrams,
            bigrams,
            english_prefix_top,
            english_next_top,
            ..
        } = self;
        english_prefix_top.clear();
        english_next_top.clear();

        // 前缀桶：按计数降序遍历词，向尚未满的桶追加——与
        // `learn` 的增量结果同口径（计数降序、并列 key 升序）。
        for w in ranked(unigrams.iter().map(|(w, c)| (w, *c))) {
            for end in 1..=w.len() {
                let list = english_prefix_top.entry(w[..end].to_string()).or_default();
                if list.len() < ENGLISH_INDEX_TOP {
                    list.push(w.clone());
                }
            }
        }

        for (prev, inner) in bigrams.iter() {
            if !is_english_key(prev) {
                continue;
            }
            let top: Vec<String> = ranked(inner.iter().map(|(w, c)| (w, *c)))
                .into_iter()
                .take(ENGLISH_INDEX_TOP)
                .cloned()
                .collect();
            if !top.is_empty() {
                english_next_top.insert(prev.clone(), top);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> UserTables {
        UserTables::default()
    }

    /// 独立 oracle：直接按计数全表重算一遍桶。
    fn oracle_prefix(t: &UserTables, prefix: &str) -> Vec<String> {
        ranked(t.unigrams.iter().map(|(w, c)| (w, *c)))
            .into_iter()
            .filter(|w| w.starts_with(prefix))
            .take(ENGLISH_INDEX_TOP)
            .cloned()
            .collect()
    }

    fn oracle_next(t: &UserTables, prev: &str) -> Vec<String> {
        match t.bigrams.get(prev) {
            Some(inner) => ranked(inner.iter().map(|(w, c)| (w, *c)))
                .into_iter()
                .take(ENGLISH_INDEX_TOP)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    #[test]
    fn incremental_matches_oracle_after_learns() {
        let mut t = table();
        for word in ["hello", "help", "world", "word", "he"] {
            t.unigrams.insert(word.to_string(), 1);
            t.english_on_learn(None, word);
        }
        for word in ["hello", "help", "world", "hello"] {
            *t.unigrams.entry(word.to_string()).or_insert(0) += 1;
            t.english_on_learn(None, word);
        }
        for p in ["h", "he", "hel", "hell", "hello", "w", "wor"] {
            assert_eq!(
                t.english_prefix_top.get(p).cloned().unwrap_or_default(),
                oracle_prefix(&t, p),
                "前缀 {p}"
            );
        }
    }

    #[test]
    fn next_word_index_matches_oracle() {
        let mut t = table();
        for (prev, next, n) in [("thank", "you", 3), ("thank", "them", 1), ("hello", "world", 2)] {
            let inner = t.bigrams.entry(prev.to_string()).or_default();
            *inner.entry(next.to_string()).or_insert(0) += n;
            t.unigrams.entry(next.to_string()).or_insert(0);
            t.english_on_learn(Some(prev), next);
        }
        for p in ["thank", "hello", "missing"] {
            assert_eq!(
                t.english_next_top.get(p).cloned().unwrap_or_default(),
                oracle_next(&t, p),
                "prev {p}"
            );
        }
        assert_eq!(t.english_next_top.get("thank").unwrap()[0], "you");
    }

    #[test]
    fn incremental_equals_rebuild_at_every_step() {
        let words = ["a", "an", "and", "android", "annex", "you", "your"];
        let mut inc = table();
        for (i, w) in words.iter().enumerate() {
            *inc.unigrams.entry(w.to_string()).or_insert(0) += (i as u64 % 3) + 1;
            inc.english_on_learn(None, w);
            let mut full = inc.clone();
            full.rebuild_english_index();
            assert_eq!(inc.english_prefix_top, full.english_prefix_top, "step {i} ({w})");
        }
    }

    #[test]
    fn rebuild_drops_entries_after_delete() {
        let mut t = table();
        t.unigrams.insert("python".into(), 5);
        t.unigrams.insert("pythons".into(), 1);
        t.rebuild_english_index();
        assert!(t.english_prefix_top.contains_key("python"));
        t.unigrams.remove("python");
        t.rebuild_english_index();
        let list = t.english_prefix_top.get("py").cloned().unwrap_or_default();
        assert!(!list.contains(&"python".to_string()), "删除后不得凭旧索引复活");
        assert_eq!(list, oracle_prefix(&t, "py"));
    }

    #[test]
    fn non_english_keys_never_enter_the_index() {
        let mut t = table();
        for w in ["你好", "Python", "py thon", "abc123", "", &"a".repeat(49)] {
            t.unigrams.insert(w.to_string(), 9);
            t.rebuild_english_index();
            assert!(
                !t.english_prefix_top.values().any(|l| l.iter().any(|k| k == w)),
                "{w:?} 不该进英文索引"
            );
        }
        assert!(t.english_prefix_top.is_empty());
    }

    #[test]
    fn bucket_is_capped_at_32_and_keeps_the_strongest() {
        let mut t = table();
        for i in 0..40 {
            // 键规则只允许 ASCII 字母，不能用数字后缀。
            let w = format!("p{}{}", (b'a' + (i / 26) as u8) as char, (b'a' + (i % 26) as u8) as char);
            t.unigrams.insert(w.clone(), i as u64);
            t.english_on_learn(None, &w);
        }
        let list = t.english_prefix_top.get("p").expect("应建桶").clone();
        assert_eq!(list.len(), ENGLISH_INDEX_TOP);
        // 最强的是计数 39 的 pbn，最弱的是计数 8 的 pai（前 8 个被挤出）
        assert_eq!(list[0], "pbn");
        assert!(!list.contains(&"paa".to_string()));
        assert_eq!(list, oracle_prefix(&t, "p"));
    }
}
