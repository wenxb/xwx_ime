//! 双通道查询编排：中文通道 + 英文通道 → 稳定混排。
//!
//! 架构见 `docs/english-input/ENGINE_INPUT_PLAN`（入口任务书第 6 节）与
//! 引擎分册第 4 节。「双通道」不等于开两个线程：一次按键内**顺序调用**，
//! 不引入线程池、异步竞态或锁等待。

use crate::candidate::Candidate;
use crate::english::{rank_english, recall_english, EnglishContext};
use crate::lm::LmContext;

use super::blend::{classify_intent, stable_blend, strong_chinese};

use super::*;

/// 一次英文查询的只读输入门槛（引擎分册第 3 节）。
///
/// 返回 `Some(key)` 表示「这个输入可以进英文通道」，`key` 是小写查询键；
/// `None` 表示只走旧中文路径。**绝不在这一步删撇号、标点或数字**：
/// `xi'an`、`abc@`、`http:` 都必须原样落到中文/识别态路径。
fn english_query_key(raw_input: &str) -> Option<String> {
    if raw_input.is_empty() {
        return None;
    }
    if raw_input.len() > 48 {
        return None;
    }
    if !raw_input.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(raw_input.to_ascii_lowercase())
}

impl ImeEngine {
    /// 组合查询入口：中文通道与英文通道的稳定混排。
    ///
    /// 与 [`ImeEngine::lookup_candidates`] 的关系：
    /// - 没装英文组件 / 输入不进英文门槛 / 英文召回为空 ⇒ **原样返回**旧
    ///   中文结果（连重排都不做一次）；
    /// - 装了英文时，中文候选的字段与相对次序不变，只有原文兜底允许后移
    ///   或被去重（见引擎分册第 9 节）。
    ///
    /// 一次查询只取**一代**英文用户快照：召回、屏蔽、打分、意图判定同源。
    pub fn lookup_composition(
        &self,
        raw_input: &str,
        chinese_context: &LmContext<'_>,
        english_context: &EnglishContext<'_>,
    ) -> Result<Vec<Candidate>, EngineError> {
        let Some(en) = self.english.as_ref() else {
            return self.lookup_candidates(raw_input, chinese_context);
        };
        let Some(key) = english_query_key(raw_input) else {
            return self.lookup_candidates(raw_input, chinese_context);
        };
        // 中文基线先跑：混排不改中文通道内部的任何判据。
        let zh = self.lookup_candidates(raw_input, chinese_context)?;

        let snapshot = en.user().snapshot();
        let hits = recall_english(en.dict(), &key, english_context, &snapshot)?;
        if hits.is_empty() {
            return Ok(zh); // 英文为空:原样返回，不重排
        }
        let pool = hits.len();
        let ranked = rank_english(hits, key.len(), english_context);
        let e0_zipf = ranked.first().map(|h| h.zipf_centi).unwrap_or(0);
        let e0_unigram = ranked.first().map(|h| h.user_unigram).unwrap_or(0);
        let recent_en = english_context.previous_word.is_some();
        let en_cands = ranked.candidates(key.len());
        let strong_zh = strong_chinese(&zh, raw_input);
        let band = match classify_intent(
            raw_input,
            &zh,
            &en_cands,
            e0_zipf,
            e0_unigram,
            recent_en,
        ) {
            Some(b) => b,
            None => return Ok(zh),
        };

        super::trace::note(|| {
            let detail = ranked
                .hits
                .iter()
                .zip(ranked.scores.iter())
                .take(3)
                .map(|(h, s)| {
                    format!(
                        "{}|{:?}|q={}|extra={}|U={}|B={}|S={:.4}",
                        h.key,
                        h.match_kind,
                        h.zipf_centi,
                        h.key.len().saturating_sub(key.len()),
                        h.user_unigram,
                        h.user_bigram,
                        s
                    )
                })
                .collect::<Vec<_>>()
                .join("  ");
            format!(
                "英文: input={key} 池={pool} 展示={} 意图={} strong_zh={strong_zh} good_en={} recent_en={recent_en}\n  命中: {detail}",
                en_cands.len(),
                band.label(),
                crate::engine::blend::good_english(e0_zipf, e0_unigram),
            )
        });

        let out = stable_blend(zh, en_cands, band);
        super::trace::note(|| {
            let head: Vec<String> = out
                .iter()
                .take(9)
                .map(|c| {
                    let src = match (c.echo, c.is_english()) {
                        (true, _) => "R",
                        (_, true) => "E",
                        _ => "Z",
                    };
                    format!("{src}:{}", c.word)
                })
                .collect();
            format!("混排: {}", head.join(" "))
        });
        Ok(out)
    }
}
