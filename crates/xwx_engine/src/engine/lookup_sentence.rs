//! 整句候选的追加与置顶。
//!
//! 调用点负责可达性与长度门槛；此处取 n-best、补回缺失的字面整句，再排序置顶。

use super::*;

impl ImeEngine {
    /// 在 `sentence_lattice` 上取整串读法，追加进 `ranked`，并按整串块口径置顶。
    ///
    /// 入参由调用点决定：
    /// - `sentence_lattice`：含纠错的格优先，否则精确格；
    /// - `ranked`：当前候选表（就地追加/排序/截断/置顶）。
    pub(super) fn append_and_promote_sentence(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        sentence_lattice: &[Vec<LatticeEdge>],
        has_doubled: bool,
        ranked: &mut Vec<Candidate>,
    ) -> Result<(), EngineError> {
        // 已有可靠精确整串词条（非填充档、无叠字母错拼）时，跳过多词组合整句，
        // 避免 `gongzuo` 塞进 `供作/共作/共做`、`zhege` 塞进 `着个` 挤掉首屏常用单字。
        let has_reliable_exact_whole = !has_doubled
            && sentence_lattice[0].iter().any(|e| {
                e.end == pinyin.len()
                    && e.freq > FILLER_FREQ_TIER
                    && !e.abbrev
                    && !e.gap
                    && e.edit_count == 0
                    && e.fuzzy_penalty == 0.0
            });
        let mut sentence_keys: Vec<(String, String, f32)> = Vec::new();
        let sentences = if has_reliable_exact_whole {
            Vec::new()
        } else if sentence_lattice[0].iter().any(|e| e.end == pinyin.len())
            || !pinyin.as_bytes().last().is_some_and(|b| b"aeiouvngr".contains(b))
        {
            self.best_chain_candidates(
                pinyin, ctx, sentence_lattice, SpellingKind::Exact, WORD_BOUNDARY_PENALTY,
            )?
        } else {
            self.sentence_candidates(pinyin, ctx, sentence_lattice)?
        };
        for c in sentences {
            if let Some(existing) = ranked
                .iter_mut()
                .find(|e| e.word == c.word && e.pinyin == c.pinyin && e.covered >= c.covered)
            {
                // 词库填充档（freq=100）或偏低词条分不该压死组合整句分（如 `nihaoshijie`→你好世界）。
                if c.score > existing.score {
                    existing.score = c.score;
                    if existing.freq <= FILLER_FREQ_TIER {
                        existing.freq = 0;
                    }
                    sentence_keys.push((existing.word.clone(), existing.pinyin.clone(), existing.score));
                }
            } else {
                sentence_keys.push((c.word.clone(), c.pinyin.clone(), c.score));
                ranked.push(c);
            }
        }
        super::rank::sort_candidates_with_lm(pinyin, ranked, false, &self.lm);
        ranked.truncate(MAX_CANDIDATES);
        // **整句进流内首位，但只在没有「可靠整串词条」时**：覆盖整串的词条
        // 若全是源数据最低档（`FILLER_FREQ_TIER`）就说明这一串在词库里没有
        // 真实依据，整句（条件概率与词频证据都更强）该占首位。判据取数据
        // 自身的最低档、等价于上游 `has_reliable_phrase`，不是拍出来的阈值；
        // **不动分数**，所以不会出现「抬多词读法压坏 womenjintianquxian」。
        // **只在首位是「垃圾档词条」时才换**：`nihaoshijie` 的首位本来
        // 就是组合候选（「你好世界」，`freq = 0` 表示分数驱动、不是词库
        // 条目），那种情况一个字都不该动——早先按「有没有 freq 高于最低档
        // 的覆盖者」判断，会把整句块自己产出的「你好时节」顶上去（实测
        // daily 的 MRR 因此从 1.000 掉到 0.958）。
        //
        // `freq == 0` 是「分数驱动」的标记（组合候选与用户造词都用它），
        // 只有 `1..=FILLER_FREQ_TIER` 才是词库里没有真实依据的填充条目。
        super::trace::note(|| {
            let t: Vec<String> = ranked
                .iter()
                .take(5)
                .map(|c| format!("{}|freq={}|cov={}|{:?}", c.word, c.freq, c.covered, c.spelling))
                .collect();
            let fire = ranked
                .first()
                .is_some_and(|c| c.covered >= pinyin.len() && c.freq > 0 && c.freq <= FILLER_FREQ_TIER);
            format!("置顶判定: 开火={} 前五=[{}]", fire, t.join("  "))
        });
        let top_is_filler_entry = ranked.first().is_some_and(|c| {
            c.covered >= pinyin.len() && c.freq > 0 && c.freq <= FILLER_FREQ_TIER
        });
        let top_syls = ranked
            .first()
            .map_or(0, |top| top.pinyin.split(' ').count());
        let top_starts_with_filler = ranked.first().is_some_and(|top| {
            top.covered >= pinyin.len()
                && top.freq == 0
                && sentence_lattice[0].iter().any(|e| {
                    e.end < pinyin.len()
                        && e.freq > 0
                        && e.freq <= FILLER_FREQ_TIER
                        && e.word.chars().count() >= 2
                        && top.word.starts_with(&e.word)
                })
        });
        let target = if top_is_filler_entry {
            // 按 **(词, 音, 分)** 三元组认「本段刚产出的那条」：同文本同拼音
            // 的词条（如最低档的「你好时节」）与整句拼出的组合会撞 key，
            // 只比 (word, pinyin) 会把词条本身顶上去（实测 daily 第 1 条
            // `nihaoshijie` 因此从「你好世界」变成「你好时节」）。
            ranked.iter().position(|c| {
                sentence_keys.iter().any(|(w, p, s)| {
                    *w == c.word && *p == c.pinyin && (*s - c.score).abs() < f32::EPSILON
                })
            })
        } else if top_starts_with_filler {
            ranked.iter().enumerate().skip(1).find_map(|(i, c)| {
                (c.pinyin.split(' ').count() == top_syls
                    && sentence_keys.iter().any(|(w, p, s)| {
                        *w == c.word && *p == c.pinyin && (*s - c.score).abs() < f32::EPSILON
                    }))
                .then_some(i)
            })
        } else {
            ranked
                .iter()
                .position(|c| c.covered >= pinyin.len() && c.spelling != SpellingKind::Abbrev)
        };
        if let Some(idx) = target {
            // 首位**不是**填充档词条时保持原行为（无条件换位）；只有
            // 「首位是填充档」这一支才需要按词串口径重新判断谁更该在首位。
            let wins = idx != 0
                && (!(top_is_filler_entry || top_starts_with_filler)
                    || ranked.get(idx).zip(ranked.first()).is_some_and(|(cand, top)| {
                    // **两边都按词串口径比**（同一口径才可比）：填充档词条
                    // 常常是「真短语但词频缺失」（早点休息 freq=100、我马上到
                    // freq=100），按词拆开后 LM 明确偏好它（休息 -7.17 vs
                    // 修鞋 兜底、到 -8.02 vs 道 兜底）——旧规则只看词频档，
                    // 一律把组合候选提上来，把对的挤成 #2。而 `chuzaina`
                    // （杵在那 freq=100 vs 组合「出+在+哪」）本来就该换位，
                    // 靠的是组合读法**证据更强**。判据是分数本身，不拍阈值。
                    match (self.chain_words(&cand.pinyin), self.chain_words(&top.pinyin)) {
                        (Some(a), Some(b)) => {
                            self.chain_score(ctx, &cand.word, &a)
                                > self.chain_score(ctx, &top.word, &b)
                        }
                        _ => cand.score > top.score,
                    }
                }));
            if wins {
                let c = ranked.remove(idx);
                ranked.insert(0, c);
            }
        }
        // 含叠字母错拼（如 `zhongguoo`→中国、`shanghaai`→上海）时，若容错整串读法的
        // 统一对数分显著强于精确碎片拼接（`中国哦`），以真实分数决胜负（`nanning`/`gongguan` 等合法词不受影响）。
        if has_doubled {
            let best_fuzzy = ranked
                .iter()
                .enumerate()
                .filter(|(_, c)| c.covered >= pinyin.len() && c.spelling == SpellingKind::Fuzzy)
                .max_by(|(_, a), (_, b)| a.score.total_cmp(&b.score));
            if let (Some((fidx, fcand)), Some(top)) = (best_fuzzy, ranked.first()) {
                if fidx != 0 && fcand.score > top.score {
                    let c = ranked.remove(fidx);
                    ranked.insert(0, c);
                }
            }
        }
        Ok(())
    }
}
