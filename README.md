# XWX 拼音输入法（Rust）

一个从零实现的拼音输入法：**纯 Rust 算法引擎** + **标准 IBus engine 接口**。
候选窗渲染完全交给 GNOME Shell / IBus 自带的 candidate popup，不自己画窗口。

## 架构

```
┌──────────────────────────────────────────┐
│              ibus-daemon                 │
│    D-Bus：org.freedesktop.IBus           │
│  RegisterComponent / CreateEngine / 信号  │
└───────────────────┬──────────────────────┘
                    │  org.freedesktop.IBus.Engine.xwx
                    ▼
┌──────────────────────────────────────────┐
│                 xwx_ime                  │
│    bin：连接总线 / 注册组件 / 导出对象      │
└───────────────────┬──────────────────────┘
                    │
                    ▼
┌──────────────────────────────────────────┐
│                 xwx_ibus                 │
│    IBus engine 适配层（zbus 实现）         │
│    CommitText / UpdatePreeditText /      │
│    UpdateLookupTable / HideLookupTable   │
└───────────────────┬──────────────────────┘
                    │
                    ▼
        ┌───────────────────────┐
        │      xwx_engine        │
        │  拼音算法核心（纯库）    │
        │  切分/词库/LM/纠错/会话  │
        └───────────────────────┘
```

三个 crate：

| crate | 类型 | 职责 |
| --- | --- | --- |
| `xwx_engine` | lib | 拼音切分（DP + 切分格 + beam 解码）、mmap 只读二进制词库、语言模型（bigram 文本 / 万象 `.gram`）、模糊拼音、全角标点、邮箱网址识别、用户词频与造词、按键会话状态机。不依赖任何 GUI/DBus，可独立测试 |
| `xwx_ibus` | lib | 标准 IBus engine：`org.freedesktop.IBus.Engine` 接口实现、Factory 按需创建引擎实例，用 `UpdateLookupTable`/`UpdatePreeditText` 等信号驱动 GNOME Shell 渲染候选窗 |
| `xwx_ime` | bin | 唯一进程入口：CLI 解析（`--ibus` / `--xml`）、连总线、`RegisterComponent`、导出 D-Bus 对象 |

## 功能状态

- [x] 拼音音节表（400+ 音节）与 DP 全切分
- [x] mmap 只读二进制词库（`dict.bin`，启动 O(1) 映射、查询二分；
      `scripts/dict_to_bin.py` 从 SQLite 转换；SQLite 仅作构建/迁移中间格式）
- [x] 简拼(首字母)副索引存储层(`abbrev.bin`,`scripts/abbrev_to_bin.py`
      从 `dict.bin` 只读生成;`MmapDict::with_abbrev` 装配,纯简拼 `zgr` 二分副索引、
      混合简拼 `zhongguor` 走音节切分+主索引前缀;基准 `examples/abbrev_bench.rs`)
- [x] 二元语言模型（文本格式加载）+ 万象 `.gram`（mmap 字符级窗口模型，
      未设置 `XWX_LM_PATH` 时按优先级自动探测）
- [x] **模糊拼音**（n/l、zh/z、前后鼻音等 13 组混淆对）——**默认关闭**，
      用 `XWX_FUZZY=1` 打开。默认关是实测裁决的：开启后
      `womenjintianquxian` 端到端 3.4ms → 22.0ms（6.48x，突破本项目
      12ms 上限）、`diannao` 前 10 有一处候选换血，而收益只有 2 组新召回
      且名次在 16~27。实测表格见 `DEFAULT_FUZZY_ENABLED` 的文档注释，
      基准 `crates/xwx_engine/examples/fuzzy_bench.rs`
