//! 查询入口:简拼副索引、交错简拼(混合简拼)、全拼路的统一收口。
//! 三个入口都做「收集池 → 排序 → 截断」,质量决策一律在排序之后。

use super::mixed::{is_full_pinyin, partial_tail_readings, segment_input, MixedDfs, EMPTY_FIRST,
                   MAX_MIXED_COLLECT, MAX_MIXED_DFS_STEPS, MAX_PREFIX_READINGS,
                   MIN_PARTIAL_TAIL};
use super::{Candidate, DictError, KeyPrefix, MmapDict, MAX_LOOKUP_RESULTS};

/// 每条「停在残缺音节上」的前缀读法最多留几个候选。
///
/// 一个读法下的词条动辄几百上千条(`wo men jin t*` 下有 `我们今天`、
/// `我们今天下午`……),而用户要的是「能确认一段」:每个读法留词频最高的
/// 几条就够。注意这是**最终名额**,与 `MAX_MIXED_COLLECT`(收集期的名义
/// 上界)是两个口径。
const PREFIX_READING_QUOTA: usize = 8;

/// 前缀收集一次查询**共享**的索引探测步数。
///
/// 与 `MAX_MIXED_DFS_STEPS` 分开记账:后者是「整串能不能被覆盖」的探索
/// 预算,前者是「覆盖不了时补前缀候选」的预算。它是**共享**的(不是每条
/// 读法各一份),否则 96 条读法 × 20000 步会把兜底分支拖垮。
const MAX_PREFIX_COLLECT_STEPS: usize = 8_000;

/// 折叠读法的音节校验：第 `idx` 个音节必须真的以「声母 + h」开头。
/// 音节数不足同样算不匹配（该读法需要的音节根本不存在）。
/// 折叠点升序，单趟扫描；注意 `split().nth()` 是**相对**前进，不能连用
/// （实测 `zhzhzhzh` 的 4 个折叠点因此全部错位、整池误杀）。
fn syllables_carry_digraphs(pinyin: &str, digraphs: &[(usize, u8)]) -> bool {
    let mut seen = 0usize; // 已扫过的音节数
    for (i, s) in pinyin.split(' ').enumerate() {
        seen = i + 1;
        if let Some(&(_, initial)) = digraphs.iter().find(|&&(idx, _)| idx == i) {
            let b = s.as_bytes();
            if b.first() != Some(&initial) || b.get(1) != Some(&b'h') {
                return false;
            }
        }
    }
    digraphs.last().is_none_or(|&(last, _)| seen > last)
}

