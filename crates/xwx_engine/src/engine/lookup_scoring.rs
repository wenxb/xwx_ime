//! 词条证据、格边候选与句子构造的打分辅助。
//!
//! 从 `lookup.rs` 平移而来，**不改任何判据、分数或次序**：只是给新英文
//! 通道留出 `lookup.rs` 的空间。`EntryEvidence` 类型留在 `lookup.rs`
//! （`abbrev_correct.rs` 按该路径导入）。

use super::lookup::EntryEvidence;
use super::*;

mod literal;
mod sentence;

impl ImeEngine {
    /// 统一词先验对数分（log10）：`freq == 0`（用户自造词）按 `USER_COINED_PRIOR_FREQ` 兜底，
    /// 真实大词库（`total_freq >= 1_000_000`）中的 `freq == FILLER_FREQ_TIER` 填充条目扣 `FILLER_PRIOR_PENALTY`。
    #[inline]
    pub(super) fn word_prior_logp(freq: u64, log10_total: f64, total_freq: u64) -> f32 {
        if total_freq == 0 {
            return 0.0;
        }
        let eff = if freq == 0 { USER_COINED_PRIOR_FREQ } else { freq };
        let raw = ((eff as f64).log10() - log10_total) as f32;
        if total_freq >= 1_000_000 && freq == FILLER_FREQ_TIER {
            raw - FILLER_PRIOR_PENALTY
        } else {
            raw
        }
    }

    /// 词内凝聚度加分（仅填充档多字词触发，避免与标定词频重复计分并省去热路径 DAWG 随机读）。
    #[inline]
    pub(super) fn internal_cohesion_bonus(&self, word: &str, freq: u64) -> f32 {
        if freq != FILLER_FREQ_TIER || word.len() <= 3 {
            return 0.0;
        }
        match self.lm.collocation_model() {
            Some(wx) => wx.internal_collocation_score(word) * INTERNAL_COLLOC_WEIGHT,
            None => 0.0,
        }
    }

    /// 计算简拼整词相对输入的缩写音节数对应的通道惩罚（与简拼链按音节扣 `ABBREV_PENALTY` 同刻度）。
    pub(super) fn abbrev_syllable_penalty(inp: &str, pinyin: &str) -> f32 {
        fn rec(inp: &[u8], syls: &[&[u8]]) -> Option<usize> {
            if syls.is_empty() {
                return inp.is_empty().then_some(0);
            }
            let syl = syls[0];
            let mut best: Option<usize> = None;
            let mut step = |take: usize, is_abbrev: bool| {
                if inp.len() >= take && inp[..take] == syl[..take] {
                    if let Some(rest) = rec(&inp[take..], &syls[1..]) {
                        let c = rest + usize::from(is_abbrev);
                        best = Some(best.map_or(c, |b| b.min(c)));
                    }
                }
            };
            step(syl.len(), false);
            if syl.len() > 2 && matches!(&syl[..2], b"zh" | b"ch" | b"sh") {
                step(2, true);
            }
            if syl.len() > 1 {
                step(1, true);
            }
            if syl == b"er" && inp.first() == Some(&b'r') {
                if let Some(rest) = rec(&inp[1..], &syls[1..]) {
                    let c = rest + 1;
                    best = Some(best.map_or(c, |b| b.min(c)));
                }
            }
            if syls.len() == 1 && inp.len() > 1 && inp.len() < syl.len() && syl.starts_with(inp) {
                best = Some(best.map_or(1, |b| b.min(1)));
            }
            best
        }
        let syls: Vec<&[u8]> = pinyin.split(' ').map(str::as_bytes).collect();
        let count = if syls.len() <= 8 {
            rec(inp.as_bytes(), &syls).unwrap_or(syls.len())
        } else {
            syls.len()
        };
        ABBREV_PENALTY * count as f32
    }

