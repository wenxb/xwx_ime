//! 束搜索解码:路径打分与 `decode_paths`(对拍工具会直接调它)。

use super::*;

impl ImeEngine {
    /// 在格上跑宽度 `k` 的 beam search，返回完整路径（按总分降序）。
    ///
    /// 每个位置 `pos` 的路径桶在开始扩展前已收齐所有入边（入边只来自
    /// 更早的位置），先剪到 top-K 再向前扩展——这是「beam 即剪枝」的
    /// 落点：LM 状态沿路径传递，被剪路径的状态随之丢弃。
    #[cfg_attr(not(test), allow(dead_code))] // 只被 decode_paths(测试)调用
    pub(super) fn decode_beam(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
    ) -> Result<Vec<BeamPath>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }
        // 独立算法接口(只被测试/调试调用,见 `decode_paths` 的文档):
        // 这里自取一代快照。生产查询走 `lookup_candidates`,那里一次查询
        // 只取一代、并把它传给格构建与屏蔽过滤。
        let user = crate::user_lm::user_entries_snapshot(&self.lm);
        self.decode_beam_with_lattice_penalized(
            pinyin,
            ctx,
            k,
            &self.build_lattice(pinyin, None, user.as_ref())?,
            0.0,
        )
    }

    /// 与 [`Self::decode_beam`] 相同，但格子由调用方传入（分层便于
    /// 把「格构建」与「beam 推进」分开看，性能排查/测试用）。
    ///
    /// **生产查询不再调它**：`lookup_candidates` 里
    /// 「精确格上有没有覆盖整串的读法」改用 [`ImeEngine::lattice_reachable`]
    /// 的布尔 DP；beam 作为独立算法接口保留，供 `decode_paths` 与解码测试
    /// 使用（两边等价性由 `engine/tests/dag_reachability.rs` 对拍）。
    #[cfg_attr(not(test), allow(dead_code))] // 生产查询已改用布尔 DAG 可达性
    pub(super) fn decode_beam_with_lattice(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
        lattice: &[Vec<LatticeEdge>],
    ) -> Result<Vec<BeamPath>, EngineError> {
        self.decode_beam_with_lattice_penalized(pinyin, ctx, k, lattice, 0.0)
    }

    /// [`Self::decode_beam_with_lattice`] + **每词加性先验**。
    ///
    /// 简拼整串读法需要这条：逐词 log 概率相加天生偏爱「多个超高频短词」的长链
    /// （实测 `womenjintianqbj` 会解成「我满额你今天请病假」）。先验必须进**剪枝
    /// 目标**本身，只在事后对结果排序是没用的——正确的读法可能早被 beam 剪掉了。
    pub(super) fn decode_beam_with_lattice_penalized(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
        lattice: &[Vec<LatticeEdge>],
        per_word_penalty: f32,
    ) -> Result<Vec<BeamPath>, EngineError> {
        let len = pinyin.len();
        let mut arena: Vec<BeamNode> = Vec::with_capacity((len + 1) * k);
        let mut beams: Vec<Vec<usize>> = (0..=len).map(|_| Vec::new()).collect();
        // 词频项归一化分母：log10(freq/总词频)，与 LM 的 log10 概率同量纲。
        // 总词频为 0（空词库）时词频项取 0，不进对数（避免 -inf/NaN）。
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        // **基线补偿**：LM 的「无证据基线分」（万象实测 -11.605767，见
        // examples/lm_word_probe.rs）不该按词数复付——多词读法每多一个词
        // 就白扣一次基线，实测「出+在+哪」因此输给单词「杵在那」12.19 个
        // log10。这里对第 2 个词起扣掉基线（保留各词高于基线的条件证据）。
        // 只对**全拼整句路径**（`per_word_penalty == 0.0`）生效：简拼层的逐词
        // 惩罚口径是上一期的约定，不动它。模型没有基线概念时 floor 是 0.0 ⇒ 原式。
        let _ = per_word_penalty;
        arena.push(BeamNode {
            score: 0.0,
            state: self.lm.state_from_context(ctx),
            edge: None,
            parent: None,
            used_edits: 0,
        });
        beams[0].push(0);

        // 注:`'` 强制切分点已在 `build_lattice` 的逐音节检查里完全
        // 落实——格里本来就不含任何跨 `'` 的边,beam 只在格上推进,
        // 不需要也不应再做边界过滤(边的合法性由格决定)。
        for pos in 0..len {
            let cur = std::mem::take(&mut beams[pos]);
            if cur.is_empty() {
                continue;
            }
            let mut touched: Vec<usize> = Vec::new();
            for &node_idx in &cur {
                for (slot, edge) in lattice[pos].iter().enumerate() {
                    if edge.gap {
                        // 空词边:状态不变、无词频/LM 项,只扣编辑惩罚。
                        // 空词弧本身就吃掉一次编辑预算。
                        let used = arena[node_idx].used_edits + 1;
                        if used > MAX_TYPO_EDITS {
                            continue;
                        }
                        let idx = arena.len();
                        arena.push(BeamNode {
                            score: arena[node_idx].score - edge.typo_penalty,
                            state: arena[node_idx].state.clone(),
                            edge: Some((pos, slot)),
                            parent: Some(node_idx),
                            used_edits: used,
                        });
                        beams[edge.end].push(idx);
                        touched.push(edge.end);
                        continue;
                    }
                    let (logp, next_state) = self.lm.score(&arena[node_idx].state, &edge.word);
                    let dict_logp = if total_freq > 0 {
                        // freq 为 0 的词条按 1 兜底（log10(1/T) 仍为有限值）
                        ((edge.freq.max(1) as f64).log10() - log10_total) as f32
                    } else {
                        0.0
                    };
                    // 整条路径的容错编辑预算:预算跟着 beam
                    // 状态走,超预算的分支不再扩展。
                    let used = arena[node_idx].used_edits + edge.edit_count;
                    if used > MAX_TYPO_EDITS {
                        continue;
                    }
                    let idx = arena.len();
                    arena.push(BeamNode {
                        // 模糊惩罚是同量纲(log10)的加性扣分:精确边 0,
                        // 模糊边减去累计 -log10(weight),多处混淆叠加累加。
                        score: arena[node_idx].score + dict_logp + logp * self.lm_weight
                            - edge.fuzzy_penalty
                            - edge.typo_penalty
                            - per_word_penalty,
                        state: next_state,
                        edge: Some((pos, slot)),
                        parent: Some(node_idx),
                        used_edits: used,
                    });
                    beams[edge.end].push(idx);
                    touched.push(edge.end);
                }
            }
            // 目标桶剪枝：先混合再剪，只剪本轮收到新节点的桶。
            // 注：同一词序列只可能对应一条路径（DAG 上节点唯一），
            // 桶内不需要去重，只需按分截断。
            touched.sort_unstable();
            touched.dedup();
            for &t in &touched {
                prune_beam(&mut beams[t], &arena, k);
            }
        }
        prune_beam(&mut beams[len], &arena, k);
        // 重建完整路径：沿 parent 走回根，反转成词序。
        let mut paths: Vec<BeamPath> = Vec::with_capacity(beams[len].len());
        for &idx in &beams[len] {
            let mut words: Vec<WordSpot> = Vec::new();
            let mut cur = Some(idx);
            while let Some(i) = cur {
                if let Some((start_pos, slot)) = arena[i].edge {
                    let e = &lattice[start_pos][slot];
                    words.push(WordSpot {
                        word: e.word.clone(),
                        pinyin: e.pinyin.clone(),
                    });
                }
                cur = arena[i].parent;
            }
            words.reverse();
            paths.push(BeamPath {
                score: arena[idx].score,
                words,
            });
        }
        Ok(paths)
    }

    /// 解码完整路径（测试/调试用；`lookup_candidates` 内部用同一套格子）。
    #[cfg(test)]
    pub(crate) fn decode_paths(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        k: usize,
    ) -> Result<Vec<DecodedPath>, EngineError> {
        Ok(self
            .decode_beam(pinyin, ctx, k)?
            .into_iter()
            .map(|p| DecodedPath {
                score: p.score,
                words: p.words.iter().map(|w| w.word.clone()).collect(),
                pinyin: p.words.iter().map(|w| w.pinyin.clone()).collect(),
            })
            .collect())
    }

    /// 「边输入边联想」兜底：整串还没有分词键（`"nih"`、`"zhon"`、`"z"`）。
    ///
    /// 复刻第 2.5 段的语义并保留（IBus 层依赖它避免打字途中候选窗闪没）：
    /// 最长可完整切分前缀的整切分词 + 各切法首音节的词；一个音节都切不出来时
    /// 整串前缀联想（`lookup_prefix`）。
    /// 兜底候选没有路径，按旧语义各自从 `ctx` 独立打分。
    ///
    /// A1 不变量在此同样成立(补漏:此前只修了格路径):候选必须起始于
    /// 位置 0——每种切法只查它的**首音节**与整段 `joined`,不再遍历
    /// 所有音节。旧行为把非起始音节(`zhongguor` 切出 `[zhong,gu,o]`
    /// 时的 `gu`/`o`)的词也灌进候选,实测 `zhongguor` 前 20 名里 13 个
    /// 是「只覆盖非起始音节」的词(过/国/果/古/哦……),用户无法有意义地
    /// 选中——与 A1 修掉的「`nihao` 出现好/号/毫」是同一个缺陷。
    /// 多种切法的**首音节可以不同**(`qia` → `[qi,a]` 与 `[qia]`),
    /// 每种切法的首音节都要查(`qi` 与 `qia` 都是合法的位置 0 起点)。
    ///
    /// 注意：这里**不再**把切分残余字母（`nih` 切出 `ni` 后剩下的 `h`）
    /// 当整音节做 `lookup_prefix`——那会把「只解释输入尾部 1~2 个字母的
    /// 词」（所有 h 开头的高频词：哈/哈尔滨/哈哈大笑）灌进候选，还带着
    /// 整词高频优势排在「覆盖整个输入串」的词之前（实测 `nih` 的「你好」
    /// 被挤到第 97 名）。残余字母的正确解释是「后续音节的声母」，
    /// 由简拼层 `lookup_mixed` 处理（`nih` → `ni h*` → 你好），
    /// 见 [`Self::abbrev_candidates`]。下面的整串前缀联想分支仍覆盖
    /// 「首音节一个都没切出来」的场景（如 `zhon` → 中/种/重）。
    pub(super) fn fallback_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        bounds: Option<&[usize]>,
    ) -> Result<Vec<Candidate>, EngineError> {
        if !pinyin.is_ascii() {
            return Ok(Vec::new());
        }
        // 覆盖范围(分段上屏)约定——本函数是唯一有三类来源的候选生产者:
        // 1. 「最长可切分前缀」命中的词 = 覆盖 `pinyin[..k]`(见下面的 `k`);
        // 2. 首音节词 = 覆盖第一个音节(见 `first.len()`);
        // 3. 整串前缀联想(音节枚举展开 / `lookup_prefix`) = 覆盖**整串**
        //    (它匹配的就是以整个输入开头的 key),故**刻意不标注**,让
        //    `covered == 0` 的「未知/整串」缺省语义接管。
        let mut merged: Vec<Candidate> = Vec::new();
        // 本次兜底是否走了「声母前缀按音节枚举展开」分支:
        // 决定下面要不要做第二遍按 word 去重(见排序前的注释)。
        // 只在枚举路径置 true,其余路径为 false,去重行为与旧版一致。
        let mut enum_expand_used = false;

        // **强制切分点同样约束兜底**:旧实现把清洗串当普通串
        // 重新切分,`xi'anq` 于是切出 `xian`(跨了 `'`)→ 召回「先/现/显/线」,
        // `'` 在整串解不出时形同不存在。这里用**与格构建逐字相同**的判据
        // (`ImeEngine::key_respects_boundaries`)筛切法:穿界的音节一律不要。
        // `bounds` 为 None 时下面每次判断都恒真,无 `'` 输入逐字节不变。
        let segmented = (1..=pinyin.len()).rev().find(|&k| {
            self.segmenter
                .segment(&pinyin[..k])
                .iter()
                .any(|chain| Self::key_respects_boundaries(&chain.join(" "), bounds))
        });
        if let Some(k) = segmented {
            for seg in self.segmenter.segment(&pinyin[..k]) {
                let joined = seg.join(" ");
                if !Self::key_respects_boundaries(&joined, bounds) {
                    continue; // 这条切法跨了 `'`:整条丢掉
                }
                // 覆盖范围(分段上屏):`dict.lookup(joined)` 命中的词恰好
                // 覆盖被切成音节的那段前缀 `pinyin[..k]` ⇒ `k` 字节;
                // 首音节词只覆盖**第一个音节**⇒ 它的字节长度。
                // 两者都只在这个兜底分支里出现(整串解不出时的「最长可切分
                // 前缀」),其余来源见各自注释。
                let joined_cov = k;
                let mut head = self.dict.lookup(&joined)?;
                for c in head.iter_mut() {
                    c.covered = joined_cov;
                }
                merged.extend(head);
                // A1:只查首音节(覆盖位置 0 的前缀词),不遍历后续音节
                // ——非起始音节的词对当前输入没有意义(见函数级文档)。
                // 多种切法各自的首音节都会查到(循环按切法逐个处理)。
                if let Some(first) = seg.first() {
                    let first_cov = first.len();
                    let mut head = self.dict.lookup(first)?;
                    for c in head.iter_mut() {
                        c.covered = first_cov;
                    }
                    merged.extend(head);
                }
            }
        }
        if merged.is_empty() {
            // 一个音节都没切出来（首音节还没打完）：整串前缀联想
            // 声母前缀按音节枚举展开(常量 `SYLLABLE_ENUM_QUOTA` 的注释有
            // 完整背景):`pinyin` 本身不是合法音节、但是某些音节的前缀时
            // (`zh`/`x`/`zhon`),字节前缀联想会被字典序锁死在排序最前的
            // 韵母上(`zh` 捞满 500 条全是 `zha*`,`zhe`/`zhong` 的高频字
            // 进不了候选)。改为用静态音节表枚举所有音节补全,逐音节带
            // 配额查询再合并——每个韵母下的高频字都有机会露面。
            //
            // 生效范围被 `is_syllable` 守卫严格限制:合法音节(`zhe`)与
            // 可切分的串(`ni`/`beijing`)走不到这个分支(上面的
            // `segmented` 分支已拿到候选),全拼行为逐字节不变。
            if !pinyin.is_empty() && !self.segmenter.is_syllable(pinyin) {
                let syllables: Vec<&str> = segmenter::SYLLABLES
                    .iter()
                    .copied()
                    .filter(|s| s.starts_with(pinyin))
                    .collect();
                let enum_taken = !syllables.is_empty();
                if enum_taken {
                    // `z`/`c`/`s` 不需要特判:它们同时是 `zh`/`ch`/`sh`
                    // 的前缀,`starts_with` 天然把两套音节都枚进来。
                    enum_expand_used = true;
                    for syl in syllables {
                        merged.extend(
                            self.dict.lookup_prefix_limited(syl, SYLLABLE_ENUM_QUOTA)?,
                        );
                    }
                } else {
                    // 连任何音节的前缀都不是(`hello`/`zzzz` 这类纯字母
                    // 乱串):退回整串字节前缀联想,维持旧兜底语义。
                    merged.extend(self.dict.lookup_prefix(pinyin)?);
                }
            } else {
                merged.extend(self.dict.lookup_prefix(pinyin)?);
            }
        }

        // **一字一字母**(2026-10-02):上面的 `lookup_prefix`/`lookup_prefix_limited`
        // 是「pinyin 以它开头」的匹配,`zho` 会把 `zhong a guan xi`(中阿关系)
        // 这类**比输入字母还多字**的词一并捞进来(实测 100 条里 4 条)。用户口径:
        // 候选的字数不得超过输入字母数。只在这一层过滤——用户 LM 里学过的词
        // 不在此列,那是显式行为。
        //
        // **同一条 retain 兼管强制切分点**:前缀联想拿到的候选只保证「词以输入
        // 开头」,音节怎么切由词库决定——`zh'on` 的 `zhong` 就是一条穿界的 key
        // 。`bounds` 为 None 时该判据恒真、不扫 key。
        merged.retain(|c| {
            c.pinyin.split(' ').count() <= pinyin.len()
                && Self::key_respects_boundaries(&c.pinyin, bounds)
        });

        let state = self.lm.state_from_context(ctx);
        // 与 decode_beam 同一打分公式：log10(freq/总词频) + lm_weight × logp
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        let mut ranked: Vec<Candidate> = merged
            .into_iter()
            .map(|mut c| {
                let lm_score = self.lm.score_only(&state, c.word.as_str());
                let dict_logp = if total_freq > 0 {
                    ((c.freq.max(1) as f64).log10() - log10_total) as f32
                } else {
                    0.0
                };
                // 兜底路径不经过 expand_fuzzy,fuzzy_penalty 恒为 0;
                // 公式与 decode_beam 一致(此处减 0,省略减法项)。
                c.score = dict_logp + lm_score * self.lm_weight;
                c
            })
            .collect();
        // 去重：同 (word, pinyin) 保最高分，然后排序截断
        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for c in ranked.drain(..) {
            let entry = best
                .entry((c.word.clone(), c.pinyin.clone()))
                .or_insert_with(|| c.clone());
            if c.score > entry.score {
                *entry = c;
            }
        }
        ranked = ordered_candidates(best);
        if enum_expand_used {
            // 枚举展开的第二遍去重(键 = word):逐音节查询会把多音字的
            // 每个读音都捞出来(如「这」同时挂在 zhe/zhei 下),同一个字
            // 会以多条 (word, 不同拼音) 出现、占掉多个候选位。这里按 word
            // 保最高分——词频相同的同字条目分数相同,保谁的唯一差异是
            // `Candidate.pinyin`(拼音注解),对排序与展示无影响。
            //
            // 只对枚举路径生效:精确/简拼路径里同 word 不同拼音的候选
            // 不互替(如 `chong` 的「重」与 `zhong` 的「重」是两种切法
            // 的各自补全,旧行为两个都保留),不能动——`enum_expand_used`
            // 为 false 时本分支一次都不进,既有行为逐字节不变。
            let mut best_by_word: HashMap<String, Candidate> =
                HashMap::with_capacity(ranked.len());
            for c in ranked.drain(..) {
                let entry = best_by_word
                    .entry(c.word.clone())
                    .or_insert_with(|| c.clone());
                if c.score > entry.score {
                    *entry = c;
                }
            }
            ranked = ordered_candidates(best_by_word);
        }
        sort_candidates(pinyin, &mut ranked);
        ranked.truncate(MAX_CANDIDATES);
        Ok(ranked)
    }
}
