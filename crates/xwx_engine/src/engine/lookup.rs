//! 候选查询:整串查询 [`ImeEngine::lookup_candidates`] 与格边候选
//! (`lattice_candidates` / `lattice_candidates_scored`)。

use super::rank::merge_layer;
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
        // **非拼音文本在这里就挡住**:这些字符会进到按
        // **字节**切片的排序代码(`rank.rs` 的 `inp[..cp_len]`),调用
        // `lookup_candidates("你")` 直接 panic——库调用方与探针可以击穿进程。
        // 返回**原文回声**:与识别态/回声候选同一出口(内核只做拼音,
        // 「原样上屏」交给上层),既不 panic 也不假装有候选。
        if !pinyin.is_ascii() {
            return Ok(vec![Self::echo_candidate(pinyin)]);
        }

        // B2 音节分隔符:`'` 是用户敲的**强制切分点**(`xi'an` 消歧出
        // 「西安」)。归一化成「干净串 + 边界表」两个量:
        // - 干净串:去掉 `'` 的串。`'` 不进词典 key、不进 `Candidate.pinyin`
        //   (词典 key 是空格分隔音节),但它**是**原文的一部分;
        // - 边界表:每个 `'` 在干净串中的**字节偏移**(`xi'an` → 干净串
        //   `xian` + `[2]`),即「不允许任何音节/边跨越」的位置表,贯穿格
        //   构建、beam、后向 DP 与排序的 u_min 桶。
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
        // 引擎内部只认干净拼音。三套坐标(原文 / 干净串 / 候选规范拼音)
        // 由 [`super::input::NormalizedInput`] 一处收口;
        // 无 `'` 输入零分配,既有路径**逐字节不变**(判退化的硬约束)。
        let input = super::input::NormalizedInput::new(pinyin);
        // 原文(含 `'`)从这里起一直留着:回显/逃生口这类**要展示给
        // 用户**的文本只能来自它。
        let raw = input.raw();
        let pinyin: &str = input.clean();
        if input.is_empty() {
            // 输入全是 `'`(如 `'`、`''`):归一化后为空,无候选。
            return Ok(Vec::new());
        }
        let bounds: Option<&[usize]> = input.bounds();

        // **一次查询取一代用户数据**:格构建、容错重枚举、
        // 简拼合并、屏蔽过滤全部复用这一份快照。`UserLmSnapshot` 是
        // `Arc<UserTables>` 的克隆(O(1)),所以「同一代」既便宜又天然一致;
        // 各阶段各自取快照时,并发学习/删除会让同一次查询前后看到不同的表。
        // LM 打分那一侧的快照由 `MixedLM` 在查询开始建状态时取
        // (`lm/mod.rs` 的 `state_from_context`);把用户存储访问从
        // 把用户存储访问也从 `LmStore::Mixed` 内部抽出来是后续工作。
        let user_snapshot = crate::user_lm::user_entries_snapshot(&self.lm);

        super::trace::begin();
        let lattice = super::trace::timed("建精确格", || {
            self.build_lattice(pinyin, bounds, user_snapshot.as_ref())
        })?;
        // **布尔 DAG 可达性替代 beam**:这里只需要
        // 知道「精确格上存在覆盖整串的读法吗」,旧实现为此跑完整 beam
        // (每位置最多 `BEAM_WIDTH` 个状态 × 逐边 LM 打分 + 剪枝 + 路径重建)。
        // 布尔 DP 每条边只碰一次、0 次 LM 调用,语义等价(都问「有没有从 0
        // 走到串尾的边链」)——容错那条路早在第 ③ 期就用后向 DP 这么做了,
        // 这次把同一套理由用在精确路径的是非题上。
        let reachable = Self::lattice_reachable(&lattice, pinyin.len());
        // **容错解码的门槛(结构性)**:精确路径解不出整串,或整串根本
        // 切不开。打对的输入两条都不成立 ⇒ 一次都不进容错分支,零开销、
        // 零新边——这是「精确路径逐字节不退化」的结构性保证,不靠测量。
        // 门槛①:精确路解不出整串,或整串根本切不开。
        // 门槛⓪(**输入长度**):纯声母的「还在打字中」状态(`sh`/`zh`)不启用
        // 容错——它没有「打错」可言,而容错弧会在位置 0 展开并改变已有的
        // 前缀联想结果(实测 `sh` 的第 28 名候选拼音从 `shei` 变成 `shui`,
        // 同名同分,属于对拍噪音)。单音节最多 6 个字母,4 个字母起才谈得上
        // 「组合里打错一处」。
        // 「多打一个字母」有**结构信号**:拼音里不存在叠字母(`aa`/`ii`/`oo`…),
        // 输入里出现叠字母就一定是多打了。它落在哪个音节里,那个音节的窗口
        // 就要按「≤1 次编辑」重读——`shanghaai`(shang+haai)能切成
        // `shang`+`ha`+`ai`,精确路走得通,所以只看「可达/能否切分」这两条
        // 结构判据永远发现不了它。叠字母是事实,不是阈值。
        let doubled: Vec<usize> = (1..pinyin.len())
            .filter(|&i| pinyin.as_bytes()[i] == pinyin.as_bytes()[i - 1])
            .collect();
        let tolerant_due = pinyin.len() >= MIN_TOLERANT_INPUT_LEN
            && (!reachable || !self.segmenter.is_fully_segmentable(pinyin) || !doubled.is_empty());

        // 精确路能解 → 格边候选(含整串读法);解不了 → **原有**的兜底联想。
        // 兜底不能因为有了容错就省掉:它给的「最长可切分前缀 + 首音节」
        // 候选正是分段上屏要选的第一步(见 commit_candidate)。
        let ranked: Vec<Candidate> = if !reachable {
            self.fallback_candidates(pinyin, ctx, bounds)?
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
        // 「精确/兜底路是否已经解释整串」——**必须在合并旧简拼通道之前取快照**：
        // 旧通道的候选也覆盖整串（为什么要/中国人），拿合并后的池子当判据，
        // 后面的简拼格就永远不建（实测 wsm/zgr/xxn/dbq/mgx 五条正因此拿不到
        // 词级候选；关掉旧通道时它们只剩输入回声，也是同一个原因）。
        let exact_covers_whole = ranked.iter().any(|c| c.covered >= pinyin.len());
        let abbrev = if bounds.is_none() {
            self.abbrev_candidates(pinyin, ctx, &ranked)?
        } else {
            Vec::new()
        };
        debug_assert_eq!(
            super::recall::collect_recall_hits(&abbrev, super::recall::RecallSource::Abbrev).len(),
            abbrev.len(),
            "召回事实与候选一一对应"
        );
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
        let mut stalls = Self::first_stalls(&lattice, pinyin.len());
        // 叠字母位置也是「可疑位置」:精确格在这些地方走得通(`haai` 切成
        // `ha`+`ai`),但读法是错的,需要容错重读一次才谈得上被纠正。
        if !doubled.is_empty() {
            stalls.extend(doubled.iter().copied());
            stalls.sort_unstable();
            stalls.dedup();
        }
        // 容错格由下面的分支按需构建；整句候选优先在**含纠错**的格上取
        // （上游的整句也是建在含纠错的音节图上的）——否则 `shanghaai` 这种
        // 打错一个字母的输入会出「上哈爱」，把纠正后的「上海」挤下去。
        let mut tolerant_lattice_opt: Option<Vec<Vec<LatticeEdge>>> = None;
        // 容错能不能把整串读完(只有它真的读完过,才谈得上「不需要逃生口」)。
        let mut tolerant_explains = false;
        if tolerant_due && !stalls.is_empty() {
            // 容错格与精确格**分开建**:精确格逐字节不变,容错只在需要时
            // 重来一次。合并后重排截断,层序(精确 → 模糊 → 简拼)照旧。
            let tolerant_lattice =
                self.build_tolerant_lattice(pinyin, bounds, &lattice, &stalls, user_snapshot.as_ref())?;
            let tolerant_lattice = {
                tolerant_lattice_opt = Some(tolerant_lattice);
                tolerant_lattice_opt.as_ref().expect("刚存入")
            };
            // 「容错格上有没有走到底的读法」用**后向 DP** 判,不跑 beam:
            // beam 是 O(位置 × 桶宽 × 桶内边数),长串上要几万次 LM 打分
            // (实测这一步就吃掉 10ms,是延迟超标的主因);而后向 DP 每条边
            // 只碰一次,`result[0]` 有限 ⟺ 存在覆盖整串的读法,语义等价。
            let tolerant_suffix =
                self.backward_completion(pinyin, ctx, tolerant_lattice, MAX_TYPO_EDITS)?;
            // 「走到底」必须**在编辑预算内**成立:预算这一维
            // 的最后一行就是「整条路径最多 MAX_TYPO_EDITS 次编辑」。
            tolerant_explains = tolerant_suffix[MAX_TYPO_EDITS as usize][0].is_finite();
            if tolerant_explains {
                // 复用上面那次后向 DP(`lattice_candidates` 会再算一遍)。
                let tolerant =
                    self.lattice_candidates_scored(ctx, tolerant_lattice, &tolerant_suffix)?;
                let tolerant_best = tolerant
                    .iter()
                    .map(|c| c.score)
                    .fold(f32::NEG_INFINITY, f32::max);
                // 没有整串读法(-inf)⇒ 现有答案根本没解释完整串,容错值得补;
                // 有整串读法 ⇒ 必须分数更高才动。
                let worth_it = !covering_best.is_finite() || tolerant_best > covering_best;
                if worth_it {
                    // **统一配额（A2 缺陷 3）**：容错层走与简拼层同一套
                    // `merge_layer` 保底机制。原先「push 后整表截断」在精确层
                    // 已经占满上限时会把这批纠错读法**整批截掉**——用户看不到，
                    // 容错等于没做。
                    let fresh: Vec<Candidate> = tolerant
                        .into_iter()
                        .filter(|c| {
                            !ranked
                                .iter()
                                .any(|e| e.word == c.word && e.pinyin == c.pinyin)
                        })
                        .collect();
                    if !fresh.is_empty() {
                        let (_, merged) = merge_layer(
                            pinyin,
                            ranked,
                            fresh,
                            SpellingKind::Fuzzy,
                            TOLERANT_QUOTA,
                        );
                        ranked = merged;
                    }
                }
            }
        }

        // **简拼整串候选(第 ④ 期)**:把「单个字母 = 某个音节的首字母」也建成
        // 格边,于是「多个词拼起来覆盖整串」成为一条可解码读法（`zmgai` 的
        // `z`/`m`、`wmjtqbj` 的七个首字母以前在每个窗口都走不动,整串解不出
        // 一个字——实测只剩原样回显 / 只剩第一个音节的单字）。
        //
        // 三条硬口径（与上游同构，不是调参）：
        // - **只在精确格解不出整串时启用**（与容错同一个门槛）：打对的输入
        //   零开销、零新边——「精确路径逐字节不退化」是结构性保证；
        // - **只采用覆盖整串的候选**：整串覆盖本来就是简拼层的定位（混拼层
        //   同样只产覆盖整串的词），所以简拼边不可能把单字顶进第一页
        //   （`wmjtqbj` 要的是「我们今天去北京」，不是「我」）；
        // - 走没走简拼由边自己标（`LatticeEdge::abbrev`）→ 候选层序照旧
        //   Exact → Fuzzy → Abbrev，不打乱现有分层语义。
        //
        // 与上游的一致处：整串候选进**同一张候选表**、由分数与层序排序
        // （librime 的 `PrepareCandidate` 把整句放流内首位；libime 的每个
        // 候选都是覆盖整串的 lattice 路径），不设「确定性阈值」。
        // **末尾只差一个字母时**，容错层的「解释」不该挡掉末尾补全那条路：
        // `daishix` 的容错读法是「带上/戴上/代收」，而 `x → xian` 能拼出词库里
        // 真实存在的整串键（`dai shi xian` = 待实现）。早前整体拆掉
        // `!tolerant_explains` 试过：390 行漂移、零改善——所以只在
        // [`Self::ends_one_letter_short`] 成立的形状上放行。
        // **纯缩写输入**：每个字母都能起一个音节（`wsm`/`zgr`/`xxn`/`dbq`/`mgx`）。
        // 它们此前被 `MIN_ABBREV_INPUT_LEN = 4` 挡在门外——连简拼格都不建，
        // 于是只剩「输入原样回声」（实测：关掉旧简拼通道后 20 条里有 5 条如此）。
        // 判据是**结构**的：每个字母都必须能作为某个音节的首字母，且整串在精确格
        // 上不可达（可达的就是普通全拼输入，走上面那条路）。
        // 仍**受副索引约束**（`has_abbrev`）：副索引负责纯首字母简拼这条契约
        // 不能破（见 `engine/tests/abbrev.rs::engine_without_abbrev_index_degrades_gracefully`），
        // 即使简拼格本身只用主索引。
        let pure_abbrev = self.dict.has_abbrev()
            && !reachable
            && pinyin.len() >= 2
            && pinyin
                .char_indices()
                .all(|(i, c)| {
                    c.is_ascii_alphabetic()
                        && !segmenter::abbrev_syllables(&pinyin[i..i + 1]).is_empty()
                });
        // 「已有整串读法」这条要**忽略原样回显**：纯缩写输入的解不了整串时，
        // 兜底先给的就是回显候选（`covered = 整串`、`echo = true`），
        // 它不算「已经有读法」，否则门槛永远挡住简拼格（实测 wsm/zgr/xxn/dbq/mgx
        // 五条因此一条简拼候选都拿不到）。
        let covered_whole = ranked.iter().any(|c| c.covered >= pinyin.len());
        // **开头即缩写**：首窗口是单字母、且它本身不是完整音节（`z`/`w`/`b`…）。
        // 这类输入的读法必然是「单字母缩写 + 字面尾链」（`ztai` = z + `tai`、
        // `ztailan`、`jttqzenmy`…）——旧判据（`pure_abbrev` 要求**每个**字母都能
        // 起一个音节）把它们整个排除，连简拼格都不建，读法无处产生
        // （实测 `ztai` 的门槛迹：pure_abbrev=false ⇒ needs_abbrev=false）。
        // 判据是结构事实：单字母不成音节 ⇒ 开头那一段只能当缩写读。
        // 收紧：要求**前两个字母都不能**构成音节。只看首字母会把「打错的正常输入」
        // 也算进来（`nihso` 的 `n` 不成音节，但它开头其实是正常的 `ni`），
        // 实测那样会让 `edits` 语料 MRR 0.728 → 0.680 退化。
        let leading_abbrev = self.dict.has_abbrev()
            && !reachable
            && pinyin.len() >= 3
            && self.segmenter.segment(&pinyin[..1]).is_empty()
            && self.segmenter.segment(&pinyin[..2]).is_empty()
            // `zh`/`ch`/`sh` 是**合法双字母声母**（`zhongguor` 的开头），
            // 不是"开头即缩写"——不排除它们会把全拼输入也拉进这一支
            // （实测 `zhongguor` 的「中国」被挤出前 10）。
            && !matches!(&pinyin[..2], "zh" | "ch" | "sh");
        let needs_abbrev = !reachable
            && (pure_abbrev
                || leading_abbrev
                || ((!tolerant_explains || self.ends_one_letter_short(pinyin))
                    && pinyin.len() >= MIN_ABBREV_INPUT_LEN))
            && if pure_abbrev || leading_abbrev {
                !exact_covers_whole
            } else {
                !covered_whole
            };
        super::trace::note(|| {
            format!(
                "门槛: reachable={reachable} tolerant_explains={tolerant_explains} len={} pure_abbrev={pure_abbrev} covered_whole={covered_whole} exact_covers_whole={exact_covers_whole} needs_abbrev={needs_abbrev}",
                pinyin.len()
            )
        });
        if needs_abbrev {
            let abbrev_lattice = super::trace::timed("建简拼格", || {
                self.build_abbrev_lattice(pinyin, bounds, user_snapshot.as_ref())
            })?;
            // 「覆盖整串」要的是**整条路径**，不是某条边：格里的候选语义是
            // 「第一个词 + 该词终点的最优补全分」（分段上屏的基础），所以整串
            // 读法必须自己重建（见 `best_chain_candidate`）。
            // 「覆盖整串」要的是**整条路径**，不是某条边：格里的候选语义是
            // 「第一个词 + 该词终点的最优补全分」（分段上屏的基础），整串读法
            // 由带 LM 状态的 beam 自己重建（见 `best_chain_candidate`）。
            for c in self.best_chain_candidates(
                pinyin,
                ctx,
                &abbrev_lattice,
                SpellingKind::Abbrev,
                ABBREV_PER_WORD_PENALTY,
            )? {
                if !ranked.iter().any(|e| e.word == c.word && e.pinyin == c.pinyin) {
                    ranked.push(c);
                }
            }
            // **词级简拼候选**：在简拼格上跑与精确路**同一个**抽取
            // （位置 0 的边 + 该边终点的最优补全分，见 `lattice_candidates`），
            // 得到的是 `去北京`/`我今天` 这类**单词条读法**，覆盖是「这个词
            // 吃掉多少字母」（分段上屏要的语义），而不是整串。
            // 旧通道（`lookup_abbrev`/`lookup_mixed`）仍照常给整串候选，
            // 两者按 (word, pinyin) 去重后一起排序——这一步只**加**候选、
            // 不改任何既有候选的分与序，回退点就是删掉这段。
            for mut c in self.lattice_candidates(pinyin, ctx, &abbrev_lattice)? {
                c.spelling = SpellingKind::Abbrev;
                if !ranked.iter().any(|e| e.word == c.word && e.pinyin == c.pinyin) {
                    ranked.push(c);
                }
            }
            {
                {
                    sort_candidates(pinyin, &mut ranked);
                    ranked.truncate(MAX_CANDIDATES);
                    // **整串候选置顶**(上游同构,不是调参):libime 的 n-best
                    // 整句候选占候选表 index 0..n-1,librime 的整句候选在它
                    // 自己的 Translation 流内排首位。这里也必须按结构置顶,
                    // 不能靠分数:整句分是多个词的 log 概率之和,与单词分
                    // 不同量纲,比分数必然输给只覆盖前缀的词(实测「我们今天」
                    // 压过「我们今天去北京」)。
                    //
                    // **不用阈值**:上游（librime 的 `seg.end == input.length()`、
                    // 微软的「能否构成覆盖整串的完整解析」、libpinyin 无条件
                    // 前插 n-best 句子）判的都是「覆盖」这个布尔事实,分数只
                    // 决定谁排第几。这条分支本来就只在「精确格解不出整串」时
                    // 才走到,不存在「本来有更好答案却被整句顶掉」的情况。
                    super::trace::note(|| {
                        let t: Vec<String> = ranked
                            .iter()
                            .take(5)
                            .map(|c| format!("{}|freq={}|cov={}|{:?}|{:.4}", c.word, c.freq, c.covered, c.spelling, c.score))
                            .collect();
                        format!("简拼段前五: {}", t.join("  "))
                    });
                    if let Some(idx) = ranked.iter().position(|c| c.covered >= pinyin.len()) {
                        if idx != 0 {
                            let c = ranked.remove(idx);
                            ranked.insert(0, c);
                        }
                    }
                }
            }
        }

        // **整句候选（全拼）**：上游 `script_translator.cc:497-508` 的判据——
        // 「≥2 个音节 **且没有恰好覆盖整串的词条**」就生成整句，再由
        // `PrepareCandidate`（同文件 :524-531）把它放进候选流首位。
        //
        // 这里用同一套带状态 beam 在**精确格**上取整串最优路径：`daishixian`
        // 这类「要多条词条拼起来才成句」的输入，旧实现首屏只有
        // 「带/代/待/呆/袋/戴」——整串读法根本没被产出，不是排序问题。
        //
        // 只在**精确格能覆盖整串**且当前没有整串候选时做：打一半的输入不走
        // 这里（简拼/容错各有自己的门槛），打对的短词也不受影响（那本来就是
        // 一条词条覆盖整串，`has_reliable_phrase` 为真）。
        if reachable && pinyin.len() >= MIN_ABBREV_INPUT_LEN {
            // **含纠错的格优先**：整句要在与上游同构的图上取（见上面的说明）。
            let sentence_lattice = tolerant_lattice_opt.as_ref().unwrap_or(&lattice);
            let mut sentence_keys: Vec<(String, String, f32)> = Vec::new();
            for c in
                self.best_chain_candidates(
                pinyin,
                ctx,
                sentence_lattice,
                SpellingKind::Exact,
                ABBREV_PER_WORD_PENALTY,
            )?
            {
                if !ranked.iter().any(|e| e.word == c.word && e.pinyin == c.pinyin) {
                    sentence_keys.push((c.word.clone(), c.pinyin.clone(), c.score));
                    ranked.push(c);
                }
            }
            sort_candidates(pinyin, &mut ranked);
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
            } else {
                ranked.iter().position(|c| c.covered >= pinyin.len())
            };
            if let Some(idx) = target {
                // 首位**不是**填充档词条时保持原行为（无条件换位）；只有
                // 「首位是填充档」这一支才需要按词串口径重新判断谁更该在首位。
                let wins = idx != 0
                    && (!top_is_filler_entry
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
        let explained = reachable
            || tolerant_explains
            || ranked.iter().any(|c| c.covered >= pinyin.len());
        // `ranked` 为空时不补:下面那条回声兜底给出的就是同一条「原样上屏」
        // (词、覆盖范围、层都一样,分数是回声的 0),补了反而改掉它的分数,
        // 让 `zhzhzhzh` 这类输入的对拍出现无意义差异。
        if !explained && !ranked.is_empty() {
            // 逃生口的**文本是原文**:它代表「原样上屏」,
            // 而用户敲的是 `zz'zz` 不是清洗后的 `zzzz`。
            let mut escape = Self::echo_candidate(raw);
            escape.score = ESCAPE_HATCH_SCORE;
            escape.covered = pinyin.len(); // 覆盖范围仍按干净串算,下面统一映射回原文
            ranked.push(escape);
        }

        // 回声兜底必须在**屏蔽过滤之前**追加,与改动前的格边分支逐字节一致:
        // 「候选被用户 Ctrl+Delete 全删光」时窗口要能真的收起
        // (tests/blocklist_delete.rs 的 `deleting_every_candidate_…` 钉着
        // 这条);反过来把回声放到过滤之后,会让删除全部候选后仍留一个
        // 「原样上屏」条目,窗口永远收不起来。
        // 文本同样只来自原文(D9)。
        if ranked.is_empty() {
            ranked.push(Self::echo_candidate(raw));
        }
        self.filter_blocked(&mut ranked, user_snapshot.as_ref());
        Self::fill_covered(&mut ranked, &input);
        super::trace::note(|| {
            format!(
                "最终: {} 条, 前 5 = {:?}",
                ranked.len(),
                ranked.iter().take(5).map(|c| &c.word).collect::<Vec<_>>()
            )
        });
        Ok(super::recall::fold_display_candidates(ranked))
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
        let best_suffix = self.backward_completion(pinyin, ctx, lattice, MAX_TYPO_EDITS)?;
        self.lattice_candidates_scored(ctx, lattice, &best_suffix)
    }

    /// [`Self::lattice_candidates`] 的主体,但后向 DP(带预算维)由调用方传入。
    ///
    /// 容错路径上这个 DP 已经被算过一次(判「容错格有没有走到底」),
    /// 再让 `lattice_candidates` 自己算一遍就是把整张格的 LM 打分做两次
    /// ——长串上实测约 0.4ms 的纯重复开销。
    pub(super) fn lattice_candidates_scored(
        &self,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        best_suffix: &[Vec<f32>],
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
            let logp = self.lm.score_only(&initial, &edge.word);
            let dict_logp = if total_freq > 0 {
                ((edge.freq.max(1) as f64).log10() - log10_total) as f32
            } else {
                0.0
            };
            // 边自身得分与前向 beam 完全同式;补全分取自后向 DP。
            // 边终点必有完整补全:beam 已确认整串可解,且任何有出边的
            // 位置都能沿格走到串尾(边严格前进、终点不超串长),
            // 故 best_suffix[edge.end] 必为有限值。
            // 补全只能用**剩下的**预算:首边自己用掉的编辑数
            // 必须先扣掉,否则「首边错一处 + 补全再错一处」就凑出超预算读法。
            // `best_suffix.len() - 1` 就是这次 DP 的预算上界。
            let remaining = (best_suffix.len() - 1).saturating_sub(edge.edit_count as usize);
            let score = dict_logp + logp * self.lm_weight
                - edge.fuzzy_penalty
                - edge.typo_penalty
                + best_suffix[remaining][edge.end];
            let cand = Candidate {
                word: edge.word.clone(),
                pinyin: edge.pinyin.clone(),
                score,
                freq: edge.freq, // 词典原始频率,供展示/回退
                // 该边自身是否模糊(B4 起为逐边判定的分层来源)。
                // **容错边(编辑距离)与模糊音边同层**:两者都不是「用户
                // 精确打出来的那个键」,必须整体排在精确候选之后——这一层序
                // 就是「打对了绝不会被容错候选顶掉」的结构性保证。
                spelling: if edge.abbrev {
                    // 走过简拼弧的边(这条链里至少有一个音节只打了首字母):
                    // 与混拼层同层,排在精确/模糊之后(见 `rank.rs` 的层序)。
                    SpellingKind::Abbrev
                } else if edge.fuzzy_penalty > 0.0 || edge.typo_penalty > 0.0 {
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

    /// 在简拼格上跑**项目已有的带状态 beam**，把整串路径变成候选。
    ///
    /// 复用 [`Self::decode_beam_with_lattice`]（有解码测试覆盖、并与布尔 DAG
    /// 可达性做过对拍），**不自己写搜索**：上一轮我手写的那套 beam 在层间
    /// 算出了不可能的正分（`+85758`，而逐边分量 logp/dict_logp/罚分全为负），
    /// 这是偏离「复用同一套 LM 解码」这条规格的代价。
    ///
    /// beam 给的是覆盖整串的 n-best 路径；这里只做三件薄事：把词序拼成整串、
    /// 补上整串才有的逐词先验、标成简拼层（这条分支本来只在精确格解不出整串
    /// 时才走，走通的路径必然用过至少一条简拼弧）。
    /// 「末尾只差一个字母」：去掉最后一个字母后仍能**完整切成音节**。
    ///
    /// 用 `Segmenter::segment`（既有 DP，返回空即切不开）把末尾补全的放行面限制在
    /// 最小形状上：`daishix` → 去尾 `daishi` ✓ 放行；`shanghaai` → 去尾 `shanghaa`
    /// ✗、`jintqbj` → 去尾 `jintqb` ✗ 都不放行，行为不变。
    fn ends_one_letter_short(&self, pinyin: &str) -> bool {
        let n = pinyin.len();
        n >= 2 && !self.segmenter.segment(&pinyin[..n - 1]).is_empty()
    }

    pub(super) fn best_chain_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        spelling: SpellingKind,
        per_word_penalty: f32,
    ) -> Result<Vec<Candidate>, EngineError> {
        // 先验进 beam 的剪枝目标（见 `decode_beam_with_lattice_penalized`）。
        let paths = self.decode_beam_with_lattice_penalized(
            pinyin,
            ctx,
            ABBREV_CHAIN_NBEST,
            lattice,
            per_word_penalty,
        )?;
        let mut out: Vec<Candidate> = Vec::new();
        for path in paths {
            if path.words.is_empty() {
                continue;
            }
            // **词数上限**：整串读法的词数不得超过输入字母数的一半。
            //
            // 与「简拼弧只拼多音节词」同一口径的**字母级**版本：真实简拼每个词
            // 至少要吃掉两个字母（`wmjtqbj` 7 字母 2 个词、`zmgai` 5 字母 2 个词），
            // 而「把每个字母当成一个独立的词」是病态输入的特征——实测
            // `zhzhzhzh`（连续 zh、无元音、不是真拼音）会被解成「自会在喝醉后在乎」
            // 这类读法，而它此前只该出「原样回显」。
            // 不变量：**每个音节至少吃掉一个输入字母** ⇒ 读法的音节数不得
            // 超过输入字母数。破坏它的读法只可能来自「有的弧一个字母都没
            // 消耗」的实现缺陷——实测 `zhzhzhzh`（8 字母）会被解成 9 个音节
            // 的「自会在喝醉后在乎」，而它此前只该出「原样回显」。
            let syllables: usize = path
                .words
                .iter()
                .map(|w| w.pinyin.split(' ').count())
                .sum();
            if syllables > pinyin.len() {
                continue;
            }
            let word: String = path.words.iter().map(|w| w.word.as_str()).collect();
            let key = path
                .words
                .iter()
                .map(|w| w.pinyin.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if out.iter().any(|c: &Candidate| c.word == word && c.pinyin == key) {
                continue;
            }
            out.push(Candidate {
                word,
                pinyin: key,
                score: path.score,
                freq: 0,
                spelling,
                echo: false,
                covered: pinyin.len(),
            });
        }
        Ok(out)
    }

}
