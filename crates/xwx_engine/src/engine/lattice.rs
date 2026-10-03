//! 格(lattice)的构造与切分:容错格、简拼副索引候选、回落候选、
//! 链枚举(`collect_chain_edges`)与各种边界判定。

use super::*;

mod build;

impl ImeEngine {
    /// 窗口里是否含元音(`v` 记 ü,也算)。
///
/// **每个拼音音节都含元音**(音节表里没有 `m`/`n`/`ng` 这类成音节),
/// 所以「不含元音的窗口」不可能是音节打错——它是缩写(`zgr`、`nih`)或
/// 纯乱码。容错弧只对含元音的窗口开放:实测放开后 `zgr` 会被读出
/// 「而/尬/则/砸…」49 条(靠 `zg`→`ze`、`gr`→`ga` 这类无元音窗口的
/// 替换),把简拼与回声语义整片冲掉。
fn window_has_vowel(window: &str) -> bool {
    window
        .bytes()
        .any(|b| matches!(b, b'a' | b'e' | b'i' | b'o' | b'u' | b'v'))
}

/// 窗口里有没有叠字母。拼音里没有 `aa`/`ii`/`oo` 这种写法,所以叠字母是
/// 「多打了一个字母」的**结构证据**——不是阈值,不需要标定。
fn has_doubled_letter(window: &str) -> bool {
    window.as_bytes().windows(2).any(|p| p[0] == p[1])
}

/// `syl` 是不是窗口 `w` **删掉一个字母**后得到的读法(即 `w` 由 `syl` 插入
/// 一个字母而来)。用来在叠字母窗口里挑出唯一说得通的那个读法。
fn is_one_char_deletion(w: &str, syl: &str) -> bool {
    let (w, s) = (w.as_bytes(), syl.as_bytes());
    if w.len() != s.len() + 1 {
        return false;
    }
    (0..w.len()).any(|skip| {
        w.iter()
            .enumerate()
            .filter(|(j, _)| *j != skip)
            .map(|(_, b)| *b)
            .eq(s.iter().copied())
    })
}

// 把「干净串里的字节前缀长度」映射回**原始输入串**里的长度,已收进
// `super::input::NormalizedInput::to_raw_len`。
//
// 口径是「吃掉落在覆盖范围内的分隔符 + 紧跟其后的那一个」:覆盖停在
// 分隔符前(`xi'an` 覆盖 `xi`)时,原文长度含那个 `'`(3),否则上屏
// 「西」之后 preedit 会剩下 `'an`——前导分隔符既无意义,又会被用户
// 当成多出来的字符。

/// 音节 `[pos, pos + len)` 是否**穿过**了强制切分点。
    ///
    /// 判据与格构建逐字相同:分隔符落在音节**内部**(`pos < x < pos + len`)
    /// 才非法;落在音节边界上合法——那正是用户敲 `'` 想要的切分。
    /// 抽成函数是为了让**兜底路径**用同一条判据。
    pub(super) fn syllable_crosses_boundary(pos: usize, len: usize, bounds: Option<&[usize]>) -> bool {
        super::input::crosses_boundary(bounds, pos, len)
    }

    /// 一条空格分隔的拼音 key 是否与强制切分点相容（判据见
    /// [`super::input::key_respects_boundaries`]）。
    pub(super) fn key_respects_boundaries(key: &str, bounds: Option<&[usize]>) -> bool {
        super::input::key_respects_boundaries(bounds, key)
    }

