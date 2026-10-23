//! 英文有界召回池。
//!
//! 来源与上限见 `docs/english-input/02-engine.md` 第 5 节。**有界近似**：
//! 不承诺联合个性化分数的全词库数学 Top-K，但静态前缀索引与用户计数前缀
//! 索引各自必须与各自的全扫描 oracle 一致。
//!
//! M1 不含静态二元来源，去重前上界 97；接入 M2 后为 129。

use std::collections::BTreeMap;

use crate::candidate::EnglishMatchKind;
use crate::user_lm::UserLmSnapshot;

use super::bigram::EnglishBigram;
use super::context::EnglishContext;
use super::dict::{EnglishDict, EnglishEntryRef};
use super::rank::{conditional_base, user_counts, EnglishHit};

/// 单个来源的条数上限（静态前缀 / 用户前缀 / 两种上文列表）。
const SOURCE_LIMIT: usize = 32;

/// 英文屏蔽命名空间。
///
/// 英文屏蔽是「屏蔽目标词」而不是只屏蔽当前前缀，因此三个召回源与 M2 预测
/// 共用同一个命名空间键。
pub const ENGLISH_BLOCK_NAMESPACE: &str = "__english__";

/// 该行里某个 next 的计数（未收录返回 0）。
fn count_in(row: &super::bigram::BigramRow, model: &EnglishBigram, next: &str) -> u64 {
    row.iter(model)
        .find(|e| e.next == next)
        .map(|e| e.count)
        .unwrap_or(0)
}

/// 当前输入的英文是否允许「补全」（`key != input_key`）。
///
/// 无英文上文时 1 个字母不出英文、2 个字母只查精确；有可信英文上文时允许
/// 从 1 个字母开始补全（见第 3 节门槛 5/6）。
pub(crate) fn completion_allowed(input_key: &str, ctx: &EnglishContext<'_>) -> bool {
    ctx.previous_word.is_some() || input_key.len() >= 3
}

/// 召回：合并五个来源、按完整 key 去重、统一过滤屏蔽词。
///
/// `static_bigram` 是 M2 的可选静态二元模型（`None` = M1 行为，去重前上界 97）。
pub(crate) fn recall_english(
    dict: &EnglishDict,
    input_key: &str,
    ctx: &EnglishContext<'_>,
    snapshot: &UserLmSnapshot,
    static_bigram: Option<&EnglishBigram>,
) -> Result<Vec<EnglishHit>, super::EnglishError> {
    let allow_completion = completion_allowed(input_key, ctx);
    // 去重按**目标完整 key**；BTreeMap 让遍历顺序确定（结果与插入顺序无关）。
    let mut merged: BTreeMap<String, EnglishHit> = BTreeMap::new();

    // 懒取上文行：无候选时不新增一次读取/错误路径。
    let cached_row = std::cell::Cell::new(None);
    let context_row = || -> Result<Option<super::bigram::BigramRow>, super::EnglishError> {
        if let Some(row) = cached_row.get() {
            return Ok(row);
        }
        let row = match (static_bigram, ctx.previous_word) {
            (Some(model), Some(prev)) => model.row(prev)?,
            _ => None,
        };
        cached_row.set(Some(row));
        Ok(row)
    };
    let push = |key: &str,
                    known: Option<EnglishEntryRef<'_>>,
                    merged: &mut BTreeMap<String, EnglishHit>|
     -> Result<(), super::EnglishError> {
        if merged.contains_key(key) {
            return Ok(());
        }
        if !allow_completion && key != input_key {
            return Ok(()); // 短串门槛对所有来源统一生效
        }
        let Some(e) = (match known { Some(e) => Some(e), None => dict.exact(key)? }) else {
            return Ok(()); // 升级后已移除的词不凭旧计数复活
        };
        let (uni, bi) = user_counts(snapshot, ctx, e.key);
        // M2：静态条件基础项（该 prev 行存在且收录了这个词对时才算，
        // 不存在时 `conditional_base` 返回 `None`，分数退回 M1 的 z）。
        let static_base = match (static_bigram, context_row()?) {
            (Some(model), Some(r)) =>
                conditional_base(e.zipf_centi, count_in(&r, model, e.key), r.observed_total),
            _ => None,
        };
        merged.insert(
            e.key.to_string(),
            EnglishHit {
                key: e.key.to_string(),
                display: e.display.to_string(),
                zipf_centi: e.zipf_centi,
                match_kind: if e.key == input_key {
                    EnglishMatchKind::Exact
                } else {
                    EnglishMatchKind::Completion
                },
                user_unigram: uni,
                user_bigram: bi,
                static_base,
            },
        );
        Ok(())
    };

    // ① 完整 key 精确查询：不受前缀截断影响。
    push(input_key, None, &mut merged)?;
    if allow_completion {
        // 静态来源已持有词条；用户来源仍须确认词库中存在。
        for e in dict.prefix_top(input_key)? {
            push(e.key, Some(e), &mut merged)?;
        }
        for key in snapshot.english_prefix_top(input_key).iter().take(SOURCE_LIMIT) {
            push(key, None, &mut merged)?;
        }
    }
    // ④/⑤ 上文列表（无英文上文时不查，见第 5 节）。
    if let Some(prev) = ctx.previous_word {
        for key in snapshot.english_next_top(prev).iter().take(SOURCE_LIMIT) {
            if key.starts_with(input_key) {
                push(key, None, &mut merged)?;
            }
        }
        // ⑤ M2 静态上文 Top-32 中匹配当前前缀的词。
        if let Some(model) = static_bigram {
            if let Some(row) = context_row()? {
                for e in row.iter(model).take(SOURCE_LIMIT) {
                    if e.next.starts_with(input_key) {
                        push(e.next, None, &mut merged)?;
                    }
                }
            }
        }
    }

    // 屏蔽在**所有召回源合并之后**统一过滤一次。
    merged.retain(|key, _| !snapshot.is_blocked(ENGLISH_BLOCK_NAMESPACE, key));
    Ok(merged.into_values().collect())
}
