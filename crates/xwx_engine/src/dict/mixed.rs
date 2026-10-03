//! 混合简拼的**探索层**:交错 DFS(每位要么吃字面音节、要么吃一个首字母)
//! 与「末尾半音节补全」的可达性判定。查询入口在 `lookup.rs`。

use super::{DictError, MmapDict, MAX_LOOKUP_RESULTS};

/// 全拼路径枚举的音节切分上限(每种切法都要回主索引扫一轮,限制枚举防爆炸)。
/// 只服务于 [`MmapDict::lookup_mixed_full_pinyin`](改动前的原实现)。
pub(super) const MAX_SEGMENTATIONS: usize = 8;

/// 交错简拼 DFS 单次查询的**索引探测步数**上限(见 [`MmapDict::lookup_mixed`])。
///
/// 口径:一次二分迭代、一次记录读取各记 1 步。DFS 的每一项代价最终都落在
/// 「在主索引上定位或读取一条记录」上,用同一种步数计量才能把「多分支枚举」
/// 与「单次二分」放在同一把尺子上——这是本常量唯一的作用,**不是时间**,
/// 因为时间随机器变、步数不随。
///
/// 取值依据(`tools/spike_abbrev/walk.py` 原型在真实 `data/dict.bin`
/// 186 万 key 上实测的二分次数):
/// - 真实交错输入远低于此:`feicde` 248、`zguo` 570、`nhao` 962、
///   `beijdx` 1493、`shouzm` 1923、`zgren` 2085;
/// - 病态输入才会逼近:`zzzz` 2831、`zhzhzhzh` 6922;
/// - 20_000 步给真实输入留约 10 倍余量,同时把任意病态输入的最坏情况
///   钳成常数(与 `MAX_CHAINS_PER_START` / `SYLLABLE_ENUM_QUOTA` 同一种
///   「硬上限保证最坏情况有界」的思路)。
///
/// 预算耗尽时**保留已经收下的候选**(部分结果优于空结果),不报错。
pub(super) const MAX_MIXED_DFS_STEPS: usize = 20_000;

/// 混合简拼的**探索上界**:收集池到这个条数就停止继续探索。
///
/// 取 [`MAX_LOOKUP_RESULTS`] 的 8 倍(4000),是实测标定的折中:
/// - 早了不行——在 500(最终名额)就停时,截断按**探索顺序**发生,实测
///   `meiy` 把词频最高的「没有」(504685,本该第 1)丢了,列表第 1 名
///   「每一个」只有 500354;
/// - 晚了也不行——完全不设上界时 DFS 会跑满 [`MAX_MIXED_DFS_STEPS`]
///   预算,`zhongg` 单次查询 0.343ms → **4.13ms**(5ms 门槛);
/// - 4000 配上「整 key 跳过」后,`zhongg` 2.8ms、`meiy` 1.3ms,
///   而 26 个抽样输入的候选条数**全部不变**(只有 `meiy`/`zhongg`/`nih`
///   这类「一个音节位下有几百个 key」的输入内容变好)。
///
/// 真正的质量决策仍然只在**[排序之后]**做(按词频取前
/// [`MAX_LOOKUP_RESULTS`] 条),这里只是「看多少」的上界。
pub(super) const MAX_MIXED_COLLECT: usize = MAX_LOOKUP_RESULTS as usize * 8;

/// 途中「停在残缺音节上」的读法最多记这么多条(见 [`MixedDfs::prefix_readings`])。
///
/// 整串覆盖不了时,这些读法是「覆盖输入前缀」的混拼候选来源——用户报的
/// `womenjintqubeij` 只能确认到「我」「门禁」,缺的就是 `wo men jin t*`
/// →「我们今天」(覆盖前 9 个字母)这条读法。每条读法要各收一次池子,
/// 条数直接决定兜底分支的额外成本,故设上限;DFS 是深度优先,深位置的
/// 读法先被记下。
pub(super) const MAX_PREFIX_READINGS: usize = 96;