    /// 候选收口的统一过滤:把被用户 Ctrl+Delete 屏蔽的候选从最终
    /// 列表里滤掉(在排序、截断、合并、回声兜底**全部完成后**——
    /// 这样屏蔽不改变任何「保留候选」的相对排序与得分,只做删除)。
    ///
    /// **快照由调用方传入**:一次查询只取一代用户数据,
    /// 过滤用的必须是查询开始那一刻的表——自己在这里再取一次,就会出现
    /// 「本次查询前半段用旧代、这里用新代」的混用。无用户层时一次都不查,
    /// **空屏蔽表时候选列表逐字节不变**(判退化的结构性保证)。回声候选的
    /// (key, word) = (输入串, 输入串),天然撞不上任何屏蔽键。
    pub(super) fn filter_blocked(
        &self,
        ranked: &mut Vec<Candidate>,
        user: Option<&crate::user_lm::UserLmSnapshot>,
    ) {
        let Some(snap) = user else {
            return;
        };
        ranked.retain(|c| !snap.is_blocked(&c.pinyin, &c.word));
    }

    /// 格上「存在覆盖整串的读法」的**布尔**判定:O(边数)、**0 次 LM 调用**。
    ///
    /// 生产查询在精确路径上只需要回答一个是非题
    /// (「有没有从 0 走到 n 的边链」),旧实现为此跑一整次 beam——每个位置
    /// 维护桶、逐个状态调 LM 打分、剪枝、再重建路径。边只向前
    /// (`end > start`),所以从串尾往前一遍 DP 就够。
    ///
    /// 与 [`Self::decode_beam_with_lattice`] 的等价性(`!beam.is_empty()`
    /// ⟺ 这里返回 true)由 `engine/tests/dag_reachability.rs` 在随机小格上
    /// 对拍;beam 本身保留为独立算法接口(`decode_paths`/解码测试用),
    /// 生产查询不再调它。
    pub(super) fn lattice_reachable(lattice: &[Vec<LatticeEdge>], len: usize) -> bool {
        let mut reach = vec![false; len + 1];
        if let Some(slot) = reach.get_mut(len) {
            *slot = true;
        }
        for pos in (0..len).rev() {
            let Some(edges) = lattice.get(pos) else {
                continue;
            };
            reach[pos] = edges
                .iter()
                .any(|e| reach.get(e.end).copied().unwrap_or(false));
        }
        reach.first().copied().unwrap_or(false)
    }

