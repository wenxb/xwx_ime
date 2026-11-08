//! 音节链 DFS 与词边物化。
use super::*;

impl ImeEngine {
    /// 链枚举 DFS：延长当前音节链，把每个「链覆盖区间 → 词」记为格边。
    #[allow(clippy::too_many_arguments)]
    #[inline]
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
        node: Option<u32>,
    ) -> Result<(), EngineError> {
        let n = pinyin.len();
        // 音节级转移索引：只有装配了 `syllable_graph.bin` 的词库才有。
        // `None` 时下面所有判断原样退回旧的主索引二分（功能等价、每次一次跨页二分）。
        let graph = self.dict.syllable_graph();
        if chain.len() >= MAX_EDGE_SYLLABLES || *budget == 0 {
            return Ok(());
        }
        let max_len = (n - pos).min(segmenter::MAX_SYLLABLE_LEN);
        // **简拼模式下先试长窗口**：长窗口才有「字面音节」(wo/men/jin/tian)，
        // 而 len=1 只是缩写弧。旧顺序从 1 开始，位置 0 会先钻进 `w`→整音节
        // 的子树把预算烧光，等轮到 `wo` 时预算已尽——实测 `womenjintianqbj`
        // 的「我们今天」(0→12) 边因此压根没建出来（探针：只有「去北京」在）。
        for step in 1..=max_len {
            let len = if abbrev { max_len + 1 - step } else { step };
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
                let mut typo: Vec<(&str, f32, bool)> =
                    segmenter::TypoSyllables::lookup(syl).to_vec();
                // **窗口里有叠字母时,由结构决定读法,不由代价决定**:
                // 拼音里不存在 `aa`/`ii`/`oo`…,所以叠字母只能解释成「多打
                // 了一个」⇒ 只认「删掉一个字母」得来的读法,替换读法全部丢弃。
                //
                // 实测依据(`shanghaai`):窗口 `haai` 的替换读法 `huai`
                // (概率 0.01→代价 2.0)比删除读法 `hai`(0.004→2.4)便宜,
                // 按代价截断到 1 条就把「上海」丢成「上坏/上怀」。
                let deletions: Vec<(&str, f32, bool)> = if Self::has_doubled_letter(syl) {
                    typo.iter()
                        .copied()
                        .filter(|(r, _, _)| Self::is_one_char_deletion(syl, r))
                        .collect()
                } else {
                    Vec::new()
                };
                if !deletions.is_empty() {
                    typo = deletions;
                } else if !typo.is_empty() {
                    // 排序键:代价升序 → 同价优先**首字母一致**的读法(音节最
                    // 显著的是声母,用户极少漏掉整串开头的字母)→ 同价同首字母
                    // 再按音节字典序(只为结果确定,不代表语言证据)。
                    let win_first = syl.chars().next();
                    let same_init = |s: &str| s.chars().next() == win_first;
                    typo.sort_by(|a, b| {
                        a.1.total_cmp(&b.1)
                            .then_with(|| same_init(b.0).cmp(&same_init(a.0)))
                            .then_with(|| a.0.cmp(b.0))
                    });
                    // **代价并列的读法一起留下**(并列 = 现有证据不足,不当场
                    // 用字典序替用户决定谁对):实测 `mem` 窗口的 `mei`/`men`
                    // 同价 2.0,只留字典序第一个会把「我们」丢成「我没/我妹」。
                    let cheapest = typo[0].1;
                    let mut keep: Vec<(&str, f32, bool)> = typo
                        .iter()
                        .copied()
                        .filter(|r| (r.1 - cheapest).abs() <= f32::EPSILON)
                        .take(MAX_TYPO_READINGS_PER_WINDOW)
                        .collect();
                    // 剩余名额,先补**漏打型**读法(音节比窗口长 ⇒ 用户比想打
                    // 的音节少敲了一个字母),再按上面的排序补下一条。
                    //
                    // 名额 3 + 漏打型保底,是四条实测共同压出来的:
                    // - `xang`(想):要漏打型 `xiang`(2.40)进格子——它按代价
                    //   排在十几条替换读法(2.00)之后,纯按代价取永远轮不上;
                    // - `nohao`(你好):窗口 `no` 的最便宜读法是混淆对 `lo`
                    //   (0.10),漏打型 `nao`(2.40)占一个名额后,第三个名额
                    //   才轮得到邻键 `ni`(1.30)——「你好」就靠它;
                    // - `mem`(我们):`mei`/`men` 同价 2.0 并列,都要留;
                    // - `xaing`(想):`xiang`(调换 1.52)本身最便宜,漏打型
                    //   名额自动让位。
                    // 「谁对」最终交给打分器(LM/词频),选取阶段只保证
                    // **每一类可能的本意都至少有一个代表**进了格子。
                    if keep.len() < MAX_TYPO_READINGS_PER_WINDOW {
                        let in_keep = |r: &(&str, f32, bool)| keep.iter().any(|k| k.0 == r.0);
                        // 漏打型:音节比窗口长(窗口是它删掉一个字母的变体)。
                        // 子序列读法(窗口只多打了字母)不在此列——那类读法由
                        // 「吃掉一段字母」的空词弧 + 精确音节表达,代价同域。
                        if let Some(o) = typo
                            .iter()
                            .copied()
                            .find(|t| t.0.len() > syl.len() && !in_keep(t))
                        {
                            keep.push(o);
                        }
                    }
                    while keep.len() < MAX_TYPO_READINGS_PER_WINDOW {
                        let in_keep = |r: &(&str, f32, bool)| keep.iter().any(|k| k.0 == r.0);
                        match typo.iter().copied().find(|r| !in_keep(r)) {
                            Some(next) => keep.push(next),
                            None => break,
                        }
                    }
                    typo = keep;
                }
                for (typo_syl, cost, _) in typo {
                    readings.push((typo_syl, cost, 1, false));
                }
            } else if abbrev && (len == 1 || (len == 2 && matches!(syl, "zh" | "ch" | "sh"))) {
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
                // **儿化音的 `r`**:输入末尾一个 `r` 是「儿」的通用简写
                // (`zaiyikuair` → 在一块儿),但 `abbrev_syllables("r")` 只找得到
                // 以 `r` 开头的音节(`ren`/`ri`/`rong`…),`er` 不在其中 ⇒ 单独补一条。
                // 与简拼弧同代价同标记(它本来就是缩写读法)。
                if len == 1 && &pinyin[pos..pos + 1] == "r" {
                    readings.push(("er", ABBREV_PENALTY, 0, true));
                }
            }
            // **末尾半音节补全**(与上面几支互斥的读法选择**并行**,不是 else):
            // 输入在这里到头,窗口是某个更长音节的前缀(`beijin` 的 `jin` →
            // `jing`、`gongyua` 的 `yua` → `yuan`)。单看它只多一个词
            // (`beijin` 早就能出「北京」),真正要的是让**前面的词**能跟它串
            // 起来——`nihaobeijin` → 你好北京、`womenjintianqugongyua` →
            // 我们今天去公园。所以补全必须进格子,不能只留在候选池里
            // (池里只收单词条,前面有词时串不起来)。
            //
            // **两条排除**,都是「别去抢字面已经读通的读法」:
            // - 窗口**自己就是一个音节**、同时还能切成 ≥2 个音节时不补全
            //   (`xian` = 先 也是 `xi`+`an` 西安):实测不排除的话
            //   `womenjintianquxian` 的首位会从「我们今天去西安」变成
            //   「我们今天去想」。反过来 `yua`(不是音节)与 `jin`(是音节但切
            //   不开)都放行——它们才是真正的「少打一个字母」。
            // - `p = 0`(整串都当半截)只在整串不是合法全拼时启用:整串已是
            //   合法全拼时它变成「音节前缀补全」(`xian` → `xiang*` 整个区间),
            //   不是补全要的能力,端到端实测多花 4.5ms(与候选池同一条判据)。
            //
            // 补全读法**不加惩罚**、不算编辑:惩罚在**排序层**收
            // (`rank.rs` 把「Exact 层且拼音字母数 > 输入」的候选放进补全层,
            // 排在全部精确命中之后;除非它比同桶同层最强精确命中高出
            // [`COMPLETION_DOMINANCE`])——在边上扣会经 `typo_penalty`
            // 把补全边错标成容错层(`beijin` 的北京被单字墙压到 #35)。
            // 历史裁决的 1.301(`ln 0.05`)也太大:`beijin` 首位退回「被禁」。
            if pos + len == n
                && len >= crate::dict::MIN_PARTIAL_TAIL
                && Self::window_has_vowel(syl)
                && !(segmenter::syllable_at(syl, 0, syl.len()).is_some()
                    && Self::window_splits_into_multiple_syllables(syl))
                && !(pos == 0 && self.segmenter.is_fully_segmentable(pinyin))
            {
                for full in Self::syllables_with_prefix(syl) {
                    readings.push((full, 0.0, 0, false));
                }
            }
            for (syl, arc_cost, arc_edits, arc_abbrev) in readings {
                if *budget == 0 {
                    return Ok(());
                }
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
                // 链在索引里的节点：一次内存子表二分（子表 ≤412 项）就答完
                // 「这条链还有没有更长的词条」，替掉旧路径那条跨页 mmap 二分
                // （实测 1.2 万次 ≈10.5ms，是简拼格几乎全部成本）。
                let (child_node, child_info) = match (graph, node) {
                    (Some(g), Some(n)) => match g.child(n, syl)? {
                        Some(c) => (Some(c), Some(g.node(c)?)),
                        None => (None, None),
                    },
                    _ => (None, None),
                };
                let prefix_state = if arc_abbrev {
                    match graph {
                        Some(_) => match (node, child_info) {
                            // 链本身已不是任何词条的 key 前缀 ⇒ 它的任何延长也不是，
                            // 整棵子树剪掉。旧路径在这里同样得到 `None`，只是要先付
                            // 一次主索引二分才发现。
                            (None, _) | (Some(_), None) => crate::dict::KeyPrefix::None,
                            (Some(_), Some(info)) if info.exact => crate::dict::KeyPrefix::Exact,
                            (Some(_), Some(_)) => crate::dict::KeyPrefix::Prefix,
                        },
                        None => self.dict.key_prefix_state(joined)?,
                    }
                } else {
                    crate::dict::KeyPrefix::Exact
                };
                if prefix_state == crate::dict::KeyPrefix::None {
                    if start == 0 || chain.len() < 3 || len > 1 || graph.is_none() {
                        *budget -= 1;
                    }
                    chain.pop();
                    key_buf.truncate(key_mark);
                    continue;
                }
                *budget -= 1;
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
                if !(only_typo_edges && arc_penalty == 0.0)
                    && abbrev_word_len_ok
                    && (!arc_abbrev || word_here)
                {
                    // 普通键借用缓冲；只有模糊音展开才需要分配键集合。
                    let fuzzy_keys = if self.fuzzy_enabled {
                        expand_fuzzy_scored(joined, &self.fuzzy_pairs)
                    } else {
                        Vec::new()
                    };
                    let keys = fuzzy_keys
                        .iter()
                        .map(|(key, cost)| (key.as_str(), *cost))
                        .chain((!self.fuzzy_enabled).then_some((joined, 0.0)));
                    for (key, fuzzy_penalty) in keys {
                        if !cache.contains_key(key) {
                            let cands = if fuzzy_penalty == 0.0 && graph.is_some() {
                                match child_info {
                                    Some(info) if info.exact => {
                                        self.dict.lookup_at(key, info.entry_off as usize)?
                                    }
                                    _ => Vec::new(),
                                }
                            } else {
                                self.dict.lookup(key)?
                            };
                            cache.insert(key.to_owned(), cands);
                        }
                        // 缓存候选直接借用，只复制最终格边持有的词与拼音。
                        for c in &cache[key] {
                            lattice[start].push(LatticeEdge {
                                end,
                                word: c.word.clone(),
                                pinyin: c.pinyin.clone(),
                                freq: c.freq,
                                fuzzy_penalty,
                                typo_penalty: arc_penalty,
                                // **累计**而不是本弧：词边代表 [start, end) 整段被消费，前面
                                // 音节用掉的编辑数必须算进来；否则「前面错过、末尾音节精确」的
                                // 词边会少报，后向 DP 的整条路径预算形同虚设。递归往下传的也是
                                // `typo_edits + arc_edits`，两处口径一致。
                                edit_count: typo_edits + arc_edits,
                                gap: false,
                                abbrev: past_abbrev,
                            });
                        }
                        // 造词条目合并(仅精确键):与词典条目同区间同层,
                        // 后续打分/排序/截断完全同式。freq 取 0(经既有
                        // `freq.max(1)` 兜底拿到词典最低档词频分);排序增益
                        // 由 LM 混合层的用户计数给出,不靠词频造假。
                        if fuzzy_penalty == 0.0 {
                            if let Some(snap) = user_entries {
                                for w in snap.lookup_entries(key) {
                                    lattice[start].push(LatticeEdge {
                                        end,
                                        word: w.clone(),
                                        pinyin: key.to_owned(),
                                        freq: 0,
                                        fuzzy_penalty: 0.0,
                                        typo_penalty: arc_penalty,
                                        edit_count: typo_edits + arc_edits,
                                        gap: false,
                                        abbrev: past_abbrev,
                                    });
                                }
                            }
                        }
                    }
                }

                // 深度优先：延长链（下一音节从 `end` 继续）
                self.collect_chain_edges(
                    pinyin,
                    start,
                    end,
                    chain,
                    lattice,
                    cache,
                    budget,
                    user_entries,
                    bounds,
                    tolerant,
                    stalls,
                    typo_edits + arc_edits,
                    arc_penalty,
                    only_typo_edges,
                    abbrev,
                    past_abbrev,
                    key_buf,
                    child_node,
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
                        pinyin,
                        start,
                        pos + gap,
                        chain,
                        lattice,
                        cache,
                        budget,
                        user_entries,
                        bounds,
                        tolerant,
                        stalls,
                        typo_edits + 1,
                        cost,
                        only_typo_edges,
                        abbrev,
                        used_abbrev,
                        key_buf,
                        node,
                    )?;
                }
            }
        }
        Ok(())
    }
}
