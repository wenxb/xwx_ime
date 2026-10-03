# 实机验证清单

> `cargo test` 有 **365** 个测试全绿（295 engine 单测 + 6 ibus 单测 + 9 bin +
> **55 D-Bus 集成**），但**它仍不是实机可用的证据**。
>
> **2026-10-01 更新**：根 `tests/` 现在有一组 **D-Bus 集成测试**，
> 跑真实 `xwx_ime --ibus` 进程 + 私有 `dbus-daemon`，已自动化下面清单里的
> **一部分**（英文模式透传、`CommitText`/`UpdatePreeditText`/
> `UpdateLookupTable` 信号内容与字段顺序、`FocusOut`、候选点击、翻页、
> Ctrl+Delete、可编辑缓冲的逐字符序列、隔离性、总线往返延迟、分配计数）。14 个文件（每个文件一组场景，原来是 5 个）：
> `verbatim_url`(7)、`wire_abbrev`(5)、`mods_keys`(4)、`prefix_commit`(1)、
> `paging`(7)、`blocklist_delete`(2)、`focus_lifecycle`(4)、`punctuation_paging`(1)、
> `passthrough_contract`(7)、`signal_payloads`(7)、`hermeticity`(5)、`cli`(3)、`wire_perf`(1)、`alloc_count`(1)。
>
> **回归基线**（codex 审查 Q1）：`tests/baselines/regression_v2.txt` 是
> `examples/regression_probe.rs --format v2` 的输出（2515 行），旁边
> `regression_v2.id.txt` 记着输出 sha256/行数、Git SHA 和三个数据文件的
> sha256——探针会先校验词库/副索引/万象**真的加载成功**，任一缺失直接报错，
> 不会静默拿空库产出“看着是绿的”基线。逐字节对拍历史基线用
> `--format v1`（2322 行，与 `/tmp/codex_batch1_nt.txt` diff 0 行）。
> >
> **逐键性能**（codex 审查第二批）：`just perf` 跑 `examples/perf_probe.rs`——装配与
> `src/main.rs` 一致（真词库 + 简拼副索引 + 万象 `.gram` + 用户层内存态）、数据缺失
> 直接报错不降级、按键走真实会话路径；打印每个场景的 p50/p95/max 与超线样本，
> 任一场景稳态 p95 超过 12ms 时非零退出。启动预热（`ImeEngine::prewarm`）的依据
> 就是它抓到的「新进程首次走到混合/简拼形状 ~26ms」尖峰。
> >
> **D-Bus 层**同一件事:`tests/wire_perf.rs` 量一次 `ProcessKeyEvent` 往返
> (私有总线 + 真引擎进程,debug 装配),打印分位并对 p95 ≤ 100ms 做宽带断言,
> 防止「引擎很快、总线上很慢」被漏掉。
>
> **但它用的是私有总线，不是真实 ibus-daemon + 真实应用**——下面这份清单
> 仍然要敲，因为它验的是 daemon / GNOME Shell / 应用三者与引擎的
> **真实协作**，那部分结构上测不到：
>
> - 真实 daemon 的激活时序（走 `CreateEngine`，而非预导出实例）
> - GNOME Shell candidate popup 的**实际渲染**（我们只发信号，不画窗）
> - 跨应用时 `FocusOut` 的真实到达时机
> - 与系统输入源切换（`Super+空格`）、fcitx5 抢总线名的交互
>
> 有时间时照这份清单敲一遍即可，**不需要一次做完**——每条都是独立的。

---

## 0. 准备

### 不动系统的跑法（推荐先用这个）

`just install` 要 sudo、会写 `/usr/share/ibus/component/`、还会 `ibus restart`。
**先别用它**，用下面这个只写你自己家目录的方式：