    /// 后向 DP(A3 + D13):`result[r][pos]` = 从 `pos` 到串尾、**最多再用
    /// `r` 次容错编辑**的最优补全分(log10 域,越大越好);不可达串尾的
    /// 位置为 `f32::NEG_INFINITY`。预算更大的一行恒 ≥ 更小的一行
    /// (用不完的预算不强制花掉)。
    ///
    /// 递推(从串尾往前,与 [`Self::decode_beam_with_lattice`] 的前向 beam 对称):
    /// `result[r][pos]` 取遍 `lattice[pos]` 中 `edit_count ≤ r` 的边,每条边给出的
    /// 候选值 = e 的词频项 + lm_weight × LM 近似分 − e.fuzzy_penalty − e.typo_penalty +
    /// `result[r - e.edit_count][e.end]`,取其中最大。边界 `result[r][len] = 0`
    /// (空后缀代价为零)。每条边在每个预算行里只处理一次,复杂度
    /// O(边数 × (预算 + 1))。
    ///
    /// **为什么要有预算这一维**:[`MAX_TYPO_EDITS`] 原先
    /// 只卡「单次链枚举」——编辑次数落成 [`LatticeEdge`] 后只剩罚分,后向
    /// DP 无从限制**整条路径**的累计编辑数,于是「两个各错一处的词」能拼出
    /// 一条两次编辑的读法,与「整条路径最多一次编辑」的口径自相矛盾。
    /// 现在边的 `edit_count` 参与递推:`result[预算][0]` 有限 ⟺ **在预算内**
    /// 存在覆盖整串的读法。
    ///
    /// **刻意的近似,不是 bug**:LM 是左到右的(打分需要左侧上下文),
    /// 后向 DP 在位置 `pos` 拿不到「从开头走到这里」的精确 LM 状态,
    /// 只能从「无上文」状态起评每条边的 LM 分。bigram 下等价于每条边
    /// 只拿 unigram/backoff 分、拿不到 bigram 接续分。这个近似
    /// **只影响候选之间的相对排序**(所有候选的补全分用同一口径,
    /// 比较仍然公平),**不影响 beam 选出的整句首选**——首选由前向
    /// beam 用精确 LM 状态决定,不经过这里。若未来要消除近似,只能
    /// 把后向 DP 换成「从 beam 幸存路径反查」(回到被 BEAM_WIDTH 钳死
    /// 的老路)或引入双向 LM,两者都不值得。
    pub(super) fn backward_completion(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        max_edits: u8,
    ) -> Result<Vec<Vec<f32>>, EngineError> {
        let len = pinyin.len();
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        // LM 近似的起点状态:无上文。`ctx` 仅供首词(位置 0)精确打分,
        // 后缀位置统一用 initial(见函数级注释的近似说明)。
        let _ = ctx;
        let initial = self.lm.initial_state();

        // 每条边自身的分数与剩余预算无关 ⇒ 先算一遍(每边一次 `score_only`),
        // 再按预算递推;否则预算这一维会把 LM 调用次数乘以 (预算 + 1)。
        // 空词边没有词频项、没有 LM 转移,只有它自己的编辑惩罚。
        let edge_scores: Vec<Vec<f32>> = lattice
            .iter()
            .map(|bucket| {
                bucket
                    .iter()
                    .map(|edge| {
                        if edge.gap {
                            -edge.typo_penalty
                        } else {
                            let logp = self.lm.score_only(&initial, &edge.word);
                            let dict_logp = if total_freq > 0 {
                                ((edge.freq.max(1) as f64).log10() - log10_total) as f32
                            } else {
                                0.0
                            };
                            dict_logp + logp * self.lm_weight - edge.fuzzy_penalty
                                - edge.typo_penalty
                        }
                    })
                    .collect()
            })
            .collect();

        // `'` 强制切分点的语义已在 `build_lattice` 的逐音节检查里完全
        // 落实——格里本来就不含任何跨 `'` 的边,后向 DP 在格上递推,
        // 继承格的边界语义即可,不需要也不应再做边界过滤。
        let mut result: Vec<Vec<f32>> = Vec::with_capacity(max_edits as usize + 1);
        for budget in 0..=max_edits {
            // 本行**先入容器再填**:零编辑边会读 `result[budget]` 自己这一行
            // (边严格前进 ⇒ `edge.end > pos`,而 `pos` 从串尾往前推进,
            // 所以同行的 `edge.end` 一定已经填好)。少了这一步,预算 0 的行
            // 会在第一次遇到零编辑边时越界。
            result.push(vec![f32::NEG_INFINITY; len + 1]);
            let row = result.len() - 1;
            result[row][len] = 0.0;
            for pos in (0..len).rev() {
                // 用不完的预算不强制花掉:小预算能做到的,大预算同样能做到。
                let mut best = if budget > 0 {
                    result[row - 1][pos]
                } else {
                    f32::NEG_INFINITY
                };
                for (slot, edge) in lattice[pos].iter().enumerate() {
                    if edge.edit_count > budget {
                        continue;
                    }
                    let rest = result[row - edge.edit_count as usize][edge.end];
                    if !rest.is_finite() {
                        continue; // 该边到不了串尾,不构成完整补全
                    }
                    let score = edge_scores[pos][slot] + rest;
                    if score > best {
                        best = score;
                    }
                }
                result[row][pos] = best;
            }
        }
        Ok(result)
    }

    /// 构造一个回声候选:word / pinyin 都是原始输入串本身,freq = 0。
    ///
    /// 仅在全场一个真实候选都没有时由 `lookup_candidates` 追加(照搬
    /// librime echo_translator 的最低优先级兜底项),让候选列表非空。
    pub(super) fn echo_candidate(pinyin: &str) -> Candidate {
        Candidate {
            word: pinyin.to_string(),
            pinyin: pinyin.to_string(),
            score: 0.0,
            freq: 0,
            spelling: SpellingKind::Exact,
            echo: true,
            // 回声候选整串上屏(它是「词库解不出的原始输入串」),
            // 覆盖范围即整串长度——分段上屏时它会一次吃掉全部输入。
            covered: pinyin.len(),
        }
    }

