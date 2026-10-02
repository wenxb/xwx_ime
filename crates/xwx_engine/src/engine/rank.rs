//! 排序:候选定序键(`SortKey`/`sort_key`)、覆盖长度映射、未解释字母计数、
//! 以及副索引候选并入。

use super::*;

pub(super) fn ordered_candidates<K>(
    map: std::collections::HashMap<K, Candidate>,
) -> Vec<Candidate> {
    let mut v: Vec<Candidate> = map.into_values().collect();
    v.sort_by(|a, b| a.word.cmp(&b.word).then_with(|| a.pinyin.cmp(&b.pinyin)));
    v
}

pub(super) fn sort_candidates(inp: &str, ranked: &mut [Candidate]) {
    // 排序键 = (u_min 桶, 拼写来源层序, 分数 − 覆盖罚分, 频次降序, ...):
    //
    // **首键「u_min 桶」**:候选终点位置 `pos = cp.len()` 处的「不可
    // 避免未解释字母数」(见 [`unreachable_tail`])——0 ⇔ 输入从
    // pos 起存在一条精确的音节路径到串尾;>0 ⇔ pos 落在一个不可精确
    // 切分的段内(如 `beijing` 的段内切分 `bei jin [g]`、`jinan` 的
    // `jin [an]` 与段内 `ji nan` 起点)。
    //
    // 这个首键修正的是上一轮「裸未解释字母数硬首键」的设计缺陷:裸 u
    // 把「候选拼音长度 == 输入长度」(u=0)的生僻组合词(beijing→
    // 被禁锢/悲寂/背脊,diannao→狄安娜,jinan→进啊)全部顶上第一页,
    // 把高频单字(被/北/点/电/中/种)挤出前 10。这些组合词的拼音
    // **不是输入的合法音节前缀**——共同点是终点落在不可精确切分的
    // 段内(u_min>0);而高频单字(`被`bei、`点`dian)终点在音节
    // 边界上(u_min=0)。u_min 桶把两者分开:段内词一律排在段界词
    // 之后,与词频无关——这是对裸 u 的结构性修正,不是数值调参。
    //
    // 混合简拼目标词(简拼层)构造上覆盖整串、恒 0 桶,与覆盖整串的
    // 精确词、打字中的补全词(`zhon` 时的 `中` cp=zhong 比输入长,
    // u=0)同桶;同桶内层序(第二键)保持「精确 → 模糊 → 简拼」的
    // 结构性优先(防 `xx` 类上千条简拼候选冲垮全拼第一页),简拼
    // 目标词进第一页的通道是配额(`ABBREV_QUOTA`,保留),不是排序。
    //
    // **第三键「分数 − 覆盖罚分」**:桶内把「未解释字母数」折算成
    // `COVERAGE_PENALTY × u_local` 的分数惩罚(u_local = u − u_min,
    // 可免的那部分未解释),与词频、LM 分在同一量纲竞争——系数经
    // 实测标定(见常量注释),让 `zhongguo` 的「中/种」压过段内词,
    // 但不再出现裸 u 硬键的「生僻组合词淹没高频单字」。
    // `inp` 是排序时的查询串:可能含 B2 的 `'`(会话层把原始输入串直接
    // 传进来)。排序首键 u_min 桶要与格构建/beam/后向 DP 用**同一份**
    // 「不许跨 `'`」的可达性,故在这里把 `'` 归一化成边界表(与
    // `lookup_candidates` 开头的归一化同式);无 `'` 时 `bounds` 为
    // `None`,`unreachable_tail` 走原有零开销路径。
    let (inp_clean, bounds_vec): (std::borrow::Cow<'_, str>, Vec<usize>) =
        if inp.contains('\'') {
            let mut clean = String::with_capacity(inp.len());
            let mut bounds = Vec::new();
            for ch in inp.chars() {
                if ch == '\'' {
                    bounds.push(clean.len());
                } else {
                    clean.push(ch);
                }
            }
            (std::borrow::Cow::Owned(clean), bounds)
        } else {
            (std::borrow::Cow::Borrowed(inp), Vec::new())
        };
    let inp: &str = &inp_clean;
    let bounds: Option<&[usize]> = if bounds_vec.is_empty() {
        None
    } else {
        Some(&bounds_vec)
    };
    let umin = unreachable_tail(inp, bounds);
    // 预计算每个候选的数值键:`sort_by` 每次比较调两次 `sort_key`,
    // 而 `sort_key` 里的 `unexplained_letters` / `cp_len` 都要对
    // `c.pinyin` 做一次 chars 过滤(O(拼音长));500 条候选的简拼层
    // 排序 ~4500 次比较 × 2 键 × 2 次过滤,release 下实测 zhongg
    // 从 0.99ms 涨到 1.23ms。预算成 (bucket, 罚后分) 的 PlainKey
    // 后比较是 O(1),延迟回到 0.99ms 量级。
    // 键预算 + 克隆重排:`sort_by` 每次比较调两次 `sort_key`,而
    // `sort_key` 里的 `unexplained_letters` / `cp_len` 都要对
    // `c.pinyin` 做一次 chars 过滤;500 条候选的简拼层排序
    // ~4500 次比较 × 2 键 × 2 次过滤,release 下实测 zhongg 从
    // 0.99ms 涨到 1.23ms。预算成 PlainKey 后比较是 O(1),延迟
    // 回到 1.10ms(克隆 500 条候选的代价比反复重算键小)。
    let mut keyed: Vec<(PlainKey, Candidate)> = ranked
        .iter()
        .map(|c| (plain_key(inp, &umin, c), c.clone()))
        .collect();
    keyed.sort_by(|(ka, a), (kb, b)| {
        ka.cmp(kb).then_with(|| {
            // 数值键并列时的确定性收尾,与旧 `sort_key` 尾部字段一致
            b.freq
                .cmp(&a.freq)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.pinyin.cmp(&b.pinyin))
        })
    });
    ranked.clone_from_slice(&keyed.into_iter().map(|(_, c)| c).collect::<Vec<_>>());
}

