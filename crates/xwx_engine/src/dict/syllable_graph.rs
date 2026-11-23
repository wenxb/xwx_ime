//! 音节级词典转移索引(`syllable_graph.bin`,脚本 `scripts/build_syllable_graph.py`)。
//!
//! 缩写弧建边要在「当前已拼到哪个音节链」上反复问两件事：哪些音节能接在后面、
//! 这条链本身是不是完整词条。没有索引时只能拿音节表逐个去 186 万条 key 上二分
//! (实测 `womenjintqubeij` 一次输入约 1.2 万次 ≈10.5ms)；索引把这两件事变成
//! 内存树walk：子节点就是词库里真实存在的后继音节。
//!
//! 索引是**可选**数据：文件缺失/损坏/与当前词库不同代时调用方回退旧匹配器，
//! 所以这里只报错、不 panic，也不替调用方决定要不要降级。

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use super::{DictError, read_u16, read_u32, read_u64};

const MAGIC: &[u8; 8] = b"XWXSYL1\0";
const MIN_VERSION: u32 = 1;
const VERSION: u32 = 3;
const HEADER_LEN: usize = 60;
const NODE_LEN_V1: usize = 16;
const EDGE_LEN_V1: usize = 6;
const NODE_LEN_V2: usize = 8;
const EDGE_LEN_V2: usize = 4;
/// 词条区间的空标记(非完整词条的节点)。
pub const NO_ENTRY: u32 = u32::MAX;
/// 音节名条数上限：正常 412 条。损坏文件不该让我们按它去分配巨大向量。
const MAX_SYLLABLES: u64 = 4096;

/// 一个音节节点的只读视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphNode {
    children_off: u32,
    pub children_len: u16,
    /// 该音节链本身就是一条完整词条。
    pub exact: bool,
    /// 完整词条时是 dict.bin 的记录号；否则为 [`NO_ENTRY`]。
    pub entry_off: u32,
    pub entry_len: u16,
}

/// 音节级转移索引的 mmap 视图。
#[derive(Debug)]
pub struct SyllableGraph {
    map: Mmap,
    version: u32,
    node_count: usize,
    keys: u64,
    node_off: usize,
    edge_off: usize,
    names: Vec<String>,
    ids: HashMap<String, u16>,
}

