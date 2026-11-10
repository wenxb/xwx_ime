//! 从已加载的 [`MmapDict`] 直接在本地生成 `abbrev.bin` (v3) 与 `syllable_graph.bin` (v3)。
//!
//! 简拼副索引与音节图索引均是 `dict.bin` 的 100% 确定性派生索引，不含任何外部信息。
//! 由引擎在安装期（`--build-indexes`）或首次启动缺失时直接从 `dict.bin` 生成，
//! 省去安装包随包分发这两份索引的体积（合计约 51.4 MiB 原始 / 24.3 MiB xz）。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::Path;

use super::{
    DictError, DictStore, MmapDict, ABBREV_HEADER_LEN, ABBREV_MAGIC, ABBREV_VERSION, NO_ENTRY,
};

const GRAPH_MAGIC: &[u8; 8] = b"XWXSYL1\0";
const GRAPH_VERSION: u32 = 3;
const GRAPH_HEADER_LEN: usize = 60;

fn encode_v3_word_and_freq(
    word: &str,
    freq: u64,
    packed_syls: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), DictError> {
    let fq = freq.min(u32::MAX as u64) as u32;
    let wb = word.as_bytes();
    // 快速判定 1..=63 个 BMP CJK 汉字（UTF-8 每字 3 字节，首字节 0xE4..=0xE9）
    if wb.len() >= 3 && wb.len() <= 63 * 3 && wb.len().is_multiple_of(3) {
        let clen = wb.len() / 3;
        let mut all_cjk = true;
        let mut cps = [0u16; 63];
        for k in 0..clen {
            let b0 = wb[k * 3];
            let b1 = wb[k * 3 + 1];
            let b2 = wb[k * 3 + 2];
            if !(0xE4..=0xE9).contains(&b0) || (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 {
                all_cjk = false;
                break;
            }
            let cp = (((b0 & 0x0F) as u32) << 12)
                | (((b1 & 0x3F) as u32) << 6)
                | ((b2 & 0x3F) as u32);
            if !(0x4E00..=0x9FFF).contains(&cp) {
                all_cjk = false;
                break;
            }
            cps[k] = (cp - 0x4E00) as u16;
        }
        if all_cjk {
            let fcode: u8 = if fq == 100 {
                0
            } else if fq <= 0xFFFF {
                1
            } else {
                2
            };
            out.push((fcode << 6) | (clen as u8));
            for &off in &cps[..clen] {
                out.extend_from_slice(&off.to_le_bytes());
            }
            out.extend_from_slice(packed_syls);
            match fcode {
                1 => out.extend_from_slice(&(fq as u16).to_le_bytes()),
                2 => out.extend_from_slice(&fq.to_le_bytes()),
                _ => {}
            }
            return Ok(());
        }
    }
    if wb.len() > 255 {
        return Err(DictError::Corrupt("词条超过 255 字节"));
    }
    out.push(0xC0);
    out.push(wb.len() as u8);
    out.extend_from_slice(wb);
    out.extend_from_slice(packed_syls);
    out.extend_from_slice(&fq.to_le_bytes());
    Ok(())
}

fn write_atomic(dst: &Path, blob: &[u8]) -> Result<(), DictError> {
    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = dst.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(blob)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dst)?;
    Ok(())
}

