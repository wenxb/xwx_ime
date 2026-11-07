//! 大体积稀疏随机索引（`.gram`）的 `madvise` 建议。
//!
//! `.gram`（400MB+）是纯随机点查的 Double-Array Trie，声明 `MADV_RANDOM`
//! 可关掉内核 64KB fault-around 与预读（ext4 冷页实测 2000 次随机读常驻
//! 102MB → 8.9MB）。
//!
//! 词库类 `.bin`（`dict.bin` / `abbrev.bin` / `syllable_graph.bin` /
//! `english*.bin`）具有明显的区间顺序扫描或 DFS 局部性，且 `english*.bin`
//! 在打开时即做全表线性校验；对它们声明 `MADV_RANDOM` 会关掉 `do_fault_around`
//! 与预读，把连续区间访问打碎成单页 4KB 同步阻塞读，因此不用于词库文件。
use memmap2::Mmap;

/// 把整段映射标记为随机访问。
#[cfg(unix)]
pub(crate) fn advise_random(map: &Mmap) {
    let _ = map.advise(memmap2::Advice::Random);
}

/// 非 Unix 平台没有 `madvise`（本项目只有 Linux/IBus 一个目标）。
#[cfg(not(unix))]
pub(crate) fn advise_random(_map: &Mmap) {}