/// 末尾「半音节」读法要求尾巴至少这么长。
///
/// 取 2 而不是 1:单个字母的尾巴(`n`、`x`、`z`)是**首字母缩写**的职责
/// (副索引 + DFS 的首字母分支已经覆盖),把它也当半音节会让 `n*` 这种
/// 巨大的音节区间被整段扫一遍,而召回上并没有新增(那些词本来就能查到)。
/// 2 个字母(`zho`、`sh`、`ha`)不是任何音节的完整写法时,才是真正需要
/// 「当成长音节的半个」的形态——这正是用户报的 `zho`→中国、`niha`→你好、
/// `beijin`→北京、`meiyo`→没有。
pub(super) const MIN_PARTIAL_TAIL: usize = 2;

/// 输入能否被**完整**切成一串音节(即「整串都是合法全拼」)。
///
/// 用 DP 而不是复用 [`segment_input`]:后者有 `MAX_SEGMENTATIONS` 上限,
/// 前 8 种切法都不完整时会**误判**成「不是全拼」,从而把一批全拼输入
/// 错送进交错 DFS——冻结约束就会漏。这里 `reachable[i]` = 前缀
/// `input[..i]` 可完整切分,与切法条数无关。
///
/// 调用方已保证 `input` 是 ASCII,故按字节索引安全。
pub(super) fn is_full_pinyin(input: &str, table: &[&str]) -> bool {
    let n = input.len();
    let mut reachable = vec![false; n + 1];
    reachable[0] = true;
    for i in 0..n {
        if !reachable[i] {
            continue;
        }
        for s in table {
            if syllable_matches(s, &input.as_bytes()[i..]) {
                reachable[i + s.len()] = true;
            }
        }
    }
    reachable[n]
}

/// 规范音节与输入串在当前位置是否匹配——**全项目唯一的音节匹配口径**
/// ([`is_full_pinyin`]/[`partial_tail_readings`]/[`segment_prefix`]/
/// `segment_input`/`MixedDfs::dfs` 五处都走这里,否则会出现「这条路的
/// 切分认得、那条路的不认得」)。
///
/// 等价写法(üe 的 `lue`/`nue`)由 [`crate::segmenter::canonical_syllable`]
/// 归一,规则只有那一处。两种写法同为 3 字节,调用方按 `syl.len()` 前进即可。
pub(super) fn syllable_matches(syl: &str, rest: &[u8]) -> bool {
    if rest.len() < syl.len() {
        return false;
    }
    let head = &rest[..syl.len()];
    if head == syl.as_bytes() {
        return true;
    }
    std::str::from_utf8(head).is_ok_and(|h| crate::segmenter::canonical_syllable(h) == syl)
}

