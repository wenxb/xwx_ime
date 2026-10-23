//! 排序:候选定序键(`SortKey`/`sort_key`)、层内证据分级、未解释字母计数
//! (拆至 [`coverage`])以及副索引候选并入。

use super::*;

mod coverage;
mod compact;

pub(super) use coverage::{tail_only, unexplained_letters, unreachable_tail};

pub(super) fn ordered_candidates<K>(
    map: std::collections::HashMap<K, Candidate>,
) -> Vec<Candidate> {
    let mut v: Vec<Candidate> = map.into_values().collect();
    v.sort_by(|a, b| a.word.cmp(&b.word).then_with(|| a.pinyin.cmp(&b.pinyin)));
    v
}

pub(super) fn sort_candidates(inp: &str, ranked: &mut [Candidate], entry_tier_exception: bool) {
    // 排序键 = (u_min 桶, 拼写来源层序, 层内证据分级 band, 分数 − 覆盖罚分, 频次降序, ...):
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
    // `inp` 是排序时的查询串。排序首键 u_min 桶要与格构建/beam/后向 DP
    // 用**同一份**「不许跨 `'`」的可达性,故在这里归一化成干净串 + 边界
    // 表——归一化本身走共享的 [`super::input::NormalizedInput`];无 `'` 时它
    // 零分配,`unreachable_tail` 也走原有零开销路径。
    let normalized = super::input::NormalizedInput::new(inp);
    let inp: &str = normalized.clean();
    let bounds: Option<&[usize]> = normalized.bounds();
    let umin = unreachable_tail(inp, bounds);
    // 预计算每个候选的数值键:`sort_by` 每次比较调两次 `sort_key`,而
    // `sort_key` 里的 `unexplained_letters` / `cp_len` 都要对 `c.pinyin`
    // 做一次 chars 过滤(O(拼音长));500 条候选的简拼层排序 ~4500 次比较
    // × 2 键 × 2 次过滤,release 下实测 zhongg 从 0.99ms 涨到 1.23ms。
    // 预计算成 `PlainKey` 后比较是 O(1),延迟回到 1.0ms 量级。
    //
    // **只排下标**:旧实现把候选本身克隆进 `keyed`、排完再
    // `clone_from_slice` 写回,一次排序克隆两遍整个候选表(`Candidate` 带
    // 三个 `String`,500 条 ≈ 1500 次分配)。改成「键 + 下标」后按下标置换
    // 就地交换,零克隆——省的是分配,不是毫秒(与 `score_only` 同一结论)。
    let mut keys: Vec<PlainKey> = ranked.iter().map(|c| plain_key(inp, &umin, c, entry_tier_exception)).collect();
    demote_non_dominant_completions(&mut keys, ranked, inp);
    let mut order: Vec<usize> = (0..ranked.len()).collect();
    order.sort_by(|&a, &b| {
        keys[a].cmp(&keys[b]).then_with(|| {
            // 数值键并列时的确定性收尾,与旧 `sort_key` 尾部字段一致
            let (a, b) = (&ranked[a], &ranked[b]);
            b.freq
                .cmp(&a.freq)
                .then_with(|| a.word.cmp(&b.word))
                .then_with(|| a.pinyin.cmp(&b.pinyin))
        })
    });
    // 就地应用置换:目标是 `ranked[i] = 旧 ranked[order[i]]`。把置换分解成
    // 环,在每个环里沿 `order` 反向搬(每步 swap 一个元素),零克隆。
    let mut placed = vec![false; ranked.len()];
    for start in 0..ranked.len() {
        if placed[start] {
            continue;
        }
        let mut i = start;
        loop {
            placed[i] = true;
            let j = order[i];
            if j == start {
                break;
            }
            ranked.swap(i, j);
            i = j;
        }
    }
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
    /// 第三键:**层内证据分级**(见 [`demote_non_dominant_completions`])。
    ///
    /// `0` = 整串读法(词条命中与拼装同列,分数定序),或经支配豁免的
    /// 鼻音补全;`1` = 尾音节补全;`2` = 其余(声母碎片、非整串前缀词)。
    /// 打全了拼音的读法恒在补全之前,补全恒在碎片候选之前。
    band: u8,
    /// 第四键:补全层序(`false` = 精确命中,`true` = 尾音节补全)。
    ///
    /// 与 `band` 冗余(band 已决定补全位置),保留只为让豁免后的次序
    /// 调整可见;其打分职责见 [`COMPLETION_PENALTY`]。
    completion: bool,
    /// 第五键:分数 − 覆盖罚分,降序。
    ///
    /// `score` 里混着 LM 项,所以它**不是**补全越级的判据(见
    /// [`demote_non_dominant_completions`])。
    score: ScoreDesc,
}

