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
| `xwx_engine` | lib | 拼音切分（DP）、mmap 只读二进制词库、二元语言模型、模糊拼音、按键会话状态机。不依赖任何 GUI/DBus，可独立测试 |
| `xwx_ibus` | lib | 标准 IBus engine：`org.freedesktop.IBus.Engine` 接口实现、Factory 按需创建引擎实例，用 `UpdateLookupTable`/`UpdatePreeditText` 等信号驱动 GNOME Shell 渲染候选窗 |
| `xwx_ime` | bin | 唯一进程入口：CLI 解析（`--ibus` / `--xml`）、连总线、`RegisterComponent`、导出 D-Bus 对象 |

## 功能状态

- [x] 拼音音节表（400+ 音节）与 DP 全切分
- [x] mmap 只读二进制词库（`dict.bin`，启动 O(1) 映射、查询二分；
      `scripts/dict_to_bin.py` 从 SQLite 转换；SQLite 仅作构建/迁移中间格式）
- [x] 简拼(首字母)副索引存储层(`abbrev.bin`,`scripts/abbrev_to_bin.py`
      从 `dict.bin` 只读生成;`MmapDict::with_abbrev` 装配,纯简拼 `zgr` 二分副索引、
      混合简拼 `zhongguor` 走音节切分+主索引前缀;基准 `examples/abbrev_bench.rs`)
- [x] 二元语言模型（文本格式加载；mmap 二进制格式见 `data/README.md` 规划）
- [x] 模糊拼音（n/l、zh/z、前后鼻音等 13 组混淆对）
- [x] 按键状态机：字母、退格、空格选词、数字选词、翻页、方向键、Esc、回车
- [x] IBus 引擎：`CommitText`/`UpdatePreeditText`/`UpdateLookupTable` 信号 +
      组件注册（运行时 `RegisterComponent` + 安装用 component XML 双通道）
- [x] 候选窗由 **GNOME Shell candidate popup** 渲染（不再自绘窗口）
- [x] **Ctrl+空格 中英文切换**（切到英文时字母直接透传给应用，
      切换由引擎自己的会话状态决定）
- [ ] 用户词频学习（会话结束回写词库）
- [ ] 模糊拼音权重接入排序
- [ ] 自造词/删除词

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
4. 拷贝 `data/dict.bin` 到 `~/.local/share/xwx/`（存在才拷）；
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
> `org.freedesktop.IBus` 总线名。它在跑时 XWX 无法注册。切换输入法框架请先；
> `fcitx5 -k && pkill -x fcitx5`。

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
just xml -- --exec /usr/bin/xwx_ime   # 指定 exec 路径（打 deb 用）
```

也可以直接用 `RUST_LOG=debug cargo run -p xwx_ime -- --ibus` 前台调试。

验证引擎是否注册成功：

```bash
gdbus call --session --dest org.freedesktop.IBus --object-path /org/freedesktop/IBus \
  --method org.freedesktop.IBus.ListEngines