impl MmapDict {
    /// 从当前主词库生成 `abbrev.bin` (v3) 并原子写入 `dst`。
    pub fn build_abbrev_file<P: AsRef<Path>>(&self, dst: P) -> Result<(), DictError> {
        let dst = dst.as_ref();
        let mut by_init: [BTreeSet<String>; 26] = std::array::from_fn(|_| BTreeSet::new());
        let mut kbuf = [0u8; 256];

        // 若是 v3 词库，直接从头部音节表初始化 26 桶；否则扫一遍 key 收集音节。
        if !self.syl_table.is_empty() {
            for s in &self.syl_table {
                let st = std::str::from_utf8(s)
                    .map_err(|_| DictError::Corrupt("音节不是 UTF-8"))?;
                if let Some(&b) = s.first() {
                    if b.is_ascii_lowercase() {
                        by_init[(b - b'a') as usize].insert(st.to_owned());
                    }
                }
            }
        } else {
            for i in 0..self.pinyin_count {
                let rec = self.record(i)?;
                let py_bytes = self.record_key(rec, &mut kbuf)?;
                let py = std::str::from_utf8(py_bytes)
                    .map_err(|_| DictError::Corrupt("拼音不是 UTF-8"))?;
                for syl in py.split(' ') {
                    if let Some(&b) = syl.as_bytes().first() {
                        if !b.is_ascii_lowercase() {
                            return Err(DictError::Corrupt("音节首字符非小写字母"));
                        }
                        by_init[(b - b'a') as usize].insert(syl.to_owned());
                    }
                }
            }
        }

        let buckets: [Vec<String>; 26] =
            std::array::from_fn(|idx| by_init[idx].iter().cloned().collect());
        let syl_id: [HashMap<&str, u8>; 26] = std::array::from_fn(|idx| {
            buckets[idx]
                .iter()
                .enumerate()
                .map(|(i, s)| (s.as_str(), i as u8))
                .collect()
        });

        let mut syl_table = Vec::with_capacity(2048);
        for b in &buckets {
            if b.len() >= 64 {
                return Err(DictError::Corrupt("单字母音节桶超出 6-bit 上限"));
            }
            syl_table.push(b.len() as u8);
            for s in b {
                syl_table.push(s.len() as u8);
                syl_table.extend_from_slice(s.as_bytes());
            }
        }

        // 每条 key 预先打包好 6-bit 音节流，词条只存 (word, key_packed_off, freq)
        let mut packed_pool: Vec<u8> = Vec::with_capacity(self.pinyin_count * 4);
        let mut abbrev: BTreeMap<Vec<u8>, Vec<(String, u32, u64)>> = BTreeMap::new();
        let mut n_assoc = 0u64;
        let mut ik = Vec::with_capacity(16);
        let mut packed = [0u8; 64];

        for i in 0..self.pinyin_count {
            let rec = self.record(i)?;
            let py_bytes = self.record_key(rec, &mut kbuf)?;
            let py = std::str::from_utf8(py_bytes)
                .map_err(|_| DictError::Corrupt("拼音不是 UTF-8"))?;
            ik.clear();
            for syl in py.split(' ') {
                if let Some(&b) = syl.as_bytes().first() {
                    if !b.is_ascii_lowercase() {
                        return Err(DictError::Corrupt("音节首字符非小写字母"));
                    }
                    ik.push(b);
                }
            }
            if ik.is_empty() {
                continue;
            }
            let sl = ik.len();
            let syl_bytes = (sl * 6).div_ceil(8);
            packed[..syl_bytes].fill(0);
            for (k, syl) in py.split(' ').filter(|s| !s.is_empty()).enumerate() {
                let b0 = syl.as_bytes()[0];
                let sid = syl_id[(b0 - b'a') as usize][syl];
                let bit_off = k * 6;
                let b_idx = bit_off >> 3;
                let shift = bit_off & 7;
                let v = ((sid & 0x3F) as u16) << shift;
                packed[b_idx] |= (v & 0xFF) as u8;
                if b_idx + 1 < syl_bytes {
                    packed[b_idx + 1] |= (v >> 8) as u8;
                }
            }
            let pool_off = packed_pool.len() as u32;
            packed_pool.extend_from_slice(&packed[..syl_bytes]);

            let entries = self.parse_entries(self.entries_off + rec.ent_off, rec.ent_len)?;
            let bucket = if let Some(b) = abbrev.get_mut(ik.as_slice()) {
                b
            } else {
                abbrev.entry(ik.clone()).or_default()
            };
            for (w, fq) in entries {
                bucket.push((w, pool_off, fq));
                n_assoc += 1;
            }
        }

        let key_count = abbrev.len();
        let mut records = Vec::with_capacity((key_count + 1) * 8);
        let mut strings = Vec::with_capacity(key_count * 5);
        let mut entries = Vec::with_capacity(n_assoc as usize * 14);

        for (ik_bytes, mut list) in abbrev {
            records.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            records.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            strings.extend_from_slice(&ik_bytes);
            list.sort_by_key(|b| std::cmp::Reverse(b.2));
            let syl_bytes = (ik_bytes.len() * 6).div_ceil(8);
            for (word, pool_off, freq) in list {
                let ps = &packed_pool[pool_off as usize..pool_off as usize + syl_bytes];
                encode_v3_word_and_freq(&word, freq, ps, &mut entries)?;
            }
        }
        records.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        records.extend_from_slice(&(entries.len() as u32).to_le_bytes());

        let records_off = (ABBREV_HEADER_LEN + syl_table.len()) as u64;
        let strings_off = records_off + records.len() as u64;
        let entries_off = strings_off + strings.len() as u64;

        let mut out = Vec::with_capacity(entries_off as usize + entries.len());
        out.extend_from_slice(ABBREV_MAGIC);
        out.extend_from_slice(&ABBREV_VERSION.to_le_bytes());
        out.extend_from_slice(&(key_count as u64).to_le_bytes());
        out.extend_from_slice(&n_assoc.to_le_bytes());
        out.extend_from_slice(&records_off.to_le_bytes());
        out.extend_from_slice(&strings_off.to_le_bytes());
        out.extend_from_slice(&entries_off.to_le_bytes());
        out.extend_from_slice(&syl_table);
        out.extend_from_slice(&records);
        out.extend_from_slice(&strings);
        out.extend_from_slice(&entries);

        write_atomic(dst, &out)
    }