impl MmapDict {
    /// 主索引里以 `prefix` 为前缀的 key 存在性（见 [`Dictionary::key_prefix_state`]）。
    ///
    /// 简拼弧的剪枝与省分配：`None` ⇒ 整棵子树是死的（`wo ma …` 不存在），
    /// 当场剪掉；`Prefix` ⇒ 有词条在后面，但本级不是词，**跳过一次
    /// `parse_entries` 分配**；`Exact` ⇒ 去查词条。
    ///
    /// 零分配：一次二分 + 一次切片比较。
    pub fn key_prefix_state(&self, prefix: &str) -> Result<KeyPrefix, DictError> {
        let p = prefix.as_bytes();
        let (mut lo, mut hi) = (0usize, self.pinyin_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = self.record(mid)?;
            let k = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if k < p {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= self.pinyin_count {
            return Ok(KeyPrefix::None);
        }
        let rec = self.record(lo)?;
        let k = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
        Ok(if k == p {
            KeyPrefix::Exact
        } else if k.starts_with(p) {
            KeyPrefix::Prefix
        } else {
            KeyPrefix::None
        })
    }

    /// [`Self::key_prefix_state`] 的**带区间**版本：只在 `[lo, hi)` 里找。
    ///
    /// 简拼 DFS 用它在父链的区间内定位子链：链每延长一个音节，候选区间只会
    /// 缩小，深层节点的二分因此只碰 1~2 个 mmap 页。全局二分每次要跳 ~21 个
    /// 分散的页——实测「建格 19.33ms 却只有 1340 条弧」几乎全是页错误，不在 CPU。
    ///
    /// 返回 `(状态, 子区间下界)`；子区间上界沿用父区间（`[lo2, hi)` 一定是
    /// 「以 `prefix` 开头」的超集，宁可宽也不能窄——宽只是多几步，窄会误剪）。
    pub fn key_prefix_state_in(
        &self,
        prefix: &str,
        lo: usize,
        hi: usize,
    ) -> Result<(KeyPrefix, usize), DictError> {
        let p = prefix.as_bytes();
        let (mut lo, mut hi) = (lo.min(self.pinyin_count), hi.min(self.pinyin_count));
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = self.record(mid)?;
            let k = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
            if k < p {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= self.pinyin_count {
            return Ok((KeyPrefix::None, lo));
        }
        let rec = self.record(lo)?;
        let k = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
        let state = if k == p {
            KeyPrefix::Exact
        } else if k.starts_with(p) {
            KeyPrefix::Prefix
        } else {
            KeyPrefix::None
        };
        Ok((state, lo))
    }

    /// 纯简拼查询:`zgr` → 直接二分副索引,返回带全拼的词条。
    ///
    /// 副索引缺失时返回空(优雅退化,调用方走精确路径)。结果带早停:
    /// 凑够 `MAX_LOOKUP_RESULTS` 即止,不物化全部命中。
    pub fn lookup_abbrev(&self, abbrev: &str) -> Result<Vec<Candidate>, DictError> {
        let Some(idx) = &self.abbrev else {
            return Ok(Vec::new());
        };
        // **双字母声母的两种写法都要查**（见 [`abbrev_variant_reads`]）：副索引按
        // 「每音节一个字母」建 key，`shh`（时候）在表里对应 `sh`，
        // 只查字面串会漏掉它——实测 `shh` 的候选里根本没有「时候」。
        let mut out: Vec<Candidate> = Vec::new();
        for read in super::abbrev::abbrev_variant_reads(abbrev) {
            // **折叠校验（⑤⑩⑥）**：「去 h」变体声称用户把 zh/ch/sh 当整体声母
            // 敲，对应音节必须真的以该声母开头。不放行它，`sh` 会经折叠变体
            // `s` 命中**一切 s 开头的音节**——三(san)/色(se)/死(si) 与 是(shi)
            // 的 key 都是 `s`，`matched_len`（key 长度）分不出二者；只有这里
            // （知道每个候选是哪个变体命中的）能校验，引擎侧放不了。
            for c in idx.lookup_prefix(&read.query)? {
                if !read.digraphs.is_empty()
                    && !syllables_carry_digraphs(&c.pinyin, &read.digraphs)
                {
                    continue;
                }
                if let Some(e) = out
                    .iter_mut()
                    .find(|e| e.word == c.word && e.pinyin == c.pinyin)
                {
                    // 同一词条经不同写法命中：保留词频更高的一条。
                    if c.freq > e.freq {
                        *e = c;
                    }
                } else if out.len() < MAX_LOOKUP_RESULTS as usize {
                    out.push(c);
                }
            }
        }
        // **一字一字母**(2026-10-02):副索引按 key 前缀匹配,`wsm` 会连带命中
        // key 更长的 `wsmy`(为什么要/为什么会……实测 356 条里 276 条如此),
        // 候选比输入字母还多字,按用户口径排除。判据用候选自带的**全拼音节数**
        // 而不是 key 长度:key 里 `zh`/`ch`/`sh` 可能是两个字母一个音节。
        let letters = abbrev.len();
        out.retain(|c| c.pinyin.split(' ').count() <= letters);
        Ok(out)
    }

    /// 混合简拼查询:`zhongguor` → 中国人,`nhao` → 您好。
    ///
    /// 词库主索引的 key 是**带空格的音节序列**(`"zhong guo ren"`),用户输入
    /// 不带空格,所以直接前缀匹配恒为 0。查询分两条路:
    ///
    /// - **输入本身就是合法全拼** → 走 [`Self::lookup_mixed_full_pinyin`],
    ///   也就是**改动前的原实现,逐字节不变**;
    /// - **否则** → 走下面这条**交错 DFS**,把首字母也当作音节来切。
    ///
    /// # 为什么必须分路(冻结约束的落点)
    ///
    /// 项目硬约束是「全拼已经跑通的行为不许退化」。交错 DFS 单独用会**破坏**
    /// 它:因为 `a`/`e`/`o` 是单字母音节,`hao` 也能被解释成
    /// `h(e|ou) + ao`,实测给「hao」多加了「后澳」「河岙」——那不是用户的
    /// 本意,**用户打的就是全拼**。
    ///
    /// 所以这里按「输入能否整体切成年节」分流。这不是「测出来一样」,
    /// 而是**结构性保证**:全拼输入走的是与改动前完全相同的代码路径,
    /// 对拍逐字节相同是必然的,不依赖测量。
    ///
    /// 这条分流也正好是「混合简拼」的定义:混合形式之所以存在,恰恰因为
    /// 有一段**不是**全拼。`nhao`(n 不是音节)、`feicde`、`shouzm`、
    /// `zhongg`、`zhongguor` 都属于这一类,交错 DFS 正是为它们而写。
    ///
    /// # 交错 DFS 的形状
    ///
    /// 每个位置要么消耗一个**字面音节**(全拼),要么消耗**一个首字母**
    /// (`zh`/`ch`/`sh` 额外试 2 字符)作为下一音节的声母,后者把该音节的
    /// 剩余部分**从索引里枚举出来**再往下走。旧实现只有「完整音节前缀 +
    /// **尾部**残余声母」,结构上只支持缩写全在末尾;`nhao`(缩写**在开头**)、
    /// `feicde`(缩写**在中间**)在旧实现下召回为 0。
    ///
    /// **至少要有一个完整音节**(`used_literal`):纯首字母串(`zgr`、`n`)是
    /// [`Self::lookup_abbrev`] 副索引的职责,本函数对它们返回空——这与旧实现
    /// `if syls.is_empty() { continue; }` 的口径**完全一致**,不是新加的限制。
    ///
    /// 收集口径(与旧实现的 `tail` 分支逐条对齐):
    /// - 前缀是**完整音节序列**时只收 `key == 前缀` 的整条词条;
    /// - 前缀停在**残缺音节**上时(末位首字母)收该前缀下的全部词条
    ///   (`zhongg` → `zhong g*` → 中国 / 中国人)。
    ///
    /// 候选的 `pinyin` **直接取索引 key**(完整音节序列),因此 `nhao` 命中的
    /// 「您好」返回的是 `nin hao` 而不是被截断的 `n hao`——**不需要任何回填**。
    ///
    /// 二分用哨兵 `'!'`(0x21)枚举同一音节位的不同取值:它 > 空格、
    /// < 任何小写字母,于是 `lower_bound(前缀 + 已取音节 + '!')` 既跳过该取值
    /// (含 `ta ...` 这类后继),又**保留** `ta`/`tan` 这类更长的同前缀音节
    /// (用 `\xff` 会把后者一并跳掉)。
    ///
    /// **成本**:单次查询的索引探测步数硬上限为 [`MAX_MIXED_DFS_STEPS`]
    /// (20_000 步),预算耗尽即停并保留已收候选——病态输入(`zzzz`、
    /// `zhzhzhzh`)的最坏情况因此是常数;收集池到 [`MAX_MIXED_COLLECT`]
    /// 也停。**注意截断只发生在排序之后**(按词频取前
    /// [`MAX_LOOKUP_RESULTS`] 条),收集期不做任何「谁先到谁留下」的决策
    /// ——那正是 2026-10-02 修掉的 bug(`meiy` 下词频最高的「没有」被
    /// 500 条早停丢掉,详见 [`MmapDict::lookup_mixed`] 的实现注释)。
    ///
    /// 音节表来自编译进二进制的静态表 `crate::segmenter::SYLLABLES`(410 项),
    /// 零扫描;**不依赖副索引存在**——本查询只走主索引,
    /// `MmapDict::open()` 未装配 abbrev.bin 时照常工作。
    pub fn lookup_mixed(&self, input: &str) -> Result<Vec<Candidate>, DictError> {
        let table = self.syllable_table();
        if table.is_empty() || input.is_empty() || !input.is_ascii() {
            return Ok(Vec::new());
        }
        // 按长度降序:字面音节分支贪心最长优先(与旧 segment_input 一致)。
        // **按首字母分桶**(纯性能优化,输出逐字节不变):
        // `syllable_matches` 要求 `rest[..syl.len()] == syl`(或 üe 归一后相等),
        // 首字节不同的音节**永远不可能匹配**,而它现在每个节点都要把整张
        // 音节表(~410 条)试一遍。分桶后每个节点只试同首字母的那十几条,
        // 匹配成功的**顺序**(长度降序)与原来完全一致 ⇒ 收集顺序、
        // `MAX_MIXED_COLLECT` 截断点、结果全都逐字节不变。
        let mut by_len: Vec<&'static str> = table.to_vec();
        by_len.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let by_first: [Vec<&'static str>; 26] = {
            let mut b: [Vec<&'static str>; 26] = std::array::from_fn(|_| Vec::new());
            for s in &by_len {
                if let Some(i) = s.as_bytes().first().and_then(|c| c.checked_sub(b'a')) {
                    if (i as usize) < 26 {
                        b[i as usize].push(s);
                    }
                }
            }
            b
        };

        let mut best: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        // 途中「停在残缺音节上」的读法(覆盖输入前缀的混拼候选来源,
        // 见下面的 (4))。整串被覆盖时用不到,记账成本只是一次 push。
        let mut prefix_readings: Vec<(Vec<u8>, usize)> = Vec::new();

        // (1)(2) 两条既有路:**整串合法全拼** 精确匹配 / 否则交错 DFS。
        // 全拼那条路的候选构造本身逐字节未变(见
        // [`Self::lookup_mixed_full_pinyin`]),这里只是把它的结果搬进池子
        // ——因为末尾半音节补充(T2)对**两者都要生效**:`niha`/`beijin`/
        // `meiyo` 都是「整串合法全拼」,但它们同时是「末尾音节还没打完」的
        // 形态(ha→hao、jin→jing、yo→you)。
        if is_full_pinyin(input, table) {
            for c in self.lookup_mixed_full_pinyin(input)? {
                let e = best.entry((c.word, c.pinyin)).or_insert(0);
                if c.freq > *e {
                    *e = c.freq;
                }
            }
        } else {
            let mut dfs = MixedDfs {
                dict: self,
                input: input.as_bytes(),
                by_first: &by_first,
                by_all: &by_len,
                out: &mut best,
                budget: MAX_MIXED_DFS_STEPS,
                threshold: 0,
                threshold_at: 0,
                prefix_readings: Some(&mut prefix_readings),
            };
            // (0) **可穷尽的约束匹配先跑**（阶段 2A）：`z + tai lan` 这种形态
            // 直接按「缩写音节的每个可能展开 × 尾巴的完整音节键」精确查词。
            // 它必须**先于** DFS：通用 DFS 会把整条步数预算烧在首字母的
            // 各条子树上（阶段 0 实测零收集），而这里的规模只有几十次查键。
            if let Some((k, rest_key)) = super::mixed::initial_plus_literal_tail(input, table) {
                // 预扫用**独立预算**：它是「固定规模、可穷尽」的枚举，不该挤占
                // DFS 的步数预算——否则会把本来能解出的其它形态挤掉（实测会改变
                // `nhao`、`womenjintian...` 这类语料输入的候选）。
                let dfs_budget = dfs.budget;
                let mut key = String::with_capacity(k + 1 + rest_key.len());
                for syl in crate::segmenter::abbrev_syllables(&input[..k]) {
                    key.clear();
                    key.push_str(syl);
                    key.push(' ');
                    key.push_str(&rest_key);
                    // 完整音节序列 ⇒ `collect` 走「只收与它完全相等的 key」那条。
                    dfs.collect(key.as_bytes(), false)?;
                }
                dfs.budget = dfs_budget;
            }
            dfs.dfs(0, &[], false, false)?;
        }

        // (3) **末尾半音节补充召回(T2,2026-10-02)**:把输入切成
        // 「完整音节前缀 + 尾巴」,尾巴当成某个**更长音节的开头**去收集。
        // 两条既有路都覆盖不到这种形态:
        // - `zho`(尾巴不构成音节)旧实现只把首字母当缩写,试的是
        //   `zhong + o`(把尾巴末位当独立音节),整词中国永远不会出现;
        // - `niha`/`beijin`/`meiyo`(尾巴「ha」/「jin」/「yo」碰巧是合法
        //   音节)被判成「整串全拼」走精确匹配,只找 key 恰好等于它的词,
        //   于是 你好(hao)/北京(jing)/没有(you)全被排除在外。
        // 尾巴长度 ≥ [`MIN_PARTIAL_TAIL`] 才启用(理由见常量注释)。
        // 门槛:池子里现有最差的一条(在借用 `best` 之前算好)。
        // 半音节区间动辄几千条(`xian` 的 `xiang*`、`sh` 的 `sh*`),而
        // **比现有最差还差**的补充候选永远进不了最终列表(排序后按词频截断),
        // 不值得扫。精确剪枝:块内 freq 降序,key 的天花板低于门槛 ⇒ 该 key
        // 一条都挤不进来。
        let partial_threshold = best.values().copied().min().unwrap_or(0);
        // 这批候选**单独收池**:它们是「精确前缀补全」读法——输入是候选拼音
        // 的前缀,只是最后一个音节还没打完(`beijin` → `bei jing`)。层序上
        // 与格子里「打字中的补全词」同类,最终标 `Exact`;而 (1)(2) 两条路给
        // 的是**缩写**读法(末位音节只给首字母,`zhongg` → 中国)→ 标 `Abbrev`。
        // 层序不同直接决定「北京/你好/没有」能不能与精确候选同台比分数:
        // 实测 `beijin` 的北京 #35 → #1、`niha` 的你好 #26 → #2、
        // `meiyo` 的没有 #32 → #1。**两池合并会反过来**把 `beijing` 的
        // 「被禁锢」这类低频缩写词抬进第一页,所以不能并成一个 map。
        let mut completions: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        {
            let mut dfs = MixedDfs {
                dict: self,
                input: input.as_bytes(),
                by_first: &EMPTY_FIRST,
                by_all: &[],
                out: &mut completions,
                budget: MAX_MIXED_DFS_STEPS,
                threshold: partial_threshold,
                threshold_at: 0,
                prefix_readings: None,
            };
            // 门槛先设成「池子里现有最差的一条」:半音节区间动辄几千条
            // (`xian` 的 `xiang*`、`sh` 的 `sh*`),而**比现有最差还差**的
            // 补充候选永远进不了最终列表(排序后按词频截断),不值得扫。
            // 这是精确剪枝:块内 freq 降序,key 的天花板低于门槛 ⇒ 该 key
            // 一条都挤不进来。实测端到端 `xian` 7.63ms → 见 bench。
            // `p = 0`(整串都当半个音节)只在**整串不是合法全拼**时启用:
            // 那种形态才是「末尾半截」(`zho` → zhong、`sh` → shi);
            // 整串已经是合法全拼时,`p = 0` 的含义变成「音节前缀补全」
            // (`xian` → `xiang*` 整个区间、几千条),那不是 T2 要的能力,
            // 而代价实测 +4.5ms 端到端。去掉它不影响四个目标形态:
            // 它们都走「最后一个音节边界」的读法(`ni|ha`、`bei|jin`、
            // `mei|yo`)。
            let allow_p0 = !is_full_pinyin(input, table);
            for (index_prefix, tail_len) in partial_tail_readings(input, table) {
                // **单字母尾巴也放行**（`daishix` 的 `x`）：早前把 `MIN_PARTIAL_TAIL`
                // 从 2 改成 1 测出「无变化」，是因为另有一道闸在引擎侧
                // （`needs_abbrev` 的 `!tolerant_explains`）挡着——读法必须先被产出，
                // 才谈得上放行。收窄条件交由引擎侧判定（见
                // `ImeEngine::ends_one_letter_short`）。
                if tail_len < MIN_PARTIAL_TAIL
                    && !(tail_len == 1 && tail_len < input.len())
                {
                    break;
                }
                if dfs.out.len() >= MAX_MIXED_COLLECT {
                    break;
                }
                // 只是**跳过这一条读法**,不是终止整轮——`p = 0` 排在
                // 最前面,后面的「最后一个音节边界」读法才是目标形态。
                if !allow_p0 && index_prefix.as_str() == input {
                    continue;
                }
                dfs.collect(index_prefix.as_bytes(), true)?;
            }
        }

        // (4) **前缀混拼读法(2026-10-03)**:整串覆盖不了时(`best` 为空),
        // 把 DFS 途中「停在残缺音节上」的读法各收一遍池子——它们覆盖的是
        // 输入的**前缀**,不是整串。
        //
        // 用户报的 `womenjintqubeij` 只能一段段确认到「我」「门禁」,剩下的
        // `tqubeij` 收不了尾:缺的就是这条通道(`wo men jin t*` →「我们今天」,
        // 覆盖前 9 个字母;而剩下的 `qubeij` 自己就能出「去北京」)。
        //
        // **只在 `best` 为空时启用**:整串能被覆盖时旧通道已经给出候选,
        // 再加前缀读法会改变那些输入的候选表——那是另一件事,不做。
        let mut prefixes: std::collections::HashMap<(String, String), (u64, usize)> =
            std::collections::HashMap::new();
        if best.is_empty() && !prefix_readings.is_empty() {
            // 长读法优先:它覆盖更多输入,正是用户想确认的那一段。
            prefix_readings.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            prefix_readings.dedup();
            prefix_readings.truncate(MAX_PREFIX_READINGS);
            // 预算**共享**(不是每条读法各一份),见 `MAX_PREFIX_COLLECT_STEPS`。
            let mut budget = MAX_PREFIX_COLLECT_STEPS;
            for (key, covered) in &prefix_readings {
                if budget == 0 {
                    break;
                }
                let mut pool: std::collections::HashMap<(String, String), u64> =
                    std::collections::HashMap::new();
                let mut dfs = MixedDfs {
                    dict: self,
                    input: input.as_bytes(),
                    by_first: &EMPTY_FIRST,
                    by_all: &[],
                    out: &mut pool,
                    budget,
                    threshold: 0,
                    threshold_at: 0,
                    prefix_readings: None,
                };
                // `mid_syllable = true`:读法末尾是残缺音节(`...jin t`),
                // 该前缀下的 key 都在这一位继续;`collect` 的「一字一字母」
                // 判据保证后面不会带出「一个字母都没打」的音节。
                dfs.collect(key, true)?;
                budget = dfs.budget;
                if pool.is_empty() {
                    // 这条读法下没有词,看下一条(更短的)。
                    continue;
                }
                // **只取「有候选的最长那条读法」**:它才是「现在能确认的
                // 最长一段」(用户按空格就该吃掉它)。更短的读法(`wo m` →
                // 我们,只覆盖 3 个字母)会把候选表灌满,还凭简拼层压在有用
                // 的长读法前面——实测 `womenjintqubeij` 第一次空格只吃掉
                // `wom`,剩下的 `enjintqubeij` 照旧收不了尾。
                let mut hits: Vec<((String, String), u64)> = pool.into_iter().collect();
                hits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                for ((word, py), freq) in hits.into_iter().take(PREFIX_READING_QUOTA) {
                    let e = prefixes.entry((word, py)).or_insert((0, *covered));
                    if freq > e.0 {
                        e.0 = freq;
                    }
                    if *covered > e.1 {
                        e.1 = *covered;
                    }
                }
                break;
            }
        }

        // 缩写读法 → `Abbrev`;补全读法 → `Exact`(层序见上面 `completions`
        // 的注释)。同一 (词, 拼音) 被两条路同时命中时**保留缩写那一份**
        // ——更保守:层序更低,不会被这条新通道抬上去。
        let mut out: Vec<Candidate> = Vec::with_capacity(best.len() + completions.len());
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::with_capacity(best.len());
        for ((word, py), freq) in best {
            seen.insert((word.clone(), py.clone()));
            let mut c = Candidate::new(word, py, freq);
            c.spelling = crate::candidate::SpellingKind::Abbrev;
            out.push(c);
        }
        for ((word, py), freq) in completions {
            if seen.contains(&(word.clone(), py.clone())) {
                continue;
            }
            seen.insert((word.clone(), py.clone()));
            out.push(Candidate::new(word, py, freq));
        }

        // 前缀混拼读法:层序同缩写(`Abbrev`——末位音节只给了首字母),
        // 但**覆盖范围是真实前缀**(`covered > 0`),于是分段上屏确认它时
        // 只吃掉输入的前一段,剩下的串继续出候选。
        for ((word, py), (freq, covered)) in prefixes {
            if seen.contains(&(word.clone(), py.clone())) {
                continue;
            }
            let mut c = Candidate::new(word, py, freq);
            c.spelling = crate::candidate::SpellingKind::Abbrev;
            c.covered = covered;
            out.push(c);
        }
        // **确定性排序**:同频词必须按 (词, 全拼) 定序,不能依赖 `HashMap`
        // 的迭代顺序——那是每个进程随机化的,实测同一二进制两次运行,
        // `sh` 第 28 名「谁」的拼音在 `shei`/`shui` 之间跳、`zh` 的简拼层
        // 整段尾序都不同。对拍工具与「同一个词不该在两次重启间换位置」
        // 都要求这里是全序。
        out.sort_by(|a, b| {
            b.freq
                .cmp(&a.freq)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.pinyin.cmp(&b.pinyin))
        });
        // **按词频截断**(而不是收集期先到先得):上限仍是 MAX_LOOKUP_RESULTS,
        // 但留下的必须是分最高的那批。实测 `meiy`:旧口径 500 条里没有
        // 「没有」(词频 504685,应为第 1),因为它的枚举分支排在靠后位置,
        // 名额先被「每一个」(500354)等占满了。
        out.truncate(MAX_LOOKUP_RESULTS as usize);
        Ok(out)
    }

    /// 混合简拼的**全拼**路径:**改动前的原实现,逐字节不动**。
    ///
    /// 只在 [`is_full_pinyin`] 为真时调用(见 [`Self::lookup_mixed`])。
    /// 刻意保留原实现而不是「让 DFS 只走字面分支」:后者只是*看起来*等价,
    /// 而冻结约束要的是**同一条代码路径**给出的硬保证。
    fn lookup_mixed_full_pinyin(&self, input: &str) -> Result<Vec<Candidate>, DictError> {
        let table = self.syllable_table();
        if table.is_empty() || input.is_empty() {
            return Ok(Vec::new());
        }
        let segs = segment_input(input, table);
        // 按 (词, 全拼) 去重,保留最高 freq;早停在收集阶段生效。
        let mut best: std::collections::HashMap<(String, String), u64> =
            std::collections::HashMap::new();
        'outer: for (syls, tail) in &segs {
            if syls.is_empty() {
                continue;
            }
            let prefix = syls.join(" ");
            let pb = prefix.as_bytes();
            // **纯性能优化(输出逐字节不变)**:
            //
            // ① `tail` 为空(整串就是这条切分)时,下面那个前缀区间扫描里
            //    唯一会被收下的 key 就是**恰好等于 prefix** 的那一条——更长的
            //    key 全部被 `if tail.is_empty() { continue; }` 丢掉。直接二分
            //    取那一条即可,不必把「jin t*」整段扫一遍。
            // ② `tail` 非空时,收下的 key 必须形如 `prefix + ' ' + <后续音节>`,
            //    且后续音节的**首字节等于 tail 首字节**(第 908-913 行的判据)。
            //    于是扫描可以直接从 `prefix + ' ' + tail[0]` 处起,跳过前面
            //    成千上万条首字母不对的 key(它们原本也只会 `continue`)。
            if tail.is_empty() {
                let i = self.lower_bound(pb)?;
                if i < self.pinyin_count {
                    let rec = self.record(i)?;
                    let key = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
                    if key == pb {
                        let py = std::str::from_utf8(key)
                            .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                            .to_owned();
                        for (word, freq) in
                            self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)?
                        {
                            let e = best.entry((word, py.clone())).or_insert(0);
                            if freq > *e {
                                *e = freq;
                            }
                            if best.len() >= MAX_LOOKUP_RESULTS as usize {
                                break 'outer;
                            }
                        }
                    }
                }
                continue;
            }
            // 收下的 key 必须形如 `prefix + ' ' + <后续音节>` 且首字节等于
            // tail 首字节,所以**扫描区间就是 `prefix + ' ' + tail[0]` 那一小段**
            // (keys 有序 ⇒ 同前缀的 key 连续)。上限用它做 break 条件,而不是
            // 用 `pb`:后者会让循环把「ji o*」「ji x*」这些整段扫完再退——
            // 它们每一条都只会在下面的判据里被丢掉。被跳过的 key 原样都是
            // 「不满足判据」的,故输出逐字节不变。
            let mut narrow = prefix.clone();
            narrow.push(' ');
            narrow.push(tail.as_bytes()[0] as char);
            let nb = narrow.as_bytes();
            let start = self.lower_bound(nb)?;
            for i in start..self.pinyin_count {
                let rec = self.record(i)?;
                let key = self.slice(self.strings_off + rec.str_off, rec.str_len)?;
                if !key.starts_with(nb) {
                    break;
                }
                // 注意:`rest` 仍从 `pb + ' '` 之后起算(与改动前一致),
                // `nb` 只用于**缩小扫描区间**,不参与下面的判据——否则
                // 判据里 `rest_syls[0][0] == tb[0]` 会拿被吃掉首字母的
                // 音节的第二个字母去比(实测 `ren` 的 `re n*` 一读少 94 条)。
                let rest = &key[pb.len() + 1..];
                let rest_syls: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                let tb = tail.as_bytes();
                // 残余声母须逐位等于后续音节首字母,且长度不超过剩余音节数
                if tb.len() <= rest_syls.len()
                    && rest_syls
                        .iter()
                        .zip(tb.iter())
                        .all(|(syl, &c)| !syl.is_empty() && syl[0] == c)
                {
                    let py = std::str::from_utf8(key)
                        .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                        .to_owned();
                    for (word, freq) in
                        self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)?
                    {
                        let e = best.entry((word, py.clone())).or_insert(0);
                        if freq > *e {
                            *e = freq;
                        }
                        if best.len() >= MAX_LOOKUP_RESULTS as usize {
                            break 'outer;
                        }
                    }
                }
            }
        }
        let mut out: Vec<Candidate> = best
            .into_iter()
            .map(|((word, py), freq)| Candidate::new(word, py, freq))
            .collect();
        // **确定性排序**:同频词必须按 (词, 全拼) 定序,不能依赖 `HashMap`
        // 的迭代顺序——那是每个进程随机化的,实测同一二进制两次运行,
        // `sh` 第 28 名「谁」的拼音在 `shei`/`shui` 之间跳、`zh` 的简拼层
        // 整段尾序都不同。对拍工具与「同一个词不该在两次重启间换位置」
        // 都要求这里是全序。
        out.sort_by(|a, b| {
            b.freq
                .cmp(&a.freq)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.pinyin.cmp(&b.pinyin))
        });
        Ok(out)
    }

}