/// `sort_key` 的数值前缀(bucket + 罚后分),供 `sort_candidates`
/// 预计算——语义与 `sort_key` 的前三键完全一致,只是抽成可 Clone
/// 的独立值,排序时不再反复重算。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PlainKey {
    /// 首键:u_min 桶(见 [`sort_candidates`])。
    bucket: usize,
    /// 第二键:拼写来源层序(Exact < Fuzzy < Abbrev)。
    spelling: SpellingKind,
    /// 第三键:分数 − 覆盖罚分,降序。
    score: ScoreDesc,
}

/// 从候选算出数值排序键(与 `sort_key` 的前三键同式)。
pub(super) fn plain_key(inp: &str, umin: &[usize], c: &Candidate) -> PlainKey {
    let k = sort_key(inp, umin, c);
    PlainKey {
        bucket: k.bucket,
        spelling: k.spelling,
        score: k.score,
    }
}

/// 覆盖罚分系数(log10 量纲):「愿意为多解释一个可免字母付出多少个
/// 数量级的词频劣势」。`log10(freq/total)` 每数量级差 1.0,故系数
/// 0.2 表示「少解释 5 个可免字母 ≈ 词频低 1 个数量级」。
///
/// 标定(sweep 实测,真实词库 + 万象 gram,逐系数表见交付报告):
/// 系数只管 0 桶内「全拼整串词」与「高 u_local 单字」之间的竞争
/// (u_min>0 桶的候选 u_local=0 不吃罚,段内字靠桶序被压,与系数
/// 无关)。窗口 (0, ~0.25],取 0.2——
/// - 上限:`diannao` 的狄安娜(u_local=1,与「店」原始分差 2.78
///   log)在系数 0.3 时爬进前 10,0.25 及以内不在;`zhongguo` 的
///   钟鼓在 1.0 时才进前 10,余量更大。瓶颈是 diannao;
/// - 下限:混合简拼的段内碎片字(renm 的「人」、zhongguor 的「中」)
///   u_local=0 恒不吃罚,目标词靠 u_min 桶保护,与系数无关——
///   系数即使取 0,B 组也不退化(实测 0.1 时 B 组名次不变);
/// - 0.2 离翻转点 0.25 有 20% 余量,且 `woshizhongguoren` 类长串
///   的前缀词(「我始终」u_local≈17)吃 3.4 log 罚分正常沉底——
///   那本来就是它们该在的位置。
pub(super) const COVERAGE_PENALTY: f32 = 0.2;