    pub(in crate::engine) fn fill_entry_evidence(
        &self,
        pinyin: &str,
        c: &mut Candidate,
        ev: Option<&EntryEvidence>,
    ) -> Result<(), EngineError> {
        let DictStore::Mmap(d) = &self.dict else {
            return Ok(());
        };
        if c.freq != 0 && ev.is_none_or(|e| e.total == 0 || c.covered < pinyin.len()) {
            return Ok(());
        }
        let Some(freq) = d.entry_freq(&c.pinyin, &c.word)? else {
            return Ok(());
        };
        let from_chain = c.freq == 0;
        let Some(ev) = ev else {
            if from_chain && freq > FILLER_FREQ_TIER {
                c.freq = freq;
            }
            return Ok(());
        };
        if ev.total == 0 || c.covered < pinyin.len() {
            if from_chain && freq > FILLER_FREQ_TIER {
                c.freq = freq;
            }
            return Ok(());
        }
        let log10_total = (ev.total as f64).log10();
        let dict_logp = Self::word_prior_logp(freq, log10_total, ev.total);
        let lm_gain = self.lm.score_gain_with_context(
            &ev.state,
            ev.context.as_ref(),
            &c.word,
            WANXIANG_GAIN_SCALE,
        );
        let abbrev_cost = if c.spelling == SpellingKind::Abbrev {
            Self::abbrev_syllable_penalty(pinyin, &c.pinyin)
        } else {
            0.0
        };
        let entry_score = dict_logp
            + lm_gain * self.lm_weight
            + self.internal_cohesion_bonus(&c.word, freq)
            - abbrev_cost;
        if from_chain {
            // 组合链分不该被词库填充档（freq=100）或偏低整词频拖垮：取两者更强证据。
            if freq > FILLER_FREQ_TIER {
                c.freq = freq;
            }
            if entry_score > c.score {
                c.score = entry_score;
            }
        } else {
            c.score = entry_score;
        }
        Ok(())
    }

    pub(super) fn push_rescored(
        ranked: &mut Vec<Candidate>,
        index: &mut crate::candidate_index::CandidateIndex,
        c: Candidate,
        rescored: bool,
    ) {
        match index.find(ranked, &c) {
            Some(i) => {
                if rescored
                    && (c.covered > ranked[i].covered
                        || (c.covered == ranked[i].covered && c.score > ranked[i].score))
                {
                    ranked[i].score = c.score;
                    ranked[i].covered = c.covered;
                    if c.freq > FILLER_FREQ_TIER || ranked[i].freq == 0 {
                        ranked[i].freq = c.freq;
                    }
                }
            }
            None => {
                index.insert(ranked.len(), &c);
                ranked.push(c);
            }
        }
    }

