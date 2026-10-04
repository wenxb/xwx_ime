//! 拼音音节表 + 动态规划（DP）切分。
//!
//! 输入法把用户敲的一串字母（如 `"nihao"`）切成一个一个合法拼音音节
//! （`"ni" + "hao"`），才能去词典里查词。本模块负责：
//!
//! 1. 维护一份**完整的普通话拼音音节表**（无声调，`ü` 写作 `v`，与输入习惯一致）；
//! 2. 用 DP（带记忆化的递归）枚举一个输入串的**所有合法切分**。
//!
//! 例子：`"xian"` 有两种合法切分：`["xian"]`（先/现…）和 `["xi", "an"]`（西安），
//! 两种都要返回，由语言模型决定哪种更可能。

use std::collections::{HashMap, HashSet};

/// 普通话拼音音节表（无声调，共 400+ 个）。
///
/// 说明：
/// - `ü` 按输入惯例写作 `v`（`lv`/`nve` 等）；
/// - 不含叹词音节 `m`/`n`/`ng`/`hm`/`hng` 等单字母音节，否则 DP 会产生大量
///   无意义的切分（TODO: 若需要支持语气词输入，可单独开关）；
/// - 含少量口语/方言音节（`dia` 嗲、`lo` 咯、`kei` 剋、`chua` 欻、`rua` 挼）。
pub const SYLLABLES: &[&str] = &[
    // a
    "a", "ai", "an", "ang", "ao",
    // b
    "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi", "bian",
    "biao", "bie", "bin", "bing", "bo", "bu",
    // c
    "ca", "cai", "can", "cang", "cao", "ce", "cen", "ceng", "cha", "chai",
    "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong", "chou",
    "chu", "chua", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci",
    "cong", "cou", "cu", "cuan", "cui", "cun", "cuo",
    // d
    "da", "dai", "dan", "dang", "dao", "de", "dei", "den", "deng", "di",
    "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan",
    "dui", "dun", "duo",
    // e
    "e", "ei", "en", "eng", "er",
    // f
    "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou", "fu",
    // g
    "ga", "gai", "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong",
    "gou", "gu", "gua", "guai", "guan", "guang", "gui", "gun", "guo",
    // h
    "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong",
    "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo",
    // j
    "ji", "jia", "jian", "jiang", "jiao", "jie", "jin", "jing", "jiong",
    "jiu", "ju", "juan", "jue", "jun",
    // k
    "ka", "kai", "kan", "kang", "kao", "ke", "kei", "ken", "keng", "kong",
    "kou", "ku", "kua", "kuai", "kuan", "kuang", "kui", "kun", "kuo",
    // l
    "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia",
    "lian", "liang", "liao", "lie", "lin", "ling", "liu", "lo", "long",
    "lou", "lu", "luan", "lun", "luo", "lv", "lve",
    // m
    "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi",
    "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu",
    // n
    "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni",
    "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou",
    "nu", "nuan", "nuo", "nv", "nve",
    // o
    "o", "ou",
    // p
    "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng", "pi", "pian",
    "piao", "pie", "pin", "ping", "po", "pou", "pu",
    // q
    "qi", "qia", "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong",
    "qiu", "qu", "quan", "que", "qun",
    // r
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru",
    "rua", "ruan", "rui", "run", "ruo",
    // s
    "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai",
    "shan", "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou",
    "shu", "shua", "shuai", "shuan", "shuang", "shui", "shun", "shuo", "si",
    "song", "sou", "su", "suan", "sui", "sun", "suo",
    // t
    "ta", "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao",
    "tie", "ting", "tong", "tou", "tu", "tuan", "tui", "tun", "tuo",
    // w
    "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo", "wu",
    // x
    "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong",
    "xiu", "xu", "xuan", "xue", "xun",
    // y
    "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong",
    "you", "yu", "yuan", "yue", "yun",
    // z
    "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha",
    "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi",
    "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui",
    "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

/// 以 `prefix`（1~2 个字母的音节首字母，如 `j`/`zh`）开头的音节，长音节优先。
///
/// 简拼（只打首字母）读法要在**每个窗口**枚举候选音节，链枚举里线性扫 412 条
/// 会把 `wmjtqbj` 这类长串放大成几万次比较；建一次表之后是 O(1) 切片查。
/// 顺序按「长音节优先、同长字典序」（理由见 `sort_len_desc`），保证结果确定
/// （同一输入两次查询逐字节一致）。
///
/// 两字母前缀只认 `zh`/`ch`/`sh`：其余两字母窗口（如 `wm`）不是音节的
/// 首字母对，返回空——它们该走别的分支，不是简拼。
pub fn abbrev_syllables(prefix: &str) -> &'static [&'static str] {
    struct Index {
        one: [Vec<&'static str>; 26],
        two: Vec<(&'static str, Vec<&'static str>)>,
    }
    // 短音节优先：简拼里最省事的读法就是「一个声母就定下来的常用短音节」
    // (`wo`/`men`/`ji`/`qu`…)。链枚举是按顺序花预算的，长音节先试会把
    // `w`→`wang`、`m`→`mang` 这类分支排到前面，实测 `wmjtqbj` 因此只解出
    // 「外媒镜头前不久」，真正的「我们今天去北京」连格子都没进。
    fn sort_len_asc(v: &mut Vec<&'static str>) {
        v.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    }
    static INDEX: std::sync::OnceLock<Index> = std::sync::OnceLock::new();
    let idx = INDEX.get_or_init(|| {
        let mut one: [Vec<&'static str>; 26] = std::array::from_fn(|_| Vec::new());
        let mut two: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
        for s in SYLLABLES {
            if let Some(i) = s.as_bytes().first().and_then(|c| c.checked_sub(b'a')) {
                if (i as usize) < 26 {
                    one[i as usize].push(s);
                }
            }
            for p in ["zh", "ch", "sh"] {
                if s.starts_with(p) {
                    match two.iter_mut().find(|(k, _)| *k == p) {
                        Some((_, v)) => v.push(s),
                        None => two.push((p, vec![s])),
                    }
                }
            }
        }
        for v in one.iter_mut() {
            sort_len_asc(v);
        }
        for (_, v) in two.iter_mut() {
            sort_len_asc(v);
        }
        Index { one, two }
    });
    if prefix.len() >= 2 {
        return idx
            .two
            .iter()
            .find(|(k, _)| *k == prefix)
            .map_or(&[][..], |(_, v)| v.as_slice());
    }
    match prefix.as_bytes().first().and_then(|c| c.checked_sub(b'a')) {
        Some(i) if (i as usize) < 26 => idx.one[i as usize].as_slice(),
        _ => &[],
    }
}

/// 音节最大长度：`shuang` / `chuang` / `zhuang` 均为 6。
pub(crate) const MAX_SYLLABLE_LEN: usize = 6;

/// 单个输入串最多返回的切分数，防止病态输入（如 `"aaaaaaaa"`）导致组合爆炸。
///
/// 引擎主查询路径（[`crate::ImeEngine::lookup_candidates`]）自第 2b 步起
/// 改在切分格上做 beam search 解码，不再调用本函数枚举全部切分；这里只
/// 服务「边打字边联想」兜底（整串未完整解码时的最长前缀切分）与测试。
/// 兜底路径仍会以病态串为前缀，因此保留该硬上限。
const MAX_SEGMENTATION_RESULTS: usize = 4096;

/// üe 的两套键盘写法：输入里可能是 `lue`/`nue`，词库一律写 `lve`/`nve`。
/// 本函数把输入写法归一成词库写法（其余音节原样返回），是**全项目唯一的
/// ü 口径**——[`Segmenter`] 的匹配与 `dict.rs` 的音节匹配都走它，免得
/// 「分词器认得、词库匹配不认得」。
///
/// **只限这两个音节**：`lu`/`nu` 本身就是「路/努」的合法全拼，不能当
/// `lv`/`nv`（否则打 `nu` 会混进「女」），用户已明确否掉。
pub(crate) fn canonical_syllable(syl: &str) -> &str {
    match syl {
        "lue" => "lve",
        "nue" => "nve",
        other => other,
    }
}

/// 输入切片 `input[pos..pos + len]` 是不是一个合法音节;是则返回**规范
/// 写法**(查词库用的形式),不是则 `None`。
///
/// 这是「拿输入切片去查音节表」的唯一原语:归一化与
/// 合法性一起做完,调用方不再各写一份「先 `canonical_syllable` 再
/// `SYLLABLES.contains`」。漏掉归一化的一方会把合法的 `lue`/`nue`
/// 当成「解释不了的字母」——[`crate::engine::rank::unreachable_tail`]
/// 原先正是这样,排序的 u_min 桶与格构建对同一输入给出不同答案。
///
/// 查的是**编译期静态表**、`binary_search` O(log 412)、零分配:热路径
/// 上每次 `sort_candidates` 都要调几十次,不能在这里建 `HashSet`
/// (实测会把击键延迟放大 7~50 倍,见 `unreachable_tail` 的注释)。
pub(crate) fn syllable_at(input: &str, pos: usize, len: usize) -> Option<&str> {
    let slice = input.get(pos..pos + len)?;
    let canon = canonical_syllable(slice);
    SYLLABLES.binary_search(&canon).is_ok().then_some(canon)
}

/// 容错解码用的「音节级 ≤1 次编辑」索引:输入窗口 → 可达的规范音节 + 代价。
///
/// # 依据(业界标准做法,不是自创)
///
/// - 输入错误只有四类:**插入 / 删除 / 替换 / 调换相邻字母**(Cooper 1983);
/// - 80% 的输入错误只有**一次**编辑操作,阈值取 2 就能高覆盖地找到正确
///   拼音(Damerau 1964)——映射到本项目就是「单个音节内 ≤1 次编辑」;
/// - 代价口径照**噪声信道** `-log10 P(编辑操作)`(CHIME, IJCAI 2011;
///   Joint Graph Model, ACL 2014):混淆对的概率直接复用
///   [`crate::corrector::COMMON_CONFUSION_PAIRS`] 那一张表(全项目只有一份
///   「哪些字母容易混」的口径),见 [`substitution_penalty`]。
///
/// # 为什么是查表而不是 trie
///
/// CHIME 用 trie + 增量编辑距离搜索,因为它的词表是百万级拼音串。本项目的
/// **音节表只有 410 项且编译进二进制**,把每个音节的 1-编辑变体预先展开成
/// 一张 map(约 10 万条、首次使用时懒构建)更简单、更确定,查询退化成一次
/// 哈希查找。真正贵的是随后的词库查询,由 `MAX_CHAINS_PER_START` 与容错
/// 编辑预算一起封顶。
pub(crate) struct TypoSyllables {
    /// 窗口 → (规范音节, 代价, 是否替换类读法)。
    /// 第三项供调用方做「每窗口按编辑类别留读法」的选取
    /// (替换类与非替换类各留最便宜一条,见 `engine/mod.rs`
    /// `MAX_TYPO_READINGS_PER_WINDOW`)。
    map: HashMap<String, Vec<(&'static str, f32, bool)>>,
}

/// 默认替换概率(即「两个毫不相干的字母」):代价 2.0。
const TYPO_P_SUBST_DEFAULT: f64 = 0.01;
/// 方言型插入/删除概率(`z/c/s` 后多打/漏打 `h`、`n` 后多打/漏打 `g`)。
const TYPO_P_DIALECT: f64 = 0.05;
/// 一般插入(用户漏打一个字母)概率。
const TYPO_P_INSERT_DEFAULT: f64 = 0.005;
/// 一般删除(用户多打一个字母)概率:比漏打略低(多打的字母通常看得见)。
const TYPO_P_DELETE_DEFAULT: f64 = 0.004;
/// 调换相邻两字母的概率——单次编辑里最常见的一类(`shenem`→`shen me`)。
const TYPO_P_TRANSPOSE: f64 = 0.03;
/// 键盘**邻键误触**的概率:手指挪到隔壁键,与方言型插入同档(0.05)。
const TYPO_P_ADJACENT: f64 = 0.05;

/// QWERTY 同一行的键位排布(参照 libime `PinyinCorrectionProfile` 的
/// `mappingFromRows`:只取**同行左右邻键**,不含上下行)。
///
/// 依据:MS Pinyin(ACL 2000)对真实拼写错误的类型学统计,同行邻键占
/// 替换类错误的 ~43%(同列 15%);librime 的 `keyboard_map` 同样只收同行。
const QWERTY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// `a`、`b` 是否是键盘同一行的相邻键(如 `i`↔`o`、`m`↔`n`)。
/// `pub(crate)`:简拼纠错(`engine/abbrev_correct.rs`)的变体代价用同一张键盘模型。
pub(crate) fn is_qwerty_neighbor(a: char, b: char) -> bool {
    for row in QWERTY_ROWS {
        if let (Some(i), Some(j)) = (row.find(a), row.find(b)) {
            return i.abs_diff(j) == 1;
        }
    }
    false
}

/// `c` 在键盘上的同行左右邻键(`(左, 右)`,行端为 `None`)。
/// 简拼纠错的变体生成按它枚举替换变体——与 [`is_qwerty_neighbor`]
/// 同一张 [`QWERTY_ROWS`],只枚举、不判定。
pub(crate) fn qwerty_row_neighbors(c: char) -> (Option<char>, Option<char>) {
    for row in QWERTY_ROWS {
        if let Some(i) = row.find(c) {
            let left = i.checked_sub(1).and_then(|j| row.chars().nth(j));
            let right = row.chars().nth(i + 1);
            return (left, right);
        }
    }
    (None, None)
}

/// 把概率换算成对数域的**加性惩罚**(越小越像用户本意)。
/// `pub(crate)`:简拼纠错的同列邻键档(`TYPO_P_ADJACENT_COL`)只有概率、
/// 没有现成换算,复用这份换算而不是再抄一份 log10。
pub(crate) fn penalty(p: f64) -> f32 {
    -((p.log10()) as f32)
}

/// `a`/`b` 是否命中混淆对;命中则返回其代价。证据表只有这一份
/// ([`crate::corrector::COMMON_CONFUSION_PAIRS]),容错解码与简拼纠错的
/// 变体代价都从这里取——两处各抄一份迟早漂移。
pub(crate) fn confusion_pair_penalty(a: char, b: char) -> Option<f32> {
    for pair in crate::corrector::COMMON_CONFUSION_PAIRS {
        let (mut x, mut y) = (pair.a.chars(), pair.b.chars());
        if let (Some(x), Some(y), None, None) = (x.next(), y.next(), x.next(), y.next()) {
            if (x, y) == (a, b) || (x, y) == (b, a) {
                return Some(penalty(pair.weight.max(0.001) as f64).max(0.1));
            }
        }
    }
    None
}

/// 单字母替换的代价,证据从强到弱:
/// ①混淆对(方言音系,是语言层面的强证据)→ ②键盘邻键误触(手指挪到
/// 隔壁键,MS Pinyin 类型学里占替换错误的 ~43%)→ ③毫不相干的字母。
///
/// 实测意义:`nohao`(想打 `nihao`,`i` 被邻键 `o` 打掉)里 `o→i` 只 1.3,
/// 独占「最便宜读法」的位置,「你好」才进得了候选——邻键模型没有时它与
/// `na`/`ne`/`nu` 同价 2.0,谁上全看 `HashMap` 迭代顺序。
fn substitution_penalty(a: char, b: char) -> f32 {
    if let Some(p) = confusion_pair_penalty(a, b) {
        return p;
    }
    if is_qwerty_neighbor(a, b) {
        return penalty(TYPO_P_ADJACENT);
    }
    penalty(TYPO_P_SUBST_DEFAULT)
}

/// 插入一个字母的代价:`h` 插在 `z/c/s` 后、`g` 插在 `n` 后算方言型。
fn insertion_penalty(inserted: char, prev: Option<char>) -> f32 {
    let dialect = matches!((prev, inserted), (Some('z' | 'c' | 's'), 'h') | (Some('n'), 'g'));
    if dialect {
        penalty(TYPO_P_DIALECT)
    } else {
        penalty(TYPO_P_INSERT_DEFAULT)
    }
}

/// 吃掉 `len` 个无法解释的字母(用户多打了这些字母)的代价。
///
/// 按「一般删除」概率**线性累加**,不给折扣:多打两个字母本来就比多打
/// 一个罕见得多,折扣会让长乱码段也变得「便宜」。
pub(crate) fn gap_cost(len: usize) -> f32 {
    penalty(TYPO_P_DELETE_DEFAULT) * len as f32
}

/// 删除一个字母的代价:与插入同源(`zh`→`z`、`ang`→`an` 都是方言型)。
/// `pub(crate)`:简拼纠错的「删一个字母」变体用同一口径(含方言型判定)。
pub(crate) fn deletion_penalty(deleted: char, prev: Option<char>) -> f32 {
    let dialect = matches!((prev, deleted), (Some('z' | 'c' | 's'), 'h') | (Some('n'), 'g'));
    if dialect {
        penalty(TYPO_P_DIALECT)
    } else {
        penalty(TYPO_P_DELETE_DEFAULT)
    }
}

/// 简拼纠错(`engine/abbrev_correct.rs`)变体代价的**共用出口**:概率表只有
/// 这一份,变体生成侧按「混淆对 > 同行邻键 > 同列邻键 > 无关替换」取值,
/// 换位/删除分别用 `TYPO_P_TRANSPOSE` 与上面的 `deletion_penalty`。
pub(crate) fn typo_penalty_adjacent() -> f32 {
    penalty(TYPO_P_ADJACENT)
}
pub(crate) fn typo_penalty_transpose() -> f32 {
    penalty(TYPO_P_TRANSPOSE)
}
pub(crate) fn typo_penalty_default_sub() -> f32 {
    penalty(TYPO_P_SUBST_DEFAULT)
}

/// 启动期预热:把懒构建的纠错变体表**先建好**。
///
/// 表本身是 `OnceLock`,不加这一句也能工作——代价是「用户第一次打错」
/// 才构建。生产在发布 IBus 服务名之前调用它(见
/// `ImeEngine::prepare_startup`),把成本搬到启动期。
pub(crate) fn prepare_typo_table() {
    let _ = TypoSyllables::get();
}

impl TypoSyllables {
    /// 懒构建的全局实例(首次容错查询时展开,正常打字一次都不付这个成本)。
    fn get() -> &'static TypoSyllables {
        static INSTANCE: std::sync::OnceLock<TypoSyllables> = std::sync::OnceLock::new();
        INSTANCE.get_or_init(Self::build)
    }

    fn build() -> Self {
        let mut map: HashMap<String, Vec<(&'static str, f32, bool)>> = HashMap::new();
        let mut push = |window: String, syl: &'static str, cost: f32, is_subst: bool| {
            let slot = map.entry(window).or_default();
            // 同一窗口可能有多条路径到达同一音节(如「少一个字母」既能解释成
            // 删除、也能解释成调换),只保留**代价最小**的那条,并记住那条
            // 属于哪个编辑类别。
            match slot.iter_mut().find(|(s, _, _)| *s == syl) {
                Some((_, c, sub)) => {
                    if cost < *c {
                        *c = cost;
                        *sub = is_subst;
                    }
                }
                None => slot.push((syl, cost, is_subst)),
            }
        };
        for &syl in SYLLABLES {
            let chars: Vec<char> = syl.chars().collect();
            // 替换
            for (i, &orig) in chars.iter().enumerate() {
                for c in 'a'..='z' {
                    if c == orig {
                        continue;
                    }
                    let mut v = chars.clone();
                    v[i] = c;
                    push(v.iter().collect(), syl, substitution_penalty(orig, c), true);
                }
            }
            // 插入(用户漏打了一个字母)
            for i in 0..=chars.len() {
                for c in 'a'..='z' {
                    let mut v = chars.clone();
                    v.insert(i, c);
                    push(
                        v.iter().collect(),
                        syl,
                        insertion_penalty(c, i.checked_sub(1).map(|j| chars[j])),
                        false,
                    );
                }
            }
            // 删除(用户多打了一个字母)
            for i in 0..chars.len() {
                let mut v = chars.clone();
                v.remove(i);
                push(
                    v.iter().collect(),
                    syl,
                    deletion_penalty(chars[i], i.checked_sub(1).map(|j| chars[j])),
                    false,
                );
            }
            // 调换相邻两字母
            for i in 0..chars.len().saturating_sub(1) {
                let mut v = chars.clone();
                v.swap(i, i + 1);
                push(v.iter().collect(), syl, penalty(TYPO_P_TRANSPOSE), false);
            }
        }
        Self { map }
    }

    /// 查一个输入窗口(未被 `'` 打断的一段字母)的容错候选。
    ///
    /// 返回 `(规范音节, 代价, 是否替换类)`;空 = 这个窗口不像任何音节的
    /// 1-编辑变体(调用方据此放弃这条弧)。
    pub(crate) fn lookup(window: &str) -> &'static [(&'static str, f32, bool)] {
        Self::get()
            .map
            .get(window)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// 拼音切分器：持有一张音节集合，提供查音节与 DP 切分。
#[derive(Debug, Clone, Default)]
pub struct Segmenter {
    syllables: HashSet<&'static str>,
}

impl Segmenter {
    /// 用完整音节表构造切分器。
    pub fn new() -> Self {
        Self {
            syllables: SYLLABLES.iter().copied().collect(),
        }
    }

    /// 判断一个字符串是否是合法音节（`lue`/`nue` 按 üe 算合法）。
    pub fn is_syllable(&self, s: &str) -> bool {
        self.syllables.contains(canonical_syllable(s))
    }

    /// 整串能否**完整**切成音节(布尔 DP,O(n·6),不枚举切法)。
    ///
    /// 与 [`Self::segment`] 的区别是成本:`segment` 会枚举**所有**切法
    /// (预算 4096),长串上徒增开销;这里只回答「可切 / 不可切」,用来决定
    /// 要不要启用容错解码(见 `ImeEngine::lookup_candidates` 的门槛)。
    pub fn is_fully_segmentable(&self, input: &str) -> bool {
        if input.is_empty() || !input.is_ascii() {
            return false;
        }
        let bytes = input.as_bytes();
        let n = bytes.len();
        // reachable[i] = input[..i] 能完整切成音节。
        let mut reachable = vec![false; n + 1];
        reachable[0] = true;
        for i in 0..n {
            if !reachable[i] {
                continue;
            }
            let max_len = (n - i).min(MAX_SYLLABLE_LEN);
            for len in 1..=max_len {
                let syl = canonical_syllable(&input[i..i + len]);
                if self.syllables.contains(syl) {
                    reachable[i + len] = true;
                }
            }
        }
        reachable[n]
    }

    /// 返回 `input` 的**所有**合法音节切分。
    ///
    /// - 每个切分是一个音节序列，如 `"nihao"` -> `[["ni","ha","o"], ["ni","hao"]]`；
    /// - 顺序为确定性顺序（音节短的优先），无切分时返回空 `Vec`；
    /// - 非 ASCII 输入（含数字、标点）直接返回空。
    pub fn segment(&self, input: &str) -> Vec<Vec<String>> {
        if input.is_empty() || !input.is_ascii() {
            return Vec::new();
        }
        let mut memo: HashMap<usize, Option<Vec<Vec<String>>>> = HashMap::new();
        let mut budget = MAX_SEGMENTATION_RESULTS;
        self.segment_from(input, 0, &mut memo, &mut budget)
            .unwrap_or_default()
    }

    /// DP 递归主体：求 `input[pos..]` 的所有切分。
    ///
    /// - `memo`：按字节位置记忆结果，避免重复计算（每个位置至多算一次）；
    /// - `budget`：剩余可产出的切分数，归零后停止（防止组合爆炸）。
    fn segment_from(
        &self,
        input: &str,
        pos: usize,
        memo: &mut HashMap<usize, Option<Vec<Vec<String>>>>,
        budget: &mut usize,
    ) -> Option<Vec<Vec<String>>> {
        if let Some(cached) = memo.get(&pos) {
            return cached.clone();
        }
        if pos == input.len() {
            // 递归出口：空后缀只有「空切分」这一种切法。
            return Some(vec![Vec::new()]);
        }
        if *budget == 0 {
            // 预算耗尽：本位置不再展开，向上返回"无结果"。
            // TODO: 改为 beam search 保留最有希望的路径。
            return None;
        }

        let mut results: Vec<Vec<String>> = Vec::new();
        let mut any = false;
        let max_len = (input.len() - pos).min(MAX_SYLLABLE_LEN);

        // 依次尝试 1..=max_len 长的前缀是否构成音节；前缀长度升序，
        // 保证结果顺序确定（短音节切分在前）。
        for len in 1..=max_len {
            // 归一成词库写法后再查表：`lue` 命中 `lve`、`nue` 命中 `nve`，
            // 且**切分结果里放的是规范写法**（`dict.lookup` 的 key 用它拼）。
            let raw = &input[pos..pos + len];
            let syl = canonical_syllable(raw);
            if !self.syllables.contains(syl) {
                continue;
            }
            if let Some(suffixes) = self.segment_from(input, pos + len, memo, budget) {
                any = true;
                for mut suffix in suffixes {
                    if *budget == 0 {
                        break;
                    }
                    *budget -= 1;
                    let mut seg = Vec::with_capacity(1 + suffix.len());
                    seg.push(syl.to_string());
                    seg.append(&mut suffix);
                    results.push(seg);
                }
            }
        }

        let out = if any { Some(results) } else { None };
        memo.insert(pos, out.clone());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(s: &str) -> Vec<Vec<String>> {
        Segmenter::new().segment(s)
    }

    #[test]
    fn simple_two_syllables() {
        let r = segs("nihao");
        // "ni" + "ha" + "o" 与 "ni" + "hao" 都合法
        assert!(r.contains(&vec!["ni".into(), "hao".into()]));
        assert!(r.contains(&vec!["ni".into(), "ha".into(), "o".into()]));
    }

    #[test]
    fn ambiguous_xian() {
        let r = segs("xian");
        assert!(r.contains(&vec!["xian".into()]));
        assert!(r.contains(&vec!["xi".into(), "an".into()]));
    }

    #[test]
    fn longest_syllable() {
        let r = segs("zhuang");
        assert!(r.contains(&vec!["zhuang".into()]));
    }

    #[test]
    fn no_segmentation() {
        assert!(segs("qqq").is_empty());
        assert!(segs("你好").is_empty());
    }

    /// üe 的两种键盘写法:`lue`/`nue` 与词库的 `lve`/`nve` 等价,且切分结果
    /// 必须是**词库写法**(`dict.lookup` 的 key 靠它拼)。**只限 üe**:
    /// `lu`/`nu` 是「路/努」的合法全拼,不得当成 `lv`/`nv`(用户 2026-10-02 口径)。
    ///
    /// 失败见证:把 `canonical_syllable` 的 `"lue" => "lve"` 去掉 → 红
    /// (切分退化成 `lu`+`e`);把 `"lu" => "lv"` 加进去 → 「nu 不得切出 nv」红。
    #[test]
    fn ue_accepts_both_spellings_but_lu_is_not_lv() {
        let s = Segmenter::new();
        assert!(s.is_syllable("lue") && s.is_syllable("lve"));
        assert!(s.is_syllable("nue") && s.is_syllable("nve"));
        assert!(s.is_syllable("lu") && s.is_syllable("nu"));

        let r = segs("hulue");
        assert!(
            r.contains(&vec!["hu".into(), "lve".into()]),
            "`hulue` 必须切出词库写法 hu+lve(否则「忽略」查不到): {r:?}"
        );
        assert!(
            r.contains(&vec!["hu".into(), "lu".into(), "e".into()]),
            "原来的 hu+lu+e 读法不得消失: {r:?}"
        );
        let n = segs("nu");
        assert_eq!(n, vec![vec!["nu".to_string()]], "`nu` 不得被切出 `nv`(女)");
    }

    #[test]
    fn empty_input() {
        assert!(segs("").is_empty());
    }

    /// 整串可切性(容错解码的门槛):只回答「能不能整串切成音节」。
    #[test]
    fn fully_segmentable_flags() {
        let s = Segmenter::new();
        assert!(s.is_fully_segmentable("nihao"));
        assert!(s.is_fully_segmentable("zhongguo"));
        assert!(!s.is_fully_segmentable("nihoa"));
        assert!(!s.is_fully_segmentable("zgr"));
        assert!(!s.is_fully_segmentable(""), "空串不算「可切」");
        assert!(!s.is_fully_segmentable("你好"), "非 ASCII 不算");
    }

    /// 音节级容错索引:四类编辑各给一条代表,并钉住「代价序」。
    #[test]
    fn typo_index_covers_four_edit_kinds_and_orders_costs() {
        let has = |w: &str, syl: &str| {
            TypoSyllables::lookup(w).iter().any(|(s, _, _)| *s == syl)
        };
        let cost = |w: &str, syl: &str| {
            TypoSyllables::lookup(w)
                .iter()
                .find(|(s, _, _)| *s == syl)
                .map(|(_, c, _)| *c)
                .unwrap_or_else(|| panic!("{w} 应能容错到 {syl}"))
        };
        // 替换:`shenem` 里的 `mem` → men
        assert!(has("mem", "men"), "替换:mem → men");
        // 调换相邻字母:`shenem` 里的 `em` → me(用户报的 nihoa 同理)
        assert!(has("em", "me"), "调换:em → me");
        assert!(has("hoa", "hao"), "调换:hoa → hao");
        // 删除(多打一个字母):`shanghaai` 里的 `haai` → hai
        assert!(has("haai", "hai"), "删除:haai → hai");
        // 插入(漏打一个字母):`jitian` 里的 `ji` → jin
        assert!(has("ji", "jin"), "插入:ji → jin");
        // 无关窗口没有候选(纯辅音串不该被读成音节)
        assert!(TypoSyllables::lookup("zgr").is_empty(), "zgr 不该有容错读法");
        assert!(TypoSyllables::lookup("zzzz").is_empty(), "zzzz 不该有容错读法");
        // 代价序:混淆对(n/l)< 任意替换;调换比一般删除便宜。
        assert!(
            cost("lan", "nan") < cost("lan", "ban"),
            "混淆对(n/l)应比任意替换便宜"
        );
        assert!(
            cost("em", "me") < cost("haai", "hai"),
            "调换(单次编辑里最常见)应比一般删除便宜"
        );
    }
}



