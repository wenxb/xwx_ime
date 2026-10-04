//! 简拼纠错（阶段 B）：纯缩写串的 1-编辑纠正。
//!
//! 核心判断（见 `docs/correction-research.md` §6.4）：全拼「何时可纠」有
//! 结构信号（切不开 → 容错格），简拼没有——任何辅音字母串都能被别的读法
//! 解释掉，打错一个字母后串仍「合法」。所以简拼纠错的难点不在生成读法，
//! 在**触发时机**；时机错了洪泛与误纠同时爆发。
//!
//! 设计 = 证据闸 + 渐进披露 + 分数闸：
//! 1. **触发闸**（调用方 `lookup.rs`）：纯缩写 + ≥3 字母 + 原串整串候选
//!    全无词条级证据——原串有强读法（`wsm`=为什么、`gshj`=工商局）时
//!    纠错不参与，强读法路径零成本（保守裁决，2026-10-04）；
//! 2. **变体**（本文件 [`generate_variants`]）：1 次编辑 = 换位 / 删字母 /
//!    邻键替换，代价口径与容错解码共用同一张噪声信道表（`segmenter.rs`）；
//! 3. **一级披露**（便宜）：逐变体副索引精确二分——词条级（变体本身是某
//!    词条的首字母串）或链级（真实词条的首字母串是变体的 ≥4 字母真前缀）；
//! 4. **二级披露**（贵）：有证据的变体 ≤2 条才升级**整串简拼解码**
//!    （`build_abbrev_lattice` 同款链路——`wmjtqbj` 型读法不在副索引里，
//!    是链拼出来的），独立预算，不与主解码共享；
//! 5. **呈现**：编辑代价扣进同一对数域 + 整串覆盖 + 分数闸（领先原串最佳
//!    ≥1 个数量级）+ 配额 2；层序照走 Abbrev（真实词条经 `effective_layer`
//!    收编进 Exact 档竞争）。

use super::*;

/// 键盘**同列**邻键的误触概率。MS Pinyin（ACL 2000）的类型学：同行邻键
/// 占替换错误 ~43%、同列 ~15%——按比例从同行档（0.05）折算 ≈0.0174，
/// 取 0.02（对数域代价 1.70，介于同行 1.30 与无关替换 2.0 之间）。
/// 放本文件而不是 `segmenter.rs`：全拼容错的替换证据没有同列档
/// （libime/librime 的 keyboard_map 都只收同行），它是简拼变体生成新开的档。
const TYPO_P_ADJACENT_COL: f64 = 0.02;

/// QWERTY 列对齐（自上而下）：标准错位布局的近似列（`y`-`h`-`n`、
/// `u`-`j`-`m`…）。与 `segmenter.rs` 的 `QWERTY_ROWS`（同行）互补。
const QWERTY_COLS: [&str; 10] = ["qaz", "wsx", "edc", "rfv", "tgb", "yhn", "ujm", "ik", "ol", "p"];

/// 一条 1-编辑变体：`key` = 变体首字母串，`cost` = 这次编辑的噪声信道
/// 代价（log10 域，呈现时扣在每个纠正候选的分上）。
pub(in crate::engine) struct Variant {
    pub(in crate::engine) key: String,
    pub(in crate::engine) cost: f32,
}

/// `a`/`b` 是否是键盘**同列**相邻键（如 `y`↔`h`、`h`↔`n`）。
fn is_qwerty_column_neighbor(a: char, b: char) -> bool {
    for col in QWERTY_COLS {
        if let (Some(i), Some(j)) = (col.find(a), col.find(b)) {
            return i.abs_diff(j) == 1;
        }
    }
    false
}

/// 替换变体的代价，证据从强到弱：混淆对 → 同行邻键 → 同列邻键。
/// 与全拼容错的 `substitution_penalty` 同一张表、同一顺序，只多了同列档。
fn variant_sub_cost(a: char, b: char) -> f32 {
    if let Some(p) = segmenter::confusion_pair_penalty(a, b) {
        return p;
    }
    if segmenter::is_qwerty_neighbor(a, b) {
        return segmenter::typo_penalty_adjacent();
    }
    if is_qwerty_column_neighbor(a, b) {
        return segmenter::penalty(TYPO_P_ADJACENT_COL);
    }
    segmenter::typo_penalty_default_sub()
}