/// 候选的完整排序键,供 [`sort_candidates`] 与 [`merge_with_abbrev`]
/// 的配额位置换算共用(两处用同一个函数,键定义不可能漂移)。
/// 各字段含义见 `sort_candidates` 函数内注释。分数字段包成
/// [`ScoreDesc`] 表达「降序」语义(高分排前),与旧实现的
/// `b.score.partial_cmp(&a.score)` 等价;引擎分数域不会出现 NaN
/// (打分是 log10(正数) 的加权和),NaN 在 [`ScoreDesc`] 里钳到最末,
/// 与旧实现 `unwrap_or(Ordering::Equal)` 的兜底最接近。
pub(super) fn sort_key<'a>(inp: &str, umin: &[usize], c: &'a Candidate) -> SortKey<'a> {
    let u = unexplained_letters(inp, c);
    // u_min(终点处的不可避免未解释数):补全词(cp 比输入长)必以
    // 输入开头(u=0),桶恒 0;简拼层构造上覆盖整串,恒 0;只覆盖
    // 前缀的候选查表。
    let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
    let u_min = if c.spelling == SpellingKind::Abbrev || cp_len > inp.len() {
        0
    } else {
        umin[cp_len]
    };
    let u_local = u.saturating_sub(u_min) as f32;
    SortKey {
        bucket: u_min,
        spelling: c.spelling,
        score: ScoreDesc(c.score - COVERAGE_PENALTY * u_local),
        freq: std::cmp::Reverse(c.freq),
        word: c.word.as_str(),
        pinyin: c.pinyin.as_str(),
    }
}

/// [`sort_key`] 的返回类型:具名字段的全序键(派生 `Ord` 即按声明
/// 序的字典序,字段声明顺序就是键优先级)。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SortKey<'a> {
    /// 首键:候选终点处的不可避免未解释字母数(u_min 桶,越小越靠前)。
    bucket: usize,
    /// 第二键:拼写来源层序(Exact < Fuzzy < Abbrev,枚举声明序)。
    spelling: SpellingKind,
    /// 第三键:分数 − 覆盖罚分,降序(高分在前,见 [`ScoreDesc`])。
    score: ScoreDesc,
    /// 第四键:同分时原始词频降序(回退基准)。
    freq: std::cmp::Reverse<u64>,
    /// 第五、六键:词与拼音的字典序(确定性收尾)。
    word: &'a str,
    pinyin: &'a str,
}

/// `umin[pos]` = 输入从 `pos` 起的**不可避免未解释字母数**:
/// 0 ⇔ 存在一条从 pos 到串尾的精确音节路径(只查音节表,不查词典);
/// >0 ⇔ pos 落在某个不可精确切分段内,取到「最近一个可达点」的距离。
///
/// 后向一次 DP 算出可达性,再正向把不可达位置映射到距离;返回表长
/// `inp.len() + 1`,索引即字节位置(调用方已保证 `inp` 为 ASCII)。
pub(super) fn unreachable_tail(inp: &str, bounds: Option<&[usize]>) -> Vec<usize> {
    let n = inp.len();
    let mut reach = vec![false; n + 1];
    reach[n] = true;
    // 直接查编译期静态音节表(`SYLLABLES` 已按字典序排列,binary_search
    // O(log 412));不要在这里 `Segmenter::new()`——它每次要把 412 个
    // 音节 collect 进 HashSet,而本函数每次 `sort_candidates` 调用都
    // 跑一次(一次查询几十次),实测会把击键延迟放大 7~50 倍
    // (lookup_bench:womenjintianquxian 1.5ms→10.2ms)。
    //
    // `bounds` 是 B2 的 `'` 强制切分点(干净串字节偏移):音节链不得跨越
    // 它(与 `build_lattice` 同一语义)。长度为 l 的音节 [i, i+l) 若跨过
    // 某个边界(边界严格落在 (i, i+l) 内部)就跳过该链——`xi'an`(干净串
    // `xian`,边界 2)的 `xian` 单音节链由此不可达,[xi, an] 才是唯一的
    // 整串切法。`bounds` 为 `None`(无 `'` 输入)时此分支一次都不进,与
    // 改动前逐字节一致(判退化的硬约束)。
    for i in (0..n).rev() {
        let max_len = (n - i).min(segmenter::MAX_SYLLABLE_LEN);
        reach[i] = (1..=max_len).any(|l| {
            if !reach[i + l] {
                return false;
            }
            if let Some(b) = bounds {
                // 跨过 ⇔ 存在边界严格落在 (i, i+l) 内部。
                if b.iter().any(|&x| i < x && x < i + l) {
                    return false;
                }
            }
            SYLLABLES.binary_search(&&inp[i..i + l]).is_ok()
        });
    }
    (0..=n)
        .map(|pos| {
            if reach[pos] {
                0
            } else {
                let mut d = 1;
                while pos + d <= n && !reach[pos + d] {
                    d += 1;
                }
                d.min(n - pos)
            }
        })
        .collect()
}

