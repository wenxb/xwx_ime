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

    pub(crate) fn score_with_context(
        &self,
        state: &LmState,
        context: Option<&PreparedContext>,
        word: &str,
    ) -> f32 {
        match (self, state) {
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => match context {
                Some(ctx) => lm.score_prepared(ctx, word),
                None => lm.score_only(s, word),
            },
            (LmStore::Mixed(lm), LmState::Mixed(s)) => {
                let base_logp = lm.base.score_with_context(&s.base, context, word);
                let (uni, bi) = lm.user.counts_with(&s.snap, s.user_prev.as_deref(), word);
                let boost = count_gain(uni) + count_gain(bi);
                base_logp + boost
            }
            _ => self.score_only(state, word),
        }
    }
}

impl PreparedScorer<'_> {
    pub(crate) fn score(&self, word: &str) -> f32 {
        self.model
            .score_with_context(self.state, self.context.as_ref(), word)
    }
}
