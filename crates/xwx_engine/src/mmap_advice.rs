//! mmap 访问模式的建议（`madvise`）。
//!
//! 只读索引都是**随机访问**：二分查找跨页跳、trie 沿子节点走。内核默认的
//! fault-around（一次缺页顺手映射周边 16 页）对这类访问几乎全是浪费。
//! 声明 `MADV_RANDOM` 可关掉它。
//!
//! # 实测（本机 ext4，冷页）
//!
//! 量的时候必须先 `POSIX_FADV_DONTNEED` 逐出页缓存：**页缓存热时两种策略
//! 读数完全一样**（碰到的都是已在缓存里的页），据此判「madvise 没用」是错的
//! —— 这一点踩过一次。
//!
//! | 数据 | 访问模式 | 默认 | `MADV_RANDOM` |
//! | --- | --- | --- | --- |
//! | `.gram`(400MB) | 2000 次随机页读 | 102 MB | **8.9 MB** |
//! | `dict.bin`(76MB) | 2000 次随机页读 | 72 MB | 63 MB |
//! | `dict.bin` | 每 1MB 读一页 | 56 MB | 56 MB |
//!
//! `.gram` 收益极大（-91%），因为大模型查询是纯随机点查。
//! `dict.bin` 收益有限（-11%~13%），且**不在启动路径上**：它的常驻是随
//! 使用逐步涨的（前缀区间扫描会扫整段 strings），属于「用过的页留在
//! 缓存里」，不是启动就付出的成本。
//!
//! 注：tmpfs（`/tmp`）上的行为与 ext4 不同，别拿 `/tmp` 的副本下结论——
//! 两种文件系统的 folio 粒度不一样。
//!
//! 建议失败不影响正确性（纯性能提示），所以这里吞掉错误：`madvise` 在个别
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
