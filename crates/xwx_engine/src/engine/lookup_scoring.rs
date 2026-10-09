//! 词条证据、格边候选与句子构造的打分辅助。
//!
//! 从 `lookup.rs` 平移而来，**不改任何判据、分数或次序**：只是给新英文
//! 通道留出 `lookup.rs` 的空间。`EntryEvidence` 类型留在 `lookup.rs`
//! （`abbrev_correct.rs` 按该路径导入）。

use super::lookup::EntryEvidence;
use super::*;

impl ImeEngine {
    pub(in crate::engine) fn fill_entry_evidence(
        &self,
        pinyin: &str,
        c: &mut Candidate,
        ev: Option<&EntryEvidence>,
    ) -> Result<(), EngineError> {
        let DictStore::Mmap(d) = &self.dict else {
            return Ok(());
        };
        let Some(e) = d.lookup(&c.pinyin)?.into_iter().find(|e| e.word == c.word) else {
            return Ok(());
        };
        if c.freq == 0 {
            c.freq = e.freq;
        }
        let Some(ev) = ev else {
            return Ok(());
        };
        if ev.total == 0 || c.covered < pinyin.len() {
            return Ok(());
        }
        let dict_logp = ((e.freq.max(1) as f64).log10() - (ev.total as f64).log10()) as f32;
        let lm = self.lm.score_only(&ev.state, &c.word);
        c.score = dict_logp + (lm - self.lm.floor_log10()) * self.lm_weight;
        Ok(())
    }

    /// 推入一条**已重算**的整串候选；同一 (词, 音) 已有既排行（精确补全/
    /// 兜底给出的同款读法）时就地改分，而不是按旧判据去重丢弃。
    ///
    /// 为什么必须就地改分：同一 (词,音) 的整串读法只有一个真实分数（词条
    /// 口径），两套刻度并存时旧刻度压死新刻度——实测 W1a 后 `sh` 的
    /// 是(词频 31M，词条口径 −2.39) 撞上 Exact 补全旧行(−13.99)被丢弃，
    /// 首页被 500K 档的 [s][h] 二字词(−4.18)整页占住。改分后同读法同分。
    /// `rescored=false`（重算未发生，如非降级范围）维持原「去重跳过」，
    /// 非 降级世界的分数一个都不动。
    pub(super) fn push_rescored(ranked: &mut Vec<Candidate>, c: Candidate, rescored: bool) {
        match ranked.iter_mut().find(|e| e.word == c.word && e.pinyin == c.pinyin) {
            Some(e) => {
                if rescored {
                    e.score = c.score;
                    e.covered = c.covered;
                    e.freq = c.freq;
                }
            }
            None => ranked.push(c),
        }
    }

    /// 词条口径重算所需的**共享证据**（整词频 + 上文 LM 状态）。
    ///
    /// 按查询算一次而不是按候选算：整串条目边一批可达数百条，每条各构造一次
    /// LM 状态是纯浪费（实测把 `full:weishenme` 的 p95 抬高约 0.9ms）。
    pub(in crate::engine) fn entry_evidence(&self, ctx: &LmContext) -> Result<EntryEvidence, EngineError> {
        Ok(EntryEvidence {
            total: self.dict.total_freq()?,
            state: self.lm.state_from_context(ctx),
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
                matched_len: 0,
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
            !self.segmenter.segment(&pinyin[..n - k]).is_empty()
                && pinyin[n - k..]
                    .bytes()
                    .all(|c| INITIAL_LETTERS.contains(&c))
        })
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
                matched_len: 0,
            });
        }
        Ok(out)
    }

}