/// 从候选算出数值排序键(与 `sort_key` 的前三键同式)。
pub(super) fn plain_key(
    inp: &str,
    umin: &[usize],
    c: &Candidate,
    entry_tier_exception: bool,
) -> PlainKey {
    let k = sort_key(inp, umin, c, entry_tier_exception);
    PlainKey {
        bucket: k.bucket,
        spelling: k.spelling,
        band: k.band,
        completion: k.completion,
        score: k.score,
    }
}

/// 词条级词频证据(`freq == 0` = 拼装候选/用户造词 ⇒ 无证据)。
fn lexical_evidence(c: &Candidate) -> f32 {
    if c.freq == 0 {
        f32::NEG_INFINITY
    } else {
        (c.freq as f32).log10()
    }
}

/// 补全层的**支配例外**:补全自己的**打字通道证据**比「同桶同层最强精确命中」
/// 高出 [`COMPLETION_DOMINANCE`] 时,算它把精确命中压得没话说(`beijin` → 北京
/// 对被禁差 1.93),升回精确层照常排前面;否则一律降到所有精确命中之后。
///
/// **比较轴不用 `score`**:`score = log10(freq/total) + (lm − floor)·w`,LM 项
/// 回答的是「这个词配这段上文吗」,与「用户打完了没有」不是同一个问题。混在
/// 一根轴上正是本函数存在的理由——实测 `liji` 在上文「他的」下,「理解」靠 LM
/// 拿到 1.46 的优势压过「立即」,而两者的词频差只有 0.00007。分开之后:词频
/// 证据(通道)决定谁在哪个层,LM 只决定层内次序。
///
/// 拼装候选没有词条词频。其分数只允许已有 n 的 n→ng 混淆豁免；
/// 完整开音节不能仅凭整句 LM 被追加 n/ng（`ka` 不应被猜成 `kan`）。
///
/// 对手池只收**拼音字母数与输入相等**的候选:`beijin` 的「被」(f2998064)只解释
/// 了输入的前三个字母,不是「用户打完了」的同类;拿它的词频当门槛会把北京
/// (6.05 对 被禁 4.12)误杀——实测踩过这个坑。
fn demote_non_dominant_completions(keys: &mut [PlainKey], ranked: &[Candidate], inp: &str) {
    const LAYERS: usize = 3;
    let max_bucket = keys.iter().map(|k| k.bucket).max().unwrap_or(0);
    let slots = ((max_bucket + 1) * LAYERS).max(1);
    // 同桶同层的两张「最强精确命中」榜:词频证据榜(通道)与分数榜(拼装候选)。
    let mut best_lexical = vec![f32::NEG_INFINITY; slots];
    let mut best_score = vec![f32::NEG_INFINITY; slots];
    let letters = |c: &Candidate| c.pinyin.chars().filter(|ch| *ch != ' ').count();
    for (k, c) in keys.iter_mut().zip(ranked) {
        let n = letters(c);
        // **层首**:一切**整串读法**(候选拼音恰好覆盖整个输入——词条命中
        // 与拼装读法同列,后者靠分数/LM 补偿竞争,如 `chuzaina` 的「出在哪」
        // 对「杵在那」)恒置 band 0——同桶 Exact 层内领先一切补全与碎片
        // 候选。这是「精确匹配靠后」问题的结构性修复:补全是否让位不再看
        // 词频差,而看**输入是不是该词的完整拼音**(事实),不是「用户是否
        // 还没打完」(猜测)。
        if !k.completion && k.spelling == SpellingKind::Exact && n == inp.len() {
            k.band = 0;
        }
        if k.completion || n != inp.len() {
            continue;
        }
        let slot = k.bucket * LAYERS + k.spelling as usize;
        let evidence = lexical_evidence(c);
        if evidence > best_lexical[slot] {
            best_lexical[slot] = evidence;
        }
        if k.score.0 > best_score[slot] {
            best_score[slot] = k.score.0;
        }
    }
    for (k, c) in keys.iter_mut().zip(ranked).filter(|(k, _)| k.completion) {
        let slot = k.bucket * LAYERS + k.spelling as usize;
        let evidence = lexical_evidence(c);
        // 同桶同层没有精确命中时两榜都是 −∞:没有对手要避让,照常放行。
        let (mine, theirs) = if evidence.is_finite() {
            (evidence, best_lexical[slot])
        } else {
            (k.score.0, best_score[slot])
        };
        // **豁免口只对鼻音截断开放**(见 [`nasal_completion`]):n/ng 混淆
        // 是拼音最典型的打错形态,补全假设可信(`beijin`→北京、`beiji`→
        // 北京);元音/其他类截断(`xiala`→下来、`jianji`→简介)不是现实的
        // 打错形态——输入更可能是另一个打完整的词,永不越过整串精确词条
        // (band 1),只在一切层首之后按分数竞争。
        let can_dominate = evidence.is_finite() || inp.ends_with('n') || !theirs.is_finite();
        if can_dominate && mine >= theirs + COMPLETION_DOMINANCE && nasal_completion(inp, c) {
            k.completion = false;
            k.band = 0;
        } else {
            k.band = 1;
        }
    }
}

