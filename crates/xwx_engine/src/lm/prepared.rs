//! 同一状态给一批词打分时，复用万象上文侧的编码和 trie 定位。
use super::*;
use crate::wanxiang::PreparedContext;

pub(crate) struct PreparedScorer<'a> {
    model: &'a LmStore,
    state: &'a LmState,
    context: Option<PreparedContext>,
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
}

impl PreparedScorer<'_> {
    pub(crate) fn score_gain(&self, word: &str, wanxiang_scale: f32) -> f32 {
        self.model.score_gain_with_context(
            self.state,
            self.context.as_ref(),
            word,
            wanxiang_scale,
        )
    }
}
