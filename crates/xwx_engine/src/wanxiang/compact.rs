//! Compact CSR DAWG backend (`Rime::Grammar/2.0`) for `WanxiangLM`.
use memmap2::Mmap;
use crate::lm::LmError;

#[derive(Debug)]
pub(super) struct CompactDawg {
    pub(super) total_states: usize,
    total_edges: usize,
    total_both: usize,
    end_bits_words: usize,
    end_bits_off: usize,
    edges_off: usize,
    both_off: usize,
}

impl CompactDawg {
    pub(super) fn parse(mmap: &[u8]) -> Result<Self, LmError> {
        if mmap.len() < 64 {
            return Err(LmError::BadGram("Rime::Grammar/2.0 文件头不足 64 字节".into()));
        }
        let total_states = u32::from_le_bytes(mmap[36..40].try_into().unwrap()) as usize;
        let total_edges = u32::from_le_bytes(mmap[52..56].try_into().unwrap()) as usize;
        let total_both = u32::from_le_bytes(mmap[56..60].try_into().unwrap()) as usize;
        let end_bits_words = u32::from_le_bytes(mmap[60..64].try_into().unwrap()) as usize;

        if total_states == 0 || total_edges == 0 {
            return Err(LmError::BadGram("Rime::Grammar/2.0 状态或边数为 0".into()));
        }
        let end_bits_off = 64usize;
        let edges_off = end_bits_off
            .checked_add(end_bits_words.checked_mul(8).ok_or_else(|| LmError::BadGram("overflow".into()))?)
            .ok_or_else(|| LmError::BadGram("overflow".into()))?;
        let both_off = edges_off
            .checked_add(total_edges.checked_mul(4).ok_or_else(|| LmError::BadGram("overflow".into()))?)
            .ok_or_else(|| LmError::BadGram("overflow".into()))?;
        let end_off = both_off
            .checked_add(total_both.checked_mul(6).ok_or_else(|| LmError::BadGram("overflow".into()))?)
            .ok_or_else(|| LmError::BadGram("overflow".into()))?;
        if end_off > mmap.len() {
            return Err(LmError::BadGram(format!(
                "Rime::Grammar/2.0 越界: 需要 {end_off}B > 文件长度 {}B",
                mmap.len()
            )));
        }
        Ok(Self {
            total_states,
            total_edges,
            total_both,
            end_bits_words,
            end_bits_off,
            edges_off,
            both_off,
        })
    }

    #[inline]
    fn state_range(&self, data: &[u8], state: usize) -> Option<(usize, usize)> {
        if state >= self.total_edges {
            return None;
        }
        let mut word_idx = state >> 6;
        if word_idx >= self.end_bits_words {
            return None;
        }
        let bit_off = state & 63;
        let woff = self.end_bits_off + word_idx * 8;
        let w = u64::from_le_bytes(data[woff..woff + 8].try_into().unwrap()) >> bit_off;
        if w != 0 {
            let end = state + (w.trailing_zeros() as usize) + 1;
            return Some((state, end));
        }
        loop {
            word_idx += 1;
            if word_idx >= self.end_bits_words {
                return None;
            }
            let woff = self.end_bits_off + word_idx * 8;
            let w2 = u64::from_le_bytes(data[woff..woff + 8].try_into().unwrap());
            if w2 != 0 {
                let end = (word_idx << 6) + (w2.trailing_zeros() as usize) + 1;
                return Some((state, end));
            }
        }
    }

    #[inline]
    fn find_edge(&self, data: &[u8], state: usize, b: u8) -> Option<u32> {
        let (s, e) = self.state_range(data, state)?;
        if s >= e || e > self.total_edges {
            return None;
        }
        let mut lo = s;
        let mut hi = e;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let eoff = self.edges_off + mid * 4;
            let edge = u32::from_le_bytes(data[eoff..eoff + 4].try_into().unwrap());
            let label = (edge & 0xFF) as u8;
            if label == b {
                return Some(edge);
            } else if label < b {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    #[inline]
    fn decode_edge(&self, data: &[u8], edge: u32) -> (Option<usize>, Option<i32>) {
        let payload = edge >> 8;
        if (payload & 0xFF_0000) != 0xFF_0000 {
            if (payload as usize) < self.total_edges {
                (Some(payload as usize), None)
            } else {
                let bi = (payload as usize) - self.total_edges;
                if bi >= self.total_both {
                    return (None, None);
                }
                let boff = self.both_off + bi * 6;
                let csid = u32::from_le_bytes(data[boff..boff + 4].try_into().unwrap()) as usize;
                let q = u16::from_le_bytes(data[boff + 4..boff + 6].try_into().unwrap()) as u32;
                let v = (20_000 + (q << 2)) as i32;
                (Some(csid), Some(v))
            }
        } else {
            let q = payload & 0xFFFF;
            let v = (20_000 + (q << 2)) as i32;
            (None, Some(v))
        }
    }

    pub(super) fn traverse(&self, mmap: &Mmap, key: &[u8], start_node: usize) -> (bool, usize, usize) {
        let data = &mmap[..];
        let mut state = start_node;
        let mut matched = 0usize;
        for &b in key {
            let Some(edge) = self.find_edge(data, state, b) else {
                return (false, state, matched);
            };
            let (Some(next_state), _) = self.decode_edge(data, edge) else {
                return (false, state, matched);
            };
            state = next_state;
            matched += 1;
        }
        (true, state, matched)
    }

    pub(super) fn common_prefix_search(
        &self,
        mmap: &Mmap,
        key: &[u8],
        node_pos: usize,
        results: &mut [(i32, usize)],
    ) -> usize {
        let data = &mmap[..];
        let mut n = 0;
        let mut state = node_pos;
        let mut pos = 0usize;
        for &b in key {
            pos += 1;
            let Some(edge) = self.find_edge(data, state, b) else {
                break;
            };
            let (next_state, leaf_val) = self.decode_edge(data, edge);
            if let Some(v) = leaf_val {
                results[n] = (v, pos);
                n += 1;
                if n >= results.len() {
                    break;
                }
            }
            if let Some(ns) = next_state {
                state = ns;
            } else {
                break;
            }
        }
        n
    }
}
