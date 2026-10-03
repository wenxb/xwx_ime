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
const VERSION: u32 = 1;
const HEADER_LEN: usize = 60;
const NODE_LEN: usize = 16;
const EDGE_LEN: usize = 6;
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
        if read_u32(bytes, 8)? != VERSION {
            return Err(DictError::Corrupt("音节级索引版本不兼容"));
        }
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
            .checked_mul(NODE_LEN)
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
        let base = self
            .node_off
            .checked_add(idx as usize * NODE_LEN)
            .ok_or(DictError::Corrupt("节点偏移溢出"))?;
        let b = self.bytes(base, NODE_LEN)?;
        Ok(GraphNode {
            children_off: read_u32(b, 0)?,
            children_len: read_u16(b, 4)?,
            exact: b[6] & 1 != 0,
            entry_off: read_u32(b, 8)?,
            entry_len: read_u16(b, 12)?,
        })
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

    fn edge(&self, off: u32, i: u16) -> Result<(u16, u32), DictError> {
        let base = self
            .edge_off
            .checked_add((off as usize + i as usize) * EDGE_LEN)
            .ok_or(DictError::Corrupt("边偏移溢出"))?;
        let b = self.bytes(base, EDGE_LEN)?;
        Ok((read_u16(b, 0)?, read_u32(b, 2)?))
    }

    fn bytes(&self, off: usize, len: usize) -> Result<&[u8], DictError> {
        let end = off.checked_add(len).ok_or(DictError::Corrupt("区段长度溢出"))?;
        self.map.get(off..end).ok_or(DictError::Corrupt("区段越界"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dict_path() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/dict.bin");
        p.exists().then_some(p)
    }

    fn data_path() -> Option<PathBuf> {
        // 单元测试的 CARGO_MANIFEST_DIR 是 crate 目录(集成测试才是仓库根)。
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/syllable_graph.bin");
        p.exists().then_some(p)
    }

    #[test]
    fn target_forms_are_reachable_in_the_graph() {
        let Some(path) = data_path() else {
            eprintln!("跳过：没有 data/syllable_graph.bin，先跑 scripts/build_syllable_graph.py");
            return;
        };
        let g = SyllableGraph::open(&path).expect("音节级索引应能打开");
        assert!(g.syllable_count() > 400, "音节表应覆盖全部音节");
        // 「我们今天」的链存在；半音节 t 的后继音节只该有 tian
        let n = g
            .walk(&["wo", "men", "jin"])
            .unwrap()
            .expect("wo men jin 应在索引里");
        let typed: Vec<String> = g
            .children_starting_with(n, "t")
            .unwrap()
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(typed, vec!["tian".to_string()]);
        // 目标整词必须是完整词条，且带 dict.bin 的记录号(不是空标记)
        for key in [["zhuang", "tai", "lan"], ["zen", "me", "gai"]] {
            let idx = g
                .walk(&key)
                .unwrap()
                .unwrap_or_else(|| panic!("{key:?} 应在索引里"));
            let node = g.node(idx).unwrap();
            assert!(node.exact, "{key:?} 应是完整词条");
            assert_ne!(node.entry_off, NO_ENTRY, "{key:?} 应带词条区间");
            assert!(node.entry_len >= 1);
        }
        assert!(g.child(0, "zzz").unwrap().is_none(), "不存在的音节不该有子节点");
    }

    #[test]
    fn graph_is_assembled_only_when_it_matches_the_dict() {
        let (Some(graph_path), Some(dict_path)) = (data_path(), dict_path()) else {
            eprintln!("跳过：缺少 data/syllable_graph.bin 或 data/dict.bin");
            return;
        };
        let d = crate::dict::MmapDict::open(&dict_path)
            .unwrap()
            .with_syllable_graph(&graph_path);
        assert!(d.syllable_graph().is_some(), "同代索引应被装配");
        // 装配判据就是这一条：索引头部记录数 == 词库记录数
        assert_eq!(d.syllable_graph().unwrap().keys(), d.pinyin_count as u64);
        // 拿词库文件冒充索引(魔数不符) → 不装配、不 panic
        let bad = crate::dict::MmapDict::open(&dict_path)
            .unwrap()
            .with_syllable_graph(&dict_path);
        assert!(bad.syllable_graph().is_none(), "不是索引的文件不该被装配");
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
