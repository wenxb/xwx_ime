//! 查询入口:简拼副索引、交错简拼(混合简拼)、全拼路的统一收口。
//! 三个入口都做「收集池 → 排序 → 截断」,质量决策一律在排序之后。

use super::mixed::{is_full_pinyin, partial_tail_readings, segment_input, MixedDfs, EMPTY_FIRST,
                   MAX_MIXED_COLLECT, MAX_MIXED_DFS_STEPS, MIN_PARTIAL_TAIL};
use super::{Candidate, DictError, MmapDict, MAX_LOOKUP_RESULTS};

impl MmapDict {
    /// 纯简拼查询:`zgr` → 直接二分副索引,返回带全拼的词条。
    ///
    /// 副索引缺失时返回空(优雅退化,调用方走精确路径)。结果带早停:
    /// 凑够 `MAX_LOOKUP_RESULTS` 即止,不物化全部命中。
    pub fn lookup_abbrev(&self, abbrev: &str) -> Result<Vec<Candidate>, DictError> {
        let Some(idx) = &self.abbrev else {
            return Ok(Vec::new());
        };
        let mut out = idx.lookup_prefix(abbrev)?;
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
            };
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
                if tail_len < MIN_PARTIAL_TAIL || dfs.out.len() >= MAX_MIXED_COLLECT {
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
            out.push(Candidate::new(word, py, freq));
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
