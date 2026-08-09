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

/// 音节最大长度：`shuang` / `chuang` / `zhuang` 均为 6。
pub(crate) const MAX_SYLLABLE_LEN: usize = 6;

/// 单个输入串最多返回的切分数，防止病态输入（如 `"aaaaaaaa"`）导致组合爆炸。
///
/// 引擎主查询路径（[`crate::ImeEngine::lookup_candidates`]）自第 2b 步起
/// 改在切分格上做 beam search 解码，不再调用本函数枚举全部切分；这里只
/// 服务「边打字边联想」兜底（整串未完整解码时的最长前缀切分）与测试。
/// 兜底路径仍会以病态串为前缀，因此保留该硬上限。
const MAX_SEGMENTATION_RESULTS: usize = 4096;

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

    /// 判断一个字符串是否是合法音节。
    pub fn is_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
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
            let syl = &input[pos..pos + len];
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

    #[test]
    fn empty_input() {
        assert!(segs("").is_empty());
    }
}