    /// 回填候选的**覆盖范围**(干净串坐标 → 原始输入串坐标)。
    ///
    /// 未标注(`covered == 0`,词典直接产出/旧数据)的候选按**整串**处理:
    /// 这正是本字段引入前的语义(上屏即复位),所以任何没标注的来源都
    /// 自动保持旧行为,不会退化成「只消耗 0 字节」。
    ///
    /// `clean_len` 是干净串(去掉 `'`)的字节长度,`bounds` 是每个 `'`
    /// 在干净串中的偏移。映射口径见
    /// [`super::input::NormalizedInput::to_raw_len`]。
    pub(super) fn fill_covered(cands: &mut [Candidate], input: &super::input::NormalizedInput<'_>) {
        for c in cands.iter_mut() {
            let clean = if c.covered == 0 {
                input.clean_len()
            } else {
                c.covered.min(input.clean_len())
            };
            c.covered = input.to_raw_len(clean);
        }
    }

    /// 简拼来源候选(2):`lookup_abbrev` / `lookup_mixed` 命中的整串词。
    ///
    /// 打分口径与 [`Self::fallback_candidates`] 完全同式(同一量纲):
    /// `log10(freq/总词频) + lm_weight × LM(ctx) 分`——简拼候选覆盖
    /// 整串,只打一次词频项与 LM 项,与「一个词覆盖全串」的兜底词同权。
    ///
    /// 去重:已在 `existing`(精确/模糊/兜底层)出现过的 `(word, pinyin)`
    /// 不再进简拼层——同一词条只能属于一个层,靠前层优先(精确 > 模糊
    /// > 简拼的结构性保证因此成立,见 `sort_candidates`)。
    ///
    /// 优雅退化:副索引缺失时 `lookup_abbrev` 返回空、`lookup_mixed`
    /// 靠编译期静态音节表照常工作(见 `MmapDict::lookup_mixed` 注释),
    /// 两条路径都不 panic;`DictStore::Sqlite`(测试/内存词库)没有
    /// 简拼能力,直接返回空。
    ///
    /// 调用方约定:本函数只在 `existing` 排序截断**之前**合并,
    /// 否则截断后的 100 条会丢掉简拼层(merge-then-truncate,
    /// 见 `lookup_candidates` 两条路径的合并点)。
    pub(super) fn abbrev_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        existing: &[Candidate],
    ) -> Result<Vec<Candidate>, EngineError> {
        let raw: Vec<Candidate> = match &self.dict {
            DictStore::Mmap(d) => {
                // **层序由来源标定**:
                // - `lookup_abbrev`(首字母副索引)是缩写层 → `Abbrev`;
                // - `lookup_mixed` 自己分两种:交错 DFS / 全拼路的候选是缩写
                //   (末位音节只给首字母)→ `Abbrev`;**末尾半音节补全**的候选
                //   是精确前缀读法(输入是它拼音的前缀,只是没打完)→ 留
                //   `Exact`,与格子里「打字中的补全词」同层。
                // 所以这里**不再统一覆盖** spelling:统一覆盖会把「北京/你好/
                // 没有」按缩写层压到所有精确候选之后(实测 `beijin` 的北京从
                // #1 掉到 #35),而那正是本次要修的行为。
                let mut v = d.lookup_abbrev(pinyin)?;
                for c in &mut v {
                    c.spelling = SpellingKind::Abbrev;
                }
                v.extend(d.lookup_mixed(pinyin)?);
                v
            }
            DictStore::Sqlite(_) => Vec::new(),
        };
        if raw.is_empty() {
            return Ok(Vec::new());
        }

        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        let state = self.lm.state_from_context(ctx);
        let seen: std::collections::HashSet<(&str, &str)> = existing
            .iter()
            .map(|c| (c.word.as_str(), c.pinyin.as_str()))
            .collect();

        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for mut c in raw {
            if seen.contains(&(c.word.as_str(), c.pinyin.as_str())) {
                continue; // 已在更靠前的层出现:同词不进简拼层
            }
            let lm_score = self.lm.score_only(&state, c.word.as_str());
            let dict_logp = if total_freq > 0 {
                ((c.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            c.score = dict_logp + lm_score * self.lm_weight;
            // spelling 保持来源给的值(见上面 `raw` 的注释),这里不再覆盖。
            // 简拼候选**默认**覆盖整串(它的 key 就是整串的缩写),分段上屏时
            // 一次吃掉全部输入。此刻 `pinyin` 是干净串,写入的是干净串
            // 坐标;调用方 [`Self::fill_covered`] 负责映射回原文长度。
            //
            // 例外:前缀混拼读法(见 `MmapDict::lookup_mixed` 的 (4))已经带上
            // 了**真实覆盖**(`womenjintqubeij` 的「我们今天」= 9),不能在这里
            // 被抹成整串长度——那会让它一次吃掉全部输入,剩下的 `qubeij` 就
            // 没了落点。只有缺省值 0 才回填。
            if c.covered == 0 {
                c.covered = pinyin.len();
            }
            // lookup_abbrev(前缀命中多个简拼 key)与 lookup_mixed(多种
            // 切法)都可能重复命中同一词,简拼层内部仍按 (word, pinyin) 去重。
            let entry = best
                .entry((c.word.clone(), c.pinyin.clone()))
                .or_insert_with(|| c.clone());
            if c.score > entry.score {
                *entry = c;
            }
        }
        Ok(ordered_candidates(best))
    }

    /// 位置 `pos` 是否允许开容错弧(现在恒为「允许」,`center` 只表示
    /// 「这次构建启用了容错」)。保留函数形态是为了把「为什么不做位置限制」
    /// 的结论留在代码里,而不是只留在提交信息里。
    pub(super) fn window_covers_stall(stalls: Option<&[usize]>, pos: usize, len: usize) -> bool {
        match stalls {
            None => false,
            Some(s) => s.iter().any(|&x| pos <= x && x < pos + len),
        }
    }

    /// 精确格上「最先走不动的位置」集合:从位置 0 出发沿格边**可达**、
    /// 却没有出边的位置(串尾不算「走不动」)。
    ///
    /// 这就是容错弧的定位判据,也是唯一可靠的判据——试过两种都失败了:
    /// - 「任何长度都不构成音节的位置」:音节内部的位置(`men` 的 `n`)命中,
    ///   错处被定位到第 4 字节,正确读法消失;
    /// - 「第一个不可精确切分的位置」(可达性 DP):别的切法会造出伪边界,
    ///   同样定位错。
    ///
    /// 用**格上的可达性 + 有没有出边**就不会错:它问的是「精确解码走到这里
    /// 之后还能不能往下走」,与真实打字行为完全同构。容错弧只对**覆盖这些
    /// 位置**的窗口开放,于是每个输入最多只有几个窗口能开弧(而不是每个窗口
    /// 都开),长串延迟从 13~20ms 回到门槛内。
    pub(super) fn first_stalls(lattice: &[Vec<LatticeEdge>], n: usize) -> Vec<usize> {
        let mut reachable = vec![false; n + 1];
        reachable[0] = true;
        let mut queue = std::collections::VecDeque::from([0usize]);
        let mut stalls: Vec<usize> = Vec::new();
        while let Some(pos) = queue.pop_front() {
            if lattice[pos].is_empty() {
                if pos < n {
                    stalls.push(pos);
                }
                continue;
            }
            for e in &lattice[pos] {
                if !reachable[e.end] {
                    reachable[e.end] = true;
                    queue.push_back(e.end);
                }
            }
        }
        stalls.sort_unstable();
        stalls.dedup();
        stalls
    }
}