```bash
cd ~/Desktop/xwx_ime

# 1. 数据文件就位（三个都要有）
for f in dict.bin abbrev.bin wanxiang-lts-zh-hans.gram; do
  printf '%-32s ' "$f"; [ -f ~/.local/share/xwx/$f ] && echo OK || echo 缺失
done
# abbrev.bin 若缺（简拼副索引，缺了第 18~21 条全废）：
python3 scripts/abbrev_to_bin.py && cp data/abbrev.bin ~/.local/share/xwx/

# 2. 装组件 XML 到用户目录（不用 sudo，整段可直接复制；须在项目根执行）
cargo build --release -p xwx_ime
mkdir -p ~/.local/share/ibus/component
BIN=$(cargo metadata --no-deps --format-version 1 \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/xwx_ime
"$BIN" --xml > ~/.local/share/ibus/component/xwx.xml
grep exec ~/.local/share/ibus/component/xwx.xml    # 核一眼路径对不对
```

> `--xml` 会**自动把当前二进制路径填进 `<exec>`**（`main.rs` 用 `current_exe()`），
> 不需要 `sed` 替换模板。上面这条已实跑验证，输出是
> `<exec>/mnt/hdd/target/xwx_ime/release/xwx_ime --ibus</exec>`。
>
> target 目录被 `.cargo/config.toml` 指到了 `/mnt/hdd/target/xwx_ime`，
> 所以用 `cargo metadata` 查而不是写死 `./target`。
>
> ⚠ 别写成 `--xml --exec <路径>`：`--exec` 后面的参数会被当成**输出文件名**
> （`main.rs:46` 的 `out_path` 判定是「不以 `--` 开头的第一个参数」），
> XML 会被写进那个文件而不是 stdout。

### ⚠ 和 fcitx5 的冲突

**你日常用的 fcitx5-rime 会占用 `org.freedesktop.IBus` 总线名**
（`justfile` 自己也警告了这条）。验证时二选一：

- **切到 IBus 会话**验证，验完切回来；
- 或临时 `fcitx5 -k` 停掉 fcitx5，验完再启动。

无论哪种，**都不要动 `~/.local/share/fcitx5/rime`**——那是你日常打字的配置。

### 前台跑引擎（看得见日志）

```bash
just dev        # debug 版，前台运行，Ctrl+C 退出
```

出问题时日志直接打在终端上，比装成服务好排查。

---

## 1. 清单

每条独立，随手挑着验。**「实际」栏留空给你填**，不对的地方记下来告诉我。

### 基本功能

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 1 | `nihao` 空格 | 「你好」上屏 | |
| 2 | `nihao` 然后数字 `2` | 第 2 个候选上屏 | |
| 3 | `zhongguo` | 「中国」第 1 | |

### 标点（punctuator）

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 4 | 空闲时打 `,` | 「，」全角，**不是半角 `,`** | |
| 5 | 空闲时打 `.` `?` `!` `;` `:` | 「。？！；：」全角 | |
| 6 | `nihao,`（拼音后直接打逗号） | 「你好，」——先上屏候选再跟标点 | |
| 7 | 空闲时打 `+` `#` `%` `@` | 原样半角（这些没有全角映射，应透传） | |

### 翻页（⚠ 最容易出问题）

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 8 | `nihao` 后打 `.` | **翻到第 2 页**（不是出句号！） | |
| 9 | 承上，在第 2 页打 `,` | **回第 1 页** | |
| 10 | 承上，回到第 1 页后再打 `,` | 这次出「，」并上屏候选（第 1 页的 `,` 不翻页） | |
| 11 | `nihao` 后打 `=` / `-` | 下一页 / 上一页 | |

> 第 8~10 条是 librime 的状态机语义：`.` 只要候选窗开着就翻页；
> `,` **必须已经翻过页**才回翻。这样中文逗号和翻页才不打架。

### 大写与英文模式

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 12 | Shift+G Shift+P Shift+U | `GPU` | |
| 13 | `nihao` 后 Shift+G | 「你好G」 | |
| 14 | CapsLock 开，打 `g` | `G`（不进拼音串） | |
| 15 | Ctrl+空格切英文，打 `nihao` | 原样 `nihao`，无候选窗 | |
| 16 | 承上（英文模式）Shift+G | 应用收到大写 `G` | |
| 17 | 再按 Ctrl+空格切回中文 | 恢复拼音输入 | |