impl SyllableGraph {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DictError> {
        let file = File::open(path)?;
        let map = unsafe { Mmap::map(&file)? };
        let bytes: &[u8] = &map;
        if bytes.len() < HEADER_LEN {
            return Err(DictError::Corrupt("文件过短，不是完整 syllable_graph.bin"));
        }
        if bytes.get(0..8) != Some(MAGIC.as_slice()) {
            return Err(DictError::Corrupt("魔数不匹配，不是音节级索引"));
        }
        let version = read_u32(bytes, 8)?;
        if !(MIN_VERSION..=VERSION).contains(&version) {
            return Err(DictError::Corrupt("音节级索引版本不兼容"));
        }
        let node_len = if version >= 2 { NODE_LEN_V2 } else { NODE_LEN_V1 };
        let node_count = read_u64(bytes, 12)? as usize;
        let syllable_count = read_u64(bytes, 20)?;
        let node_off = read_u64(bytes, 28)? as usize;
        let edge_off = read_u64(bytes, 36)? as usize;
        let syl_off = read_u64(bytes, 44)? as usize;
        let keys = read_u64(bytes, 52)?;
        if syllable_count > MAX_SYLLABLES {
            return Err(DictError::Corrupt("音节名条数超出上限，头部可疑"));
        }
        // 区段顺序先证明，后面的加法才有意义(与 MmapDict::from_bytes 同一套判据)。
        if !(HEADER_LEN <= node_off && node_off <= edge_off && edge_off <= syl_off && syl_off <= bytes.len()) {
            return Err(DictError::Corrupt("区段顺序/范围不合法: node/edge/syllable 起点必须递增且落在文件内"));
        }
        let node_bytes = node_count
            .checked_mul(node_len)
            .ok_or_else(|| DictError::Corrupt("节点数溢出"))?;
        if node_off + node_bytes > edge_off {
            return Err(DictError::Corrupt("节点数组越界: 节点数 × 记录长度超出边表区"));
        }
        let mut names = Vec::with_capacity(syllable_count as usize);
        let mut cursor = syl_off;
        for _ in 0..syllable_count {
            let len = read_u16(bytes, cursor)? as usize;
            cursor += 2;
            let raw = bytes
                .get(cursor..cursor + len)
                .ok_or(DictError::Corrupt("音节名表越界"))?;
            let name = std::str::from_utf8(raw)
                .map_err(|_| DictError::Corrupt("音节名不是合法 UTF-8"))?;
            names.push(name.to_string());
            cursor += len;
        }
        let ids = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as u16))
            .collect();
        Ok(Self {
            map,
            version,
            node_count,
            keys,
            node_off,
            edge_off,
            names,
            ids,
        })
    }

    /// 生成该索引时词库的记录数：与当前词库不一致即视为不同代。
    pub fn keys(&self) -> u64 {
        self.keys
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }

    pub fn syllable_count(&self) -> usize {
        self.names.len()
    }

    /// 第 `idx` 个节点(0 号是根)。
    pub fn node(&self, idx: u32) -> Result<GraphNode, DictError> {
        if self.version >= 3 && (idx as usize) >= self.node_count {
            let entry_off = idx - self.node_count as u32;
            if (entry_off as u64) >= self.keys {
                return Err(DictError::Corrupt("叶子节点记录号越界"));
            }
            return Ok(GraphNode {
                children_off: 0,
                children_len: 0,
                exact: true,
                entry_off,
                entry_len: 1,
            });
        }
        let node_len = if self.version >= 2 { NODE_LEN_V2 } else { NODE_LEN_V1 };
        let base = self
            .node_off
            .checked_add(idx as usize * node_len)
            .ok_or(DictError::Corrupt("节点偏移溢出"))?;
        let b = self.bytes(base, node_len)?;
        if self.version >= 2 {
            let packed = read_u32(b, 0)?;
            let entry_off = read_u32(b, 4)?;
            let exact = entry_off != NO_ENTRY;
            Ok(GraphNode {
                children_off: packed >> 10,
                children_len: (packed & 0x3FF) as u16,
                exact,
                entry_off,
                entry_len: u16::from(exact),
            })
        } else {
            Ok(GraphNode {
                children_off: read_u32(b, 0)?,
                children_len: read_u16(b, 4)?,
                exact: b[6] & 1 != 0,
                entry_off: read_u32(b, 8)?,
                entry_len: read_u16(b, 12)?,
            })
        }
    }

    /// 子音节查表：子表按音节名升序，二分即可。
    pub fn child(&self, idx: u32, syllable: &str) -> Result<Option<u32>, DictError> {
        let Some(want) = self.ids.get(syllable).copied() else {
            return Ok(None);
        };
        let node = self.node(idx)?;
        let (mut lo, mut hi) = (0u16, node.children_len);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (sid, child) = self.edge(node.children_off, mid)?;
            match sid.cmp(&want) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(child)),
            }
        }
        Ok(None)
    }

    /// 从节点枚举「以 `typed` 开头的后继音节」——缩写弧的半音节展开。
    ///
    /// 扇出是词库里真实存在的后继音节数(根 412，深层通常个位数)，所以直接线性
    /// 过滤：这正是替换「拿整张音节表逐个去词库二分」的那一步。
    pub fn children_starting_with(
        &self,
        idx: u32,
        typed: &str,
    ) -> Result<Vec<(String, u32)>, DictError> {
        let node = self.node(idx)?;
        let mut out = Vec::new();
        for i in 0..node.children_len {
            let (sid, child) = self.edge(node.children_off, i)?;
            let name = &self.names[sid as usize];
            if name.starts_with(typed) {
                out.push((name.clone(), child));
            }
        }
        Ok(out)
    }

    /// 整条音节链走一遍；链不存在返回 `None`。
    pub fn walk(&self, syllables: &[&str]) -> Result<Option<u32>, DictError> {
        let mut cur = 0u32;
        for s in syllables {
            match self.child(cur, s)? {
                Some(c) => cur = c,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// 按空格分隔的全拼键走完整条链，命中完整词条时返回该节点。
    pub fn walk_key(&self, pinyin: &str) -> Result<Option<GraphNode>, DictError> {
        if pinyin.is_empty() {
            return Ok(None);
        }
        let mut cur = 0u32;
        for s in pinyin.split(' ') {
            match self.child(cur, s)? {
                Some(c) => cur = c,
                None => return Ok(None),
            }
        }
        let node = self.node(cur)?;
        Ok(node.exact.then_some(node))
    }

    fn edge(&self, off: u32, i: u16) -> Result<(u16, u32), DictError> {
        let edge_len = if self.version >= 2 { EDGE_LEN_V2 } else { EDGE_LEN_V1 };
        let base = self
            .edge_off
            .checked_add((off as usize + i as usize) * edge_len)
            .ok_or(DictError::Corrupt("边偏移溢出"))?;
        let b = self.bytes(base, edge_len)?;
        if self.version >= 2 {
            let packed = read_u32(b, 0)?;
            Ok(((packed >> 23) as u16, packed & 0x7F_FFFF))
        } else {
            Ok((read_u16(b, 0)?, read_u32(b, 2)?))
        }
    }

    fn bytes(&self, off: usize, len: usize) -> Result<&[u8], DictError> {
        let end = off.checked_add(len).ok_or(DictError::Corrupt("区段长度溢出"))?;
        self.map.get(off..end).ok_or(DictError::Corrupt("区段越界"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::tests::build_test_bin;
    use crate::dict::MmapDict;

    #[test]
    fn synthetic_graph_walks_expands_and_enforces_generation() {
        let dir = std::env::temp_dir().join(format!("xwx_syl_graph_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dict_p = dir.join("dict.bin");
        let graph_p = dir.join("syllable_graph.bin");
        std::fs::write(&dict_p, build_test_bin(2)).unwrap();

        let dict = MmapDict::open(&dict_p).unwrap();
        dict.build_syllable_graph_file(&graph_p).unwrap();
        let g = SyllableGraph::open(&graph_p).expect("音节级索引应能打开");
        assert_eq!(g.syllable_count(), 2);
        assert_eq!(g.keys(), dict.pinyin_count as u64);

        // walk / children_starting_with / walk_key / 不存在音节
        let ni = g.walk(&["ni"]).unwrap().expect("ni 应在索引里");
        let ni_node = g.node(ni).unwrap();
        assert!(ni_node.exact && ni_node.entry_off != NO_ENTRY && ni_node.entry_len == 1);
        let next: Vec<(String, u32)> = g.children_starting_with(ni, "h").unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].0, "hao");
        let leaf = g.node(next[0].1).unwrap();
        assert!(leaf.exact && leaf.entry_off != NO_ENTRY);
        assert_eq!(g.walk_key("ni hao").unwrap(), Some(leaf));
        assert!(g.child(0, "zzz").unwrap().is_none());

        // 同代装配成功
        let attached = dict.with_syllable_graph(&graph_p);
        assert!(attached.syllable_graph().is_some());
        assert_eq!(attached.syllable_graph().unwrap().keys(), attached.pinyin_count as u64);

        // 异代(改 keys 计数)与非索引文件均不装配、不 panic
        let mut wrong_gen = std::fs::read(&graph_p).unwrap();
        let bad_keys = (attached.pinyin_count as u64) + 1;
        wrong_gen[52..60].copy_from_slice(&bad_keys.to_le_bytes());
        let wrong_p = dir.join("wrong_gen.bin");
        std::fs::write(&wrong_p, &wrong_gen).unwrap();
        assert!(MmapDict::open(&dict_p).unwrap().with_syllable_graph(&wrong_p).syllable_graph().is_none());
        assert!(MmapDict::open(&dict_p).unwrap().with_syllable_graph(&dict_p).syllable_graph().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_files_are_rejected() {
        let dir = std::env::temp_dir();
        let tag = std::process::id();
        let cases: [(&str, Vec<u8>); 3] = [
            ("short", vec![0u8; 8]),
            ("magic", {
                let mut b = vec![0u8; HEADER_LEN];
                b[0..8].copy_from_slice(b"NOTXWX1\0");
                b
            }),
            ("offsets", {
                let mut b = vec![0u8; HEADER_LEN];
                b[0..8].copy_from_slice(MAGIC);
                b[8..12].copy_from_slice(&VERSION.to_le_bytes());
                // node_off 指向文件外
                b[28..36].copy_from_slice(&(u64::MAX / 2).to_le_bytes());
                b
            }),
        ];
        for (name, blob) in cases {
            let p = dir.join(format!("xwx_syl_{tag}_{name}.bin"));
            std::fs::write(&p, &blob).unwrap();
            let err = SyllableGraph::open(&p).err();
            let _ = std::fs::remove_file(&p);
            assert!(err.is_some(), "{name} 应被拒绝");
        }
    }
}
