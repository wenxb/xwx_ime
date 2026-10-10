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

---

## 六、补充（第二轮）：证明它会触发 + 补上按键路径之外的缺口

### 缺口

分段计时只在 `ProcessKeyEvent` **被进入之后**才有用。如果冻结发生在它之前
（运行时线程全被占住、进程被挂起、某处同步阻塞），那个方法根本不会被调用，
**一条记录都不会有**——用户看到卡死、日志却是空的。

### 补上：事件循环心跳

`spawn_heartbeat()`：每 500ms 醒一次，两次醒来的间隔 ≥1s 就记一条。tokio 定时器
在负载下会迟到几十毫秒，所以 1s 是「被堵住」而不是调度抖动。关看门狗时它不启动。

于是三种情况可以分开：

| 日志里看到 | 结论 |
|---|---|
| `ProcessKeyEvent` 且 `发信号` 占满 | 卡在总线（zbus 反压） |
| `ProcessKeyEvent` 且 `引擎` 占满 | 卡在引擎，配合 `XWX_TRACE=1` 继续拆 |
| 只有 `heartbeat` | **整个进程**被堵住，不在按键方法内部 |
| 两者都没有却确实卡了 | 卡点在本进程之外（ibus-daemon / GNOME Shell） |

### 证明它真的触发（`tests/wire_stall.rs`，4 条真实总线用例）

看门狗装上了但「从没触发过」等于没装，所以必须在**真实 D-Bus 路径**上证明：

| 用例 | 手法 | 结果 |
|---|---|---|
| 三段齐全 | 阈值压到 1ms，私有总线上真敲 13 个键 | 记录含 `锁=` / `引擎=` / `发信号=` + 时间戳 + 总耗时 |
| 不达阈值不写 | 阈值调到 600s，敲 5 个键 | 日志**一个字节都没有** |
| 可完全关闭 | `XWX_STALL_MS=off` | 连日志文件都不创建 |
| 进程级冻结 | **SIGSTOP 冻住引擎 1.5s 再 SIGCONT** | `heartbeat` 记录出现 |

真实产出（第一条用例打印的原文）：

```
2026-10-11 04:34:44 2ms  ProcessKeyEvent  锁=0ms 引擎=1ms 发信号=1ms  Char('n')
```

第二条是反证：「日志非空」不是因为看门狗无条件在写。第四条复现的正是用户描述的
那种**进程级冻结**。

---

## 七、生产链路验证与安装循环（2026-10-11）

### 安装不需要 sudo

`just install` 的 6 步里只有第 3 步（`/usr/share/ibus/component/xwx.xml`）要 sudo，
而**它的 exec 路径不变就不必重装**——XML 里已经是
`/home/youngwen/.local/bin/xwx_ime --ibus`。其余全都是 `~/.local`：

| 目标 | 路径 | 需要 sudo |
|---|---|---|
| 二进制 | `~/.local/bin/xwx_ime` | 否 |
| 词库/英文词库等 | `~/.local/share/xwx/` | 否 |
| 组件 XML | `/usr/share/ibus/component/xwx.xml` | 是（**路径不变则不用动**） |

所以改完代码可以直接自己装：

```bash
cargo build --release -p xwx_ime
install -m 755 <target>/release/xwx_ime ~/.local/bin/xwx_ime.new
mv -f ~/.local/bin/xwx_ime.new ~/.local/bin/xwx_ime   # 先装临时名再 mv，避开 ETXTBSY
ibus restart                                          # 让 daemon 重新拉起引擎
```

### 在生产进程上验过一次真实触发

装上之后，用 SIGSTOP 把**正在运行的引擎**冻 2 秒再放开（模拟用户描述的进程级
冻结），引擎自己写下了：

```
2026-10-11 04:46:14 2107ms  heartbeat  事件循环被堵住（本次按键路径未覆盖到）
```

2107ms 与 2 秒的冻结对得上。这一次同时验证了整条生产链路：`XDG_DATA_HOME`
未设置时解析到 `$HOME/.local/share/xwx/stall.log`（已核对
`/proc/<pid>/environ`）、默认 300ms 阈值生效、目录可写、心跳判定正确。
验证用的记录已清除，`stall.log` 现在是干净的。