/// 「完整音节前缀 + 尾巴」的全部读法(T2:末尾半音节)。
///
/// 返回 `(空格分隔的音节前缀, 尾巴字节数)`,按前缀从短到长排列。
/// `p` 遍历所有「`input[..p]` 恰好能切成音节」的位置(含 `p = 0`),
/// 尾巴就是 `input[p..]`——它既可能是半个音节(`zho` → zhong、
/// `ha` → hao),也可能碰巧是完整音节(`jin` → jing),两种都要当半音节试。
///
/// 不做去重:不同 `p` 给出的前缀不同,收集区间也不同。
/// 不持有任何音节时的空分桶(`MixedDfs` 只用 `collect` 的场景,如 T2 循环)。
pub(super) static EMPTY_FIRST: [Vec<&'static str>; 26] = [const { Vec::new() }; 26];


pub(super) fn partial_tail_readings(input: &str, table: &[&str]) -> Vec<(String, usize)> {
    let n = input.len();
    if n == 0 {
        return Vec::new();
    }
    // reach[i] = input[..i] 可完整切成音节(与 is_full_pinyin 同一套 DP)。
    let mut reach = vec![false; n + 1];
    reach[0] = true;
    for i in 0..n {
        if !reach[i] {
            continue;
        }
        for s in table {
            if syllable_matches(s, &input.as_bytes()[i..]) {
                reach[i + s.len()] = true;
            }
        }
    }
    let mut out = Vec::new();
    for p in 0..n {
        if !reach[p] {
            continue;
        }
        let Some(prefix) = segment_prefix(&input[..p], table) else {
            continue;
        };
        // 送给主索引的前缀 = 「完整音节前缀 + 半个音节」本身。
        // `p = 0`(整串都是半音节,如 `zho`/`sh`)时前缀就是尾巴本身,
        // **绝不能传空前缀**——那等于从索引第 0 条开始扫全表。
        let tail = &input[p..];
        let index_prefix = if prefix.is_empty() {
            tail.to_string()
        } else {
            format!("{prefix} {tail}")
        };
        debug_assert!(!index_prefix.is_empty(), "半音节读法不得产出空前缀");
        out.push((index_prefix, n - p));
    }
    out
}

/// 把已知可完整切分的串拼成空格分隔的音节串(回溯:每一步取最长的合法音节)。
pub(super) fn segment_prefix(s: &str, table: &[&str]) -> Option<String> {
    if s.is_empty() {
        return Some(String::new());
    }
    let n = s.len();
    let mut reach = vec![false; n + 1];
    reach[0] = true;
    for i in 0..n {
        if !reach[i] {
            continue;
        }
        for syl in table {
            if syllable_matches(syl, &s.as_bytes()[i..]) {
                reach[i + syl.len()] = true;
            }
        }
    }
    if !reach[n] {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut end = n;
    while end > 0 {
        let mut picked = None;
        for syl in table {
            let len = syl.len();
            if len <= end
                && reach[end - len]
                && syllable_matches(syl, &s.as_bytes()[end - len..])
                && picked.is_none_or(|(_, best): (_, usize)| len > best)
            {
                picked = Some((*syl, len));
            }
        }
        let (syl, len) = picked?;
        parts.push(syl);
        end -= len;
    }
    parts.reverse();
    Some(parts.join(" "))
}

/// 交错简拼 DFS 的状态(见 [`MmapDict::lookup_mixed`])。
///
/// 独立成结构体而不是闭包:递归要在「只读词库」与「可变收集池」之间同时持有
/// 两份借用,闭包写不出这种分离;结构体字段天然分离,也顺带把「步数预算」
/// 这种横切状态从递归参数里拿出来。
pub(super) struct MixedDfs<'a> {
    pub(super) dict: &'a MmapDict,
    /// 输入字节(上游已拒绝非 ASCII)。
    pub(super) input: &'a [u8],
    /// `SYLLABLES` 按长度降序(字面分支贪心最长优先)。
    pub(super) by_first: &'a [Vec<&'static str>; 26],
    /// 非小写字母起点时的回落表(见 `dfs` 的注释)。
    pub(super) by_all: &'a [&'static str],
    /// 收集池:按 (词, 全拼) 去重,保留最高 freq。
    pub(super) out: &'a mut std::collections::HashMap<(String, String), u64>,
    /// 剩余索引探测步数(二分迭代或读记录各 1 步),归零即停。
    pub(super) budget: usize,
    /// 收集池第 [`MAX_LOOKUP_RESULTS`] 名的**下界**(0 = 还没收满)。
    ///
    /// 只作下界用:它是「已收集集合」的第 N 名,后续还会收到更高分的条目,
    /// 真实的第 N 名只会更高——用下界做整 key 跳过**不会误杀**
    /// (见 [`MixedDfs::collect`])。
    pub(super) threshold: u64,
    /// 上次算 `threshold` 时收集池的大小(翻倍才重算,O(log) 次)。
    pub(super) threshold_at: usize,
    /// 途中「停在残缺音节上」的读法:(主索引前缀, 已消耗字节数)。
    ///
    /// `None` = 不记录。整串**覆盖不了**时,这些读法就是「覆盖输入前缀」的
    /// 混拼候选来源(见 [`MmapDict::lookup_mixed`] 的前缀收集);整串被覆盖时
    /// 它们没有用武之地,故由调用方决定要不要付这份记账成本。
    pub(super) prefix_readings: Option<&'a mut Vec<(Vec<u8>, usize)>>,
}

/// 把 `part` 接到空格分隔的前缀 `prefix` 之后(空前缀不加前导空格)。
fn joined(prefix: &[u8], part: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(prefix.len() + part.len() + 1);
    v.extend_from_slice(prefix);
    if !prefix.is_empty() {
        v.push(b' ');
    }
    v.extend_from_slice(part);
    v
}

