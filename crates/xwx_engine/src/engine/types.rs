//! 引擎聚合结构体。
//!
//! 从 `engine/mod.rs` 移出：那里还要放常量与子模块声明，聚合体单独成文件
//! 才好继续加字段（本任务的英文组件就挂在这里）。

use super::*;

use crate::english::EnglishEngine;

/// 引擎聚合体：切分器 + 词库 + 语言模型 + 模糊拼音配置。
///
/// 结构上刻意用具体类型（而非 `Arc<dyn Dictionary>`）：
/// 当前引擎是单线程同步模型，具体类型让字段访问和测试都更直接。
/// TODO: 若未来需要插件化词库/模型，再引入 trait object 或泛型。
#[derive(Debug)]
pub struct ImeEngine {
    /// 拼音切分器。
    pub segmenter: Segmenter,
    /// 词库（运行时为 mmap 只读二进制；`new(None)` 时是内存 SQLite）。
    pub dict: DictStore,
    /// 语言模型（`LmStore` 枚举聚合，当前为 bigram 实现）。
    pub lm: LmStore,
    /// LM 得分的权重（默认 [`DEFAULT_LM_WEIGHT`]；评测扫参时由调用方改）。
    pub lm_weight: f32,
    /// 模糊拼音混淆对。
    pub fuzzy_pairs: Vec<ConfusionPair>,
    /// 是否启用模糊拼音。
    pub fuzzy_enabled: bool,
    /// 可选英文组件（`None` = 英文关闭：查询、学习、按键透传全部保持旧行为）。
    ///
    /// 裸构造器（[`ImeEngine::new`] / `new_with_lm`）**不**装配它——生产入口
    /// 用 [`ImeEngine::with_english`] 显式接上，测试与探针同理。
    pub english: Option<EnglishEngine>,
    /// **跨语言共享**的最近一次删除记录（Ctrl+Shift+Delete 撤销用）。
    ///
    /// 为什么必须共享：中英各有一个用户实例，各自的「最近一条」槽位互不
    /// 知道对方——用户先删中文、再删英文，撤销时按当前高亮的语言猜就会
    /// 撤错那一边（会话分册第 8 节明确禁止这种做法）。
    ///
    /// 锁顺序固定为 **undo → UserLM**：删除与撤销在这里串行化，查询路径
    /// **不取这把锁**，UserLM 回调也不反向取它。
    pub undo: std::sync::Mutex<Option<UndoRecord>>,
}

/// 删除发生在哪条通道上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteLanguage {
    /// 中文：屏蔽键是 `(拼音 key, 词)`。
    Chinese,
    /// 英文：屏蔽键是 `(目标完整小写 key, "__english__")`。
    English,
}

/// 共享撤销记录：**最近一次真正发生的删除**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoRecord {
    /// 删除发生在哪条通道。
    pub language: DeleteLanguage,
    /// 被删的词面（英文是显示形式，中文是词）。
    pub word: String,
    /// 屏蔽键的另一半（中文是拼音 key，英文是英文命名空间）。
    pub key: String,
}
