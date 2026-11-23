//! 普通与字面整句共享前缀搜索；到终点后只留分数和回溯位置。
use super::*;

struct SentenceEnd {
    score: f32,
    parent: usize,
    edge: (usize, usize),
    used_edits: u8,
}

struct SentenceSearch {
    arena: Vec<BeamNode>,
    ends: Vec<SentenceEnd>,
    normal: Vec<usize>,
}

fn prune_ends(bucket: &mut Vec<usize>, ends: &[SentenceEnd], k: usize) {
    bucket.sort_by(|&a, &b| {
        ends[b]
            .score
            .partial_cmp(&ends[a].score)
            .unwrap_or(Ordering::Equal)
    });
    bucket.truncate(k);
}

fn literal_edge(edge: &LatticeEdge, pos: usize) -> bool {
    !edge.gap
        && !edge.abbrev
        && edge.fuzzy_penalty == 0.0
        && edge.edit_count == 0
        && ImeEngine::edge_spells_span(edge, pos)
}

impl ImeEngine {
    #[inline(never)]
    pub(in crate::engine) fn sentence_candidates(
        &self,
        input: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
    ) -> Result<Vec<Candidate>, EngineError> {
        let no_whole = !lattice[0].iter().any(|e| e.end == input.len());
        // 完整音节不以其他辅音结尾，这类输入不可能触发字面补取。
        if !no_whole
            || !input
                .as_bytes()
                .last()
                .is_some_and(|b| b"aeiouvngr".contains(b))
        {
            return self.best_chain_candidates(
                input,
                ctx,
                lattice,
                SpellingKind::Exact,
                WORD_BOUNDARY_PENALTY,
            );
        }
        // 仅尾边可非字面时，两种搜索的所有中间桶完全相同。
        let share = lattice.iter().enumerate().all(|(pos, edges)| {
            edges
                .iter()
                .all(|e| e.end == input.len() || literal_edge(e, pos))
        });
        if !share {
            let mut out = self.best_chain_candidates(
                input,
                ctx,
                lattice,
                SpellingKind::Exact,
                WORD_BOUNDARY_PENALTY,
            )?;
            self.append_literal_sentence(input, ctx, lattice, &mut out)?;
            return Ok(out);
        }
        let search = self.sentence_search(
            input,
            ctx,
            lattice,
            SENTENCE_BEAM_WIDTH,
            WORD_BOUNDARY_PENALTY,
        )?;
        let paths = sentence_paths(
            &search.arena,
            &search.ends,
            &search.normal[..search.normal.len().min(ABBREV_CHAIN_NBEST)],
            lattice,
        );
        let mut out = Self::chain_candidates_from_paths(input, SpellingKind::Exact, paths);
        if super::literal::needs_literal(input, lattice, &out) {
            let literal = literal_ends(&search.ends, lattice, ABBREV_CHAIN_NBEST);
            let paths = sentence_paths(&search.arena, &search.ends, &literal, lattice);
            for c in Self::chain_candidates_from_paths(input, SpellingKind::Exact, paths) {
                if !out
                    .iter()
                    .any(|old| old.word == c.word && old.pinyin == c.pinyin)
                {
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    // 保持通用候选转换的热代码不动，此处只处理整句搜索的路径。
    fn chain_candidates_from_paths(
        pinyin: &str,
        spelling: SpellingKind,
        paths: Vec<BeamPath>,
    ) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        for path in paths {
            if path.words.is_empty() {
                continue;
            }
            // 与通用转换相同：每个音节至少解释一个输入字母。
            let syllables: usize = path.words.iter().map(|w| w.pinyin.split(' ').count()).sum();
            if syllables > pinyin.len() {
                continue;
            }
            let word: String = path.words.iter().map(|w| w.word.as_str()).collect();
            let key = path
                .words
                .iter()
                .map(|w| w.pinyin.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if out
                .iter()
                .any(|c: &Candidate| c.word == word && c.pinyin == key)
            {
                continue;
            }
            out.push(Candidate {
                word,
                pinyin: key,
                score: path.score,
                freq: 0,
                spelling: if path.used_edits > 0 && spelling == SpellingKind::Exact {
                    SpellingKind::Fuzzy
                } else {
                    spelling
                },
                echo: false,
                covered: pinyin.len(),
                matched_len: 0,
                english: None,
            });
        }
        out
    }

    fn sentence_search(
        &self,
        input: &str,
        ctx: &LmContext<'_>,
        lattice: &[Vec<LatticeEdge>],
        k: usize,
        per_word_penalty: f32,
    ) -> Result<SentenceSearch, EngineError> {
        let len = input.len();
        let mut arena = Vec::with_capacity((len + 1) * k);
        let mut ends = Vec::new();
        let mut normal = Vec::new();
        let mut beams: Vec<Vec<usize>> = (0..=len).map(|_| Vec::new()).collect();
        let total = self.dict.total_freq()?;
        let log_total = (total as f64).log10();
        arena.push(BeamNode {
            score: 0.0,
            state: Some(self.lm.state_from_context(ctx)),
            edge: None,
            parent: None,
            used_edits: 0,
        });
        beams[0].push(0);
        for pos in 0..len {
            let cur = std::mem::take(&mut beams[pos]);
            if cur.is_empty() {
                continue;
            }
            let mut touched = Vec::new();
            let mut touched_end = false;
            let dict_scores: Vec<f32> = lattice[pos]
                .iter()
                .map(|edge| {
                    if edge.gap {
                        0.0
                    } else {
                        Self::word_prior_logp(edge.freq, log_total, total)
                            + self.internal_cohesion_bonus(&edge.word, edge.freq)
                    }
                })
                .collect();
            let has_terminal = lattice[pos].iter().any(|e| e.end == len);
            for node in cur {
                if arena[node].state.is_none() {
                    let parent_idx = arena[node].parent.expect("非根节点必有父节点");
                    let (epos, eslot) = arena[node].edge.expect("非根节点必有边");
                    let edge = &lattice[epos][eslot];
                    let pstate = arena[parent_idx].state.as_ref().expect("父节点必已物化");
                    let next_state = if edge.gap {
                        pstate.clone()
                    } else {
                        self.lm.advance_state(pstate, &edge.word)
                    };
                    arena[node].state = Some(next_state);
                }
                let node_score = arena[node].score;
                let node_used = arena[node].used_edits;
                let boundary_cost = if arena[node].edge.is_some() {
                    per_word_penalty
                } else {
                    0.0
                };
                let cur_state = arena[node].state.as_ref().expect("刚物化").clone();
                let scorer = self.lm.prepare(&cur_state);
                if has_terminal {
                    for (slot, edge) in lattice[pos]
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.end == len)
                    {
                        let used = node_used + if edge.gap { 1 } else { edge.edit_count };
                        if used > MAX_TYPO_EDITS {
                            continue;
                        }
                        let score = if edge.gap {
                            node_score - edge.typo_penalty
                        } else {
                            let lm_gain = scorer.score_gain(&edge.word, WANXIANG_GAIN_SCALE);
                            node_score
                                + dict_scores[slot]
                                + lm_gain * self.lm_weight
                                - edge.fuzzy_penalty
                                - edge.typo_penalty
                                - boundary_cost
                        };
                        let idx = ends.len();
                        ends.push(SentenceEnd {
                            score,
                            parent: node,
                            edge: (pos, slot),
                            used_edits: used,
                        });
                        normal.push(idx);
                        touched_end = true;
                    }
                }
                for (slot, edge) in lattice[pos]
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.end != len)
                {
                    let used = node_used + edge.edit_count;
                    if used > MAX_TYPO_EDITS {
                        continue;
                    }
                    let lm_gain = scorer.score_gain(&edge.word, WANXIANG_GAIN_SCALE);
                    let idx = arena.len();
                    arena.push(BeamNode {
                        score: node_score
                            + dict_scores[slot]
                            + lm_gain * self.lm_weight
                            - edge.fuzzy_penalty
                            - edge.typo_penalty
                            - boundary_cost,
                        state: None,
                        edge: Some((pos, slot)),
                        parent: Some(node),
                        used_edits: used,
                    });
                    beams[edge.end].push(idx);
                    touched.push(edge.end);
                }
            }
            touched.sort_unstable();
            touched.dedup();
            for t in touched {
                prune_beam(&mut beams[t], &arena, k);
            }
            if touched_end {
                prune_ends(&mut normal, &ends, k);
            }
        }
        prune_ends(&mut normal, &ends, k);
        Ok(SentenceSearch {
            arena,
            ends,
            normal,
        })
    }
}

fn sentence_paths(
    arena: &[BeamNode],
    ends: &[SentenceEnd],
    bucket: &[usize],
    lattice: &[Vec<LatticeEdge>],
) -> Vec<BeamPath> {
    bucket
        .iter()
        .map(|&idx| {
            let end = &ends[idx];
            let mut edges = vec![end.edge];
            let mut cur = Some(end.parent);
            while let Some(i) = cur {
                if let Some(edge) = arena[i].edge {
                    edges.push(edge);
                }
                cur = arena[i].parent;
            }
            let words = edges
                .into_iter()
                .rev()
                .map(|(pos, slot)| {
                    let e = &lattice[pos][slot];
                    WordSpot {
                        word: e.word.clone(),
                        pinyin: e.pinyin.clone(),
                    }
                })
                .collect();
            BeamPath {
                score: end.score,
                words,
                used_edits: end.used_edits,
            }
        })
        .collect()
}

fn literal_ends(ends: &[SentenceEnd], lattice: &[Vec<LatticeEdge>], k: usize) -> Vec<usize> {
    let mut literal = Vec::new();
    let mut previous = None;
    for (idx, end) in ends.iter().enumerate() {
        let (pos, slot) = end.edge;
        if previous != Some(pos) {
            // 按原搜索的起点分组回放剪枝，保住同分路径的先后次序。
            prune_ends(&mut literal, ends, k);
            previous = Some(pos);
        }
        if literal_edge(&lattice[pos][slot], pos) {
            literal.push(idx);
        }
    }
    prune_ends(&mut literal, ends, k);
    literal
}

#[cfg(test)]
mod tests {
    #[test]
    fn literal_tail_gate_covers_all_syllables() {
        for syllable in crate::segmenter::SYLLABLES {
            assert!(
                syllable
                    .as_bytes()
                    .last()
                    .is_some_and(|b| b"aeiouvngr".contains(b)),
                "{syllable}"
            );
        }
    }
}
