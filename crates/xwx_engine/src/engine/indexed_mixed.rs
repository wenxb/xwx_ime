//! 索引驱动的混合读法并入（简拼通道的**第四类门槛**）。
//!
//! 前三类门槛（纯缩写 / 开头即缩写 / 尾音残缺）都要求「每个字母都能起一个
//! 音节」，于是 `meixdao`（mei+x+dao）、`julbu`（ju+l+bu）、`yougbmen`
//! （you+g+b+men）这类**全拼部分含 `i`/`u`/`v`** 的混合形态连简拼格都不建——
//! 而它们恰恰是最常见的缩写位置（中间一个音节取首字母）。
//!
//! 这里走的是另一条路：不问「输入像不像简拼」，直接**问索引**——
//! [`MmapDict::lookup_abbrev_patterns`] 把输入按「完整音节 / 缩写」的全部
//! 切法对齐到副索引的键，命中即证据（词条存在、全拼逐位验证过）。
//! 代价 ∝ 命中词条数（实测 0.05~0.7ms），所以可以无条件先问一次。
//!
//! **判据与放行的分工**：本模块只负责「把索引已经证实了的整串读法并入候选
//! 表」，层序标 `Abbrev`、覆盖标整串；要不要显示、排第几，仍由既有排序
//! （u_min 桶 → 层序 → 分数）与配额决定。不建简拼格——那是给**词链**解码
//! 用的（成本高一个量级），与「整串命中的单个词条」不是一件事。
//!
//! **门槛**：输入**逐字节解释不了**（精确格解不出整串，见 `lookup.rs`）且
//! **含不能起音节的字母**（`i`/`u`/`v`）——后者正是前三类门槛失败的结构原因：
//! 它们都要求「每个字母都能起一个音节」，而 `meixdao`/`julbu`/`yougbmen`
//! 这类混合形态里 `i`/`u`/`v` 只能属于某个完整音节。
//!
//! 放宽到「只要逐字节解释不了就查索引」是试过的（能多修 `defe`→得分儿 等
//! 形态），但那条路会把索引遍历放进**每次非逐字节解释的击键**上，实测单线程
//! p95 +1.3ms、>12ms 占比 0.31%→0.81%，而评测只多召回 63/485,054 条——
//! 详见 `lookup.rs` 里那段门槛注释。

use super::*;

/// 把索引证实的整串混合读法并入 `ranked`；有无并入返回给调用方（诊断用）。
///
/// 只在「输入含不能起音节的字母」时调用：纯首字母串由前三类门槛走简拼格，
/// 那条路已经覆盖，这里再问一次只是白花钱。
pub(super) fn merge_indexed_mixed(
    engine: &ImeEngine,
    pinyin: &str,
    ctx: &LmContext<'_>,
    ranked: &mut Vec<Candidate>,
) -> Result<bool, EngineError> {
    let DictStore::Mmap(d) = &engine.dict else {
        return Ok(false);
    };
    if !d.has_abbrev() || !rank::has_non_initial_letter(pinyin) {
        return Ok(false);
    }
    let indexed = d.lookup_abbrev_patterns(pinyin)?;
    // **填充档（freq ≤ `FILLER_FREQ_TIER`）也并入**——但它们在**容错层的分数闸**
    // 里不算「覆盖整串」的证据（见 `lookup.rs` 的 `covering_best`）。两头都对：
    // - 词条本身进名单：`jiejyonggong` → 解决用工、`zaoshenjhua` → 造神计划
    //   这类**只有索引能证实**的混合读法，词频恰好落在填充档——词库 70% 的
    //   条目都在这一档，一律拒之门外等于把这些形态也一并拒掉；
    // - 但它们无权否决容错纠错：`zhuagn` 按混合形态切出来的唯一词条是填充档的
    //   「转化功能」，早先它「覆盖整串」⇒ 容错层的「装」被分数闸挡住、纠错失效。
    //
    // 「填充档没有真实依据」仍然成立：它决定的是**能不能否决别人的证据**，
    // 不是**能不能出现在名单里**。
    if indexed.is_empty() {
        return Ok(false);
    }
    let total = engine.dict.total_freq()?;
    if total == 0 {
        // 空词库：词频项无从归一，按「没有证据」处理。
        return Ok(false);
    }
    // 分数口径与 `ImeEngine::fill_entry_evidence` **同式**（词条词频 +
    // 该词在真实上文下的 LM 分）。区别只在这里不查一次主索引：`freq` 就是
    // 副索引里那条词条自己的词频（与主索引同源，见 `abbrev_to_bin.py`）。
    let log10_total = (total as f64).log10();
    let state = engine.lm.state_from_context(ctx);
    let scorer = engine.lm.prepare(&state);
    let floor = engine.lm.floor_log10();
    // 去重:同一个 (词, 拼音) 已在表里就不再进第二份——兜底路/精确路与索引
    // 供给有重叠(如 `defe` 的「德风」来自补全边,而它同时是一条混合读法),
    // 重复项会白占候选位。
    let mut seen: std::collections::HashSet<(String, String)> = ranked
        .iter()
        .map(|c| (c.word.clone(), c.pinyin.clone()))
        .collect();
    let mut merged = false;
    for mut c in indexed {
        if !seen.insert((c.word.clone(), c.pinyin.clone())) {
            continue;
        }
        let dict_logp = ((c.freq.max(1) as f64).log10() - log10_total) as f32;
        let lm = scorer.score(&c.word);
        c.score = dict_logp + (lm - floor) * engine.lm_weight;
        c.spelling = SpellingKind::Abbrev;
        c.covered = pinyin.len();
        ranked.push(c);
        merged = true;
    }
    Ok(merged)
}
