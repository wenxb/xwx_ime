//! 覆盖度原语:u_min 不可达表、未解释字母数、尾音节判据。
//! 从 `mod.rs` 拆出(单文件 600 行约束),语义不变。

use super::*;

/// `umin[pos]` = 输入从 `pos` 起的**不可避免未解释字母数**:
/// 0 ⇔ 存在一条从 pos 到串尾的精确音节路径(只查音节表,不查词典);
/// >0 ⇔ pos 落在某个不可精确切分段内,取到「最近一个可达点」的距离。
///
/// 后向一次 DP 算出可达性,再正向把不可达位置映射到距离;返回表长
/// `inp.len() + 1`,索引即字节位置(调用方已保证 `inp` 为 ASCII)。
pub(in crate::engine) fn unreachable_tail(inp: &str, bounds: Option<&[usize]>) -> Vec<usize> {
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
            // 跨过 ⇔ 存在边界严格落在 (i, i+l) 内部（共享判据）。
            if super::input::crosses_boundary(bounds, i, l) {
                return false;
            }
            // 音节匹配走共享原语(A1):归一化(üe 的 `lue`/`nue`)与合法
            // 性一起做,和格构建/词典层同一口径。旧实现在这里直接查
            // `SYLLABLES`,漏了归一化——`lue` 被当成解释不了的字母。
            segmenter::syllable_at(inp, i, l).is_some()
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

/// 候选是否**只解释了输入的尾部**：去掉空格的拼音是输入的**真后缀**。
///
/// 这类读法来自容错格里「吃掉一段前缀」的弧（`zmgai` → 该，拼音 `gai`）：
/// 它解释的字母全在输入末尾，前缀一个都没解释。与「只覆盖前缀」（`zmgai`
/// → 「怎么」）正好相反，两者都不该被当成「覆盖整串」的读法参与同桶竞争。
pub(in crate::engine) fn tail_only(inp: &str, c: &Candidate) -> bool {
    if c.spelling == SpellingKind::Abbrev {
        return false; // 简拼层覆盖整串（构造保证），不存在「只解释尾部」
    }
    compact::letters(&c.pinyin).count() < inp.len() && compact::is_suffix_of(&c.pinyin, inp)
}

/// 候选**未解释的输入字母数**(覆盖度度量;不再是排序首键——
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
pub(in crate::engine) fn unexplained_letters(inp: &str, c: &Candidate) -> usize {
    if c.spelling == SpellingKind::Abbrev {
        return 0; // 简拼层构造上覆盖整串
    }
    let cp_len = compact::letters(&c.pinyin).count();
    if compact::starts_with(&c.pinyin, inp) {
        0 // 补全:候选拼音比输入更长(或相等),输入还没打完
    } else if cp_len < inp.len()
        && (compact::is_prefix_of(&c.pinyin, inp) || compact::is_suffix_of(&c.pinyin, inp))
    {
        // 只覆盖前缀:漏掉 inp[cp.len()..]；只覆盖后缀(zmgai → 该):
        // 漏掉前缀 inp[..inp.len() - cp.len()]。两种都只是「解释了一段」。
        inp.len() - cp_len
    } else {
        0 // 非前缀关系:解释数取保守下界 0(见函数级注释)
    }
}