### 简拼

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 18 | `zgr` | 「中国人」第 1 | |
| 19 | `bjdx` | 「北京大学」第 1 | |
| 20 | `zhongguor` | 「中国人」第 1 | |
| 21 | `renm` | 「人们」第 1 | |

> 全都不出候选 → 多半是 `~/.local/share/xwx/abbrev.bin` 没装（见准备一节）。

### 输入缓冲与分隔符

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 22 | `nihao`，按 ← ← 再打 `x` | preedit 变 `nihxao`，**光标停在 x 后面** | |
| 23 | 承上，按 Backspace | 删掉 `x`，回到 `nihao` | |
| 24 | `xi'an` | 「西安」第 1（不带 `'` 时它排第 18） | |
| 25 | `xian`（不带 `'`） | 「先/现/显/线…」，西安在后面——**这是对的** | |

### 造词（⚠ 第 27 条跨应用）

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 26 | 逐字选「张」「三」「丰」上屏，再打 `zhangsanfeng` | 「张三丰」出现在候选里 | |
| 27 | 在**另一个应用**里逐字选两个字，回到原应用打对应拼音 | **不该**和第 26 步的字粘成一个词 | |
| 28 | 逐字选「你」「好」，打逗号，再逐字选「世」「界」 | 不该造出「你好世界」 | |

### 删候选

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 29 | 打某拼音，高亮一个候选，按 Ctrl+Delete | 该候选消失 | |
| 30 | 重新打同一拼音 | 被删的候选**不再出现** | |
| 31 | 重启输入法后再打同一拼音 | 仍然不出现（落盘生效） | |

### 邮箱 / 网址（⚠ 第 33 条曾经是坏的）

| # | 敲什么 | 应该看到 | 实际 |
|---|---|---|---|
| 32 | `zhangsan@gmail.com` 回车 | 原样上屏，**全程不出拼音候选** | |
| 33 | `http://x.com` 回车 | 原样上屏（**曾出过 `http:、、xcom`**） | |
| 34 | `www.example.com` 回车 | 原样上屏 | |
| 35 | `mailto:a@b.c` 回车 | 原样上屏 | |
| 36 | `www` 单独打（后面不接点） | 出「万维网」等拼音候选——**这是对的** | |
| 37 | `example.com` | 走拼音路径（裸域名有意不识别，见 PLAN.md） | |

---

## 2. 最可能出问题的四条

如果时间只够验几条，验这些：

| # | 为什么 |
|---|---|
| **8~10** | 键位抢占。`.` 和 `,` 同时是标点和翻页键，靠状态机区分 |
| **27** | 跨进程。靠 IBus 的 `FocusOut` 触发，单测摸不到 |
| **33** | 端到端序列。初版实现下真的产出过 `http:、、xcom` |
| **16** | 英文模式在 ibus 层直接 `return false` 透传，单测覆盖不到 |

---

## 3. 出问题了怎么反馈

告诉我这三样就够：

1. **哪一条**（编号）
2. **实际得到什么**（原样贴出来，包括乱码）
3. 如果 `just dev` 前台跑着，把**终端最后几行日志**贴上

不用自己排查。多数问题我能从「敲了什么 → 得到什么」直接定位。

## 已知有意差异（简拼整串候选，2026-10-02）

- `zhzhzhzh`（连续 zh、无元音的病态串）：v1 对拍的 9 行差异全部来自它。
  以前只出「原样回显」，现在多一条覆盖整串的读法（`自会在盒子盒子会` 一类）。
  **决定：接受**——它不是真拼音，且新行为与上游一致（librime/libime 对「能解析
  出完整覆盖的读法」就给整串候选，不做「像不像拼音」的额外判断）。
  若将来要收回，判据应加在「无元音连续串」这一层，而不是回到逐输入白名单。
- `womenjtqbjgy`：电池里由「我们今天/我们」变为「我们今天去把几个月」（整串覆盖、
  但语义不对）。**决定：记账**（非验收项）；同类问题与 `.gram` 查询异常值、
  词长先验有关，跟 `t-mur4twht-j2pd2j` 一起看。

## 已知有意差异（2026-10-03 更新）

