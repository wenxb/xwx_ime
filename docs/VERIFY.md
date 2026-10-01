# 实机验证清单

> `cargo test` 有 **275** 个测试全绿（226 单元 + **49 D-Bus 集成**），
> 但**它仍不是实机可用的证据**。
>
> **2026-10-01 更新**：根 `tests/` 现在有一组 **D-Bus 集成测试**，
> 跑真实 `xwx_ime --ibus` 进程 + 私有 `dbus-daemon`，已自动化下面清单里的
> **一部分**（英文模式透传、`CommitText`/`UpdatePreeditText`/
> `UpdateLookupTable` 信号内容与字段顺序、`FocusOut`、候选点击、翻页、
> Ctrl+Delete、可编辑缓冲的逐字符序列、隔离性）。5 个文件：`passthrough_contract`(7)、
> `focus_paging_delete`(14)、`signal_payloads`(7)、`interactive_sequences`(16)、
> `hermeticity`(5)。
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