# 输出里应能看到 XWX 拼音输入法
```

## 数据准备

首次运行词库为空，输入法不会出候选。准备词库：

1. 按 `data/README.md` 第四节教程从 Rime `luna_pinyin` 导入（产出
   `dict.sqlite3` 中间格式），再运行
   `python3 scripts/dict_to_bin.py ~/.local/share/xwx/dict.sqlite3 ~/.local/share/xwx/dict.bin`
   转成运行时二进制（也可直接用 `XWX_DICT_PATH` 指向自己生成的 dict.bin）；
2. 可选：按第五节用语料训练语言模型 `bigram.txt`。

数据文件路径 `xwx_ime` 默认从 `$XDG_DATA_HOME/xwx/`(通常
`~/.local/share/xwx/`)读取。环境变量方向不同,别搞混:

- `XWX_DICT_PATH`:覆盖词库 `dict.bin` 的读取路径;
- `XWX_LM_PATH`:覆盖**运行时读取**的语言模型路径。未设置时按优先级自动
  探测 `$XDG_DATA_HOME/xwx/` 下的 `.gram`(首选 `wanxiang-lts-zh-hans.gram`,
  否则任意 `.gram` 按文件名字典序第一个),最后回落 `bigram.txt`;
- `XWX_BIGRAM_OUT`:`data/README.md` 第五节训练脚本的**输出**路径,
  默认 `$XDG_DATA_HOME/xwx/bigram.txt`。和 `XWX_LM_PATH` 是独立的,
  避免脚本误把训练产出覆盖到 `.gram` 模型文件上。

## 开发

```bash
cargo check                       # 全量编译校验
cargo test -p xwx_engine          # 引擎单元测试（切分/词库/LM/会话）
cargo test -p xwx_ibus             # 协议序列化与按键翻译测试
```

`xwx_ibus` 的测试里有一组「与 ibus 1.5.29 类型串逐字对照」的断言——
IBus 的 D-Bus 序列化要求类型串与字段顺序严格一致，改协议前先跑它。

## 目录结构

```
.                        ← 根即 xwx_ime 二进制（workspace 根 + bin crate）
├── Cargo.toml            package.metadata.deb：deb 打包声明（assets/postinst）
├── src/
│   └── main.rs           进程入口：CLI、连总线、注册组件、导出对象
├── crates/
│   ├── xwx_engine/
│   │   └── src/
│   │       ├── candidate.rs   候选词结构（serde 序列化）
│   │       ├── segmenter.rs   音节表 + DP 切分
│   │       ├── dict.rs        Dictionary trait + MmapDict（运行时）/ SqliteDict（构建/测试）
│   │       ├── lm.rs          LanguageModel trait + BigramLM
│   │       ├── corrector.rs   模糊拼音混淆对 + 展开
│   │       ├── session.rs     按键状态机（InputSession）
│   │       └── lib.rs         ImeEngine 聚合入口
│   └── xwx_ibus/
│       └── src/
│           ├── ibus_proto.rs  IBus 键值/按键翻译/GVariant/component XML 构造
│           ├── engine.rs      Engine 接口 + Factory（GNOME Shell 候选窗信号）
│           └── lib.rs         适配层导出入口
data/
├── dict.bin              运行时只读二进制词库（dict_to_bin.py 生成，mmap 映射）
├── abbrev.bin           简拼(首字母)副索引(abbrev_to_bin.py 生成,mmap 映射)
├── xwx.xml                component XML 模板（安装时注入 exec 路径）
├── README.md              词库二进制格式、LM 格式、Rime 导入教程
└── debian/               deb 打包资产（cargo deb 用）
    ├── postinst             deb 装完刷新 ibus-daemon 的钩子
    └── xwx.xml             打 deb 生成的 component XML（构建产物，exec=/usr/bin/xwx_ime）
scripts/
├── build_dict.py          词库构建工具（Rime → dict.sqlite3）
├── dict_to_bin.py         dict.sqlite3 → dict.bin(运行时二进制格式)
└── abbrev_to_bin.py      dict.bin → abbrev.bin(简拼副索引,只读主索引)
justfile                    任务入口：build / dev / install / deb
```

## TODO Roadmap

- [ ] **用户词频学习**：会话上屏后把 (word, pinyin) 频次回写词库，查询时
      叠加用户权重（`dict.rs` 已预留 `user_entries` 的 TODO）
- [ ] **模糊音接入排序**：`ConfusionPair::weight` 目前只做配置占位，
      展开变体应降权
- [ ] **bigram.bin mmap 格式**：`lm.rs` 的 TODO，布局见 `data/README.md`
- [ ] **候选生成算法升级**：当前是穷举 + 合并，改为 Viterbi/beam search
      在切分图上直接做语言模型解码（`lib.rs` TODO）
- [ ] **全角标点**：拼音串非空时 `,`/`.` 等直接上屏全角标点（`session.rs` TODO）
- [ ] 配置化（模糊音开关、候选页大小）