impl MixedDfs<'_> {
    /// 前缀 `prefix` 区间下界;**每次二分迭代消耗 1 步预算**。
    fn lower_bound(&mut self, prefix: &[u8]) -> Result<usize, DictError> {
        let dict = self.dict;
        let (mut lo, mut hi) = (0usize, dict.pinyin_count);
        while lo < hi {
            if self.budget == 0 {
                // 预算耗尽:返回「空区间」下标,让调用方的 `starts_with`
                // 检查自然失败——已收候选保留,不再产出。
                return Ok(dict.pinyin_count);
            }
            self.budget -= 1;
            let mid = (lo + hi) / 2;
            let rec = dict.record(mid)?;
            let s = dict.slice(dict.strings_off + rec.str_off, rec.str_len)?;
            if s < prefix {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// 读出第 `i` 条记录的 key(读记录 1 步)。返回 owned 值,避开「持有
    /// 词库切片的同时再 `&mut self` 二分」的借用冲突。
    fn key_at(&mut self, i: usize) -> Result<Vec<u8>, DictError> {
        let dict = self.dict;
        self.budget = self.budget.saturating_sub(1);
        let rec = dict.record(i)?;
        Ok(dict
            .slice(dict.strings_off + rec.str_off, rec.str_len)?
            .to_vec())
    }

    /// 枚举 `base` 区间内**本音节位**的所有不同取值(该位音节的剩余字母),
    /// 靠 `'!'` 哨兵二分跳转。见 [`MmapDict::lookup_mixed`] 的哨兵说明。
    fn distinct_tokens(&mut self, base: &[u8]) -> Result<Vec<Vec<u8>>, DictError> {
        let mut toks: Vec<Vec<u8>> = Vec::new();
        let mut i = self.lower_bound(base)?;
        loop {
            if i >= self.dict.pinyin_count || self.budget == 0 {
                return Ok(toks);
            }
            let k = self.key_at(i)?;
            if !k.starts_with(base) {
                return Ok(toks);
            }
            let rest = &k[base.len()..];
            let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
            let t = rest[..end].to_vec();
            let mut jump = base.to_vec();
            jump.extend_from_slice(&t);
            jump.push(b'!');
            toks.push(t);
            i = self.lower_bound(&jump)?;
        }
    }

    /// 收集池超 [`MAX_MIXED_COLLECT`] 时**按词频**裁剪到一半。
    ///
    /// 为什么不早停:早停(收满即止)是按 DFS 探索顺序决定谁留下,会丢高频词
    /// ——实测 `meiy` 撞上 500 条早停时,词频最高的「没有」(504685)竟不在
    /// 列表里,而第 1 名「每一个」只有 500354。裁剪按分数,与探索顺序无关。
    /// 正常情况下 `len < MAX_MIXED_COLLECT`,本函数只做一次整数比较。
    /// 重新估计「收集池第 N 名的下界」——池子翻倍才重算一次。
    ///
    /// 不重算的代价只是少跳过几个 key(下界偏保守),不会误杀;每次重算
    /// 是 O(池子大小) 的选择,所以按 2 倍增长触发,O(log) 次。
    fn refresh_threshold(&mut self) {
        let n = MAX_LOOKUP_RESULTS as usize;
        if self.out.len() < n || self.out.len() < self.threshold_at.saturating_mul(2) {
            return;
        }
        self.threshold_at = self.out.len();
        let mut f: Vec<u64> = self.out.values().copied().collect();
        f.select_nth_unstable_by(n - 1, |a, b| b.cmp(a));
        self.threshold = f[n - 1];
    }

    /// 收下 `prefix` 区间内的词条。`mid_syllable` = 前缀停在残缺音节上
    /// (末位首字母),此时该前缀下的每个 key 都在这一位继续,全部收下;
    /// 否则前缀是完整音节序列,只收与它完全相等的 key。
    pub(super) fn collect(&mut self, prefix: &[u8], mid_syllable: bool) -> Result<(), DictError> {
        let dict = self.dict;
        let mut i = self.lower_bound(prefix)?;
        while i < dict.pinyin_count {
            if self.budget == 0 {
                return Ok(());
            }
            self.refresh_threshold();
            self.budget = self.budget.saturating_sub(1);
            let rec = dict.record(i)?;
            let k = dict.slice(dict.strings_off + rec.str_off, rec.str_len)?;
            if !k.starts_with(prefix) {
                return Ok(());
            }
            // **一字一字母**(2026-10-02):`mid_syllable` 时前缀停在末位声母上,
            // 而字节前缀匹配会把「这个音节之后**还有**音节」的 key 一起扫进来
            // (`wei shen m` 既命中 `wei shen me`,也命中 `wei shen me yao`;
            // `yij` 既命中 `yi jing`,也命中 `yi jing shi`)。后面那些音节
            // **一个字母都没打**,按用户口径(每个字都要有字母)必须排除。
            // 旧判据拿「候选字数 ≤ 输入字母数」把关,`yij`(3 字母)配「已经是」
            // (3 字)正好放行——漏的正是这一类(用户报 `yij` 出现「已经是」)。
            // key 顺序不与音节数单调,故只跳过这一条,不能像 `!mid_syllable`
            // 那样提前收工。
            if mid_syllable && k[prefix.len()..].contains(&b' ') {
                i += 1;
                continue;
            }
            // **整 key 跳过**(分支界限):词块内 freq 降序,若这个 key 的最高
            // 词频都低于「收集池第 N 名的下界」,它一条都进不了最终列表。
            // 没有这一条时,`zhongg` 要把「zhong g*」下几百个 key、8000+ 条
            // 词条全部解析并分配字符串:0.343ms → 5.35ms(破 5ms 门槛)。
            if self.threshold > 0
                && dict.first_entry_freq(dict.entries_off + rec.ent_off, rec.ent_len)? < self.threshold
            {
                i += 1;
                continue;
            }
            // 完整音节前缀:key 按字节序排,与前缀相等的那条最先出现,
            // 之后的只会更长,故可直接收工(与旧实现 `key.len() == pb.len()`
            // 等价)。
            if !mid_syllable && k.len() != prefix.len() {
                return Ok(());
            }
            let py = std::str::from_utf8(k)
                .map_err(|_| DictError::Corrupt("拼音不是合法文本"))?
                .to_owned();
            for (word, freq) in dict.parse_entries(dict.entries_off + rec.ent_off, rec.ent_len)?
            {
                let e = self.out.entry((word, py.clone())).or_insert(0);
                if freq > *e {
                    *e = freq;
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// DFS 主体:在 `pos` 处,`prefix` 是已消耗音节以空格连接的主索引前缀。
    ///
    /// `mid_syllable` = 前缀末尾是残缺音节;`used_literal` = 已消耗过至少一个
    /// **完整音节**(纯首字母串不算混合简拼,见 [`MmapDict::lookup_mixed`])。
    pub(super) fn dfs(
        &mut self,
        pos: usize,
        prefix: &[u8],
        mid_syllable: bool,
        used_literal: bool,
    ) -> Result<(), DictError> {
        // 探索上界:`MAX_MIXED_COLLECT`(8 倍于最终名额)。**不是** 500:
        // 早停在 500 时按探索顺序截断会丢高频词(实测 `meiy` 把词频最高的
        // 「没有」丢了),而完全不设上界会让 DFS 跑满 20_000 步预算
        // (`zhongg` 0.343ms → 4.13ms)。8 倍是一个实测标定的折中。
        if self.budget == 0 || self.out.len() >= MAX_MIXED_COLLECT {
            crate::engine::trace::note(|| {
                format!(
                    "混合 DFS 提前停止: 剩余步数={} 已收集={} 原因={}",
                    self.budget,
                    self.out.len(),
                    if self.budget == 0 {
                        "步数预算耗尽"
                    } else {
                        "收集池满"
                    }
                )
            });
            return Ok(());
        }
        let input = self.input;
        if pos == input.len() {
            // 纯首字母串(`zgr`/`n`)= 副索引的职责,这里不产候选。
            if !used_literal {
                return Ok(());
            }
            return self.collect(prefix, mid_syllable);
        }
        let rest = &input[pos..];

        // (a) 字面音节分支(全拼)。只试与 `rest` 首字节同桶的音节:
        // 首字节不同的音节不可能通过 `syllable_matches`,跳过它们不影响
        // 任何匹配结果,也不改变匹配顺序(桶内仍是长度降序)。
        let bucket: &[&'static str] = match rest.first().and_then(|c| c.checked_sub(b'a')) {
            Some(i) if (i as usize) < 26 => &self.by_first[i as usize],
            // 非小写字母起点:回落到整表(与分桶前完全一致的行为)。
            _ => self.by_all,
        };
        for s in bucket {
            let sb = s.as_bytes();
            if !syllable_matches(s, rest) {
                continue;
            }
            let np = joined(prefix, sb);
            self.dfs(pos + sb.len(), &np, false, true)?;
        }

        // (b) 首字母分支(简拼):1 字符;zh/ch/sh 额外试 2 字符。
        let mut cands: [&[u8]; 2] = [&rest[..1], &rest[..1]];
        let mut ncand = 1usize;
        if rest.len() >= 2 && matches!(&rest[..2], b"zh" | b"ch" | b"sh") {
            cands[1] = &rest[..2];
            ncand = 2;
        }
        for c in cands.into_iter().take(ncand) {
            let base = joined(prefix, c);
            if pos + c.len() == input.len() {
                // 末位首字母:音节残缺,按前缀收更长的词(声母语义)。
                self.dfs(pos + c.len(), &base, true, used_literal)?;
            } else {
                // 中间首字母:把该音节的剩余字母从索引里枚举出来再往下走。
                // 顺手记下「停在这个残缺音节上」的读法(`wo men jin t`):
                // 它覆盖输入的前 `pos + c.len()` 个字母,整串覆盖不了时
                // 就是「我们今天」这类前缀候选的来源(见 `prefix_readings`)。
                //
                // `used_literal` 门槛与`pos == input.len()` 那一支同口径:
                // **纯首字母串**(`zgr`/`n`)是副索引的职责,这条通道一个
                // 候选都不该产(`dict::tests::real_abbrev_mixed_requires_a_full_syllable`
                // 与 `engine::tests::abbrev::engine_without_abbrev_index_degrades_gracefully`
                // 钉的就是这条)。
                if used_literal {
                    if let Some(rec) = self.prefix_readings.as_mut() {
                        if rec.len() < MAX_PREFIX_READINGS {
                            rec.push((base.clone(), pos + c.len()));
                        }
                    }
                }
                for t in self.distinct_tokens(&base)? {
                    let mut full = base.clone();
                    full.extend_from_slice(&t);
                    self.dfs(pos + c.len(), &full, false, used_literal)?;
                }
            }
        }
        Ok(())
    }
}

/// 把输入切成「完整音节段 + 残余声母」,枚举前 `MAX_SEGMENTATIONS` 种切法。
///
/// 在每个位置取**最长**合法音节优先递归;走不通时把剩余全部当声母残余。
/// 这是存储层查询的私有辅助,不改动 `segmenter.rs` 的全拼切分逻辑。
pub(super) fn segment_input(input: &str, table: &[&str]) -> Vec<(Vec<String>, String)> {
    // 预按长度降序,贪心最长匹配
    let mut by_len: Vec<&str> = table.to_vec();
    by_len.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out: Vec<(Vec<String>, String)> = Vec::new();

    fn rec(
        s: &str,
        pos: usize,
        by_len: &[&str],
        syls: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, String)>,
    ) {
        if out.len() >= MAX_SEGMENTATIONS {
            return;
        }
        if pos == s.len() {
            out.push((syls.clone(), String::new()));
            return;
        }
        let mut matched = false;
        for syl in by_len {
            if syllable_matches(syl, &s.as_bytes()[pos..]) {
                syls.push((*syl).to_owned());
                rec(s, pos + syl.len(), by_len, syls, out);
                syls.pop();
                matched = true;
                if out.len() >= MAX_SEGMENTATIONS {
                    return;
                }
            }
        }
        if !matched {
            out.push((syls.clone(), s[pos..].to_owned()));
        }
    }

    rec(input, 0, &by_len, &mut Vec::new(), &mut out);
    out
}
