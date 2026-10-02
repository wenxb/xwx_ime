//! 格(lattice)的构造与切分:容错格、简拼副索引候选、回落候选、
//! 链枚举(`collect_chain_edges`)与各种边界判定。

use super::*;

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

// 把「干净串里的字节前缀长度」映射回**原始输入串**里的长度,已收进
// `super::input::NormalizedInput::to_raw_len`(codex 审查 A1 把归一化、
// 边界表与这条换算集中到一处)。
//
// 口径是「吃掉落在覆盖范围内的分隔符 + 紧跟其后的那一个」:覆盖停在
// 分隔符前(`xi'an` 覆盖 `xi`)时,原文长度含那个 `'`(3),否则上屏
// 「西」之后 preedit 会剩下 `'an`——前导分隔符既无意义,又会被用户
// 当成多出来的字符。

/// 音节 `[pos, pos + len)` 是否**穿过**了强制切分点。
    ///
    /// 判据与格构建逐字相同:分隔符落在音节**内部**(`pos < x < pos + len`)
    /// 才非法;落在音节边界上合法——那正是用户敲 `'` 想要的切分。
    /// 抽成函数是为了让**兜底路径**用同一条判据(codex 审查 D9:兜底曾把
    /// 清洗串当普通串重新切分,`xi'anq` 因此召回了跨界的「先」)。
    pub(super) fn syllable_crosses_boundary(pos: usize, len: usize, bounds: Option<&[usize]>) -> bool {
        bounds.is_some_and(|b| b.iter().any(|&x| pos < x && x < pos + len))
    }

    /// 一条空格分隔的拼音 key(`xi an`)是否与强制切分点相容。
    ///
    /// 用在兜底的前缀联想上:那里拿到的候选只保证「词以输入开头」,
    /// 音节怎么切是词库说了算——`zh'on` 的 `zhong` 就是一条跨界的 key。
    pub(super) fn key_respects_boundaries(key: &str, bounds: Option<&[usize]>) -> bool {
        if bounds.is_none() {
            return true; // 无 `'` 输入:恒真,零开销(不扫 key)
        }
        let mut pos = 0usize;
        for syl in key.split(' ') {
            if Self::syllable_crosses_boundary(pos, syl.len(), bounds) {
                return false;
            }
            pos += syl.len();
        }
        true
    }

    /// 候选收口的统一过滤:把被用户 Ctrl+Delete 屏蔽的候选从最终
    /// 列表里滤掉(在排序、截断、合并、回声兜底**全部完成后**——
    /// 这样屏蔽不改变任何「保留候选」的相对排序与得分,只做删除)。
    ///
    /// **快照由调用方传入(codex 审查 A3)**:一次查询只取一代用户数据,
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
    /// codex 审查第二批 P1:生产查询在精确路径上只需要回答一个是非题
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
    /// **为什么要有预算这一维(codex 审查 D13)**:[`MAX_TYPO_EDITS`] 原先
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

    /// 构造**容错格**:以精确格为底,只在「走不动位置」附近补容错弧。
    ///
    /// 为什么不重建整张格:实测重建(即使弧已限定在错处附近)会把长串单次
    /// 查询从 8ms 推到 15ms——成本主要在**重复建整张格**上,不在弧上。这里
    /// 克隆精确格(几百条边,微秒级),只对 `[stall − MAX_TYPO_WINDOW, stall + 1]`
    /// 这些起点重跑链枚举,而且只推「用过弧」的新边(精确边原样复用)。
    ///
    /// 覆盖性:任何覆盖 `stall` 的容错窗口,其起点必落在
    /// `[stall − MAX_TYPO_WINDOW + 1, stall]` 内;这里两端各放宽一格。
    pub(super) fn build_tolerant_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        exact: &[Vec<LatticeEdge>],
        stalls: &[usize],
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = exact.to_vec();
        if !pinyin.is_ascii() || stalls.is_empty() {
            return Ok(lattice);
        }
        let mut cache: HashMap<String, Vec<Candidate>> = HashMap::new();
        // **从哪里重新枚举链**:从 0 起到最深的错处为止。
        //
        // - 不能只枚举错处附近:`shenem` 的「什么」需要 [shen][me] 这条
        //   **从 0 起、止于 6** 的整链边(`em`→`me` 的调换弧覆盖错处 5),
        //   起点被卡在 ≥1 时它永远生成不出来,「什么」整条丢掉(第 ③ 期
        //   收尾时就是这样丢的,靠点名目标的 oracle 才抓回来)。
        // - 错处之后的部分由弧自己沿链走完,不需要单独当起点。
        // - **每个起点只枚举一次**(而不是对每个错处各来一遍):错处位置集合
        //   内部的链枚举大量重复,去重后长串上的开销从 5 份降到 1 份。
        if let Some(&deepest) = stalls.iter().max() {
            let to = (deepest + 1).min(n.saturating_sub(1));
            for start in 0..=to {
                let mut budget = MAX_TOLERANT_CHAINS_PER_START;
                let mut chain: Vec<&str> = Vec::new();
                let mut key_buf = String::new();
                self.collect_chain_edges(
                    pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                    user_entries, bounds, true, Some(stalls), 0, 0.0, true, false, false,
                    &mut key_buf,
                )?;
            }
        }
        Ok(lattice)
    }

    /// 构造格子(精确模式)。容错模式见 [`Self::build_tolerant_lattice`]。
    pub(super) fn build_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        self.build_lattice_with(pinyin, bounds, false, None, user_entries)
    }

    /// 构造格子;`tolerant` 打开**容错解码**:
    ///
    /// - 窗口不是合法音节时,问一次音节级的「≤1 次编辑」索引
    ///   ([`segmenter::TypoSyllables`]):替换(混淆对)、漏打、多打、调换
    ///   相邻字母四类错误都能落到一个规范音节上;
    /// - 额外允许「吃掉 1~2 个无法解释的字母」的空词弧(多打/打乱):中途的
    ///   空词弧由**后继音节**的边覆盖那一段(边的区间含它),尾部的空词弧
    ///   直接把路径送到串尾。
    ///
    /// 只在 [`ImeEngine::lookup_candidates`] 判定「整串根本切不开」或
    /// 「精确解码解不出完整路径」时才打开,因此**打对的输入零开销、零新边**。
    pub(super) fn build_lattice_with(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        tolerant: bool,
        stalls: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        self.build_lattice_flags(pinyin, bounds, tolerant, false, stalls, user_entries)
    }

    /// **简拼格**:字面读法之外,允许「单个字母 = 某个音节的首字母」这条读法
    /// (见 [`segmenter::abbrev_syllables`]),让 `wmjtqbj` 这种全首字母串
    /// 也能在格里有边、被同一个后向 DP + LM 解码读出「我们今天去北京」。
    ///
    /// 只在 [`ImeEngine::lookup_candidates`] 判定「精确格解不出整串」时才构建
    /// (与容错同一个门槛思路),且调用方**只留覆盖整串的候选**——所以打对的
    /// 输入零新边、简拼也不会把单字顶进第一页。
    pub(super) fn build_abbrev_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        self.build_lattice_flags(pinyin, bounds, false, true, None, user_entries)
    }

    /// [`Self::build_lattice_with`] 的带全部门槛开关版本:`tolerant` 打开容错
    /// 弧,`abbrev` 打开简拼弧。两者互不影响,各自只在需要时才打开。
    pub(super) fn build_lattice_flags(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        tolerant: bool,
        abbrev: bool,
        stalls: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = (0..=n).map(|_| Vec::new()).collect();
        if !pinyin.is_ascii() {
            return Ok(lattice);
        }
        // 造词条目快照由调用方给(codex 审查 A3):一次查询只取一代,
        // 与 LM 打分使用的用户计数快照同源。引擎没挂用户层时为 None,
        // 下面的合并一次都不发生。
        let mut cache: HashMap<String, Vec<Candidate>> = HashMap::new();
        // 长串试过把容错预算从 64 降到 16:延迟只从 13~20ms 降到 12~16ms
        // (仍超 12ms 门槛),却把「我们今天」这种深一点的整句读法饿掉了
        // (`womenjinxtianqugongyuan` 从「我们今天 #1」退回「我们 #1」)。
        // 两头不讨好,故保持与精确模式同量级——**代价只落在本来就解不出的
        // 长串上**(改动前 7~9ms),打对的输入一次都不进这条分支。
        let per_start_budget = if tolerant {
            MAX_TOLERANT_CHAINS_PER_START
        } else if abbrev {
            MAX_ABBREV_CHAINS_PER_START
        } else {
            MAX_CHAINS_PER_START
        };
        if abbrev {
            // **简拼弧只从「真正走到了的位置」枚举**（工作队列 + 全局预算）。
            //
            // 旧实现对**每个**起点都吃满一份预算：15 字母输入 = 15 × 2048 弧
            // （实测 `womenjintqubeij` p95 16.3ms，破 12ms 门槛），而其中绝大多数
            // 起点根本接不上前一个词——简拼格是**词串链**，只有从 0 走得到的
            // 位置才可能出现在解码路径里。
            let mut seen = vec![false; n + 1];
            let mut queue = std::collections::VecDeque::new();
            seen[0] = true;
            queue.push_back(0usize);
            while let Some(start) = queue.pop_front() {
                // **每个位置一份预算**（不共享）：共享时起点 0 会把预算一次吃光，
                // 后面的位置（第二个词的起点）再也枚举不到，整串读法直接消失
                // （实测 `wmjtqbj` 退回只剩原样回显）。省下来的是「接不上的起点」，
                // 不是「走到了的位置」。
                // **首词富枚举、后续词够用即可**：访问量 = 预算 × 走到位置数，
                // 第一个词承担「整串从哪开始」的全部枚举，后续词只要能接上就够。
                let mut budget = if start == 0 {
                    per_start_budget
                } else {
                    MAX_ABBREV_TAIL_CHAINS
                };
                let mut chain: Vec<&str> = Vec::new();
                let mut key_buf = String::new();
                self.collect_chain_edges(
                    pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                    user_entries, bounds, tolerant, stalls, 0, 0.0, false, abbrev, false,
                    &mut key_buf,
                )?;
                // 这一步到达的位置 = 从 `start` 建出的所有边的终点，它们才需要
                // 继续枚举（下一个词的起点）。
                let ends: Vec<usize> = lattice[start]
                    .iter()
                    .map(|e| e.end)
                    .filter(|&e| e > start && !seen[e])
                    .collect();
                for end in ends {
                    if !seen[end] {
                        seen[end] = true;
                        queue.push_back(end);
                    }
                }
            }
            return Ok(lattice);
        }
        for start in 0..n {
            let mut budget = per_start_budget;
            let mut chain: Vec<&str> = Vec::new();
            let mut key_buf = String::new();
            self.collect_chain_edges(
                pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                user_entries, bounds, tolerant, stalls, 0, 0.0, false, abbrev, false,
                &mut key_buf,
            )?;
        }
        Ok(lattice)
    }

    /// 链枚举 DFS：延长当前音节链，把每个「链覆盖区间 → 词」记为格边。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn collect_chain_edges<'a>(
        &self,
        pinyin: &'a str,
        start: usize,
        pos: usize,
        chain: &mut Vec<&'a str>,
        lattice: &mut Vec<Vec<LatticeEdge>>,
        cache: &mut HashMap<String, Vec<Candidate>>,
        budget: &mut usize,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
        bounds: Option<&[usize]>,
        tolerant: bool,
        stalls: Option<&[usize]>,
        typo_edits: u8,
        typo_penalty: f32,
        only_typo_edges: bool,
        abbrev: bool,
        used_abbrev: bool,
        key_buf: &mut String,
    ) -> Result<(), EngineError> {
        let n = pinyin.len();
        if chain.len() >= MAX_EDGE_SYLLABLES || *budget == 0 {
            return Ok(());
        }
        let max_len = (n - pos).min(segmenter::MAX_SYLLABLE_LEN);
        // **简拼模式下先试长窗口**：长窗口才有「字面音节」(wo/men/jin/tian)，
        // 而 len=1 只是缩写弧。旧顺序从 1 开始，位置 0 会先钻进 `w`→整音节
        // 的子树把预算烧光，等轮到 `wo` 时预算已尽——实测 `womenjintianqbj`
        // 的「我们今天」(0→12) 边因此压根没建出来（探针：只有「去北京」在）。
        let lens: Vec<usize> = if abbrev {
            (1..=max_len).rev().collect()
        } else {
            (1..=max_len).collect()
        };
        for len in lens {
            if *budget == 0 {
                return Ok(());
            }
            // `'` 强制切分点:当前音节 [pos, pos+len) 内部不得含边界
            // (边界落在音节*边界*上是合法的、正是消歧要的切分;落在音节
            // *内部*才非法——如 `xian` 单音节跨 2)。`bounds` 为 None
            // (无 `'` 输入)时此分支一次都不进,与改动前逐字节一致。
            if Self::syllable_crosses_boundary(pos, len, bounds) {
                continue;
            }
            // 归一成词库写法(`lue`→`lve`、`nue`→`nve`)后再判合法性,且
            // **推进链里放的是规范写法**:下面的 `chain.join(" ")` 就是查
            // 词库的 key(`hu lue` 查不到 忽略,`hu lve` 才能)。两种写法同为
            // 3 字节,`end = pos + len` 仍按输入原文推进。
            let syl = segmenter::canonical_syllable(&pinyin[pos..pos + len]);
            // 本窗口的读法:(音节, 这条弧本身的编辑惩罚, 本弧引入的编辑数)。
            // - 精确命中:唯一读法、0 惩罚、0 编辑;
            // - 否则在容错模式且有预算时,问音节级 ≤1 编辑索引:一个窗口
            //   可能同时像好几个音节(`em` 像 `me`、`en`…),全部展开,由
            //   LM 与词频打分决定谁赢——**不在这里替用户选**。
            let mut readings: Vec<(&str, f32, u8, bool)> = Vec::new();
            if self.segmenter.is_syllable(syl) {
                readings.push((syl, 0.0, 0, false));
            } else if tolerant
                && typo_edits < MAX_TYPO_EDITS
                && Self::window_covers_stall(stalls, pos, len)
                && (MIN_TYPO_WINDOW..=MAX_TYPO_WINDOW).contains(&syl.len())
                && Self::window_has_vowel(syl)
            {
                let mut typo: Vec<(&str, f32)> = segmenter::TypoSyllables::lookup(syl).to_vec();
                // 最便宜的读法最像用户本意:只留前几条,避免分支数相乘。
                typo.sort_by(|a, b| a.1.total_cmp(&b.1));
                typo.truncate(MAX_TYPO_READINGS_PER_WINDOW);
                for (typo_syl, cost) in typo {
                    readings.push((typo_syl, cost, 1, false));
                }
            } else if abbrev && len == 1 {
                // **简拼弧**:单字母窗口不是任何完整音节,但它是若干音节的
                // 首字母(`j` → `ji`/`jin`/`jing`…)。旧实现只认「窗口本身是
                // 合法音节」,于是 `wmjtqbj` 这种全首字母串在每个位置都走不动,
                // 整串一个字都解不出来(实测:候选只剩原样回显);`zmgai` 的
                // `z`/`m` 同理,只剩 `gai`→「改」。
                //
                // 代价 `ABBREV_PENALTY` 逐弧累加后扣在 `typo_penalty` 上
                // (同一对数域,量纲一致),「走没走简拼」另由 `abbrev` 标记,
                // 供候选层序使用。
                for s in segmenter::abbrev_syllables(&pinyin[pos..pos + len]) {
                    readings.push((s, ABBREV_PENALTY, 0, true));
                }
            }
            for (syl, arc_cost, arc_edits, arc_abbrev) in readings {
                if *budget == 0 {
                    return Ok(());
                }
                *budget -= 1;
                let arc_penalty = typo_penalty + arc_cost;
                // 这条链里有没有简拼弧:逐边记录(见 `LatticeEdge::abbrev`)。
                let past_abbrev = used_abbrev || arc_abbrev;
                chain.push(syl);
                let end = pos + len;
                // **key 用可复用缓冲维护**（不再每弧 `chain.join(" ")` 分配）：
                // 简拼格构建是 2 万次量级的弧访问，每次一次 String 分配就是
                // 几毫秒——实测建格 12.8ms 的主项之一。
                let key_mark = key_buf.len();
                if key_mark > 0 {
                    key_buf.push(' ');
                }
                key_buf.push_str(syl);
                let joined = key_buf.as_str();
            // **简拼弧的索引剪枝**（见 [`crate::MmapDict::has_key_prefix`]）：
            // 词内音节链只要不再是任何词条的 key 前缀，后面的扩展都落不到词上，
            // 整棵子树当场剪掉（不花预算、不进递归）。没有它时死分支会把 DFS
            // 预算吃光——实测 `wmjtqbj` 只解出「王莽讲堂前北京」，真正的
            // 「我们今天去北京」连格子都没进。
            //
            // 代价：只按主索引剪枝，用户自造词（不在主索引里的 key）在简拼
            // 弧上会一起被剪掉——简拼弧本来就只在精确格解不出整串时才开，
            // 优先保证「有词可解」。
            // 三态：`None` 剪子树；`Prefix` 只继续往下走、**不查词条**（省掉每个
            // 链节点一次 `parse_entries` 分配——实测 22K 链就是 5~7ms 的来源）；
            // `Exact` 才是本级真有词，去查。
            let prefix_state = if arc_abbrev {
                self.dict.key_prefix_state(joined)?
            } else {
                crate::dict::KeyPrefix::Exact
            };
            if prefix_state == crate::dict::KeyPrefix::None {
                chain.pop();
                key_buf.truncate(key_mark);
                continue;
            }
            let word_here = prefix_state == crate::dict::KeyPrefix::Exact;

            // 「只补新边」模式(容错格以精确格为底时的复用):这条链一次弧都
            // 没用过 ⇒ 它的边在精确格里已经在,不必重复推。
            //
            // **简拼弧只拼多音节词**(2026-10-02):一个字母配单字词等于把
            // 「一个字」和「一个字母」划等号(`w`→王/我、`m`→莽/忙 全是高频
            // 单字),词频项会把它们顶到整句读法前面——实测 `wmjtqbj` 出的是
            // 「王莽讲堂前北京」。这也是用户早前否掉的「一字一字母」形态
            // (见 `MixedDfs` 的 `used_literal` 门槛,同一口径)。多音节词
            // (`wo men`、`qu bei jing`)不受影响。
            let abbrev_word_len_ok = !past_abbrev || chain.len() >= 2;
            // `keys` 只在这里构造：前面的剪枝/精确性检查已经挡掉了绝大多数弧
            // （它们只是某个更长词条的前缀），那些弧不该付 `join` + Vec 的分配。
            let joined_owned: String = key_buf.clone();
            let keys: Vec<(String, f32)> = if self.fuzzy_enabled {
                expand_fuzzy_scored(&joined_owned, &self.fuzzy_pairs)
            } else {
                vec![(joined_owned, 0.0)]
            };
            for (key, fuzzy_penalty) in &keys {
                if only_typo_edges && arc_penalty == 0.0 {
                    continue;
                }
                if !abbrev_word_len_ok {
                    continue;
                }
                // 简拼弧且本级不是词条（只是某个更长词条的前缀）⇒ 没有边可建，
                // 但 DFS 仍要往下走（`chain` 不动）。
                if arc_abbrev && !word_here {
                    continue;
                }
                let cands = if let Some(c) = cache.get(key.as_str()) {
                    c.clone()
                } else {
                    let c = self.dict.lookup(key)?;
                    cache.insert(key.clone(), c.clone());
                    c
                };
                for c in &cands {
                    lattice[start].push(LatticeEdge {
                        end,
                        word: c.word.clone(),
                        pinyin: c.pinyin.clone(),
                        freq: c.freq,
                        fuzzy_penalty: *fuzzy_penalty,
                        typo_penalty: arc_penalty,
                        edit_count: arc_edits,
                        gap: false,
                        abbrev: past_abbrev,
                    });
                }
                // 造词条目合并(仅精确键):与词典条目同区间同层,
                // 后续打分/排序/截断完全同式。freq 取 0(经既有
                // `freq.max(1)` 兜底拿到词典最低档词频分);排序增益
                // 由 LM 混合层的用户计数给出,不靠词频造假。
                if *fuzzy_penalty == 0.0 {
                    if let Some(snap) = user_entries {
                        for w in snap.lookup_entries(key) {
                            lattice[start].push(LatticeEdge {
                                end,
                                word: w.clone(),
                                pinyin: key.clone(),
                                freq: 0,
                                fuzzy_penalty: 0.0,
                                typo_penalty: arc_penalty,
                                edit_count: arc_edits,
                                gap: false,
                                abbrev: past_abbrev,
                            });
                        }
                    }
                }
                }

                // 深度优先：延长链（下一音节从 `end` 继续）
                self.collect_chain_edges(
                    pinyin, start, end, chain, lattice, cache, budget, user_entries, bounds,
                    tolerant, stalls, typo_edits + arc_edits, arc_penalty, only_typo_edges,
                    abbrev, past_abbrev, key_buf,
                )?;
                chain.pop();
                key_buf.truncate(key_mark);
            }
        }

        // 「吃掉一段无法解释的字母」的空词弧(多打/打乱)。链不变、没有词,
        // 因此不会自己产出候选;它的作用只是让 beam 跨过去继续前进。
        if tolerant && typo_edits < MAX_TYPO_EDITS {
            let max_gap = MAX_TYPO_GAP.min(n - pos);
            for gap in 1..=max_gap {
                if *budget == 0 {
                    // 预算可能已被上面的音节弧/递归用光(它们返回时不保证
                    // 还有余额),这里必须自己再判一次——否则 `-= 1` 下溢 panic。
                    return Ok(());
                }
                // 空词弧同样只对**覆盖走不动位置**的窗口开放(否则长串上
                // 每个位置都能吃掉 1~2 个字母,格爆炸、正确链被饿死)。
                if !Self::window_covers_stall(stalls, pos, gap) {
                    continue;
                }
                // `'` 强制切分点:被吃掉的字母里不得含分隔符。
                if bounds.is_some_and(|b| b.iter().any(|&x| pos < x && x <= pos + gap)) {
                    continue;
                }
                *budget -= 1;
                let cost = typo_penalty + segmenter::gap_cost(gap);
                if pos + gap == n {
                    // 尾部残留:本位置直接送一条空词边到串尾,否则 beam
                    // 到不了 `n`、整串白解(这就是 `…gongyuann` 的形状)。
                    if !lattice[pos].iter().any(|e| e.end == n && e.gap) {
                        lattice[pos].push(LatticeEdge {
                            end: n,
                            word: String::new(),
                            pinyin: String::new(),
                            freq: 0,
                            fuzzy_penalty: 0.0,
                            typo_penalty: cost,
                            edit_count: 1,
                            gap: true,
                            abbrev: used_abbrev,
                        });
                    }
                } else {
                    self.collect_chain_edges(
                        pinyin, start, pos + gap, chain, lattice, cache, budget, user_entries,
                        bounds, tolerant, stalls, typo_edits + 1, cost, only_typo_edges,
                        abbrev, used_abbrev, key_buf,
                    )?;
                }
            }
        }
        Ok(())
    }

}
