//! mmap 访问模式的建议（`madvise`）。
//!
//! 只读索引都是**随机访问**：二分查找跨页跳、trie 沿子节点走。这类访问下
//! 内核默认策略会把常驻放大成几倍到十几倍，两种机制各占一半：
//!
//! - **fault-around**：一次缺页顺手映射周边 16 页（64KB）；
//! - **large folio**：页缓存按 64KB 分配，碰一个字节就拉起整个 folio。
//!
//! 声明 `MADV_RANDOM` 两者都关掉（`fault_around_bytes=0` + 退回 4KB folio）。
//! 实测（冷页、`POSIX_FADV_DONTNEED` 逐出后逐字节读，本机 7.0 内核）：
//!
//! | 数据 | 默认 | `MADV_RANDOM` |
//! | --- | --- | --- |
//! | 单字节首缺页 | 64 KB | 8 KB |
//! | 每后续页 | 64 KB 起、每 16 页再 +64 | 4 KB |
//! | `.gram` 5000 次随机页读 | 203 MB | 107 MB |
//! | `dict.bin` 2000 次随机页读 | 57.8 MB | 7.6 MB |
//!
//! **必须用冷页量**：页缓存热的时候两种策略读数完全一样（读到的都是
//! 已在缓存里的页），据此下「madvise 没用」的结论是错的——这一点踩过。
//!
//! 建议失败不影响正确性（纯提示），所以这里吞掉错误：`madvise` 在个别
//! 文件系统上会返回 `EINVAL`，不该让它挡住引擎启动。

use memmap2::Mmap;

/// 把整段映射标记为随机访问。
#[cfg(unix)]
pub(crate) fn advise_random(map: &Mmap) {
    let _ = map.advise(memmap2::Advice::Random);
}

/// 非 Unix 平台没有 `madvise`（本项目只有 Linux/IBus 一个目标）。
#[cfg(not(unix))]
pub(crate) fn advise_random(_map: &Mmap) {}
