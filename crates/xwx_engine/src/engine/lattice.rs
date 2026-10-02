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

/// 把「干净串里的字节前缀长度」映射回**原始输入串**里的长度。
    ///
    /// `'` 只是强制切分符(见 [`ImeEngine::lookup_candidates`] 的归一化),
    /// 但它在用户**看到的**串里占一个字节。分段上屏要消耗的是原文长度,
    /// 且口径必须是「吃掉落在覆盖范围内的分隔符 + 紧跟其后的那一个」:
    /// 覆盖停在分隔符前(`xi'an` 覆盖 `xi`)时,原文长度含那个 `'`(3),
    /// 否则上屏「西」之后 preedit 会剩下 `'an`——前导分隔符既无意义,又会被
    /// 用户当成多出来的字符。
    ///
    /// `bounds` 为空(输入不含 `'`,绝大多数场景)时是**恒等映射**,零开销。
    pub(super) fn original_covered_len(clean_len: usize, bounds: Option<&[usize]>) -> usize {
        match bounds {
            None => clean_len,
            Some(b) => clean_len + b.iter().filter(|&&x| x <= clean_len).count(),
        }
    }

    /// 候选收口的统一过滤:把被用户 Ctrl+Delete 屏蔽的候选从最终
    /// 列表里滤掉(在排序、截断、合并、回声兜底**全部完成后**——
    /// 这样屏蔽不改变任何「保留候选」的相对排序与得分,只做删除)。
    ///
    /// 快照取自用户层(若有):无用户层时一次都不查,**空屏蔽表时
    /// 候选列表逐字节不变**(判退化的结构性保证)。回声候选的
    /// (key, word) = (输入串, 输入串),天然撞不上任何屏蔽键。
    pub(super) fn filter_blocked(&self, ranked: &mut Vec<Candidate>) {
        let Some(snap) = crate::user_lm::user_entries_snapshot(&self.lm) else {
            return;
        };
        ranked.retain(|c| !snap.is_blocked(&c.pinyin, &c.word));
    }

    /// 后向 DP(A3):`result[pos]` = 从 `pos` 到串尾的最优补全分
    /// (log10 域,越大越好);不可达串尾的位置为 `f32::NEG_INFINITY`。
    ///
    /// 递推(从串尾往前,与 [`Self::decode_beam_with_lattice`] 的前向
    /// beam 对称):`result[pos] = max over lattice[pos] 的边 e of
    /// (e 的词频项 + lm_weight × LM 近似分 - e.fuzzy_penalty + result[e.end])`,
    /// 边界 `result[len] = 0`(空后缀代价为零)。每条边只处理一次,
    /// 复杂度 O(边数)。
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
    ) -> Result<Vec<f32>, EngineError> {
        let len = pinyin.len();
        let mut result = vec![f32::NEG_INFINITY; len + 1];
        result[len] = 0.0;
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();
        // LM 近似的起点状态:无上文。`ctx` 仅供首词(位置 0)精确打分,
        // 后缀位置统一用 initial(见函数级注释的近似说明)。
        let _ = ctx;
        let initial = self.lm.initial_state();

        // `'` 强制切分点的语义已在 `build_lattice` 的逐音节检查里完全
        // 落实——格里本来就不含任何跨 `'` 的边,后向 DP 在格上递推,
        // 继承格的边界语义即可,不需要也不应再做边界过滤。
        for pos in (0..len).rev() {
            for edge in &lattice[pos] {
                if !result[edge.end].is_finite() {
                    continue; // 该边到不了串尾,不构成完整补全
                }
                // 空词边(容错解码里「吃掉一段字母」的弧):没有词频项、没有
                // LM 转移,只有它自己的编辑惩罚。
                let score = if edge.gap {
                    -edge.typo_penalty + result[edge.end]
                } else {
                    let (logp, _) = self.lm.score(&initial, &edge.word);
                    let dict_logp = if total_freq > 0 {
                        ((edge.freq.max(1) as f64).log10() - log10_total) as f32
                    } else {
                        0.0
                    };
                    dict_logp + logp * self.lm_weight - edge.fuzzy_penalty - edge.typo_penalty
                        + result[edge.end]
                };
                if score > result[pos] {
                    result[pos] = score;
                }
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
    /// 在干净串中的偏移。映射口径见 [`original_covered_len`]。
    pub(super) fn fill_covered(cands: &mut [Candidate], clean_len: usize, bounds: Option<&[usize]>) {
        for c in cands.iter_mut() {
            let clean = if c.covered == 0 {
                clean_len
            } else {
                c.covered.min(clean_len)
            };
            c.covered = Self::original_covered_len(clean, bounds);
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
            let (lm_score, _) = self.lm.score(&state, c.word.as_str());
            let dict_logp = if total_freq > 0 {
                ((c.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            c.score = dict_logp + lm_score * self.lm_weight;
            // spelling 保持来源给的值(见上面 `raw` 的注释),这里不再覆盖。
            // 简拼候选覆盖整串(它的 key 就是整串的缩写),分段上屏时
            // 一次吃掉全部输入。此刻 `pinyin` 是干净串,写入的是干净串
            // 坐标;调用方 [`Self::fill_covered`] 负责映射回原文长度。
            c.covered = pinyin.len();
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
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = exact.to_vec();
        if !pinyin.is_ascii() || stalls.is_empty() {
            return Ok(lattice);
        }
        let user_entries = crate::user_lm::user_entries_snapshot(&self.lm);
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
                self.collect_chain_edges(
                    pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                    user_entries.as_ref(), bounds, true, Some(stalls), 0, 0.0, true,
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
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        self.build_lattice_with(pinyin, bounds, false, None)
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
    ) -> Result<Vec<Vec<LatticeEdge>>, EngineError> {
        let n = pinyin.len();
        let mut lattice: Vec<Vec<LatticeEdge>> = (0..=n).map(|_| Vec::new()).collect();
        if !pinyin.is_ascii() {
            return Ok(lattice);
        }
        // 造词条目快照:一次查询取一次(Arc 克隆 O(1)),与 LM 打分
        // 使用的用户计数快照同源(同一份 Arc,不同字段)。引擎没挂
        // 用户层时为 None,下面的合并一次都不发生。
        let user_entries = crate::user_lm::user_entries_snapshot(&self.lm);
        let mut cache: HashMap<String, Vec<Candidate>> = HashMap::new();
        // 长串试过把容错预算从 64 降到 16:延迟只从 13~20ms 降到 12~16ms
        // (仍超 12ms 门槛),却把「我们今天」这种深一点的整句读法饿掉了
        // (`womenjinxtianqugongyuan` 从「我们今天 #1」退回「我们 #1」)。
        // 两头不讨好,故保持与精确模式同量级——**代价只落在本来就解不出的
        // 长串上**(改动前 7~9ms),打对的输入一次都不进这条分支。
        let per_start_budget = if tolerant {
            MAX_TOLERANT_CHAINS_PER_START
        } else {
            MAX_CHAINS_PER_START
        };
        for start in 0..n {
            let mut budget = per_start_budget;
            let mut chain: Vec<&str> = Vec::new();
            self.collect_chain_edges(
                pinyin, start, start, &mut chain, &mut lattice, &mut cache, &mut budget,
                user_entries.as_ref(), bounds, tolerant, stalls, 0, 0.0, false,
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
    ) -> Result<(), EngineError> {
        let n = pinyin.len();
        if chain.len() >= MAX_EDGE_SYLLABLES || *budget == 0 {
            return Ok(());
        }
        let max_len = (n - pos).min(segmenter::MAX_SYLLABLE_LEN);
        for len in 1..=max_len {
            if *budget == 0 {
                return Ok(());
            }
            // `'` 强制切分点:当前音节 [pos, pos+len) 内部不得含边界
            // (边界落在音节*边界*上是合法的、正是消歧要的切分;落在音节
            // *内部*才非法——如 `xian` 单音节跨 2)。`bounds` 为 None
            // (无 `'` 输入)时此分支一次都不进,与改动前逐字节一致。
            if bounds.is_some_and(|b| b.iter().any(|&x| pos < x && x < pos + len)) {
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
            let mut readings: Vec<(&str, f32, u8)> = Vec::new();
            if self.segmenter.is_syllable(syl) {
                readings.push((syl, 0.0, 0));
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
                    readings.push((typo_syl, cost, 1));
                }
            }
            for (syl, arc_cost, arc_edits) in readings {
                if *budget == 0 {
                    return Ok(());
                }
                *budget -= 1;
                let arc_penalty = typo_penalty + arc_cost;
                chain.push(syl);
                let end = pos + len;
            let joined = chain.join(" ");

            // (key, fuzzy_penalty):精确键惩罚为 0,模糊键带上
            // 展开时的累计 -log10(weight)。fuzzy 关闭时只有精确键。
            let keys: Vec<(String, f32)> = if self.fuzzy_enabled {
                expand_fuzzy_scored(&joined, &self.fuzzy_pairs)
            } else {
                vec![(joined.clone(), 0.0)]
            };
            // 「只补新边」模式(容错格以精确格为底时的复用):这条链一次弧都
            // 没用过 ⇒ 它的边在精确格里已经在,不必重复推。
            for (key, fuzzy_penalty) in &keys {
                if only_typo_edges && arc_penalty == 0.0 {
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
                        gap: false,
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
                                gap: false,
                            });
                        }
                    }
                }
                }

                // 深度优先：延长链（下一音节从 `end` 继续）
                self.collect_chain_edges(
                    pinyin, start, end, chain, lattice, cache, budget, user_entries, bounds,
                    tolerant, stalls, typo_edits + arc_edits, arc_penalty, only_typo_edges,
                )?;
                chain.pop();
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
                            gap: true,
                        });
                    }
                } else {
                    self.collect_chain_edges(
                        pinyin, start, pos + gap, chain, lattice, cache, budget, user_entries,
                        bounds, tolerant, stalls, typo_edits + 1, cost, only_typo_edges,
                    )?;
                }
            }
        }
        Ok(())
    }

}