    /// 词条口径重算所需的**共享证据**（整词频 + 上文 LM 状态）。
    ///
    /// 按查询算一次而不是按候选算：整串条目边一批可达数百条，每条各构造一次
    /// LM 状态是纯浪费（实测把 `full:weishenme` 的 p95 抬高约 0.9ms）。
    pub(in crate::engine) fn entry_evidence(&self, ctx: &LmContext) -> Result<EntryEvidence, EngineError> {
        let state = self.lm.state_from_context(ctx);
        let context = self.lm.prepare_context(&state);
        Ok(EntryEvidence {
            total: self.dict.total_freq()?,
            context,
            state,
        })
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

        let initial = self.lm.state_from_context(ctx);
        let scorer = self.lm.prepare(&initial);

        let mut best: HashMap<(&str, &str), Candidate> = HashMap::new();
        for edge in &lattice[0] {
            if edge.gap {
                continue;
            }
            let lm_gain = scorer.score_gain(&edge.word, WANXIANG_GAIN_SCALE);
            let dict_logp = Self::word_prior_logp(edge.freq, log10_total, total_freq);
            let remaining = (best_suffix.len() - 1).saturating_sub(edge.edit_count as usize);
            let score = dict_logp
                + lm_gain * self.lm_weight
                + self.internal_cohesion_bonus(&edge.word, edge.freq)
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
                matched_len: 0,
                english: None,
            };
            match best.entry((&edge.word, &edge.pinyin)) {
                std::collections::hash_map::Entry::Vacant(e) => { e.insert(cand); }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if cand.score > e.get().score { e.insert(cand); }
                }
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
    /// 「末尾只差一个声母」：去掉末尾字母后仍能**完整切成音节**。
    ///
    /// 三种放行形状：
    /// - 去掉最后 1 个字母：`daishix` → 去尾 `daishi` ✓；
    /// - 去掉最后 2 个字母且它们是双字母声母 `zh`/`ch`/`sh`：`yiwanch` →
    ///   去尾 `yiwan` ✓。格子与混合 DFS 都把双字母声母当一个声母
    ///   （`collect_chain_edges` 的 `len==2 && zh|ch|sh` 弧），门槛必须同构,
    ///   否则「全拼+ch 尾」打字中状态没有供给；
    /// - 尾部连续 k≥2 个单字母声母：`buyy`（bu + yi/yang 两个首字母）→
    ///   去尾 `bu` ✓。两个连续首字母的打字中状态没有别的类别认领——
    ///   `pure_abbrev` 不认（`u` 不能起音节）、`leading_abbrev` 不认
    ///   （`bu` 是音节）、去尾 1 字母剩 `buy` 切不开。
    ///
    /// 连续声母分支只认**真声母**字母（`INITIAL_LETTERS`）：零声母
    /// `a`/`e`/`o` 与自成音节的 `i`/`u`/`v` 不算——它们要么本身是完整
    /// 音节要么不是任何音节的声母，不能给任意垃圾尾放行面。
    ///
    /// 不放行的形状不变：`shanghaai` → 去尾 `shanghaa` ✗、`jintqbj` →
    /// 去尾 `jintqb` ✗（后者去 2 字母剩 `jintq` 也切不开）。
    pub(super) fn ends_one_letter_short(&self, pinyin: &str) -> bool {
        let n = pinyin.len();
        if n >= 2 && !self.segmenter.segment(&pinyin[..n - 1]).is_empty() {
            return true;
        }
        if n >= 3
            && matches!(&pinyin[n - 2..], "zh" | "ch" | "sh")
            && !self.segmenter.segment(&pinyin[..n - 2]).is_empty()
        {
            return true;
        }
        // 尾部连续 k≥2 个真声母字母(`buyy` 的 `yy`)。
        const INITIAL_LETTERS: &[u8] = b"bpmfdtnlgkhjqxrzcsyw";
        (2..n).any(|k| {
            pinyin[n - k..].bytes().all(|c| INITIAL_LETTERS.contains(&c))
                && !self.segmenter.segment(&pinyin[..n - k]).is_empty()
        })
    }

    /// 输入是否呈「完整音节前缀 + 中段声母缩写 + 后续音节/残缺尾」形态（如 `womenxqu`/`womenqbeijing`/`meixqu`/`taqle`/`yougbmen`）。
    pub(super) fn has_mid_abbrev_shape(&self, pinyin: &str) -> bool {
        const INITIAL_LETTERS: &[u8] = b"bpmfdtnlgkhjqxrzcsyw";
        let n = pinyin.len();
        if n < 4 {
            return false;
        }
        for i in 2..n.saturating_sub(1) {
            if self.segmenter.segment(&pinyin[..i]).is_empty() {
                continue;
            }
            for k in 1..=3usize {
                if i + k >= n {
                    continue;
                }
                let mid = &pinyin[i..i + k];
                let is_init = mid.bytes().all(|b| INITIAL_LETTERS.contains(&b))
                    || (k == 2 && matches!(mid, "zh" | "ch" | "sh"));
                if !is_init {
                    continue;
                }
                let tail = &pinyin[i + k..];
                if !self.segmenter.segment(tail).is_empty() || self.ends_one_letter_short(tail) {
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn best_chain_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        spelling: SpellingKind,
        per_word_penalty: f32,
    ) -> Result<Vec<Candidate>, EngineError> {
        let beam_width = if spelling == SpellingKind::Abbrev {
            ABBREV_BEAM_WIDTH
        } else {
            SENTENCE_BEAM_WIDTH
        };
        let paths = self.decode_beam_with_lattice_penalized(
            pinyin,
            ctx,
            beam_width,
            lattice,
            per_word_penalty,
        )?;
        let mut out: Vec<Candidate> = Vec::new();
        for path in paths {
            if out.len() >= ABBREV_CHAIN_NBEST {
                break;
            }
            if path.words.is_empty() {
                continue;
            }
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
            if out.iter().any(|c: &Candidate| c.word == word) {
                continue;
            }
            out.push(Candidate {
                word,
                pinyin: key,
                score: path.score,
                freq: 0,
                spelling: if path.used_edits > 0 && spelling == SpellingKind::Exact {
                    SpellingKind::Fuzzy
                } else {
                    spelling
                },
                echo: false,
                covered: pinyin.len(),
                matched_len: 0,
                english: None,
            });
        }
        Ok(out)
    }

}
