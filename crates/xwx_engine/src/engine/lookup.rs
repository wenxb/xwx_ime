//! 候选查询:整串查询 [`ImeEngine::lookup_candidates`] 与格边候选
//! (`lattice_candidates` / `lattice_candidates_scored`)。

use super::*;

impl ImeEngine {
    /// 核心查询管线:给定拼音串,返回排序后的候选列表。
    ///
    /// 做法(Block A 重构后:候选来源与整句解码解耦):
    ///
    /// 1. 构建切分格:[`Self::build_lattice`] 从每个位置枚举音节链,
    ///    每条链的拼音串查词库得「词边」(含模糊拼音变体边),
    ///    不再枚举全部切分;
    /// 2. beam search:[`Self::decode_beam_with_lattice`] 沿位置推进,
    ///    每个位置只保留 top-K 条部分路径,LM 状态**沿路径传递**——
    ///    beam 的职责只剩「算出完整路径是否存在」(它同时是兜底分支的
    ///    判定信号),不再充当候选来源(旧实现候选被 BEAM_WIDTH 钳死);
    /// 3. 候选收集(A1+A2):候选 = `lattice[0]` 上的**全部**词边——
    ///    只取起始于位置 0 的词(旧实现把路径上所有词位都列为候选,
    ///    导致 `nihao` 里混进只覆盖 `hao` 的「好/号/毫」),且不被
    ///    beam 宽度钳制(词典查询在格构建时已完成,这里零新增查询);
    /// 4. 打分口径(A3):候选分 = **「以该候选开头的最优完整路径分」**
    ///    = 边自身得分 + 从 `edge.end` 到串尾的最优补全分,补全分由
    ///    一次后向 DP 在格上算出(见 [`Self::backward_completion`])。
    ///    这样「覆盖整串的词」(你好)与「只覆盖前缀的词」(你)落在
    ///    同一量纲上,前者自然胜出;
    /// 5. 整串无法完整解码(还没打完,如 `"nih"`、`"z"`)时走
    ///    [`Self::fallback_candidates`] 的边打字边联想兜底。
    ///
    /// 两条路径(正常 / 兜底)末尾都会再合并一次简拼来源
    /// ([`Self::abbrev_candidates`]):简拼候选本来就覆盖整个输入串
    /// (用户打前缀、拿到整词),不进 lattice、不拆成边,按
    /// [`SpellingKind::Abbrev`] 排进候选列表的简拼层。
    pub fn lookup_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
    ) -> Result<Vec<Candidate>, EngineError> {
        if pinyin.is_empty() {
            return Ok(Vec::new());
        }
        // **非拼音文本在这里就挡住**(codex 审查 D12):这些字符会进到按
        // **字节**切片的排序代码(`rank.rs` 的 `inp[..cp_len]`),调用
        // `lookup_candidates("你")` 直接 panic——库调用方与探针可以击穿进程。
        // 返回**原文回声**:与识别态/回声候选同一出口(内核只做拼音,
        // 「原样上屏」交给上层),既不 panic 也不假装有候选。
        if !pinyin.is_ascii() {
            return Ok(vec![Self::echo_candidate(pinyin)]);
        }

        // B2 音节分隔符:`'` 是用户敲的**强制切分点**(`xi'an` 消歧出
        // 「西安」)。这里把它归一化成「干净串 + 边界表」两个量:
        // - `pinyin`:去掉 `'` 的干净串。`'` 不进词典 key、不进
        //   `Candidate.pinyin`(词典 key 是空格分隔音节);
        // - `boundaries`:每个 `'` 在干净串中对应位置的**字节偏移**
        //   (`xi'an` → 干净串 `xian` + `[2]`),即「不允许任何音节/边
        //   跨越」的位置表,贯穿格构建、beam、后向 DP 与排序的 u_min
        //   桶([`Self::build_lattice`] 与 [`unreachable_tail`] 的注释)。
        //
        // **只删 `'` 不传边界是错的**(上一版实现):删完 `xi'an` 变回
        // `xian`,而 `xian` 本身是合法音节,`[xian]` 这条单音节切法原样
        // 复活,「先/现/显/线」照常在候选里——`'` 完全没起到强制切分的
        // 作用(实机:`xian` 与 `xi'an` 候选逐字节相同)。正确语义是
        // librime 的 `delimiters`:`'` 把输入钉成 [xi | an] 两段,
        // 段内照常枚举音节链,**跨段边一律不建**。
        //
        // 归一化在这里做(而不是会话层):让「展示给用户的串」与
        // 「查词典的串」解耦——会话/preedit 保留用户敲的 `xi'an`,
        // 引擎内部只认干净拼音。无 `'` 输入走 `Cow::Borrowed` +
        // 空边界表,零分配,既有路径**逐字节不变**(判退化的硬约束)。
        let (pinyin, boundaries): (std::borrow::Cow<'_, str>, Vec<usize>) =
            if pinyin.contains('\'') {
                let mut clean = String::with_capacity(pinyin.len());
                let mut bounds = Vec::new();
                for ch in pinyin.chars() {
                    if ch == '\'' {
                        // `'` 是单字节 ASCII,clean.len() 即干净串中的字节偏移
                        bounds.push(clean.len());
                    } else {
                        clean.push(ch);
                    }
                }
                (std::borrow::Cow::Owned(clean), bounds)
            } else {
                (std::borrow::Cow::Borrowed(pinyin), Vec::new())
            };
        let pinyin: &str = &pinyin;
        if pinyin.is_empty() {
            // 输入全是 `'`(如 `'`、`''`):归一化后为空,无候选。
            return Ok(Vec::new());
        }
        let bounds: Option<&[usize]> = if boundaries.is_empty() {
            None
        } else {
            Some(&boundaries)
        };

        let lattice = self.build_lattice(pinyin, bounds)?;
        let paths = self.decode_beam_with_lattice(pinyin, ctx, BEAM_WIDTH, &lattice)?;
        // **容错解码的门槛(结构性)**:精确路径解不出整串,或整串根本
        // 切不开。打对的输入两条都不成立 ⇒ 一次都不进容错分支,零开销、
        // 零新边——这是「精确路径逐字节不退化」的结构性保证,不靠测量。
        // 门槛①:精确路解不出整串,或整串根本切不开。
        // 门槛⓪(**输入长度**):纯声母的「还在打字中」状态(`sh`/`zh`)不启用
        // 容错——它没有「打错」可言,而容错弧会在位置 0 展开并改变已有的
        // 前缀联想结果(实测 `sh` 的第 28 名候选拼音从 `shei` 变成 `shui`,
        // 同名同分,属于对拍噪音)。单音节最多 6 个字母,4 个字母起才谈得上
        // 「组合里打错一处」。
        let tolerant_due = pinyin.len() >= MIN_TOLERANT_INPUT_LEN
            && (paths.is_empty() || !self.segmenter.is_fully_segmentable(pinyin));

        // 精确路能解 → 格边候选(含整串读法);解不了 → **原有**的兜底联想。
        // 兜底不能因为有了容错就省掉:它给的「最长可切分前缀 + 首音节」
        // 候选正是分段上屏要选的第一步(见 commit_candidate)。
        let ranked: Vec<Candidate> = if paths.is_empty() {
            self.fallback_candidates(pinyin, ctx)?
        } else {
            self.lattice_candidates(pinyin, ctx, &lattice)?
        };

        // —— 统一尾部(精确/兜底/容错三条来源共用,顺序即改动前的顺序)——
        // 简拼来源合并(2):简拼候选覆盖整串,本身就是整串候选。
        // 注意:必须在 fallback 截断**之后**合并——fallback 自己
        // 已按 MAX_CANDIDATES 截断,若先合并再截断,简拼层(在排序
        // 最末)会整体被截掉。合并后重排截断,简拼层得以保留
        // (它只与同样排在末尾的兜底长尾竞争最后的候选位)。
        // 简拼层只在「无 `'`」时合并:用户敲了 `'` 就是给了明确的
        // 全拼切分,`lookup_abbrev`/`lookup_mixed` 会把干净串当简拼
        // 前缀重新查出 `[xian]` 系的字(先/现/显),把刚消掉的歧义
        // 又灌回来——含 `'` 的查询语义上不是简拼,跳过简拼层。
        let abbrev = if bounds.is_none() {
            self.abbrev_candidates(pinyin, ctx, &ranked)?
        } else {
            Vec::new()
        };
        let (_, mut ranked) = merge_with_abbrev(pinyin, ranked, abbrev);

        // **第二个门槛(分数)**:已经有一个「覆盖整串」的读法、而且它比容错
        // 读法更自信时,**不动它**。
        //
        // 为什么需要这道门槛:简拼/兜底层经常给出「垃圾但覆盖整串」的读法
        // (`shenem` → 神而明之),而它恰好是简拼层——层序在容错层之下,一旦
        // 放容错候选进来就会把它顶掉;反过来,`nih`/`zhongg` 这类**真·简拼**
        // 输入的「你好」「中国」也是简拼层,分数很高,绝不该被容错候选挤下去。
        // 两者结构上无法区分,用**分数**区分:容错读法要能打得过现有答案才
        // 值得出现(`shenem`:什么 -13.7 打得过神而明之 -15.3 → 放进来;
        // `nih`:容错垃圾普遍带编辑惩罚、打不过「你好」→ 整批丢弃)。
        let covering_best = ranked
            .iter()
            .filter(|c| c.covered >= pinyin.len())
            .map(|c| c.score)
            .fold(f32::NEG_INFINITY, f32::max);
        // 容错弧的定位:精确格上「走不动」的位置(见 `first_stalls`)。
        // 一个都没有 ⇒ 精确解码本来就能走到底 ⇒ 不需要容错。
        //
        let stalls = Self::first_stalls(&lattice, pinyin.len());
        // 容错能不能把整串读完(只有它真的读完过,才谈得上「不需要逃生口」)。
        let mut tolerant_explains = false;
        if tolerant_due && !stalls.is_empty() {
            // 容错格与精确格**分开建**:精确格逐字节不变,容错只在需要时
            // 重来一次。合并后重排截断,层序(精确 → 模糊 → 简拼)照旧。
            let tolerant_lattice =
                self.build_tolerant_lattice(pinyin, bounds, &lattice, &stalls)?;
            // 「容错格上有没有走到底的读法」用**后向 DP** 判,不跑 beam:
            // beam 是 O(位置 × 桶宽 × 桶内边数),长串上要几万次 LM 打分
            // (实测这一步就吃掉 10ms,是延迟超标的主因);而后向 DP 每条边
            // 只碰一次,`result[0]` 有限 ⟺ 存在覆盖整串的读法,语义等价。
            let tolerant_suffix = self.backward_completion(pinyin, ctx, &tolerant_lattice)?;
            tolerant_explains = tolerant_suffix[0].is_finite();
            if tolerant_explains {
                // 复用上面那次后向 DP(`lattice_candidates` 会再算一遍)。
                let tolerant =
                    self.lattice_candidates_scored(ctx, &tolerant_lattice, &tolerant_suffix)?;
                let tolerant_best = tolerant
                    .iter()
                    .map(|c| c.score)
                    .fold(f32::NEG_INFINITY, f32::max);
                // 没有整串读法(-inf)⇒ 现有答案根本没解释完整串,容错值得补;
                // 有整串读法 ⇒ 必须分数更高才动。
                let worth_it = !covering_best.is_finite() || tolerant_best > covering_best;
                if worth_it {
                    let before = ranked.len();
                    for c in tolerant {
                        if !ranked.iter().any(|e| e.word == c.word && e.pinyin == c.pinyin) {
                            ranked.push(c);
                        }
                    }
                    if ranked.len() != before {
                        sort_candidates(pinyin, &mut ranked);
                        ranked.truncate(MAX_CANDIDATES);
                    }
                }
            }
        }

        // **剩余串的出路(第 ③ 期)**:候选里没有任何条目覆盖整串 ⇒ 这一串
        // 解不完整(打错的尾巴、乱码段)。此时补一条「把你敲的这些字母原样
        // 上屏」的逃生口,用户不必退格删光重打。
        //
        // 三个口径:
        // - 与回声候选同一语义(`echo = true`:原样上屏、不学词、不动
        //   `last_word`),所以 **Enter 也能上屏剩余串**(那条路早就有了,
        //   这里只是让它从候选面板直接可达);
        // - 分数压到远低于任何真实候选:它进精确层、桶也落在 0(覆盖整串),
        //   但排在所有真实候选之后——**只在覆盖整串的读法缺失时**才出现,
        //   所以打对的输入候选列表逐字节不变;
        // - 只在**没有任何读法覆盖整串**时才补(精确路与容错路都读不下去);
        //   打对的输入永远有一条完整读法,因此候选列表逐字节不变。
        // 判据是「**有没有覆盖整串的读法**」,不是「候选里有没有覆盖整串的
        // 条目」:长串正常输入(如 `womenjintianqugongyuan` 23 个字母)本来
        // 就没有单条候选能覆盖整串,但它有一条完整读法——按条目判会让正确
        // 输入凭空多一条逃生口(实测把 5 个「打对的输入逐字节不变」的断言
        // 打破)。所以门槛 = 精确没读到底 **且** 容错也没读到底。
        // 三种「已经解释清楚」都算:① 精确路读到底;② 容错路读到底;
        // ③ 候选里已有覆盖整串的条目(简拼/混合简拼层构造上覆盖整串——
        // `zgr` 这种纯缩写就是这样被解释掉的)。
        let explained = !paths.is_empty()
            || tolerant_explains
            || ranked.iter().any(|c| c.covered >= pinyin.len());
        // `ranked` 为空时不补:下面那条回声兜底给出的就是同一条「原样上屏」
        // (词、覆盖范围、层都一样,分数是回声的 0),补了反而改掉它的分数,
        // 让 `zhzhzhzh` 这类输入的对拍出现无意义差异。
        if !explained && !ranked.is_empty() {
            let mut escape = Self::echo_candidate(pinyin);
            escape.score = ESCAPE_HATCH_SCORE;
            escape.covered = pinyin.len();
            ranked.push(escape);
        }

        // 回声兜底必须在**屏蔽过滤之前**追加,与改动前的格边分支逐字节一致:
        // 「候选被用户 Ctrl+Delete 全删光」时窗口要能真的收起
        // (tests/blocklist_delete.rs 的 `deleting_every_candidate_…` 钉着
        // 这条);反过来把回声放到过滤之后,会让删除全部候选后仍留一个
        // 「原样上屏」条目,窗口永远收不起来。
        if ranked.is_empty() {
            ranked.push(Self::echo_candidate(pinyin));
        }
        self.filter_blocked(&mut ranked);
        Self::fill_covered(&mut ranked, pinyin.len(), bounds);
        Ok(ranked)
    }

    /// 把格子**位置 0** 上的每条边打成一个候选:候选分 = 边自身的词频/LM
    /// 分 + 该边终点的最优补全分(后向 DP)。
    ///
    /// 空词边(容错解码里「吃掉一段字母」的弧)不是候选,直接跳过——顺带
    /// 保住「真实候选的 `covered` 一定 > 0」这条不变量:0 在会话层的语义是
    /// 「未知/整串」,空词边要是漏进候选,分段上屏会一次吃掉整串。
    ///
    /// 精确路与容错路都调它,打分口径完全同式(差别只在格子里有哪些边)。
    pub(super) fn lattice_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
    ) -> Result<Vec<Candidate>, EngineError> {
        // A3 打分:候选分 = 以该候选开头的最优完整路径分。
        // 先跑后向 DP 拿「每个位置到串尾的最优补全分」,再给 lattice[0]
        // 上每条边打分。
        let best_suffix = self.backward_completion(pinyin, ctx, lattice)?;
        self.lattice_candidates_scored(ctx, lattice, &best_suffix)
    }

    /// [`Self::lattice_candidates`] 的主体,但后向 DP 由调用方传入。
    ///
    /// 容错路径上这个 DP 已经被算过一次(判「容错格有没有走到底」),
    /// 再让 `lattice_candidates` 自己算一遍就是把整张格的 LM 打分做两次
    /// ——长串上实测约 0.4ms 的纯重复开销。
    pub(super) fn lattice_candidates_scored(
        &self,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        best_suffix: &[f32],
    ) -> Result<Vec<Candidate>, EngineError> {
        let total_freq = self.dict.total_freq()?;
        let log10_total = (total_freq as f64).log10();

        // 首词(位置 0)的 LM 状态由真实上文 ctx 给出,这是精确的
        // (首词的前文就是已上屏文本,不存在近似问题)。
        let initial = self.lm.state_from_context(ctx);

        // 同一 (word, pinyin) 可能由多条边给出(不同音节链切到同一拼音
        // key,或模糊展开键与精确键撞车),保留得分最高的一条。
        let mut best: HashMap<(String, String), Candidate> = HashMap::new();
        for edge in &lattice[0] {
            if edge.gap {
                continue; // 空词边不是候选(它只是让 beam 跨过一段字母)
            }
            // 注意:这里**不**过滤补全分为 -inf 的边(它到不了串尾)。
            // 这类边是「只解释输入前缀」的词(如 `women…` 的「我」),分段
            // 上屏下恰恰有用(用户可以先上屏这一段,剩下的留给下一轮);
            // 它们在排序里落在自己的 u_min 桶内、按分数靠后,不会挤掉
            // 完整读法。容错格会带来更多这类前缀边,同样按低分排在桶内。
            let (logp, _) = self.lm.score(&initial, &edge.word);
            let dict_logp = if total_freq > 0 {
                ((edge.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            // 边自身得分与前向 beam 完全同式;补全分取自后向 DP。
            // 边终点必有完整补全:beam 已确认整串可解,且任何有出边的
            // 位置都能沿格走到串尾(边严格前进、终点不超串长),
            // 故 best_suffix[edge.end] 必为有限值。
            let score = dict_logp + logp * self.lm_weight
                - edge.fuzzy_penalty
                - edge.typo_penalty
                + best_suffix[edge.end];
            let cand = Candidate {
                word: edge.word.clone(),
                pinyin: edge.pinyin.clone(),
                score,
                freq: edge.freq, // 词典原始频率,供展示/回退
                // 该边自身是否模糊(B4 起为逐边判定的分层来源)。
                // **容错边(编辑距离)与模糊音边同层**:两者都不是「用户
                // 精确打出来的那个键」,必须整体排在精确候选之后——这一层序
                // 就是「打对了绝不会被容错候选顶掉」的结构性保证。
                spelling: if edge.fuzzy_penalty > 0.0 || edge.typo_penalty > 0.0 {
                    SpellingKind::Fuzzy
                } else {
                    SpellingKind::Exact
                },
                echo: false,     // 解码出的都是真实候选
                // 分段上屏的依据:这条边只覆盖输入串开头的 `edge.end` 个字节
                // (干净串坐标),剩余部分由会话留着继续组合。同一输入的不同
                // 候选覆盖范围可以不同(「我们」5 字节 vs「我们今天」13 字节)
                // ——**选哪个候选就等于选哪种分段**,这正是格边候选比
                // 「先定分段再出候选」更灵活的地方。
                covered: edge.end,
            };
            let entry = best
                .entry((cand.word.clone(), cand.pinyin.clone()))
                .or_insert_with(|| cand.clone());
            if cand.score > entry.score {
                *entry = cand;
            }
        }

        Ok(ordered_candidates(best))
    }

}
