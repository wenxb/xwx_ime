//! 一批候选共享的上文侧；不改变窗口、命中顺序或浮点运算顺序。
use super::*;

pub(crate) struct PreparedContext {
    chars: usize,
    nodes: [(usize, usize); 5],
    len: usize,
}

impl WanxiangLM {
    /// 核心查询：`ctx_tail`（≤5 字）为上文、`word` 为候选词，返回 log10 分。
    ///
    /// 对齐 octagram.cc `Query()` 的滑动窗：k 自 `ctx_tail` 字数递减到 1，
    /// 逐级 `traverse(上文末 k 字)` + `common_prefix_search(词首 ≤5 字)`，
    /// 取所有命中里折算后的最高分；兜底为非词配分。完整词配
    /// （`collocation_len >= min_length` 或「整段上文 + 整个词首」同时
    /// 精确命中）用 collocation_penalty，否则 weak_collocation_penalty。
    pub(super) fn score_word(&self, ctx_tail: &str, word: &str) -> f32 {
        let n = window_chars(&self.config);
        let mut best = self.floor_log10();
        if ctx_tail.is_empty() {
            // octagram: 空上下文直接回退非词配分。
            return best;
        }

        // —— 词首 ≤n 字的编码：固定缓冲，零堆分配（CJK 每字 2 字节）——
        let mut wd_buf = [0u8; 24];
        let (wd_len, wd_chars) = encode_first_chars(word, n, &mut wd_buf);
        let wd_enc = &wd_buf[..wd_len];

        // —— 上文末 ≤n 字编码 + 每字字节边界（编码逐码点变长，不能按
        //    字节直接切后缀，先记字符边界）——
        let mut ctx_buf = [0u8; 24];
        let (ctx_len, ctx_chars) = encode_first_chars(ctx_tail, n, &mut ctx_buf);
        let ctx_enc = &ctx_buf[..ctx_len];
        // pos[j] = 第 j 个字符的字节起点（j = 0..=ctx_chars）
        const MAX_WINDOW: usize = 8; // 调用方已保证 ≤5，给足余量
        let mut pos = [0usize; MAX_WINDOW + 1];
        for (i, ch) in ctx_tail.chars().take(n).enumerate() {
            let mut tmp = [0u8; 4];
            let len = encode_cp_into(ch as u32, &mut tmp);
            pos[i + 1] = pos[i] + len;
        }

        let mut hits = [(0i32, 0usize); 8]; // kMaxResults
        for k in (1..=ctx_chars).rev() {
            // 上文末 k 字（按字符边界切字节）
            let suffix = &ctx_enc[pos[ctx_chars - k]..pos[ctx_chars]];
            let (ok, node, matched) = self.array.traverse(self.data(), suffix, 0);
            if !ok || matched != suffix.len() {
                continue;
            }
            let nh = self
                .array
                .common_prefix_search(self.data(), wd_enc, node, &mut hits);
            for &(v, match_bytes) in &hits[..nh] {
                let match_chars = unicode_length(wd_enc, match_bytes);
                let colloc_len = k + match_chars;
                // octagram: `collocation_len >= min`，或「整段上文 + 整个
                // 词」都精确命中 → 词配惩罚；半截窗口 → 弱词配。
                let whole = colloc_len >= self.config.collocation_min_length
                    || (k == ctx_chars && match_chars == wd_chars);
                let penalty = if whole {
                    self.config.collocation_penalty
                } else {
                    self.config.weak_collocation_penalty
                };
                let s = self.log10_prob_raw(v, penalty);
                if s > best {
                    best = s;
                }
            }
        }
        best
    }

    pub(crate) fn prepare_context(&self, tail: &str) -> Option<PreparedContext> {
        let n = window_chars(&self.config);
        // 非默认的大窗口仍走原评分路径，不把公开配置写死为 5。
        if n > 5 {
            return None;
        }
        let mut buf = [0u8; 24];
        let (len, chars) = encode_first_chars(tail, n, &mut buf);
        let mut pos = [0usize; 6];
        for (i, ch) in tail.chars().take(n).enumerate() {
            let mut tmp = [0u8; 4];
            pos[i + 1] = pos[i] + encode_cp_into(ch as u32, &mut tmp);
        }
        let mut context = PreparedContext {
            chars,
            nodes: [(0, 0); 5],
            len: 0,
        };
        for k in (1..=chars).rev() {
            let suffix = &buf[pos[chars - k]..len];
            let (ok, node, matched) = self.array.traverse(self.data(), suffix, 0);
            if ok && matched == suffix.len() {
                context.nodes[context.len] = (k, node);
                context.len += 1;
            }
        }
        Some(context)
    }

    pub(crate) fn score_prepared(&self, ctx: &PreparedContext, word: &str) -> f32 {
        let mut best = self.floor_log10();
        if ctx.chars == 0 {
            return best;
        }
        let mut buf = [0u8; 24];
        let (len, chars) = encode_first_chars(word, window_chars(&self.config), &mut buf);
        let encoded = &buf[..len];
        let mut hits = [(0i32, 0usize); 8];
        for &(k, node) in &ctx.nodes[..ctx.len] {
            let nh = self
                .array
                .common_prefix_search(self.data(), encoded, node, &mut hits);
            for &(v, match_bytes) in &hits[..nh] {
                let match_chars = unicode_length(encoded, match_bytes);
                let whole = k + match_chars >= self.config.collocation_min_length
                    || (k == ctx.chars && match_chars == chars);
                let penalty = if whole {
                    self.config.collocation_penalty
                } else {
                    self.config.weak_collocation_penalty
                };
                let score = self.log10_prob_raw(v, penalty);
                if score > best {
                    best = score;
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_scores_match_original_bits_for_windows_and_unicode() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/wanxiang-lts-zh-hans.gram");
        if !path.is_file() {
            return;
        }
        let mut lm = WanxiangLM::open(&path).unwrap();
        for max in 0..=6 {
            lm.config.collocation_max_length = max;
            for tail in ["", "我", "我们今天", "hello", "𠀀你🙂", "天地玄黄宇宙洪荒"]
            {
                let ctx = lm.prepare_context(tail).unwrap();
                for word in [
                    "",
                    "你好",
                    "北京大学",
                    "满额",
                    "Python",
                    "🙂𠀀",
                    "中华人民共和国",
                ] {
                    assert_eq!(
                        lm.score_prepared(&ctx, word).to_bits(),
                        lm.score_word(tail, word).to_bits(),
                        "{max} {tail} {word}"
                    );
                }
            }
        }
        lm.config.collocation_max_length = 7;
        assert!(lm.prepare_context("我").is_none());
    }
}