- **末尾单字母补全带来的 2 处首位变化**（`womenjintianqugongyuann` → 我们今天去公园内；
  `womenjintianqugongyuam` → 我们今天去公寓阿曼）：两条都走引擎**原有**的「覆盖整串优先」
  排序桶，不是新规则；后者内容牵强（分数比前缀差 44 个 log10），**记账**，不单独为它开阈值。
- **`nihaoshijie` 的 MRR 代价**：最低档整串词条（freq=100）让位给整句后，期望「你好世界」
  落到第 2 位（daily MRR 0.958；召回 12/12、首屏 12/12 不变）。**决定：保验收形态**
  （`chuzaina` → 出在哪），并已查明两条目在频率上不可区分（都是 100 档）。
- **`jintqbj` 不做**：期望「今天请病假」与上游「缩写不能单独成词」规则冲突（放宽会放回
  「一字一字母」垃圾读法），语料保留人工期望不改实现（mixed 13/14）。
- **`shanghaai` 挂起**：三次结构障碍（格子内折叠的生命周期级联、`tolerant_due` 的开启条件、
  音标级编辑代价模型），机制与失败原因已留档。

## 门槛现状（2026-10-03）

| 门槛 | 状态 |
|---|---|
| `cargo test --workspace` | 单元 **327 passed / 0 failed / 0 ignored** ✓ |
| clippy `-D warnings` | **0** ✓ |
| 单文件 ≤600 行 | 全部达标 ✓ |
| v1 对拍 | 9/44 输入内容变化、**2 个首位变化**（均已点名）✓ |
| 验收形态 | 全绿（chuzaina→出在哪、ztai→状态、ztailan→状态栏、wmjtqbj/womenjtqbj→我们今天去北京、zmgai→怎么改、zguor→中国人、nihao→你好、xian→先、daishixian/daishix→待实现）✓ |
| eval 语料 | edits **5/6 · MRR 0.478**、daily 12/12（MRR 0.958）、mixed 13/14 |
| perf p95 ≤ 12ms | **未稳定达标** ✗：引擎内部只占 4.0ms（建精确格 1.1 + 建简拼格 2.8 + 合并/折叠 0.04），A/B 显示本轮改动 ≤0.5ms；本机负载 13~18 下同负载反复在 11.5~14.4ms 之间摆动，需安静环境复测 |

## 已知有意差异（2026-10-03 叠字母纠错）

`shanghaai`（多打一个字母）首位由逐字读法「上哈爱」变成纠正后的「上海」。
v1 对拍 44 个输入 **0 行差异**；`tests/corpora/edits.jsonl` 召回 6/6。

## 已知有意差异（2026-10-03 末尾半音节补全进格子）

`nihaobeijin`→你好北京、`womenjintianqugongyua`→我们今天去公园 从「不可召回」变成首位；
v1 对拍 44 输入 **0 行差异**；keystrokes 语料召回 10/12→12/12。

## 已知有意差异（2026-10-03 投影层填充档判断）

填充档（freq<=100）的**真短语**不再被组合候选挤掉：zaodianxiuxi→早点休息、
womashangdao→我马上到、jintiantianqizenmeyang→今天天气怎么样、nizaiganshenme→你在干什么、
nihaoshijie→你好世界（daily MRR 0.958→1.000）。v1 对拍 44 输入 **0 变化**；
chuzaina→出在哪、shanghaai→上海、wmjtqbj/womenjtqbj 不变。钉子用例
engine/tests/projection_filler.rs。

## perf 复核（2026-10-03 15:51，load average 17.55）

投影层改动（填充档按词串口径比）新增的词串 LM 计算**不改变稳态**：
同条件交替两份二进制（改动前 = 尾补全那版 `perf_before_completion`，
改动后 = 当前）实测
- 改动前：full 12.65/14.23/18.86 · mixed 14.46/16.98/17.96
- 改动后：full 14.19/14.06/12.86 · mixed 12.46/12.75/12.33

两版落在同一区间、无可区分差异；12ms 线在本机 load 12~18 时对**两版都**不可达
（既有记录同此结论），判定 perf 时一律用同条件 A/B，不看单次绝对值。