    /// 从当前主词库生成 `syllable_graph.bin` (v3) 并原子写入 `dst`。
    pub fn build_syllable_graph_file<P: AsRef<Path>>(&self, dst: P) -> Result<(), DictError> {
        struct BuildNode {
            children: Vec<(u16, u32)>,
            exact: bool,
            ent: u32,
            ent_len: u32,
        }

        let dst = dst.as_ref();
        let mut nodes = Vec::with_capacity(self.pinyin_count * 2);
        nodes.push(BuildNode {
            children: Vec::new(),
            exact: false,
            ent: 0,
            ent_len: 0,
        });
        let mut stack: Vec<(u16, usize)> = Vec::with_capacity(32);
        let mut prev: Vec<u16> = Vec::with_capacity(32);
        let mut cur_parts: Vec<u16> = Vec::with_capacity(32);
        let mut temp_names: Vec<String> = Vec::with_capacity(420);
        let mut temp_id: HashMap<String, u16> = HashMap::with_capacity(420);
        let mut kbuf = [0u8; 256];

        for i in 0..self.pinyin_count {
            let rec = self.record(i)?;
            let py_bytes = self.record_key(rec, &mut kbuf)?;
            let py = std::str::from_utf8(py_bytes)
                .map_err(|_| DictError::Corrupt("拼音不是 UTF-8"))?;
            cur_parts.clear();
            for syl in py.split(' ') {
                let tid = if let Some(&id) = temp_id.get(syl) {
                    id
                } else {
                    let id = temp_names.len() as u16;
                    temp_names.push(syl.to_owned());
                    temp_id.insert(syl.to_owned(), id);
                    id
                };
                cur_parts.push(tid);
            }
            let mut common = 0usize;
            let limit = prev.len().min(cur_parts.len());
            while common < limit && prev[common] == cur_parts[common] {
                common += 1;
            }
            stack.truncate(common);
            let mut parent = stack.last().map_or(0, |&(_, idx)| idx);
            for &tid in &cur_parts[common..] {
                let child = nodes.len();
                nodes.push(BuildNode {
                    children: Vec::new(),
                    exact: false,
                    ent: 0,
                    ent_len: 0,
                });
                nodes[parent].children.push((tid, child as u32));
                stack.push((tid, child));
                parent = child;
            }
            let node = &mut nodes[parent];
            let idx_u32 = i as u32;
            if !node.exact || node.ent + node.ent_len != idx_u32 {
                node.exact = true;
                node.ent = idx_u32;
                node.ent_len = 1;
            } else {
                node.ent_len += 1;
            }
            std::mem::swap(&mut prev, &mut cur_parts);
        }

        let mut names = temp_names.clone();
        names.sort();
        let sid_map: HashMap<&str, u32> = names
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i as u32))
            .collect();
        let tid_to_sid: Vec<u32> = temp_names
            .iter()
            .map(|s| sid_map[s.as_str()])
            .collect();

        let mut new_id = vec![u32::MAX; nodes.len()];
        let mut internal_indices = Vec::with_capacity(nodes.len() / 2);
        for (idx, n) in nodes.iter().enumerate() {
            if !n.children.is_empty() || idx == 0 {
                new_id[idx] = internal_indices.len() as u32;
                internal_indices.push(idx);
            }
        }
        let internal_count = internal_indices.len();

        let mut nodes_bytes = Vec::with_capacity(internal_count * 8);
        let mut edges_bytes = Vec::with_capacity(nodes.len() * 4);
        let mut cur_edge_off = 0u32;

        for &orig_idx in &internal_indices {
            let n = &nodes[orig_idx];
            let clen = n.children.len() as u32;
            if cur_edge_off >= (1 << 22) || clen >= (1 << 10) {
                return Err(DictError::Corrupt("音节图子表偏移/长度超出位宽"));
            }
            let mut kids: Vec<(u32, u32)> = n
                .children
                .iter()
                .map(|&(tid, child)| {
                    let s = tid_to_sid[tid as usize];
                    let c_idx = child as usize;
                    let c_enc = if new_id[c_idx] != u32::MAX {
                        new_id[c_idx]
                    } else {
                        internal_count as u32 + nodes[c_idx].ent
                    };
                    (s, c_enc)
                })
                .collect();
            kids.sort_unstable();
            let packed_node = (cur_edge_off << 10) | clen;
            let ent_field = if n.exact { n.ent } else { NO_ENTRY };
            nodes_bytes.extend_from_slice(&packed_node.to_le_bytes());
            nodes_bytes.extend_from_slice(&ent_field.to_le_bytes());
            for (s, c_enc) in kids {
                let packed_edge = (s << 23) | (c_enc & 0x7F_FFFF);
                edges_bytes.extend_from_slice(&packed_edge.to_le_bytes());
            }
            cur_edge_off += clen;
        }

        let mut syl_bytes = Vec::with_capacity(names.len() * 6);
        for s in &names {
            syl_bytes.extend_from_slice(&(s.len() as u16).to_le_bytes());
            syl_bytes.extend_from_slice(s.as_bytes());
        }

        let node_off = GRAPH_HEADER_LEN as u64;
        let edge_off = node_off + nodes_bytes.len() as u64;
        let syl_off = edge_off + edges_bytes.len() as u64;

        let mut out = Vec::with_capacity(syl_off as usize + syl_bytes.len());
        out.extend_from_slice(GRAPH_MAGIC);
        out.extend_from_slice(&GRAPH_VERSION.to_le_bytes());
        out.extend_from_slice(&(internal_count as u64).to_le_bytes());
        out.extend_from_slice(&(names.len() as u64).to_le_bytes());
        out.extend_from_slice(&node_off.to_le_bytes());
        out.extend_from_slice(&edge_off.to_le_bytes());
        out.extend_from_slice(&syl_off.to_le_bytes());
        out.extend_from_slice(&(self.pinyin_count as u64).to_le_bytes());
        out.extend_from_slice(&nodes_bytes);
        out.extend_from_slice(&edges_bytes);
        out.extend_from_slice(&syl_bytes);

        write_atomic(dst, &out)
    }
}

