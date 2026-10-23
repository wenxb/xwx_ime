//! 同一状态给一批词打分时，复用万象上文侧的编码和 trie 定位。
use super::*;
use crate::wanxiang::PreparedContext;

pub(crate) struct PreparedScorer<'a> {
    model: &'a LmStore,
    state: &'a LmState,
    context: Option<PreparedContext>,
}

impl LmStore {
    pub(crate) fn prepare<'a>(&'a self, state: &'a LmState) -> PreparedScorer<'a> {
        let (mut model, mut base_state) = (self, state);
        while let (LmStore::Mixed(lm), LmState::Mixed(s)) = (model, base_state) {
            model = &lm.base;
            base_state = &s.base;
        }
        let context = match (model, base_state) {
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => lm.prepare_context(&s.tail),
            _ => None,
        };
        PreparedScorer {
            model: self,
            state,
            context,
        }
    }
}

impl PreparedScorer<'_> {
    pub(crate) fn score(&self, word: &str) -> f32 {
        self.score_inner(self.model, self.state, word)
    }

    fn score_inner(&self, model: &LmStore, state: &LmState, word: &str) -> f32 {
        match (model, state) {
            (LmStore::Wanxiang(lm), LmState::Wanxiang(s)) => match &self.context {
                Some(ctx) => lm.score_prepared(ctx, word),
                None => lm.score_only(s, word),
            },
            (LmStore::Mixed(lm), LmState::Mixed(s)) => {
                let base_logp = self.score_inner(&lm.base, &s.base, word);
                let (uni, bi) = lm.user.counts_with(&s.snap, s.user_prev.as_deref(), word);
                let boost = count_gain(uni) + count_gain(bi);
                base_logp + boost
            }
            _ => model.score_only(state, word),
        }
    }
}
