//! 只增不重排候选池的首现索引；哈希碰撞仍用原字符串核对。
use std::collections::{hash_map::RandomState, HashMap};
use std::hash::BuildHasher;

use crate::Candidate;

#[derive(Default)]
pub(crate) struct CandidateIndex {
    hash: RandomState,
    first: HashMap<u64, usize>,
}

impl CandidateIndex {
    pub(crate) fn new(candidates: &[Candidate]) -> Self {
        let mut index = Self::default();
        for (i, c) in candidates.iter().enumerate() {
            index.insert(i, c);
        }
        index
    }

    pub(crate) fn find(&self, candidates: &[Candidate], c: &Candidate) -> Option<usize> {
        let &i = self.first.get(&self.hash.hash_one((&c.word, &c.pinyin)))?;
        let matches = |e: &Candidate| e.word == c.word && e.pinyin == c.pinyin;
        if matches(&candidates[i]) {
            Some(i)
        } else {
            // 碰撞时回到原来的首现查找，绝不把哈希值当成身份。
            candidates.iter().position(matches)
        }
    }

    // 候选的词/音和已有下标在索引生命周期内必须保持不变。
    pub(crate) fn insert(&mut self, i: usize, c: &Candidate) {
        self.first
            .entry(self.hash.hash_one((&c.word, &c.pinyin)))
            .or_insert(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_duplicate_and_hash_collision_keep_linear_search_semantics() {
        let a = Candidate::new("甲", "jia", 1);
        let b = Candidate::new("乙", "yi", 2);
        let candidates = vec![a.clone(), b.clone(), a.clone()];
        let mut index = CandidateIndex::new(&candidates);
        assert_eq!(index.find(&candidates, &a), Some(0));
        // 模拟 b 的哈希桶已由碰撞的 a 占据。
        index
            .first
            .insert(index.hash.hash_one((&b.word, &b.pinyin)), 0);
        assert_eq!(index.find(&candidates, &b), Some(1));
        let absent = Candidate::new("丙", "bing", 3);
        index
            .first
            .insert(index.hash.hash_one((&absent.word, &absent.pinyin)), 0);
        assert_eq!(index.find(&candidates, &absent), None);
    }
}
