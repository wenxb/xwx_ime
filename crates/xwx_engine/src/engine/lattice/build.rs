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
                    &mut key_buf, Some(0),
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
    ///
    /// `demote_scope` 是**降级范围**（纯缩写 ∪ 开头即缩写，两者都排除 `zh`/`ch`/`sh`
    /// 开头）：与「是否降级旧通道」「是否按词条口径重算分数」共用同一个判据
    /// （见 `lookup.rs` 的 `demote_scope`，⑤⑩③）。
    pub(in crate::engine) fn build_abbrev_lattice(
        &self,
        pinyin: &str,
        bounds: Option<&[usize]>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
        demote_scope: bool,
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let mut lattice =
            self.build_lattice_flags(pinyin, bounds, false, true, None, user_entries)?;
        // **整串条目边**：整串输入按简拼/混拼
        // 约束展开后**恰好命中词库条目**的读法，作为位置 0 的一条边。
        // 数据依据：20 条纯缩写输入里 18 条的正确答案属于这一类（生日快乐/
        // 新年快乐/状态…都是词条级命中），而格子原先只会逐词拼链。
        // 复用现成匹配语义（`lookup_abbrev` 纯首字母 + `lookup_mixed` 的 MixedDfs）；
        // 旧通道保留作回退点，两者按 (词, 音) 去重。
        // **只在切换开关打开时才加**：默认（旧通道仍在供给）下这些边会扰动既有排序
        // （⑤⑦② 实测 keystrokes 0.877→0.794、edits 0.728→0.561），所以必须挂开关。
        if crate::engine::DEMOTE_OLD_ABBREV_CHANNEL && demote_scope && self.dict.has_abbrev() {
            // 与旧 `ImeEngine::abbrev_candidates`（第 8 步已删）同一取法：匹配在 `MmapDict` 上。
            let DictStore::Mmap(d) = &self.dict else {
                return Ok(lattice);
            };
            // **只放行真正覆盖整串的候选**：以前对两个来源一律写
            // `end = pinyin.len()`（整串），于是 `womenjtqbj` 的「我们今天」被
            // 标成 covered=10（⑤⑨④ 的实测迹），与真正覆盖整串的「我们今天去北京」
            // 同桶 ⇒ 胜负落回分数 ⇒ 有真实词频的前缀读法赢（⑤⑨①/⑤⑨④）。
            // 去重判据 `e.end == pinyin.len()` 也拦不住它：那条前缀边自己的
            // `end` 是 7，与新建的整串边**不相等**，于是同一 (词, 音) 在位置 0
            // 上出现两次（实测）。
            //
            // 判据落在**候选自己的 `covered`** 上，因为两个来源里出问题的那个
            // 是 `lookup_mixed`：MixedDfs 按输入逐位消费没问题，但它的返回还含
            // **前缀混拼读法**那一路（`lookup_mixed_full_pinyin` 的 (4)），那一路
            // 的 `covered` 是**真实前缀长度**（实测 `womenjtqbj` → 「我们今天」
            // `covered=7`）——旧代码正是把它抬成了整串。
            // `covered == 0` 保留：那是「未知/整串」的约定值（字段文档），
            // `lookup_abbrev` 的返回恒为 0（⑤⑦③ 实测），而 `abbrev_candidates`
            // 对 0 一律回填整串长度。
            //
            // **`lookup_abbrev` 侧不要再按 `matched_len == n_letters` 筛**
            // （试过一次，已回退）：它虽然是「整串命中」的真值判据
            // （见 `Candidate::matched_len`），但 `lookup_abbrev` 在收集阶段就到
            // `MAX_LOOKUP_RESULTS`（500 条）**早停**，筛是**早停之后**才做的 ⇒
            // 排在 500 名以后的整串命中被误杀。实测 `smsh`：命中的 `matched_len`
            // 只有 {3,4} 两种，而 500 条里「什么是」（ml=4）已被挡在窗口外 ⇒
            // 20 条清单从 20/20 掉到 19/20、「什么是」直接丢召回。要用它必须先把
            // 早停放宽或改成按 ml 优先收集，那是 dict 侧的改动，不在本阶段范围。
            let n_letters = pinyin.len(); // 纯 ASCII 输入：字节数 == 字母数
            // **三条供给，按代价从低到高**：
            // ① `lookup_abbrev_patterns`（索引驱动，代价 ∝ 命中词条数）——
            //    覆盖「缩写位在开头/中间/末尾」的全部混合写法；折叠与全声母
            //    走同一条路径（约束在拿到词条拼音后逐位验证）；
            // ② `lookup_abbrev`（纯简拼 + 折叠变体，前缀匹配 + 500 早停）；
            // ③ `lookup_mixed`（交错 DFS + 末尾半音节补全）——预算式探索，
            //    只负责 ① 表达不了的形态（`zho`→中国 这类末尾半音节）。
            let mut whole: Vec<Candidate> = d.lookup_abbrev_patterns(pinyin)?;
            whole.extend(d.lookup_abbrev(pinyin)?);
            whole.extend(d.lookup_mixed(pinyin)?);
            // 去重从「逐条线性扫 `lattice[0]`」改成哈希集合：索引驱动那条供给
            // 一次可带回上千条（2 字母键的词条数 p99≈900），线性扫是 O(n²)。
            let mut seen_whole: std::collections::HashSet<String> = lattice[0]
                .iter()
                .filter(|e| e.end == pinyin.len())
                .map(|e| e.word.clone())
                .collect();
            for c in whole {
                // 来源分流的具体落点：只放行真正覆盖整串的候选。`covered`
                // 在这里**不是**「恒为 0」——`lookup_mixed` 的前缀混拼读法会
                // 带上真实前缀长度（实测 `womenjtqbj` → 「我们今天」`covered=7`），
                // 正是它以前被下面的 `end: pinyin.len()` 抬成「覆盖整串」。
                if c.covered > 0 && c.covered < n_letters {
                    continue;
                }
                if !seen_whole.insert(c.word.clone()) {
                    continue;
                }
                lattice[0].push(LatticeEdge {
                    end: pinyin.len(),
                    word: c.word,
                    pinyin: c.pinyin,
                    freq: c.freq,
                    fuzzy_penalty: 0.0,
                    typo_penalty: 0.0,
                    edit_count: 0,
                    gap: false,
                    abbrev: true,
                });
            }
        }
        Ok(lattice)
    }

    /// [`Self::build_lattice_with`] 的带全部门槛开关版本:`tolerant` 打开容错
    /// 弧,`abbrev` 打开简拼弧。两者互不影响,各自只在需要时才打开。
    #[inline]
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
        // 造词条目快照由调用方给:一次查询只取一代,
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
                    &mut key_buf, Some(0),
                )?;
                // 这一步到达的位置 = 从 `start` 建出的所有边的终点，它们才需要
                // 继续枚举（下一个词的起点）。
                for edge in &lattice[start] {
                    let end = edge.end;
                    if end > start && !seen[end] {
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
                &mut key_buf, Some(0),
            )?;
        }
        Ok(lattice)
    }

}