/// `f32` 分数的「降序 + 全序」包装:高分排前;NaN 钳到最末
/// (引擎分数域不会出现 NaN,这只是把 `partial_cmp` 补齐为 `Ord`
/// 的防御,语义同旧实现的 `unwrap_or(Equal)` 兜底)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ScoreDesc(f32);
impl Eq for ScoreDesc {}
impl PartialOrd for ScoreDesc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ScoreDesc {
    fn cmp(&self, other: &Self) -> Ordering {
        // 降序:自右向左比较(other vs self)。
        other
            .0
            .partial_cmp(&self.0)
            .unwrap_or_else(|| match (other.0.is_nan(), self.0.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater, // 对方 NaN ⇒ 对方排更后 ⇒ self 更小
                (false, true) => Ordering::Less,    // self NaN ⇒ self 排更后
                (false, false) => unreachable!("partial_cmp 只对 NaN 返回 None"),
            })
    }
}

/// 候选**未解释的输入字母数**(覆盖度度量;**不再是排序首键**——
/// 首键已改为 [`unreachable_tail`] 的 u_min 桶,本函数的返回值只在
/// 罚分项 `u_local = u − u_min` 里参与打分,并供测试引用)。
///
/// 语义保留不变(任务书硬约束),变的只是用法:从「排序键」改成
/// 「罚分因子」。
///
/// 设 `cp` = 候选拼音去掉空格后的音节串:
/// - `cp` 以 `inp` 开头(含相等):候选是整个输入的**补全**(如 `zhon`
///   时的 `zhong` 系字)——没解释的不是「没覆盖」而是「输入还没打完」,
///   计 **0**,保持联想靠前;
/// - `inp` 以 `cp` 开头(真前缀):候选**只覆盖前缀**(如 `zhongguor`
///   时的 `zhong` 系字),计 `inp.len() - cp.len()`(漏掉多少字母);
/// - 都不是前缀关系(兜底路径的异常键、个别模糊变体):解释数按
///   保守下界 0 计(与补全同档)。理由:够不到「候选真前缀于输入」
///   这条下界时,无法证明候选漏掉了输入的任何字母,零罚最稳;
///   且这类候选在正常路径不出现(`lattice[0]` 的边必然前缀匹配),
///   在兜底路径按分数竞争时本就在补全词之后,极少浮到前排。
/// - 简拼层候选构造上覆盖整串(`lookup_abbrev` / `lookup_mixed` 的
///   key 由整串切出),恒为 **0**——不走拼音比对,避免「简拼词的
///   完整拼音与残余声母输入天然不是前缀关系」被误伤(如 `zhongguor`
///   的「中国人」`cp` = zhongguoren,与 inp 非前缀关系)。
///
/// 字节序比较合法:`inp` 与 `cp` 都是 ASCII 拼音串(非 ASCII 输入在
/// `build_lattice` / `fallback_candidates` 已被挡掉),前缀关系按字节
/// 判定即按字母判定。
pub(super) fn unexplained_letters(inp: &str, c: &Candidate) -> usize {
    if c.spelling == SpellingKind::Abbrev {
        return 0; // 简拼层构造上覆盖整串
    }
    let cp: String = c.pinyin.chars().filter(|ch| *ch != ' ').collect();
    if cp.starts_with(inp) {
        0 // 补全:候选拼音比输入更长(或相等),输入还没打完
    } else if cp.len() < inp.len() && inp.starts_with(cp.as_str()) {
        inp.len() - cp.len() // 只覆盖前缀:漏掉 inp[cp.len()..]
    } else {
        0 // 非前缀关系:解释数取保守下界 0(见函数级注释)
    }
}

