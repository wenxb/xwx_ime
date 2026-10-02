# XWX 拼音输入法 — 任务入口
#
# 依赖：cargo, python3, just, ibus（运行），sudo（install/deb 需要）
#
# 常用：
#   just build    编译所有
#   just dev      前台跑引擎（debug），挂到运行中的 ibus-daemon
#   just xml      输出 component XML（默认 stdout）
#   just install  一键安装到系统（二进制 + XML，需要 sudo）
#   just deb      打 .deb 包

set shell := ["bash", "-uc"]
set ignore-comments := true

# target 目录由 cargo 决定（尊重 .cargo/config.toml 的 target-dir）
target_dir := `cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])'`
home      := env("HOME")
bin_dir   := env_var_or_default("XDG_BIN_HOME", home + "/.local/bin")
data_home := env_var_or_default("XDG_DATA_HOME", home + "/.local/share")
xwx_data  := data_home + "/xwx"
sys_comp_dir := "/usr/share/ibus/component"

default:
    @just --list

# 编译整个 workspace
build:
    cargo build --workspace

# 编译 release 版整个 workspace
build-release:
    cargo build --release --workspace

# 全量测试（引擎/会话层单测 + 协议测试 + D-Bus 集成测试）
test:
    cargo test --workspace --offline

# clippy 全绿（-D warnings），验收硬门槛
check:
    cargo clippy --workspace --all-targets --offline -- -D warnings

# test + check 一把过（提交前跑这个）
verify: check test

# 逐键性能验收(生产装配:真词库 + 简拼副索引 + 万象 .gram + 用户层内存态)
# 任一场景稳态 p95 超过 12ms 时非零退出;max 与超线样本照实打印。
perf:
    cargo run --release --offline -p xwx_engine --example perf_probe -- --repeat 5

# dev：前台跑引擎（debug，日志直接打在终端）。一个总线名只能一个 owner：
# daemon 按 component XML 拉起过 release 引擎时要先让它让位，否则 NameTaken；
# 让出来后 daemon 会直接改用这个实例（实测有效）。
#
# **先确保 daemon 在跑**，再让位——顺序反了第一次必失败（实测）：daemon 不在时
# 调试引擎自己会拉起一个 daemon，而那个 daemon 一启动就按 XML 激活 release
# 引擎抢走名字，0.5 秒窗口完全失效。daemon 已存在时它**不会**立刻回拉
# （实测 5 轮窗口安全），所以这一步之后让位窗口才是可靠的。
# 按**进程名**精确匹配（pgrep -x）而不是全命令行：全命令行匹配会连
# 「正在执行这条命令的 shell」一起匹配上，等于自杀（调试时踩过两次）。
dev:
    cargo build -p xwx_ime
    if ! pgrep -x ibus-daemon >/dev/null; then ibus-daemon -rdx; sleep 2; fi
    -for pid in $(pgrep -x xwx_ime); do kill "$pid"; done
    sleep 0.5
    exec {{target_dir}}/debug/xwx_ime --ibus

# 输出 component XML 到 stdout（打 deb 时用 --exec 指 /usr/bin/xwx_ime）
xml *args:
    cargo build -p xwx_ime
    @{{target_dir}}/debug/xwx_ime --xml {{args}}

# 安装：二进制、XML（sudo 到系统目录）、词库，并刷新 ibus-daemon
# 每行都以 `@` 开头：不回显命令文本，只留警告与结尾的四行摘要。
install:
    @cargo build --release --quiet -p xwx_ime
    @mkdir -p {{bin_dir}} {{xwx_data}}
    @install -m 755 {{target_dir}}/release/xwx_ime {{bin_dir}}/xwx_ime
    @sed 's#__XWX_EXEC__#{{bin_dir}}/xwx_ime#g' data/xwx.xml > /tmp/xwx-component.xml
    @sudo install -d {{sys_comp_dir}}
    @sudo install -m 644 /tmp/xwx-component.xml {{sys_comp_dir}}/xwx.xml
    @rm -f /tmp/xwx-component.xml
    # 词库与副索引：缺任何一个只是功能降级，提醒但不算失败。
    @if test -f data/dict.bin; then install -m 644 data/dict.bin {{xwx_data}}/dict.bin; \
    else echo "警告：data/dict.bin 不存在，引擎只剩极小的内置词库。"; fi
    @if test -f data/abbrev.bin; then install -m 644 data/abbrev.bin {{xwx_data}}/abbrev.bin; \
    else echo "警告：data/abbrev.bin 不存在，简拼不可用（生成：python3 scripts/abbrev_to_bin.py）。"; fi
    @if pgrep -x fcitx5 >/dev/null; then echo "警告：fcitx5 正在运行，它会占用 IBus 总线名（需要时 fcitx5 -k）。"; fi
    @if ! ls {{xwx_data}}/*.gram >/dev/null 2>&1 && ! test -f {{xwx_data}}/bigram.txt; then \
        echo "提示：未检测到语言模型，候选排序退化为纯词频（把万象 .gram 放进 {{xwx_data}}/ 即可）。"; \
    fi
    # 刷新 daemon：重启会话里的 ibus-daemon，让它重扫 component 目录。
    @if command -v ibus >/dev/null 2>&1; then ibus restart >/dev/null 2>&1 || true; else for pid in $(pgrep -x ibus-daemon); do kill "$pid" 2>/dev/null || true; done; fi
    @echo "已安装  {{bin_dir}}/xwx_ime  $(stat -c%s {{bin_dir}}/xwx_ime) 字节  sha256 $(sha256sum {{bin_dir}}/xwx_ime | cut -c1-12)"
    @echo "组件    {{sys_comp_dir}}/xwx.xml  engine xwx / zh_CN"
    @echo "词库    {{xwx_data}}/dict.bin + abbrev.bin"
    @echo "输入源  gsettings set org.gnome.desktop.input-sources sources \"[('xkb','us'),('ibus','xwx')]\""

# 打 deb 包
deb:
    cargo build --release -p xwx_ime
    mkdir -p data/debian
    {{target_dir}}/release/xwx_ime --xml data/debian/xwx.xml --exec /usr/bin/xwx_ime
    cargo deb -p xwx_ime --no-strip