- [x] 按键状态机：字母、退格、空格选词、数字选词、翻页、方向键、Esc、回车
- [x] **可编辑输入缓冲**（左右方向键移动光标、光标处插入/退格）
- [x] **中文标点**（`,`/`.`/`?`/`!` 等出全角；`,`/`.` 在候选窗开着时是翻页）
- [x] **`'` 音节分隔符**（`xi'an` → 「西安」第 1；不带 `'` 时排第 18）
- [x] **Shift/CapsLock 大写**（`GPU`/`API` 可打；英文模式按键原样透传）
- [x] IBus 引擎：`CommitText`/`UpdatePreeditText`/`UpdateLookupTable` 信号 +
      组件注册（运行时 `RegisterComponent` + 安装用 component XML 双通道）
- [x] 候选窗由 **GNOME Shell candidate popup** 渲染（不再自绘窗口）
- [x] **Ctrl+空格 中英文切换**（切到英文时字母直接透传给应用，
      切换由引擎自己的会话状态决定）
- [x] **用户词频学习**（会话上屏回写 `user_bigram.txt`；`MixedLM` 把用户层
      叠在通用模型之上，30s 定时 flush + Ctrl+C 兜底）
- [x] **造词**（用户**逐字**选出的整段短语写成用户词条，重打同样拼音可召回）
- [x] **Ctrl+Delete 删候选**（词库词只屏蔽，用户造词条目连词条删掉）
- [x] **邮箱 / 网址识别**（`zhangsan@gmail.com`、`http://x.com`、
      `www.example.com` 原样上屏，不再被切成拼音）
- [x] **英文前缀补全（M1）**：纯字母输入额外走一条独立英文通道——精确查询 +
      前缀补全（`pyth` → `Python`），中英意图分档后再稳定混排（中文候选的
      字段与相对次序不变）。英文有**独立**用户文件 `user_english.txt`，
      选择即学习、Ctrl+Delete 按目标完整词屏蔽、Ctrl+Shift+Delete 撤销
      **真正最近**的一次删除（中英共享一个撤销槽位）。连续英文单词自动补
      前导空格（`hello` + ` world`），用户显式敲过空格时不重复补。
      词库缺失只关闭英文通道，中文完全不受影响。
- [x] **下一词预测（M2）**：提交一个英文词后进入**独立预测态**（preedit 为空的
      候选面板），`Tab`/数字/点击接受高亮项，`Space` 只把空格交给应用、
      `Shift+Tab` 原样透传。候选池 = 静态上一词 Top-32 ∪ 用户上一词 Top-32，
      用平滑频率估计 `p = 0.8×c/T + 0.2×10^(z-9)` 排序，**没有来源就不展示面板**
      （绝不用 the/and 硬填）。静态二元数据 `english_bigram.bin` 缺失或损坏时
      只降级条件排序与预测面板，英文补全、中文输入与用户学习全部照常。
      预测态零按键学习：**展示不学习**，只有真正接受才写用户计数。

## 依赖

- Rust 工具链（rustc ≥ 1.73）
- 编译依赖：无系统库（SQLite 用 rusqlite 的 bundled 特性；D-Bus 走 zbus）
- 运行依赖：`ibus-daemon`（或至少一个 IBus 会话）、GNOME Shell /
  IBus 候选窗（`ibus-extension`、gnome-shell 扩展 `im`）

## 构建

```bash
just build             # debug，整个 workspace
just build-release     # release
```

产物：`target/release/xwx_ime`（引擎进程）。

> 注意：若本地 `.cargo/config.toml` 的 `target-dir` 指向只读/受限目录，
> 可用 `CARGO_TARGET_DIR=/tmp/xwx_target cargo build` 覆盖。

## 一键安装

```bash
just install
```

做的事：

1. `cargo build --release`；
2. 把 `xwx_ime` 装到 `~/.local/bin/`（用户级）；
3. 把 component XML 用 sudo 装到 `/usr/share/ibus/component/xwx.xml`
   （exec 指向第 2 步的二进制）；
