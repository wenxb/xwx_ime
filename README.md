<div align="center">

# XWX 拼音输入法

**用 Rust 写的 Linux 拼音输入法 · 标准 IBus 引擎**

[![Version](https://img.shields.io/badge/version-0.1.0-blue.svg)]()
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-Linux%20%C2%B7%20IBus-2ea44f.svg)]()

</div>

XWX 是一个从零实现的 Linux 拼音输入法：算法层用纯 Rust 编写，接口层实现标准
IBus Engine，拼音切分、词库、语言模型和会话状态机都在 `xwx_engine` 内。
候选窗交给 GNOME Shell / IBus 原生组件渲染，不自己画窗口。

## 功能

- 全拼、首字母简拼（`zgr`）、混合简拼（`zhongg`），单引号 `'` 作音节分隔符
- 每页 9 条候选，支持数字键、方向键、鼠标点击选择
- 用户词频学习、逐字造词、`Ctrl+Delete` 屏蔽候选、`Ctrl+Shift+Delete` 撤销
- 中文标点；邮箱和网址原样上屏
- 中英混输：英文前缀补全、连续英文自动补空格、可选的下一词预测
- 可编辑输入缓冲、一次编辑纠错、模糊拼音（默认关闭）
- 词库 mmap 只读加载，语言模型支持万象 `.gram`

## 架构

```text
应用 / ibus-daemon
  └── xwx_ime      进程入口：连接总线、注册组件
        └── xwx_ibus  IBus 适配：按键翻译、Engine 接口
              └── xwx_engine  算法核心：切分、词格、词库、LM、会话
```

| crate | 说明 |
| --- | --- |
| `xwx_engine` | 拼音切分、mmap 词库、语言模型、模糊拼音、标点、网址识别、用户词频、会话状态机 |
| `xwx_ibus` | `org.freedesktop.IBus.Engine` 接口实现，用 `UpdateLookupTable` / `UpdatePreeditText` 驱动候选窗 |
| `xwx_ime` | 进程入口：CLI、总线连接、组件注册、D-Bus 对象导出 |

## 构建

需要 Rust 1.85+。编译不依赖系统库。

```bash
git clone https://github.com/wenxb/xwx_ime.git
cd xwx_ime
just build-release
```

## 安装

```bash
just install
```

会把二进制装到 `~/.local/bin/xwx_ime`，component XML 装到
`/usr/share/ibus/component/xwx.xml`（这一步需要 sudo），词库复制到
`~/.local/share/xwx/`，最后重启 `ibus-daemon`。

注册输入源：

```bash
gsettings set org.gnome.desktop.input-sources sources \
  "[('xkb','us'),('ibus','xwx')]"
```

之后用 `Super+空格` 切到 XWX 拼音，引擎内用 `Ctrl+空格` 切换中英文。

