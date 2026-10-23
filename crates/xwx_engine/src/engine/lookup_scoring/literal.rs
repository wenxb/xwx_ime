use super::*;

fn completes_spelled_tail(input: &str, c: &Candidate) -> bool {
    let letters = || c.pinyin.bytes().filter(|&b| b != b' ');
    let Some(extra) = letters()
        .count()
        .checked_sub(input.len())
        .filter(|&n| n > 0)
    else {
        return false;
    };
    let token = c.pinyin.split(' ').next_back().unwrap_or("");
    let Some(typed) = token
        .len()
        .checked_sub(extra)
        .filter(|&n| n > 0 && n <= input.len())
    else {
        return false;
    };
    let mut expanded = letters();
    input.bytes().all(|b| expanded.next() == Some(b))
        && segmenter::syllable_at(input, input.len() - typed, typed).is_some()
}

pub(super) fn needs_literal(input: &str, lattice: &[Vec<LatticeEdge>], sentences: &[Candidate]) -> bool {
    !sentences.iter().any(|c| c.pinyin.bytes().filter(|&b| b != b' ').eq(input.bytes()))
        && sentences.iter().any(|c| completes_spelled_tail(input, c))
        && ImeEngine::lattice_exactly_reachable(lattice, input.len())
}

impl ImeEngine {
    #[cold]
    #[inline(never)]
    pub(in crate::engine) fn append_literal_sentence(
        &self,
        input: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        sentences: &mut Vec<Candidate>,
    ) -> Result<(), EngineError> {
        if !needs_literal(input, lattice, sentences) { return Ok(()); }
        let literal: Vec<Vec<LatticeEdge>> = lattice
            .iter()
            .enumerate()
            .map(|(pos, edges)| {
                edges
                    .iter()
                    .filter(|edge| {
                        !edge.gap
                            && !edge.abbrev
                            && edge.fuzzy_penalty == 0.0
                            && edge.edit_count == 0
                            && Self::edge_spells_span(edge, pos)
                    })
                    .cloned()
                    .collect()
            })
            .collect();
        for c in self.best_chain_candidates(
            input,
            ctx,
            &literal,
            SpellingKind::Exact,
            ABBREV_PER_WORD_PENALTY,
        )? {
            if !sentences
                .iter()
                .any(|old| old.word == c.word && old.pinyin == c.pinyin)
            {
                sentences.push(c);
            }
        }
        Ok(())
    }
}