4. 拷贝数据文件到 `~/.local/share/xwx/`（各自**存在才拷**，缺了只降级不报错）：
   `dict.bin`（词库）、`abbrev.bin`（简拼副索引）、`syllable_graph.bin`
   （缩写弧建边的加速索引，缺了简拼走慢路径、功能不变）、`english.bin`
   （英文词库，缺了只是没有英文前缀补全、中文不受影响）；
5. 重启 `ibus-daemon`。

之后 `gsettings set org.gnome.desktop.input-sources sources "[('xkb','us'),('ibus','xwx')]"`
添加输入源，`Super+空格` 切到 **XWX 拼音** 即可输入。引擎内 **Ctrl+空格**
来回切换中/英文。

> **为什么 XML 必须进系统目录（需要 sudo）**：IBus 1.5.x 的源码里有一段
> 「加载用户目录组件」的代码（`ibus/src/ibusregistry.c`）被 `#if 0` 注释掉
> （上游 issue [ibus#2754](https://github.com/ibus/ibus/issues/2754)，
> closed as not planned）—— **`~/.local/share/ibus/component/` 永远不会被
> daemon 扫描**。XML 只能放 `/usr/share/ibus/component/`。

> **与 fcitx5 冲突**：fcitx5 出于兼容需要，会在启动时伪装占用
> `org.freedesktop.IBus` 总线名。它在跑时 XWX 无法注册，切换输入法框架请先
> `fcitx5 -k`（没停干净时再 `kill $(pgrep -x fcitx5)`）。

## 手动安装（查错用）

```bash
just build --release   # 或 cargo build --release

# 二进制（用户级）
mkdir -p ~/.local/bin
install -m 755 $(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/release/xwx_ime ~/.local/bin/xwx_ime

# component XML（必须进系统目录）
sed 's#__XWX_EXEC__#$HOME/.local/bin/xwx_ime#g' data/xwx.xml > /tmp/xwx.xml
sudo install -m 644 /tmp/xwx.xml /usr/share/ibus/component/xwx.xml

# 重启 daemon 重读组件
ibus restart   # 或 ibus-daemon -rd
```

## 开发调试

```bash
just dev            # 前台跑引擎（debug，连接到 ibus-daemon），Ctrl+C 退出
just xml            # 输出 component XML 到 stdout
just xml -- --exec /usr/libexec/ibus-engine-xwx   # 指定 exec 路径（打 deb 用）
```

也可以直接用 `RUST_LOG=debug cargo run -p xwx_ime -- --ibus` 前台调试。

验证引擎是否注册成功：

```bash
gdbus call --session --dest org.freedesktop.IBus --object-path /org/freedesktop/IBus \
  --method org.freedesktop.IBus.ListEngines
# 输出里应能看到 XWX 拼音输入法
```

**开发期的用户模型是独立的**：`debug` 构建默认读写
`$XDG_DATA_HOME/xwx/user_bigram.dev.txt`，不会把你开发时打的字混进真实用户模型
（真实文件里装着计数、造词、Ctrl+Delete 屏蔽表三段状态）。`just install` 装的是
`release` 二进制，实机用的仍是 `user_bigram.txt`；`XWX_USER_LM_PATH` 优先级最高，
集成测试靠它隔离。启动日志会打印实际用了哪个文件（`用户学习模型已接入 path=…`）。


## 数据准备

首次运行词库为空，输入法不会出候选。准备词库：

1. 运行 `python3 scripts/build_dict.py` 构建词库：脚本自动优先读
   fcitx5-rime 的 **rime_ice** 词表（雾凇拼音，主体是腾讯词向量词库；
   需已装 fcitx5-rime），找不到时回落系统 `luna_pinyin`
   （`sudo apt install rime-data-luna-pinyin`），产出 `dict.sqlite3`
   中间格式；再运行
   `python3 scripts/dict_to_bin.py ~/.local/share/xwx/dict.sqlite3 ~/.local/share/xwx/dict.bin`
   转成运行时二进制（也可直接用 `XWX_DICT_PATH` 指向自己生成的 dict.bin；
   详细说明与 luna_pinyin 兜底教程见 `data/README.md` 第四节）；
2. 语言模型：下载**万象 `.gram`** 放到 `~/.local/share/xwx/`
   （引擎自动探测 `wanxiang-lts-zh-hans.gram`），或可选按 `data/README.md`
   第五节用语料训练 `bigram.txt`；
3. 英文通道（可选）：按 `data/ENGLISH.md` 生成 `english.bin`。**要给
   `--reference` 参考词表**——wordfreq 是语料统计不是词典，不过滤会把
   `ve`/`gra`/`lun` 这类碎片当英文单词，中文模式下打拼音缩写时它们排第一、
   按空格直接上屏。

数据文件路径 `xwx_ime` 默认从 `$XDG_DATA_HOME/xwx/`(通常
`~/.local/share/xwx/`)读取。环境变量方向不同,别搞混:

- `XWX_DICT_PATH`:覆盖词库 `dict.bin` 的读取路径;
- `XWX_ABBREV_PATH`:覆盖简拼副索引 `abbrev.bin`（缺省与 `dict.bin` 同目录）;
- `XWX_SYLLABLE_GRAPH_PATH`:覆盖音节级转移索引 `syllable_graph.bin`
  （缺省与 `dict.bin` 同目录）;
- `XWX_ENGLISH`:英文通道总开关(默认 `1`)。设为 `0` 时**不打开英文词库、
  也不打开/创建英文用户文件**——关闭功能后不留下任何新的路径访问与写文件
  副作用;
- `XWX_ENGLISH_DICT_PATH`:覆盖英文词库 `english.bin` 的读取路径(缺省
  `$XDG_DATA_HOME/xwx/english.bin`,其次 `/usr/share/xwx/english.bin`);
- `XWX_ENGLISH_USER_LM_PATH`:覆盖英文用户文件(缺省
  `$XDG_DATA_HOME/xwx/user_english.txt`;debug 构建用
  `user_english.dev.txt`)。**可写路径永不回落系统目录**;
- `XWX_ENGLISH_PREDICT`:零输入下一词面板开关(默认 `0`)。关掉它**不会**关闭
  英文上下文排序——有输入字母时的条件排序照常使用静态二元数据；
- `XWX_ENGLISH_BIGRAM_PATH`:静态二元数据 `english_bigram.bin` 的路径(缺省
  `$XDG_DATA_HOME/xwx/english_bigram.bin`,其次 `/usr/share/xwx/english_bigram.bin`)。
  打不开只降级条件排序与预测面板,不影响已加载的英文词库;
- `XWX_LM_PATH`:覆盖**运行时读取**的语言模型路径。未设置时按优先级自动
  探测 `$XDG_DATA_HOME/xwx/` 下的 `.gram`(首选 `wanxiang-lts-zh-hans.gram`,
  否则任意 `.gram` 按文件名字典序第一个),最后回落 `bigram.txt`;
- `XWX_BIGRAM_OUT`:`data/README.md` 第五节训练脚本的**输出**路径,
  默认 `$XDG_DATA_HOME/xwx/bigram.txt`。和 `XWX_LM_PATH` 是独立的,
  避免脚本误把训练产出覆盖到 `.gram` 模型文件上。

## 开发

```bash
just verify                               # check + test 一把过（提交前跑这个）
cargo test --workspace --offline          # 全部测试（必须在项目根跑）
cargo clippy --workspace --all-targets --offline -- -D warnings   # 硬门槛，必须全绿
cargo test -p xwx_engine                  # 只跑引擎（切分/词库/LM/会话/造词）
cargo test -p xwx_ibus                    # 协议序列化与按键翻译测试
```

当前基线 **290 passed / 0 failed / 1 ignored** = 241 单元（227 引擎 + 5 协议
+ 9 二进制）+ **49 D-Bus 集成**。

> 测试分三层，**别把上层当成下层的证据**：
> - **引擎/会话层单测**（`crates/*/src/*.rs` 的 `mod tests`）——算法与状态机，
>   数量最多、跑得最快；
> - **D-Bus 集成测试**（根 `tests/`）——跑真实 `xwx_ime --ibus` 进程 + 私有
>   `dbus-daemon`，覆盖单测**结构上摸不到**的路径：英文模式透传、
>   `CommitText`/`UpdatePreeditText`/`UpdateLookupTable` 信号内容、
>   `FocusOut`、候选点击、翻页、按键透传返回值。11 个文件（每个文件一组场景，
>   原来是 5 个）：`verbatim_url`(7)、`wire_abbrev`(5)、`mods_keys`(4)、
>   `prefix_commit`(1)、`paging`(7)、`blocklist_delete`(2)、
>   `focus_lifecycle`(4)、`punctuation_paging`(1)、`passthrough_contract`(7)、
>   `signal_payloads`(7)、`hermeticity`(5)；
> - **实机验证**（`docs/VERIFY.md`）——IBus daemon + 真实应用，仍未自动化。

> **集成层的正确性要求比单测更严**：它跨进程、跨 D-Bus，
> 「没收到信号」和「断言不成立」必须能分开（一次交付里六处设施缺陷
> 让整个套件『看起来全绿/全红但都指向引擎』）。
> 写新集成用例前请先读那一条——尤其：**不要用 `let Ok(x) = .. else { continue }`
> 静默吞掉解析失败**，**每个 D-Bus 调用必须有超时**。

`xwx_ibus` 的测试里有一组「与 ibus 1.5.29 类型串逐字对照」的断言——
IBus 的 D-Bus 序列化要求类型串与字段顺序严格一致，改协议前先跑它。

### 性能必须用 `--release` 测

debug 构建下同一份代码是 9.6ms 量级，release 是 1.5ms。曾据此误判
「退化 7 倍」。基准一律加 `--release`：

```bash
XWX_DICT_PATH=$PWD/data/dict.bin XWX_ABBREV_PATH=$PWD/data/abbrev.bin \
  cargo run --release --offline -p xwx_engine --example lookup_bench
```

可用的基准/诊断工具（都在 `crates/xwx_engine/examples/`）：
`lookup_bench`（端到端击键延迟）、`abbrev_bench`（简拼查询）、
`decode_bench`（beam 解码）、`fuzzy_bench`（模糊开关的代价/收益）、
`lm_ctx_probe`（空上下文 LM 行为）、`probe`（对拍探针）。

> `.cargo/config.toml` 把 target 目录指到了 `/mnt/hdd/target/xwx_ime`。
> **不要覆盖 `CARGO_TARGET_DIR`**——在别处验证等于没验证。

## 目录结构

```
.                        ← 根即 xwx_ime 二进制（workspace 根 + bin crate）
├── Cargo.toml            package.metadata.deb：deb 打包声明（assets/postinst）
├── src/
│   └── main.rs           进程入口：CLI、连总线、注册组件、导出对象
├── crates/
│   ├── xwx_engine/
│   │   ├── examples/         基准与对拍工具（probe / *_bench / lm_ctx_probe）
│   │   └── src/
│   │       ├── candidate.rs   候选词结构（serde 序列化）
│   │       ├── segmenter.rs   音节表 + DP 切分
│   │       ├── dict.rs        Dictionary trait + MmapDict（运行时）/ SqliteDict（构建/测试）
│   │       ├── lm.rs          LanguageModel trait + BigramLM + MixedLM（用户层混合）
│   │       ├── wanxiang.rs    万象 .gram（mmap 字符级窗口模型）
│   │       ├── user_lm.rs     用户词频/造词/屏蔽，落盘 user_bigram.txt
│   │       ├── corrector.rs   模糊拼音混淆对 + 展开
│   │       ├── punctuator.rs  全角标点映射
│   │       ├── recognizer.rs  邮箱 / 网址识别
│   │       ├── session.rs     按键状态机（InputSession）
│   │       └── lib.rs         ImeEngine 聚合入口（切分格 + beam 解码 + 排序）
│   └── xwx_ibus/
│       └── src/
│           ├── ibus_proto.rs  IBus 键值/按键翻译/GVariant/component XML 构造
│           ├── engine.rs      Engine 接口 + Factory（GNOME Shell 候选窗信号）
│           └── lib.rs         适配层导出入口
tests/                      D-Bus 端到端集成测试（单测摸不到的那几条路径）
docs/
├── VERIFY.md               实机验证清单（37 条）
└── offline-install.md      离线交付清单
data/
├── dict.bin              运行时只读二进制词库（dict_to_bin.py 生成，mmap 映射）
├── abbrev.bin           简拼(首字母)副索引(abbrev_to_bin.py 生成,mmap 映射)
├── xwx.xml                component XML 模板（安装时注入 exec 路径）
├── README.md              词库二进制格式、LM 格式、Rime 导入教程
└── debian/               deb 打包资产（cargo deb 用）
    ├── postinst             deb 装完刷新 ibus-daemon 的钩子
    └── xwx.xml             打 deb 生成的 component XML（构建产物，exec=/usr/libexec/ibus-engine-xwx）
scripts/
├── build_dict.py          词库构建工具（Rime → dict.sqlite3）
├── dict_to_bin.py         dict.sqlite3 → dict.bin(运行时二进制格式)
└── abbrev_to_bin.py      dict.bin → abbrev.bin(简拼副索引,只读主索引)
tools/
├── rime_oracle/           librime 1.10.0 驱动的差分对拍台
├── compare.py             对拍指标（注意 TOP10_OVERLAP 是陈旧标签）
└── corpus/basic.txt       81 行 9 组分类语料
justfile                    任务入口：build / dev / install / deb
```

## TODO Roadmap

> 这张表 2026-10-01 重写过一次。旧的六个条目里**有五个其实早就做完了**
> （用户词频学习、模糊音接入排序、候选生成升级成 beam、全角标点），
> 却一直挂着未打勾——对外表述比实际能力又少又错。
> 有一条（模糊拼音）还反过来：打着 `[x]` 但生产里根本没法启用。
> 现状以本表为准。

- [x] ~~用户词频学习~~ — 已完成，见上方功能状态
- [x] ~~模糊音接入排序~~ — 已完成（`expand_fuzzy_scored` 带上
      `-log10(weight)` 惩罚并按 `SpellingKind::Fuzzy` 分层），
      且已接成 `XWX_FUZZY` 开关
- [x] ~~候选生成算法升级~~ — 已完成（`build_lattice` + `decode_beam`
      的 beam search，`BEAM_WIDTH = 16`）
- [x] ~~全角标点~~ — 已完成（`punctuator.rs`）
- [x] ~~`bigram.bin` mmap 格式~~ — 已废弃：通用语言模型走了**万象
      `.gram`**（mmap 字符级窗口模型），不再自研 bigram 二进制；
      `bigram.txt` 格式保留为用户学习模型（`user_bigram.txt`）的存储格式
- [ ] **配置化**：目前只有模糊拼音做成了环境变量开关；候选页大小、
      模糊音**按组**开关（`,` 里 `weight` 已就位）、词库路径等仍是硬编码/环境变量
- [x] ~~混合简拼支持任意位置缩写~~ — 已完成（交错 DFS：缩写在开头
      `nhao`→您好、中间 `feicde`→非常地、尾部 `zhongg`→中国 都能召回，
      见 `dict/mixed.rs`）
      （`feicde`、`nhao`）。实测与算法原型见 `tools/spike_abbrev/walk.py`
- [ ] **裸 `xian` 的排序**：「西安」第 18 名。已完整诊断（空上下文下
      通用层 LM 返回常数，见 `examples/lm_ctx_probe.rs`），
      绕行手段是 `xi'an`。跨候选归一化是唯一未试过的方向
- [ ] **实机验证**：`docs/VERIFY.md` 的清单。集成层是当前可信度最低的部分

## 环境变量

| 变量 | 作用 | 默认 |
|---|---|---|
| `XWX_DICT_PATH` | 词库 `dict.bin` 路径 | `$XDG_DATA_HOME/xwx/dict.bin` |
| `XWX_ABBREV_PATH` | 简拼副索引 `abbrev.bin` 路径 | 与 `dict.bin` 同目录 |
| `XWX_LM_PATH` | 语言模型路径（`.gram` 或 bigram 文本） | 自动探测 `$XDG_DATA_HOME/xwx/` |
| `XWX_USER_LM_PATH` | 用户学习模型 `user_bigram.txt` | 同 `XWX_DICT_PATH` 的目录 |
| `XWX_FUZZY` | 模糊拼音开关（`1`/`true`/`on`/`yes` 打开） | **关闭**（见上方实测理由） |

> 无法识别的 `XWX_FUZZY` 取值会记一条 warning 后按默认值处理，
> **不会**让引擎启动失败——输入法起不来比模糊音没开严重得多。

## 离线交付与数据放置（Q5）

装完**不依赖在线重建**即可用；完整清单与验证步骤见 `docs/offline-install.md`。

**数据文件放哪**
| 文件 | 用户级（默认） | 系统级（deb 使用） | 环境变量覆盖 |
|---|---|---|---|
| 词库 `dict.bin` | `$XDG_DATA_HOME/xwx/dict.bin`（默认 `~/.local/share/xwx/`） | `/usr/share/xwx/dict.bin` | `XWX_DICT_PATH` |
| 简拼副索引 `abbrev.bin` | 同目录 | `/usr/share/xwx/abbrev.bin` | `XWX_ABBREV_PATH` |
| 语言模型 `*.gram` | 同目录 | 同目录 | `XWX_LM_PATH` |
| 用户学习数据 `user_bigram.txt` | 同目录（**只写这里**，不进包） | 不用 | `XWX_USER_LM_PATH` |

查找顺序是「用户级优先 → 系统级回退 → 都没有时报缺文件并告警」。词库与副索引
**必须同代**（副索引的关联数等于词库词条数），否则副索引不会被装配、简拼退化。

**语言模型为什么不随包分发**：万象 `.gram` 体积大且授权独立，请自行放到上表位置；
**没有模型也能用**，但候选排序会退化为纯词频（日志里会有提示）。

**注册输入源**（装完一次即可）
1. 确认组件文件在位：`/usr/share/ibus/component/xwx.xml`；
2. 刷新缓存：`ibus write-cache`（`just install`/deb 已自动做，失败也无妨）；
3. 把 `xwx` 加进输入源：GNOME 设置 → 键盘 → 输入源 → `+` → 中文（中国）→ **XWX 拼音**；
   命令行等价：`gsettings set org.gnome.desktop.input-sources sources "[('xkb','us'),('ibus','xwx')]"`；
4. 验证：`ibus list-engine | grep xwx` 应出现 `xwx - XWX 拼音`。

**排查**
- 看到的是旧行为 / 完全没有候选：先确认跑的是新二进制（`just dev` 用 debug 构建、
  `just install` 用 release），以及 `--ibus` 进程路径（`pgrep -af xwx_ime`）；
- 简拼不工作：多半是副索引缺失或不同代，看日志里有没有「副索引…不装配」的告警。

## 许可证

[MIT](LICENSE) © 2025 wenxb

词库与语言模型数据**不在本许可证覆盖范围内**（不随仓库分发），使用时请遵守
各自的授权条款。