/// 合并简拼层并按 [`MAX_CANDIDATES`] 收尾,给简拼层留
/// [`ABBREV_QUOTA`] 个保底名额。
///
/// 结构不变(配额语义逐字保留):`ranked`(head,非简拼)与 `abbrev`
/// 仍各自排序、各自截取、再按序拼接——截取边界是「head 允许的长度」,
/// 与新排序键无关,因此配额算法不需要知道简拼候选在全局排第几。
/// 变化只在拼接之后:对拼接结果**整体重排一次**
/// [`sort_candidates`](排序首键是 u_min 桶:简拼层恒 0 桶,与同桶的
/// 精确补全/全覆盖候选按层序、罚后分混排——「中国人」(0 桶) 由此
/// 能排在只覆盖 `zhong` 的单字(段内,u_min>0 桶)之前,这正是本次
/// 要修的行为;排序是稳定全序,重排不打乱两层内部各自的相对次序)。
/// 设简拼层 `n` 条:
/// - 总条数 ≤ `MAX_CANDIDATES` 时两层原样拼接,不截断;
/// - 总条数超限时,简拼层取 `max(min(ABBREV_QUOTA, n), 100 − head 条数)`
///   条——保底 ABBREV_QUOTA 条,但不封顶:head 很短时简拼层照旧
///   补满 100(head ≤ 100 时与旧「合并截 100」逐位一致,全拼无退化);
/// - head 截到 `100 − 简拼层实取条数`(`n = 0` 时照旧用满 100,
///   不白留空位)。
///
/// 配额只影响「谁被截断」,不影响排序。返回值 = (合并后总条数,
/// 截断并重排后的列表);`abbrev` 必须未经截断,总条数才真实
/// (供断言「简拼层装满时被配额截断」——截断后的前 5 条分不出
/// 「恰好 5 条」与「被截到 5 条」)。
pub(super) fn merge_with_abbrev(
    inp: &str,
    mut ranked: Vec<Candidate>,
    abbrev: Vec<Candidate>,
) -> (usize, Vec<Candidate>) {
    let abbrev_total = abbrev.len();
    ranked.extend(abbrev);
    let total = ranked.len();
    // 简拼层在「排序前」的 ranked 中是尾部 abbrev_total 条,先切出来
    // 各自排序截断——配额语义只在「分层截取」上有边界可依附。
    let mut head = ranked;
    let mut tail = head.split_off(total - abbrev_total);
    sort_candidates(inp, &mut head);
    sort_candidates(inp, &mut tail);
    if total > MAX_CANDIDATES {
        // 只有总条数超上限才需要截断。配额是「保底名额」而非「简拼层
        // 封顶」:简拼层取「剩余空位」与配额中较大的那个——
        // - head 装满(≥100−配额)时:空位 ≈ 0,配额生效,简拼层保底
        //   ABBREV_QUOTA 条(本任务要救的场景:zhongguor 等);
        // - head 很短(如 ren 只有 21 条)时:空位有 79 条,简拼层照旧
        //   填满到 100——与旧「合并排序截 100」行为逐位一致,不退化
        //   (head ≤ 100 时旧行为等价于「先取 head 全量、再用简拼层
        //   补满 100」,与这里逐位相同)。
        let tail_keep = ABBREV_QUOTA
            .min(tail.len())
            .max(MAX_CANDIDATES.saturating_sub(head.len()));
        tail.truncate(tail_keep);
        head.truncate(MAX_CANDIDATES - tail_keep);
    }
    head.extend(tail);
    // 拼接后整体重排:让排序首键(u_min 桶)跨层生效(见函数级
    // 注释)。sort_by 是稳定排序,重排不改变同键候选的相对次序。
    sort_candidates(inp, &mut head);
    (total, head)
}

/// 把一批路径节点剪到前 `k` 条（按分降序，`k = 0` 即清空）。
///
/// 路径由 arena 里的节点构成（DAG 上「词序列 → 节点」一一对应），
/// 同一个桶内不可能出现重复词序列，因此只排序截断、不去重。
pub(super) fn prune_beam(bucket: &mut Vec<usize>, arena: &[BeamNode], k: usize) {
    bucket.sort_by(|&a, &b| {
        arena[b]
            .score
            .partial_cmp(&arena[a].score)
            .unwrap_or(Ordering::Equal)
    });
    bucket.truncate(k);
}

