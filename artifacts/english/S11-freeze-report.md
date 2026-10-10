# S11：卡死（不是卡顿）——排查与现场记录

用户澄清：**不是慢，是「有时候会卡死几秒」**。这是完全不同的症状——不是算得
慢，是某次调用**被挡住**了。本页记录排查结论与为抓现场加的看门狗。

---

## 一、逐条排除（都有实测，不是推断）

| 假设 | 实测 | 结论 |
|---|---|---|
| debug 构建 | 已安装 `/home/youngwen/.local/bin/xwx_ime` 与本地 release 产物**字节数完全相同**（12 965 800） | ❌ 排除 |
| 磁盘 / 换页 | `0` 次 major page fault；根分区 `sda` **ROTA=0（SSD）**；数据不在那块 HDD（`sdb`，`/mnt/hdd`）上；`dmesg` 无 hung task / IO 卡顿记录 | ❌ 排除 |
| 内存压力 | 15 GiB 内存空余 10 GiB；swap 只用了 10 MiB；无 `MemAvailable` 紧张 | ❌ 排除 |
| 用户模型回写 | 30s 定时器 + `spawn_blocking`；`flush` 先 `snapshot()` 拿 `Arc` 克隆即**释放锁**，序列化时不持表锁；**不 fsync** | ❌ 排除 |
| 锁竞争 | 按键路径上 6 处 `session.lock()` 与 `last_ui.lock()` **逐处核对**：全部在 `.await` 之前离开作用域 | ❌ 排除 |
| 引擎重启 | 装配实测 **27ms**（含英文词库完整校验）；中文侧 **0.2ms**（惰性 mmap） | ❌ 排除 |
| 排序算得慢 | 全拼每键 ≤0.7ms；最慢的简拼 9ms（S10 已量） | ❌ 排除 |

**最关键的一条**：运行了两小时的进程累计只用 **7.0s 用户态 + 1.05s 内核态** CPU
（`/proc/<pid>/stat`）。

> **它不是在忙，是在等。**

## 二、剩下的嫌疑

`ProcessKeyEvent` 是**同步 D-Bus 方法**：ibus-daemon 每发一个键都要等它返回。
方法体内若有一个 `.await` 卡住，这个方法就不返回，**用户看到的就是输入法卡死**。

方法体里会 await 的三处：

1. `self.session.lock().await` —— 锁竞争，已排除；
2. `session.handle(event)` —— 纯 CPU，已量（≤9ms）；
3. **`ui::commit/refresh_ui` 里的 zbus 信号发送** —— 对端（ibus-daemon / GNOME
   Shell）不消费时 zbus 会**反压等待**，这是唯一无法在本机静态排除的一环。

同机外部组件（ibus-daemon、GNOME Shell）卡住也会表现成同一个症状，那属于本仓库
之外。

## 三、加了什么：卡死看门狗

无法复现的东西只能靠现场记录。

**`crates/xwx_ibus/src/stall.rs`**（新，345 行含测试）：

- `Stall` 分段计时器：把一次按键拆成「**锁 / 引擎 / 发信号**」三段，每段记的是
  **距上一段的增量**（不是累计），所以读起来直接就是「时间花在哪」。
- 单次总耗时 ≥ 300ms 就记一条：`tracing::warn!` + 追加到
  `$XDG_DATA_HOME/xwx/stall.log`（超 256 KiB 轮转出 `.log.1`，只留一代）。
- 阈值在**构造时注入**，不在每次 `mark` 里查全局状态；未设环境变量时**默认开启**
  ——这条记录只在真卡时产生，不该要求用户先去配环境变量。
- 关闭时 `mark`/`detail` 立即返回，**连 `detail` 闭包都不执行**（不给不落记录的
  路径付格式化成本）。
- 写日志失败**静默**：看门狗是诊断设施，它的 IO 问题绝不能反过来影响输入法。

**接入**：`engine.rs::handle_key` 在「取锁后」「引擎处理完」「发完信号后」三个
位置 `mark`。`ProcessKeyEvent` 的其它分支（Ctrl+空格、修饰键透传等）不经过这里，
它们本身不 await 重型操作。

**环境变量**：`XWX_STALL_MS`（毫秒，默认 300，`off`/`0` 关闭）、
`XWX_STALL_LOG`（改路径）。

### 怎么读

```bash
tail -20 ~/.local/share/xwx/stall.log
# 2026-10-10 22:41:07 1832ms  ProcessKeyEvent  锁=1ms 引擎=8ms 发信号=1823ms  Char('a')
#                                              ^^^^^^^^              ^^^^^^^^^^^^
#                                              引擎没问题            卡在总线
```

- `发信号` 占满 → zbus 反推压（对端不消费），不是引擎；
- `引擎` 占满 → 真的是引擎算得慢，把这条发给我，配合 `XWX_TRACE=1` 继续拆；
- `锁` 占满 → 有别的任务长时间持有会话锁（那就是新 bug，能直接定位）。

## 四、验证

| 项 | 命令 | 结果 |
|---|---|---|
| 全量 | `cargo test --workspace --offline` | **568 passed / 0 failed** |
| Lint | `cargo clippy --workspace --all-targets --offline -- -D warnings` | exit 0 |
| 中文回归 | `eval_rank` 五套语料 vs S0 基线 | **逐字相同** |
| 看门狗自身 | 7 条测试 | 含分段的**增量语义**、关闭时零开销、追加不覆盖、轮转只留一代、阈值解析边界（`0`/`off`/`abc`/`-5`/空白） |

顺手修了一个真错：`latency_probe` 的 `arg(name, required=false)` 被当布尔开关用，
`--english` 会取到空串 → `EnglishDict::open("")` 报「文件不存在」，掩盖了真实的
装配耗时。新增 `flag()` 把「可选参数」与「布尔开关」分开。

## 五、本轮没做的

- **没有再改核心排序**：S10 已把简拼热点定位到 `build_abbrev_lattice`（独立一轮做），
  本轮不叠加风险。
- **没有给信号发送加超时**：超时会在总线拥塞时**丢掉一次上屏**——丢字符比卡一下
  更糟。要动它得先有现场记录证明卡点确实在那里。
- **没有自动重装二进制**（项目纪律：不自动跑 `just install` / 不重启 IBus）。
  新二进制已构建：`/mnt/hdd/target/xwx_ime/release/xwx_ime`（12 986 168 字节）。