/// 尾音节补全的**截断类型**:被省略的字母是不是纯鼻音韵尾(n/ng)。
///
/// 设 `token` = 候选拼音最后一个音节,`extra` = 拼音字母数 − 输入字母数,
/// `typed` = 输入末尾 `token.len() − extra` 个字母(被补全音节覆盖的
/// 已打部分):鼻音类 ⇔ `token == typed+"n" | typed+"ng"`,或 `n→ng`
/// 互换(`xi→xin`、`xi→xing`、`jin→jing`;`xian→xiang` 这类韵腹+韵尾
/// 复合形态由 n→ng 分支覆盖)。多出元音/其他字母(`la→lai`、`ji→jie`)
/// 不算——「漏打一个元音」不是现实的打错形态。
///
/// 纯字符串推导,O(1),不触格与 `Candidate` 结构;形状异常(补全字母
/// 跨出尾音节、非前缀关系)一律按非鼻音处理,安全侧收敛。
fn nasal_completion(inp: &str, c: &Candidate) -> bool {
    let extra = compact::letters(&c.pinyin).count().saturating_sub(inp.len());
    if extra == 0 || !compact::starts_with(&c.pinyin, inp) {
        return false;
    }
    let token = c.pinyin.split(' ').next_back().unwrap_or("");
    if token.len() <= extra {
        return false;
    }
    let typed = &inp[inp.len() - (token.len() - extra)..];
    if !token.starts_with(typed) {
        return false;
    }
    match &token[typed.len()..] {
        // 多出纯鼻音韵尾:xi→xin、xi→xing(漏打 n/ng)。
        ext if ext == "n" || ext == "ng" => true,
        // n→ng(jin→jing):只差一个 g,typed 以 n 结尾。
        "g" if typed.ends_with('n') => true,
        _ => false,
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

/// **同一个 `u_min` 桶内的层序语义**（排序第二键的唯一出处）。
///
/// 桶内合法次序是 `Exact` → `Fuzzy` → `Abbrev`（枚举声明序）。`Exact` 档还
/// **收编**一类简拼候选：`Abbrev` 且带**真实证据**（见 [`entry_has_evidence`]）
/// 且覆盖整串，且输入属「纯缩写 / 开头即缩写」两类（`entry_tier_exception`，
/// 判据见 [`pure_abbrev_input`] / [`leading_abbrev_input`]）——这类读法的证据是
/// **词条级**的（整串命中词库条目），与「上千条前缀简拼候选」的洪泛不是
/// 一类，故与真精确候选同档按分数竞争；防洪泛仍靠 `ABBREV_QUOTA`（配额，
/// 不是排序）。
///
/// ⇒ 推论（契约测试钉的就是它）：`Candidate::spelling`（原始来源）在同一桶内
/// **允许且只允许一种交错**——被收编的整串条目级简拼排在 `Fuzzy` 之前。
/// 旧断言直接比 `c.spelling`，隐含假设「两层在桶内不交错」；收编生效后该
/// 假设不再成立，故判据改用本函数（⑤⑩②）。
pub(super) fn effective_layer(
    spelling: SpellingKind,
    freq: u64,
    covered: usize,
    inp_len: usize,
    entry_tier_exception: bool,
) -> SpellingKind {
    if entry_tier_exception
        && spelling == SpellingKind::Abbrev
        && entry_has_evidence(freq)
        && covered >= inp_len
    {
        SpellingKind::Exact
    } else {
        spelling
    }
}

/// 词条是否带**真实证据**：在词库里是**真实条目**，而不是填充档、也不是
/// 分数驱动的拼装候选。
///
/// - `freq == 0` 是「分数驱动」的标记（多词拼装候选与用户造词）——它们不是
///   词条级证据，收编进 `Exact` 会立刻压住 `Fuzzy` 层的纠错读法。实测
///   `xaing`：拼装候选「喜爱那个」`freq=0`/分 -31.4 一旦被收编就抢在
///   「想」(`Fuzzy`，分 -16.2)之前，首位从「想」掉到第 5；
/// - `1..=FILLER_FREQ_TIER` 是词库里没有真实依据的填充条目（约 70% 的词条），
///   与 `lookup.rs` 的填充档判据**同一式**。
///
/// 这里原先只判 `freq > 0`，于是 `freq = 100` 的填充档词条也享受「词条级
/// 证据」的待遇：实测 `xain` 的前三名是「喜爱你/心爱你/想爱你」（全是填充档），
/// 「先」排第 4。
fn entry_has_evidence(freq: u64) -> bool {
    freq > FILLER_FREQ_TIER
}

/// **输入类别判据**「纯缩写」：每个字母都能起一个音节
/// （`wsm`/`zgr`/`xxn`/`renm`）。
///
/// 与 `engine/lookup.rs` 的 `pure_abbrev` 同式，抽到这里是为了让排序、降级
/// 范围与契约测试用**同一份**判据——两份判据迟早漂移，而漂移的症状是
/// 「测试与实现各说各话」。调用方保证 `!reachable`（判据只在「精确格解不出
/// 整串」的分支里求值，见 `lookup.rs` 的 `needs_abbrev`）。
///
/// `has_abbrev` 这条约束不能省：副索引负责纯首字母简拼这条契约
/// （见 `engine/tests/abbrev.rs::engine_without_abbrev_index_degrades_gracefully`），
/// 即使简拼格本身只用主索引。
///
/// `zh`/`ch`/`sh` 开头的输入曾经被降级范围排除（`sibilant_excluded` 参数，
/// ⑤⑩③）：当时格子对 `shh`→时候 这类「双字母声母 + 首字母」简写的读法供给
/// 不足，进降级范围直接丢召回（mixed 0.760 → 0.893）。⑤⑩⑩ 的折叠校验修好
/// 供给后（校验把 san/se/si 这类伪 [sh] 读法清出去），排除失去存在理由，
/// 参数删除——降级范围与层序收编范围重新**同宽**。
pub(super) fn pure_abbrev_input(inp: &str, has_abbrev: bool) -> bool {
    has_abbrev
        && inp.len() >= 2
        && inp.char_indices().all(|(i, c)| {
            c.is_ascii_alphabetic() && !segmenter::abbrev_syllables(&inp[i..i + 1]).is_empty()
        })
}


/// 输入里是否存在**不能起音节的字母**（`i`/`u`/`v`）。
///
/// 混合输入里这些字母只能属于某个**完整音节**（`mei`/`ju`/`you`/`wu`），
/// 而 [`pure_abbrev_input`] 与 `leading_abbrev_input` 都要求「每个字母都能
/// 起一个音节」——`meixdao`（mei+x+dao）、`julbu`（ju+l+bu）、
/// `yougbmen`（you+g+b+men）因此整类被挡在简拼通道之外，是评测里
/// 「中间缩写大量不可召回」的形状之一。
///
/// 它是**结构事实**（音节表答的），不是阈值：判据的用途是决定要不要
/// 去问索引（见 `engine/indexed_mixed.rs`），不是决定候选的去留。
pub(super) fn has_non_initial_letter(inp: &str) -> bool {
    inp.char_indices().any(|(i, c)| {
        c.is_ascii_alphabetic() && segmenter::abbrev_syllables(&inp[i..i + 1]).is_empty()
    })
}

/// **层序收编的输入类别判据**「开头即缩写」：头 1、2 个字母都不是音节
/// （`zmgai`/`wmjtqbj`）。
///
/// 收紧到「前两个字母都不能构成音节」是有实测依据的：只看首字母会把
/// 「打错的正常输入」也算进来（`nihso` 的 `n` 不成音节，但它开头其实是
/// 正常的 `ni`），实测那样会让 `edits` 语料 MRR 0.728 → 0.680 退化。
/// `zh`/`ch`/`sh` 开头的输入曾经被排除（它们更像「全拼音节打到一半」，
/// 不是「开头即缩写」；实测不排除时 `zhongguor` 的「中国」被挤出前 10）。
/// ⑤⑩⑫ 删除排除：门槛只管「旧通道是否降级」，读法结构由格子自己管——
/// 字面弧（zhong+guo）与缩写弧（r→ren）本来就能共存（`womenjtqbj` 同构），
/// 而「中国」走补全读法经条目边照常供给。
///
/// 调用方保证 `!reachable`，理由同 [`pure_abbrev_input`]。
pub(super) fn leading_abbrev_input(seg: &Segmenter, inp: &str, has_abbrev: bool) -> bool {
    has_abbrev
        && inp.len() >= 3
        && seg.segment(&inp[..1]).is_empty()
        && seg.segment(&inp[..2]).is_empty()
}

/// 候选的完整排序键,供 [`sort_candidates`] 使用。
/// 各字段含义见 `sort_candidates` 函数内注释。分数字段包成
/// [`ScoreDesc`] 表达「降序」语义(高分排前),与旧实现的
/// `b.score.partial_cmp(&a.score)` 等价;引擎分数域不会出现 NaN
/// (打分是 log10(正数) 的加权和),NaN 在 [`ScoreDesc`] 里钳到最末,
/// 与旧实现 `unwrap_or(Ordering::Equal)` 的兜底最接近。
pub(super) fn sort_key<'a>(
    inp: &str,
    umin: &[usize],
    c: &'a Candidate,
    entry_tier_exception: bool,
) -> SortKey<'a> {
    let u = unexplained_letters(inp, c);
    // u_min(终点处的不可避免未解释数):补全词(cp 比输入长)必以
    // 输入开头(u=0),桶恒 0;简拼层构造上覆盖整串,恒 0;只覆盖
    // 前缀的候选查表。
    let cp_len = c.pinyin.chars().filter(|ch| *ch != ' ').count();
    let u_min = if c.spelling == SpellingKind::Abbrev || cp_len > inp.len() {
        0
    } else if tail_only(inp, c) && inp.len() - cp_len >= MAX_TYPO_GAP {
        // **吃掉了整段前缀**的读法(`zmgai` → 该,拼音 `gai` 跳过 `zm`):
        // 它一个前缀字母都没解释,桶就是被跳过的字母数。不这样做,它落在
        // `umin[cp_len]`(=0,尾巴本身可切分)——与「覆盖整串」的读法同桶,
        // 靠层序压在简拼读法前面(实测 `zmgai` 的「怎么改」被 `该/改/盖`
        // 一族挤到第 13)。
        //
        // 只跳 1 个字母的不在此列:那是最常见的「多打一个字母」纠错
        // (`qnihao` → 你好),它按容错读法照常与同层候选比分数。
        u
    } else {
        umin[cp_len]
    };
    let u_local = u.saturating_sub(u_min) as f32;
    // **尾音节补全标记**(仅 Exact 层):拼音字母数超过输入 ⇒ 尾音节是按更长
    // 音节补全的读法(`liji` 的 `ji`→`jie`/`jiang`),是「猜用户还没打完」。
    // Abbrev 层豁免:简拼读法的弧已付过 `ABBREV_PENALTY`,不确定性与补全
    // 不同源(`yiwanc` 的「已完成」拼音同样比输入长,不是补全)。
    //
    // 分层判据在 [`demote_non_dominant_completions`]:补全要压过精确命中,
    // 得先比同桶同层最强精确命中高出 [`COMPLETION_DOMINANCE`]。
    let completion = cp_len > inp.len() && c.spelling == SpellingKind::Exact;
    // 补全候选在**层内**照旧吃一点代价([`COMPLETION_PENALTY`]):跨层由
    // `completion` 分层管,这 0.25 只管层内与「Abbrev 层被收编成 Exact 的
    // 候选」之间的老次序(`sh` 的「生/事」对「时候/上海」),不动它就不引入
    // 与本问题无关的重排。
    let completion_cost = if completion { COMPLETION_PENALTY } else { 0.0 };
    let layer = effective_layer(c.spelling, c.freq, c.covered, inp.len(), entry_tier_exception);
    // **层内证据分级**的初值:`demote_non_dominant_completions` 会把
    // 整串精确真实词条升 0、豁免的鼻音补全升 0,其余保持这里的结果。
    // Fuzzy/Abbrev 层恒 2(常量,层内次序与旧实现逐字节一致)。
    let band = match layer {
        SpellingKind::Exact if completion => 1,
        _ => 2,
    };
    SortKey {
        bucket: u_min,
        spelling: layer,
        band,
        completion,
        score: ScoreDesc(c.score - COVERAGE_PENALTY * u_local - completion_cost),
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
    /// 第三键:层内证据分级(0 = 整串读法/豁免的鼻音补全,
    /// 1 = 尾音节补全,2 = 其余;语义见 [`PlainKey::band`] 与
    /// [`demote_non_dominant_completions`])。
    band: u8,
    /// 第五键:补全层序(`false` = 精确命中,`true` = 尾音节补全)。
    ///
    /// 与 band 冗余(band 已决定补全位置),保留记录豁免前的标记。
    completion: bool,
    /// 第六键:分数 − 覆盖罚分,降序(高分在前,见 [`ScoreDesc`])。
    score: ScoreDesc,
    /// 第七键:同分时原始词频降序(回退基准)。
    freq: std::cmp::Reverse<u64>,
    /// 第六、七键:词与拼音的字典序(确定性收尾)。
    word: &'a str,
    pinyin: &'a str,
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

/// **分层合并 + 保底配额**（简拼层与容错层共用同一套机制）。
///
/// `extra` 是某一条召回路径单独产出的候选：`layer` 指明它在排序里的层序，
/// `quota` 是它在本轮截断中的**保底名额**——截断按候选总数算，若只是
/// 「push 后整表 sort + truncate」，当精确层已经占满上限时，这条路径的候选
/// 会被整批截掉，用户就永远看不到它（简拼层救 `zhongguor`、容错层救打错一处
/// 的输入，都是这个场景）。
pub(super) fn merge_layer(
    inp: &str,
    mut ranked: Vec<Candidate>,
    extra: Vec<Candidate>,
    layer: SpellingKind,
    quota: usize,
) -> (usize, Vec<Candidate>) {
    ranked.extend(extra);
    let total = ranked.len();
    // **按拼写分层,不按来源分层**:简拼这条路上来的候选里,「末尾半音节补全」
    // 那批是有意的 `Exact`(与格子里打字中的补全词同层),它们不该吃简拼的
    // 保底配额——配额是给「缩写读法」的,让 `Exact` 占了就白占。
    let (mut tail, mut head): (Vec<Candidate>, Vec<Candidate>) = ranked
        .into_iter()
        .partition(|c| c.spelling == layer);
    sort_candidates(inp, &mut head, false);
    sort_candidates(inp, &mut tail, false);
    if total > MAX_CANDIDATES {
        // 只有总条数超上限才需要截断。配额是「保底名额」而非「简拼层
        // 封顶」:简拼层取「剩余空位」与配额中较大的那个——
        // - head 装满(≥100−配额)时:空位 ≈ 0,配额生效,简拼层保底
        //   ABBREV_QUOTA 条(本任务要救的场景:zhongguor 等);
        // - head 很短(如 ren 只有 21 条)时:空位有 79 条,简拼层照旧
        //   填满到 100——与旧「合并排序截 100」行为逐位一致,不退化
        //   (head ≤ 100 时旧行为等价于「先取 head 全量、再用简拼层
        //   补满 100」,与这里逐位相同)。
        let tail_keep = quota
            .min(tail.len())
            .max(MAX_CANDIDATES.saturating_sub(head.len()));
        tail.truncate(tail_keep);
        head.truncate(MAX_CANDIDATES - tail_keep);
    }
    head.extend(tail);
    // 拼接后整体重排:让排序首键(u_min 桶)跨层生效(见函数级
    // 注释)。sort_by 是稳定排序,重排不改变同键候选的相对次序。
    sort_candidates(inp, &mut head, false);
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