/// 枚举 `input` 的全部 1-编辑变体，生成序即代价优先序：
/// 换位 → 删字母 → 同行替换（每位置左→右）→ 同列替换（每位置上→下）。
/// 去重（同串保留先到者 = 更便宜的编辑类）、排除原串、截到
/// [`MAX_ABBREV_CORRECTION_VARIANTS`]。
///
/// 截断的合理性：`wnjtqbj`（7 字母）的换位 6 + 删 7 = 13 条之后，第 14~17
/// 条正是各位置的同行替换——`n→m`（想打 `wmjtqbj`）排在第 17，上限 20
/// 恰好保住它；更靠后的同列替换只在短输入（≤5 字母）上才轮得到，而短
/// 输入的变体面本来就小（`dhj` 全量 17 条）。
pub(in crate::engine) fn generate_variants(input: &str) -> Vec<Variant> {
    let bytes = input.as_bytes();
    let mut out: Vec<Variant> = Vec::new();
    let push = |v: Variant, out: &mut Vec<Variant>| {
        if v.key == input || out.iter().any(|e| e.key == v.key) {
            return;
        }
        out.push(v);
    };
    // ① 相邻换位（`zrg` ← `zgr`）：单次编辑里最常见的一类。
    for i in 0..bytes.len().saturating_sub(1) {
        let mut v = input.as_bytes().to_vec();
        v.swap(i, i + 1);
        let key = String::from_utf8(v).expect("ASCII 串换位仍是 ASCII");
        push(Variant { key, cost: segmenter::typo_penalty_transpose() }, &mut out);
    }
    // ② 删一个字母（`zgr` ← `zgrm`）：多打。方言型删除（`zh` 丢 `h`）
    // 更便宜，沿用 `deletion_penalty` 的判定。
    for i in 0..bytes.len() {
        let prev = if i > 0 { Some(bytes[i - 1] as char) } else { None };
        let mut v = input.as_bytes().to_vec();
        let deleted = v.remove(i) as char;
        let key = String::from_utf8(v).expect("ASCII 串删字母仍是 ASCII");
        push(Variant { key, cost: segmenter::deletion_penalty(deleted, prev) }, &mut out);
    }
    // ③ 替换：同行邻键（`wnjtqbj` 的 `n→m`）→ 同列邻键（`dhj` 型的 `h→y`）。
    for i in 0..bytes.len() {
        let orig = bytes[i] as char;
        let (left, right) = segmenter::qwerty_row_neighbors(orig);
        let (up, down) = column_neighbors(orig);
        for sub in [left, right, up, down].into_iter().flatten() {
            let mut v = input.as_bytes().to_vec();
            v[i] = sub as u8;
            let key = String::from_utf8(v).expect("ASCII 串替换仍是 ASCII");
            push(Variant { key, cost: variant_sub_cost(orig, sub) }, &mut out);
        }
    }
    out.truncate(MAX_ABBREV_CORRECTION_VARIANTS);
    out
}

/// `c` 在 [`QWERTY_COLS`] 里的上下邻键(`(上, 下)`,列端为 `None`)。
fn column_neighbors(c: char) -> (Option<char>, Option<char>) {
    for col in QWERTY_COLS {
        if let Some(i) = col.find(c) {
            let up = i.checked_sub(1).and_then(|j| col.chars().nth(j));
            let down = col.chars().nth(i + 1);
            return (up, down);
        }
    }
    (None, None)
}

/// 二级披露收集到的一批纠正候选去重（同 词/音 取分高者）、按分排序、
/// 截配额、过分数闸。
fn gate_and_quota(
    mut pool: Vec<Candidate>,
    covering_best: f32,
) -> Vec<Candidate> {
    pool.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.word.cmp(&b.word))
            .then_with(|| a.pinyin.cmp(&b.pinyin))
    });
    pool.dedup_by(|a, b| a.word == b.word && a.pinyin == b.pinyin);
    pool.truncate(ABBREV_CORRECTION_QUOTA);
    // **分数闸**：纠正串最佳要领先原串最佳 ≥1 个数量级。原串读法已知是
    // 弱读法（触发闸③），余量拦的是「垃圾纠正险胜垃圾原串」的边缘情形
    // ——两边都差时不动，宁缺毋滥。
    let passes = pool
        .first()
        .is_some_and(|best| best.score > covering_best + ABBREV_CORRECTION_MARGIN);
    if passes {
        pool
    } else {
        Vec::new()
    }
}