impl DictStore {
    /// 从当前主词库生成 `abbrev.bin` 与 `syllable_graph.bin`（仅 `Mmap` 变体有效）。
    pub fn build_companion_indexes(
        &self,
        abbrev_dst: Option<&Path>,
        graph_dst: Option<&Path>,
    ) -> Result<(), DictError> {
        match self {
            Self::Mmap(d) => {
                if let Some(p) = abbrev_dst {
                    d.build_abbrev_file(p)?;
                }
                if let Some(p) = graph_dst {
                    d.build_syllable_graph_file(p)?;
                }
                Ok(())
            }
            Self::Sqlite(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::tests::build_test_bin;
    use crate::dict::Dictionary;

    #[test]
    fn builds_abbrev_and_syllable_graph_directly_from_dict() {
        let dir = std::env::temp_dir().join(format!("xwx_builder_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dict_p = dir.join("dict.bin");
        let abbrev_p = dir.join("abbrev.bin");
        let graph_p = dir.join("syllable_graph.bin");

        let blob = build_test_bin(2);
        fs::write(&dict_p, &blob).unwrap();
        let dict = MmapDict::open(&dict_p).unwrap();
        dict.build_abbrev_file(&abbrev_p).unwrap();
        dict.build_syllable_graph_file(&graph_p).unwrap();

        let dict = dict.with_abbrev(&abbrev_p).with_syllable_graph(&graph_p);
        assert!(dict.has_abbrev());
        assert!(dict.syllable_graph().is_some());
        let cands = dict.lookup_abbrev("nh").unwrap();
        assert_eq!(cands[0].word, "你好");
        let cands_nh = dict.lookup("ni hao").unwrap();
        assert_eq!(cands_nh[0].word, "你好");

        let _ = fs::remove_dir_all(&dir);
    }
}