> XML 必须放在系统目录：IBus 1.5.x 不会扫描 `~/.local/share/ibus/component/`
> （[ibus#2754](https://github.com/ibus/ibus/issues/2754)）。

## 数据

词库不随仓库分发，首次使用前先生成：

```bash
python3 scripts/build_dict.py data/dict.sqlite3
python3 scripts/dict_to_bin.py data/dict.sqlite3 data/dict.bin
python3 scripts/abbrev_to_bin.py
python3 scripts/build_syllable_graph.py
just install
```

`build_dict.py` 优先读取 fcitx5-rime 的 rime_ice，找不到时回落到系统
`luna_pinyin`。语言模型可选，把万象 `wanxiang-lts-zh-hans.gram` 放进
`~/.local/share/xwx/` 即可；没有模型时排序退化为纯词频。

词库与副索引必须同代，否则简拼不会生效。二进制格式和英文数据构建见
[`data/README.md`](data/README.md) 与 [`data/ENGLISH.md`](data/ENGLISH.md)。

## 使用

| 按键 | 作用 |
| --- | --- |
| 字母 | 输入拼音 |
| 空格 | 上屏当前候选 |
| `1`–`9` / `0` | 选择当页候选（`0` 为第 10 项） |
| ↑ / ↓ | 移动候选高亮 |
| ← / → | 在拼音串内移动光标 |
| `PageUp` / `PageDown` | 翻页 |
| `-` / `=` | 候选窗打开时翻页 |
| `,` / `.` | 中文标点；候选窗打开时 `.` 翻页、`,` 回翻 |
| 单引号 `'` | 音节分隔符（`xi'an` → 西安） |
| `Enter` | 原样上屏当前输入 |
| `Esc` | 取消本次输入 |
| `Ctrl+空格` | 中英文切换 |
| `Ctrl+Delete` | 删除并屏蔽当前候选 |
| `Ctrl+Shift+Delete` | 撤销上一次屏蔽 |
| `Tab` | 预测面板中接受高亮的下一词 |

## 配置

通过环境变量配置，没有图形设置界面。

| 变量 | 作用 | 默认 |
| --- | --- | --- |
| `XWX_DICT_PATH` | 中文词库 `dict.bin` | `$XDG_DATA_HOME/xwx/dict.bin` |
| `XWX_ABBREV_PATH` | 简拼副索引 `abbrev.bin` | 与 `dict.bin` 同目录 |
| `XWX_LM_PATH` | 语言模型（`.gram` 或 `bigram.txt`） | 自动探测数据目录 |
| `XWX_USER_LM_PATH` | 中文用户模型 `user_bigram.txt` | 与 `dict.bin` 同目录 |
| `XWX_FUZZY` | 模糊拼音开关 | 关闭 |
| `XWX_ENGLISH` | 英文通道总开关 | 开启 |
| `XWX_ENGLISH_DICT_PATH` | 英文词库 `english.bin` | 用户目录，回退 `/usr/share/xwx/` |
| `XWX_ENGLISH_USER_LM_PATH` | 英文用户模型 `user_english.txt` | `$XDG_DATA_HOME/xwx/` |
| `XWX_ENGLISH_PREDICT` | 零输入下一词面板 | 关闭 |

布尔变量接受 `1/true/on/yes` 与 `0/false/off/no`。路径类变量中，可写路径
不会回落到系统目录。模糊拼音默认关闭：开启后 `womenjintianquxian` 的延迟从
3.4 ms 升到 22.0 ms，而只多召回 2 组候选。

## 开发

```bash
just verify     # clippy -D warnings + 全部测试
just dev        # 前台运行 debug 引擎
just perf       # 逐键延迟验收（release）
```

测试分单元测试、D-Bus 集成测试和实机验证三层。集成测试在根目录 `tests/`，
会启动真实的 `xwx_ime --ibus` 进程和私有 `dbus-daemon`；实机验证清单在
`docs/VERIFY.md`，尚未自动化。

性能数字必须用 release 构建测量，debug 下会慢约 6 倍。当前生产装配的稳态
逐键延迟为 p50 0.32 ms、p95 5.18 ms（609 个样本，不含 D-Bus IPC）。

## 已知问题

- `xian` 的首选是「先」，「西安」排第 18；可以用 `xi'an`，或依赖上文中的语言模型。
- 模糊拼音默认关闭，开启后长串输入明显变慢。
- 没有图形设置界面，候选页大小等参数仍是硬编码或环境变量。
- 仅支持 Linux / IBus，没有 fcitx5、macOS、Windows 版本。
- 实机验证仍需人工执行，见 `docs/VERIFY.md`。

## 文档

- [项目指南](docs/PROJECT_GUIDE.md)
- [实机验证清单](docs/VERIFY.md)
- [离线交付说明](docs/offline-install.md)
- [英文输入设计](docs/english-input/01-data.md)

## 许可证

[MIT](LICENSE) © 2025 wenxb

词库与语言模型数据不在 MIT 覆盖范围内，也不随仓库分发。英文数据派生自
[wordfreq](https://github.com/rspeer/wordfreq)（数据 CC BY-SA 4.0）和
[SymSpell](https://github.com/wolfgarbe/SymSpell)，署名与许可全文见
[`data/english/NOTICE.md`](data/english/NOTICE.md)。
