//! 同一状态给一批词打分时，复用万象上文侧的编码和 trie 定位。
use super::*;
use crate::wanxiang::PreparedContext;

pub(crate) struct PreparedScorer<'a> {
    model: &'a LmStore,
    state: &'a LmState,
    context: Option<PreparedContext>,
}

const USER_UNI_BASE_LOGP: f32 = -2.30;
const USER_UNI_SLOPE: f32 = 0.25;
const USER_UNI_CAP_LOGP: f32 = -1.60;
const USER_UNPAIRED_DAMP_LOGP: f32 = 0.85;
const USER_BI_BASE_LOGP: f32 = -1.60;
const USER_BI_SLOPE: f32 = 0.30;
const USER_BI_CAP_LOGP: f32 = -0.85;

#[inline]
fn log10_add(a: f32, b: f32) -> f32 {
    let hi = a.max(b);
    let lo = a.min(b);
    if !lo.is_finite() || hi - lo > 12.0 {
        hi
    } else {
        hi + (1.0 + 10.0_f32.powf(lo - hi)).log10()
    }
}

impl LmStore {
    pub(crate) fn prepare_context(&self, state: &LmState) -> Option<PreparedContext> {
        let (mut model, mut base_state) = (self, state);
        while let (LmStore::Mixed(lm), LmState::Mixed(s)) = (model, base_state) {
            model = &lm.base;
            base_state = &s.base;
        }
        match (model, base_state) {
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => lm.prepare_context(&s.tail),
            _ => None,
        }
    }

    pub(crate) fn prepare<'a>(&'a self, state: &'a LmState) -> PreparedScorer<'a> {
        PreparedScorer {
            model: self,
            state,
            context: self.prepare_context(state),
        }
    }

    /// 仅推进语言模型状态（零 DAWG 查询，供 beam 剪枝后延迟物化状态）。
    pub(crate) fn advance_state(&self, state: &LmState, word: &str) -> LmState {
        match (self, state) {
            (LmStore::Bigram(_), LmState::Bigram(_)) => LmState::Bigram(Some(word.to_string())),
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => {
                LmState::Wanxiang(lm.advance(s, word))
            }
            (LmStore::Mixed(lm), LmState::Mixed(s)) => LmState::Mixed(MixedState {
                base: Box::new(lm.base.advance_state(&s.base, word)),
                user_prev: Some(word.to_string()),
                snap: s.snap.clone(),
            }),
            _ => unreachable!("LmState 变体与 LmStore 变体不匹配"),
        }
    }

    /// 计算有效语言模型得分/增益：万象 `.gram` 为零中心联合搭配增益 × `wanxiang_scale`
    /// （防止跨词增益违背 P(A,B)≤P(A)），用户学习层保持 1.0，`BigramLM` 维持真实条件对数概率。
    pub(crate) fn score_gain_with_context(
        &self,
        state: &LmState,
        context: Option<&PreparedContext>,
        word: &str,
        wanxiang_scale: f32,
    ) -> f32 {
        match (self, state) {
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => {
                let raw = match context {
                    Some(ctx) => lm.score_prepared(ctx, word),
                    None => lm.score_only(s, word),
                };
                (raw - lm.floor_log10()) * wanxiang_scale
            }
            (LmStore::Mixed(lm), LmState::Mixed(s)) => {
                lm.base
                    .score_gain_with_context(&s.base, context, word, wanxiang_scale)
                    + lm.user_boost(s, word)
            }
            _ => self.score_only(state, word),
        }
    }

    /// 统一词级对数得分（词先验 + LM + 用户概率混合）。
    ///
    /// 真实大词库下的 `MixedLM` 以对数概率和 `log10(P_base + P_user)` 融合用户记忆：
    /// 无证据时逐位等价通用层，有证据时严格单调递增且用户概率严格小于 1，
    /// 避免多词拆分按词数叠加无界正分冲垮整词。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn word_score_with_context(
        &self,
        state: &LmState,
        context: Option<&PreparedContext>,
        word: &str,
        dict_logp: f32,
        real_dict: bool,
        wanxiang_scale: f32,
        lm_weight: f32,
    ) -> f32 {
        if real_dict {
            if let (LmStore::Mixed(lm), LmState::Mixed(s)) = (self, state) {
                let base_gain = lm.base.score_gain_with_context(
                    &s.base,
                    context,
                    word,
                    wanxiang_scale,
                );
                let base_score = dict_logp + base_gain * lm_weight;
                let (c_uni, c_bi) = lm.user_counts(s, word);
                if c_uni == 0 && c_bi == 0 {
                    return base_score;
                }
                let u_uni = if c_uni > 0 {
                    let raw = (USER_UNI_BASE_LOGP + USER_UNI_SLOPE * (c_uni as f32).log10())
                        .min(USER_UNI_CAP_LOGP);
                    if s.user_prev.is_some() && c_bi == 0 {
                        raw - USER_UNPAIRED_DAMP_LOGP
                    } else {
                        raw
                    }
                } else {
                    f32::NEG_INFINITY
                };
                let u_bi = if c_bi > 0 {
                    (USER_BI_BASE_LOGP + USER_BI_SLOPE * (c_bi as f32).log10())
                        .min(USER_BI_CAP_LOGP)
                } else {
                    f32::NEG_INFINITY
                };
                let user_logp = log10_add(u_uni, u_bi);
                return log10_add(base_score, user_logp);
            }
        }
        dict_logp
            + self.score_gain_with_context(state, context, word, wanxiang_scale) * lm_weight
    }
}

impl PreparedScorer<'_> {
    pub(crate) fn word_score(
        &self,
        word: &str,
        dict_logp: f32,
        real_dict: bool,
        wanxiang_scale: f32,
        lm_weight: f32,
    ) -> f32 {
        self.model.word_score_with_context(
            self.state,
            self.context.as_ref(),
            word,
            dict_logp,
            real_dict,
            wanxiang_scale,
            lm_weight,
        )
    }
}
