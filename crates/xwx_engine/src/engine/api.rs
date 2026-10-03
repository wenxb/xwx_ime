//! `ImeEngine` 的公开 API:构造、会话创建、用户模型接线、学习/造词/删除入口。

use super::*;

impl ImeEngine {
    /// 创建引擎。
    ///
    /// - `dict_path` 为 `Some(path)` 时以只读 mmap 方式打开该 dict.bin
    ///   词库；打开失败（文件缺失/损坏）仅告警，退回内存空词库——引擎
    ///   不会写入或创建任何文件；
    /// - `None` 时使用内存词库（测试/临时用）。
    ///
    /// 等价于 `new_with_lm(dict_path, None)`：不加载语言模型文件时使用
    /// 空 bigram（候选排序仅靠词频）。
    pub fn new(dict_path: Option<&Path>) -> Result<Self, EngineError> {
        Self::new_with_lm(dict_path, None)
    }

    /// 创建引擎并加载语言模型。
    ///
    /// `lm_path` 为：
    /// - `Some(path)` → 把该路径当作万象 `.gram` 文件 mmap 打开（调用方
    ///   负责判断扩展名；`src/main.rs` 只在后缀为 `gram` 时走这里）；
    /// - `None` → 与 [`Self::new`] 相同（空 bigram，由调用方自行
    ///   `engine.lm.load(...)` 加载 bigram 文本）。
    ///
    /// **语言模型文件缺失/损坏时优雅退化**：仅记录 warning 并退回空
    /// `BigramLM`，绝不 panic——不是所有用户都会下载那个 400MB 文件。
    pub fn new_with_lm(
        dict_path: Option<&Path>,
        lm_path: Option<&Path>,
    ) -> Result<Self, EngineError> {
        let dict = match dict_path {
            Some(path) => match MmapDict::open(path) {
                Ok(dict) => DictStore::Mmap(dict),
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "mmap 词库打开失败，退回内存空词库");
                    DictStore::Sqlite(SqliteDict::open_in_memory()?)
                }
            },
            None => DictStore::Sqlite(SqliteDict::open_in_memory()?),
        };
        let lm = match lm_path {
            Some(path) => match WanxiangLM::open(path) {
                Ok(lm) => {
                    tracing::info!(
                        path = %path.display(),
                        units = lm.num_units(),
                        "万象 .gram 语言模型已加载"
                    );
                    LmStore::Wanxiang(lm)
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "万象 .gram 打开失败，退回空 bigram 语言模型");
                    LmStore::Bigram(BigramLM::new())
                }
            },
            _ => LmStore::Bigram(BigramLM::new()),
        };
        Ok(Self {
            segmenter: Segmenter::new(),
            dict,
            lm,
            lm_weight: DEFAULT_LM_WEIGHT,
            fuzzy_pairs: COMMON_CONFUSION_PAIRS.to_vec(),
            fuzzy_enabled: DEFAULT_FUZZY_ENABLED,
        })
    }

    /// 创建一个输入会话（共享引擎数据）。
    pub fn create_session(self: &Arc<Self>) -> InputSession {
        InputSession::new(Arc::clone(self))
    }

    /// 在通用语言模型之外叠加用户学习模型(第 5 步)。
    ///
    /// 现有 `lm` 成为 [`MixedLM`] 的通用层(空 bigram 也合法:混合
    /// 模型对任何输入都有兜底分),`user` 是用户层(加载/持久化由
    /// [`UserLM`] 自己负责)。打分混合见 [`MixedLM`];调用方
    /// (beam/兜底/会话)不需要任何改动——这正是枚举分发的意义。
    /// 启动期**确定性**初始化:把两笔「谁先用到谁付」的成本搬到发布服务名之前。
    ///
    /// - `total_freq()`:第一次调用会**线性扫描词条区**求和(真实词库 187 万条),
    ///   结果是 `OnceLock` 缓存;不预热的话这笔钱由用户的**第一个按键**付。
    /// - 音节纠错变体表(`segmenter::prepare_typo_table`):第一次启用容错查询时
    ///   才构建;不预热的话由用户的**第一次打错**付。
    ///
    /// 与 [`Self::prewarm`] 的分工:这里是**同步、确定性**的——调用返回就代表
    /// 两笔成本已付清,所以调用方把它放在 `request_name` 之前即可保证「服务名
    /// 可见时初始化已完成」;`prewarm` 是后台线程里的查询预热,覆盖的是进程内
    /// 首次走某查询形状的懒初始化。两者互补,不是同一件事。
    pub fn prepare_startup(&self) {
        if let Err(e) = self.dict.total_freq() {
            tracing::warn!(error = %e, "启动期词频总量计算失败(词频项会退回不归一化)");
        }
        crate::segmenter::prepare_typo_table();
    }

    /// 启动预热:把「进程内第一次走到某个查询形状」的一次性成本挪到空闲时。
    ///
    /// `examples/perf_probe.rs` 实测:新进程里第一次走到**混合/简拼形状**
    /// 的查询要付 ~26ms(超过项目 12ms 逐键预算),此后同形状 <2ms;
    /// 先空跑一遍即可完全消除(26.13ms → 1.41ms)。生产在装配完成后用
    /// 后台线程调用这里,别让用户的前几个按键替我们付这笔钱。
    ///
    /// **只读**:只调 `lookup_candidates`,加一段「只打字母、不按空格」
    /// 的会话(会话路径另有一层候选表装配的懒开销)。不提交、不学习、
    /// 不造词、不改屏蔽表,因此可以随时重复调用。
    pub fn prewarm(self: &Arc<Self>) {
        let ctx = LmContext::empty();
        for probe in [
            "nihao",     // 全拼(短)
            "weishenme", // 全拼(长)
            "weis",      // 半音节/混合形状
            "zhongg",    // 混合简拼
            "ns", "nh",  // 纯简拼
            "nihoa",     // 容错
        ] {
            let _ = self.lookup_candidates(probe, &ctx);
        }
        let mut session = self.create_session();
        for ch in "womenjintianqubeixian".chars() {
            let _ = session.handle(InputEvent::Char(ch));
        }
    }

    pub fn with_user_lm(mut self, user: UserLM) -> Self {
        let base = std::mem::replace(&mut self.lm, LmStore::Bigram(BigramLM::new()));
        self.lm = LmStore::Mixed(MixedLM::new(base, user));
        self
    }

    /// 用户层句柄(存在混合模型时);供学习钩子与回写调度用。
    pub fn user_lm(&self) -> Option<&UserLM> {
        match &self.lm {
            LmStore::Mixed(m) => Some(m.user()),
            _ => None,
        }
    }

    /// 学习一次上屏(第 5 步学习钩子的引擎侧落点)。
    ///
    /// 由 [`InputSession`](crate::session::InputSession) 在上屏时调用:
    /// - `Some(word)`:候选词上屏(`commit_word`)→ 记录
    ///   `(上一个词, word)` 的二元计数与 `word` 的一元计数;
    /// - `None`:原始字母/标点上屏(`commit_raw`)→ **不学**(它不是词,
    ///   与第 2a 步「`last_word` 置 `None`」同一判断)。
    ///
    /// 只有用户模型存在(引擎经 [`Self::with_user_lm`] 组装)时才计数;
    /// 无用户模型是 no-op,上屏路径零额外开销。落盘不在此同步做
    /// (批量/定时/退出回写由进程入口 `src/main.rs` 调度)。
    pub(crate) fn learn_committed(&self, prev: Option<&str>, word: Option<&str>) {
        let (Some(word), Some(user)) = (word, self.user_lm()) else {
            return;
        };
        let dirty = user.learn(prev, word);
        if dirty == FLUSH_LEARN_THRESHOLD {
            tracing::debug!(dirty, "用户模型已达到回写阈值,等待调度层批量回写");
        }
    }

    /// 记一条造词条目(unity_table_encoder 的等价物:把上屏的整段
    /// 反查编码后写入用户词典)。
    ///
    /// 由 [`InputSession`](crate::session::InputSession) 的造词缓冲在
    /// 「连续候选上屏段被打断/超长且 ≥ 2 字」时调用。`pinyin_key`
    /// 是空格分隔的音节串(与词典 key 同格式),由缓冲里各片段的
    /// 候选拼音拼接而成。无用户层时是 no-op(与 `learn_committed`
    /// 同一取舍)。落盘复用用户模型的 dirty/flush 调度,不在此同步做。
    pub(crate) fn coin_entry(&self, word: &str, pinyin_key: &str) {
        let Some(user) = self.user_lm() else {
            return;
        };
        user.add_entry(word, pinyin_key);
        // **召回与打分必须成对**(见 `user_lm.rs` 的 `entries` 字段注释):
        // `add_entry` 让词库里没有的短语能被查到,`learn` 记一次上屏计数,
        // 它下次才会排到前面。只 add_entry 时自造词「查得到但排不上」——
        // 实测 `coined_entry_surfaces_via_lattice_like_dict_word` 里
        // 会被同串的首字「张」挤到第 2。
        user.learn(None, word);
    }

    /// 删一条候选(Ctrl+Delete 的引擎侧落点):屏蔽它,此后同拼音查询
    /// 不再出现;若是用户造词条目则连同词条删掉(词库词只屏蔽——
    /// 词库只读,mmap 不可改,屏蔽是唯一可行的删除形态)。
    ///
    /// 无用户层时是 no-op(与 [`Self::learn_committed`] / [`Self::coin_entry`]
    /// 同一取舍:删候选是用户数据,没有用户数据层就没有「删除」的载体)。
    /// 落盘复用用户模型的 dirty/flush 调度,不在此同步做。
    pub(crate) fn delete_candidate(&self, word: &str, pinyin_key: &str) {
        let Some(user) = self.user_lm() else {
            return;
        };
        user.block_candidate(word, pinyin_key);
    }

    /// **撤销最近一次屏蔽**（`Ctrl+Shift+Delete` 用）。返回是否真的撤了一条。
    ///
    /// 与「指名道姓」的解屏蔽分工不同：被屏蔽的候选本来就不在列表里，用户手上
    /// 没有可点的东西，所以这里只认槽位里的最近一条（见 `UserLM::unblock_last`）。
    pub(crate) fn unblock_last_candidate(&self) -> bool {
        self.user_lm().is_some_and(|u| u.unblock_last())
    }

    /// 当前词库词条总数（启动日志/统计用；内存空词库时为 0）。
    pub fn dict_count(&self) -> u64 {
        match &self.dict {
            DictStore::Sqlite(d) => d.count().unwrap_or(0),
            DictStore::Mmap(d) => d.count(),
        }
    }

}