impl ImeEngine {
    /// **阶段 B 的触发闸与合并**（`lookup.rs` 的 needs_abbrev 尾部调用一次）。
    ///
    /// `#[inline(never)]` 是实测出来的硬要求，不是风格偏好：纠错是冷路径
    /// （原串有词条级证据的输入在这里被闸住、一次纠错都不做），但把这段
    /// 代码体留在热函数 `lookup_candidates` 里——即便一次都不执行——也会
    /// 改变热路径的布局/内联，实测 `zgr`/`zhongg` 端到端慢 ~10%
    /// （2.8→3.1ms，bisect:仅当纠错调用可达时出现）。隔开后热函数只见
    /// 一行冷调用。`#[cold]` 让 LLVM 把整段纠错代码放进 .text.unlikely,
    /// 与热路径的布局彻底解耦(实测仅 `inline(never)` 挡不住布局耦合)。
    #[cold]
    #[inline(never)]
    pub(in crate::engine) fn try_abbrev_correction(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        ranked: &mut Vec<Candidate>,
        pure_abbrev: bool,
        ev: Option<&super::lookup::EntryEvidence>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<(), EngineError> {
        // 只对**纯缩写**输入开，且**原串弱**才纠（保守裁决，2026-10-04）：
        // 整串候选里 freq > FILLER 的词条级证据一个都没有——拼装候选
        // （freq=0）与填充档词条（freq≤100）都不算证据，回声更不算。
        // 原串有强读法的输入（`wsm`=为什么、`gshj`=工商局、`dhj`=打火机）
        // 在这里就被挡住，强读法路径零成本。
        if !(pure_abbrev && pinyin.len() >= MIN_ABBREV_CORRECTION_INPUT_LEN && ev.is_some()) {
            return Ok(());
        }
        let covering_best = ranked
            .iter()
            .filter(|c| !c.echo && c.covered >= pinyin.len())
            .map(|c| c.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let original_weak = !ranked
            .iter()
            .any(|c| !c.echo && c.covered >= pinyin.len() && c.freq > FILLER_FREQ_TIER);
        super::trace::note(|| {
            format!(
                "简拼纠错闸: 原串弱={original_weak} 原串整串最佳={covering_best:.2}"
            )
        });
        if !original_weak {
            return Ok(());
        }
        let corrected = self.abbrev_correction_candidates(
            pinyin,
            ctx,
            covering_best,
            ev,
            user_entries,
        )?;
        for c in corrected {
            if ranked
                .iter()
                .any(|e| e.word == c.word && e.pinyin == c.pinyin)
            {
                continue;
            }
            ranked.push(c);
        }
        Ok(())
    }

    /// 简拼纠错主入口：`pinyin` 是已过触发闸①②的原串（干净串），
    /// `covering_best` 是原串整串最佳分（非回声，闸③由调用方判）。
    /// 返回过分数闸的纠正候选（≤[`ABBREV_CORRECTION_QUOTA`] 条），
    /// 已按分排序；调用方按 (词, 音) 去重后推入候选表。
    pub(in crate::engine) fn abbrev_correction_candidates(
        &self,
        pinyin: &str,
        ctx: &LmContext<'_>,
        covering_best: f32,
        ev: Option<&super::lookup::EntryEvidence>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
    ) -> Result<Vec<Candidate>, EngineError> {
        let Some(ev) = ev else {
            return Ok(Vec::new()); // 词条口径重算不可用 ⇒ 纠正候选没有可比分数
        };
        // 证据探针与词条查询都在 mmap 词库上；内存词库（测试态）没有副索引，
        // 优雅退化成「不纠」。
        let DictStore::Mmap(d) = &self.dict else {
            return Ok(Vec::new());
        };
        let variants = generate_variants(pinyin);
        // —— 一级披露：逐变体副索引精确二分（O(log n)），词条级证据优先——
        // (生成序, 变体, 证据词频, 覆盖率, 是否词条级)。链级探针只对「每个
        // 字母都能起音节」的变体做：链解码要整串每个窗口都有读法，死窗口的
        // 变体必然解不出整串（词条级证据与音节起头无关，不受此限）。
        let mut promoted: Vec<(usize, &Variant, u64, f32, bool)> = Vec::new();
        for (i, v) in variants.iter().enumerate() {
            let entry = d
                .abbrev_entry_evidence(&v.key)?
                .filter(|f| *f > FILLER_FREQ_TIER);
            if let Some(f) = entry {
                promoted.push((i, v, f, 1.0, true));
                continue;
            }
            if super::rank::pure_abbrev_input(&v.key, true) {
                if let Some((depth, f)) = d.abbrev_chain_evidence(
                    &v.key,
                    ABBREV_CORRECTION_CHAIN_PREFIX,
                    FILLER_FREQ_TIER,
                )? {
                    // 覆盖率 = 最深真前缀 / 变体长：同频平局集里唯一的次级
                    // 信号（删字母型变体更短 → 覆盖率更高）。
                    let coverage = depth as f32 / v.key.len() as f32;
                    promoted.push((i, v, f, coverage, false));
                }
            }
        }
        // 升级上限**按证据级别分账**：词条级（零建格成本）最多 2 条，链级
        // （每条一次简拼格构建）只解证据最强的 1 条。链级排序键 = 证据词频
        // 降序 → 覆盖率降序（同频平局集里删字母型变体更短、覆盖率更高）→
        // 生成序（生成序即代价优先序）。
        let (mut entries, mut chains): (Vec<_>, Vec<_>) =
            promoted.into_iter().partition(|p| p.4);
        entries.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        entries.truncate(MAX_ABBREV_CORRECTION_ENTRY_PROMOTED);
        chains.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| b.3.total_cmp(&a.3))
                .then_with(|| a.0.cmp(&b.0))
        });
        chains.truncate(MAX_ABBREV_CORRECTION_CHAIN_PROMOTED);
        let promoted: Vec<_> = entries.into_iter().chain(chains).collect();
        // —— 二级披露：有证据的变体才升级整串解码——
        let mut pool: Vec<Candidate> = Vec::new();
        for (_, v, _, _, entry_level) in &promoted {
            if *entry_level {
                self.corrected_entry_candidates(&v.key, v.cost, pinyin, ev, &mut pool)?;
            } else {
                self.corrected_chain_candidates(&v.key, v.cost, pinyin, ctx, user_entries, &mut pool)?;
            }
        }
        super::trace::note(|| {
            let vs: Vec<String> = promoted
                .iter()
                .map(|(_, v, f, cov, e)| {
                    format!("{}({}{:.2}/{:.2})", v.key, if *e { "词条" } else { "链" }, f, cov)
                })
                .collect();
            format!(
                "简拼纠错: 变体 {} 条, 升级 [{}], 原串最佳 {covering_best:.2}",
                variants.len(),
                vs.join(" ")
            )
        });
        Ok(gate_and_quota(pool, covering_best))
    }

    /// **词条级**纠正：变体精确命中副索引词条（`gsjj`=公司简介）。
    /// 不建格——条目边直接由 `lookup_abbrev` 给出（折叠校验与
    /// 「一字一字母」过滤都在里面），省一次简拼格构建与链解码的预算。
    fn corrected_entry_candidates(
        &self,
        variant: &str,
        cost: f32,
        pinyin: &str,
        ev: &super::lookup::EntryEvidence,
        pool: &mut Vec<Candidate>,
    ) -> Result<(), EngineError> {
        let DictStore::Mmap(d) = &self.dict else {
            return Ok(());
        };
        for c in d.lookup_abbrev(variant)? {
            let mut cand = Candidate::new(c.word, c.pinyin, c.freq);
            // 纠正读法解释的是**整个原串**：编辑吸收多打的字母/打错的键。
            cand.covered = pinyin.len();
            cand.spelling = SpellingKind::Abbrev;
            self.fill_entry_evidence(pinyin, &mut cand, Some(ev))?;
            cand.score -= cost;
            pool.push(cand);
        }
        Ok(())
    }

    /// **链级**纠正：变体的整串链读法由简拼格解码给出
    /// （`wnjtqbj`→`wmjtqbj`→我们今天去北京——多词链不在副索引里）。
    /// `demote_scope=false`：不要 `build_abbrev_lattice` 的整串条目边块
    /// （那要跑 2 万步的 mixed DFS；变体若有词条级命中根本不会走到这条支路）。
    fn corrected_chain_candidates(
        &self,
        variant: &str,
        cost: f32,
        pinyin: &str,
        ctx: &LmContext<'_>,
        user_entries: Option<&crate::user_lm::UserLmSnapshot>,
        pool: &mut Vec<Candidate>,
    ) -> Result<(), EngineError> {
        let lattice = super::trace::timed("建纠正格", || {
            self.build_abbrev_lattice(variant, None, user_entries, false)
        })?;
        for mut c in self.best_chain_candidates(
            variant,
            ctx,
            &lattice,
            SpellingKind::Abbrev,
            ABBREV_PER_WORD_PENALTY,
        )? {
            // 变体坐标 → 原串坐标：变体与原串至多差 1 个字母，链读法覆盖
            // 整条变体 = 解释整个原串（删除型变体短 1 字母，多打的字母由
            // 编辑代价吸收）。
            c.covered = pinyin.len();
            c.score -= cost;
            pool.push(c);
        }
        Ok(())
    }
}