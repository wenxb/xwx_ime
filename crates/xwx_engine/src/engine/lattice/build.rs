//! 建格:精确格、容错格、简拼格,以及音节链枚举 DFS。
//!
//! 从 `lattice.rs` 拆出（单文件 ≤600 行的约束）：这里只放**建格与链枚举**，
//! 边界判定/可达性/后向 DP 仍在父模块，方法都挂在同一个 `ImeEngine` 上。

use super::*;

impl ImeEngine {
    /// 构造**容错格**:以精确格为底,只在「走不动位置」附近补容错弧。
    ///
    /// 为什么不重建整张格:实测重建(即使弧已限定在错处附近)会把长串单次
    /// 查询从 8ms 推到 15ms——成本主要在**重复建整张格**上,不在弧上。这里
    /// 克隆精确格(几百条边,微秒级),只对 `[stall − MAX_TYPO_WINDOW, stall + 1]`
    /// 这些起点重跑链枚举,而且只推「用过弧」的新边(精确边原样复用)。
    ///
    /// 覆盖性:任何覆盖 `stall` 的容错窗口,其起点必落在
    /// `[stall − MAX_TYPO_WINDOW + 1, stall]` 内;这里两端各放宽一格。
    pub(in crate::engine) fn build_tolerant_lattice(
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
    pub(in crate::engine) fn build_lattice(
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
    pub(in crate::engine) fn build_lattice_with(
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
    pub(in crate::engine) fn build_abbrev_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        self.build_lattice_flags(pinyin, bounds, false, true, None, user_entries)
    }

    /// [`Self::build_lattice_with`] 的带全部门槛开关版本:`tolerant` 打开容错
    /// 弧,`abbrev` 打开简拼弧。两者互不影响,各自只在需要时才打开。
    pub(in crate::engine) fn build_lattice_flags(
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
    pub(in crate::engine) fn collect_chain_edges<'a>(
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
            } else if abbrev
                && (len == 1 || (len == 2 && matches!(syl, "zh" | "ch" | "sh")))
            {
                // **简拼弧**:窗口不是完整音节时,把整段当成音节的首字母。
                //
                // **两条规则都按上游(RIME 的简拼代数)实现**:
                // - `abbrev/^(\l).+$/$1/`:首字母 ∵ `j` → `ji`/`jin`/`jing`…
                // - `abbrev/^([zcs]h).+$/$1/`:**`zh`/`ch`/`sh` 作为一个声母**,
                //   一个音节吃**两个**输入字母。
                // 只做前者会漏掉真正的双字母声母:`shh`(时候)被读成三个音节
                // `社(she)会(hui)化(hua)`——实测候选第一是「社会化」而不是「时候」。
                // (第三条 `derive/^([zcs])h(.*)$/$1$2/` 即 `zh` 可写成 `z`,
                //  由 len==1 的分支覆盖。)
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
