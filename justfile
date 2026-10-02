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
install:
    cargo build --release -p xwx_ime
    mkdir -p {{bin_dir}} {{xwx_data}}
    install -m 755 {{target_dir}}/release/xwx_ime {{bin_dir}}/xwx_ime
    sed 's#__XWX_EXEC__#{{bin_dir}}/xwx_ime#g' data/xwx.xml > /tmp/xwx-component.xml
    sudo install -d {{sys_comp_dir}}
    sudo install -m 644 /tmp/xwx-component.xml {{sys_comp_dir}}/xwx.xml
    rm -f /tmp/xwx-component.xml
    # 词库（dict.bin，源缺失时跳过；dict_to_bin.py 从 SQLite 生成）
    if test -f data/dict.bin; then install -m 644 data/dict.bin {{xwx_data}}/dict.bin; fi
    # 简拼副索引（abbrev.bin，scripts/abbrev_to_bin.py 从 dict.bin 生成）。
    # 缺了不会崩，但简拼（zgr → 中国人）整个失效，只剩全拼。
    if test -f data/abbrev.bin; then install -m 644 data/abbrev.bin {{xwx_data}}/abbrev.bin; \
    else echo "提示：data/abbrev.bin 不存在，简拼将不可用。生成：python3 scripts/abbrev_to_bin.py" >&2; fi
    # fcitx5 占用 org.freedesktop.IBus 时给出提示
    if pgrep -x fcitx5 >/dev/null; then echo "警告：fcitx5 正在运行，会占用 IBus 总线名。需要时先 fcitx5 -k 停掉。" >&2; fi
    # 刷新 daemon
    if command -v ibus >/dev/null 2>&1; then ibus restart >/dev/null 2>&1 || true; else for pid in $(pgrep -x ibus-daemon); do kill "$pid" 2>/dev/null || true; done; fi
    # 语言模型不进 install / .deb(400 MB、CC-BY-4.0、需另行下载),
    # 但装完提醒一句,免得排序默默退化成纯词频。
    if ! ls {{xwx_data}}/*.gram >/dev/null 2>&1 && ! test -f {{xwx_data}}/bigram.txt; then \
        echo "提示:未检测到语言模型,候选排序将退化为纯词频。"; \
        echo "     把万象 .gram(如 wanxiang-lts-zh-hans.gram)放到 {{xwx_data}}/ 下即可自动启用,"; \
        echo "     或按 data/README.md 第五节训练 bigram.txt 放到该目录。"; \
    fi
    echo "完成。输入源：gsettings set org.gnome.desktop.input-sources sources \"[('xkb','us'),('ibus','xwx')]\""

# 打 deb 包
deb:
    cargo build --release -p xwx_ime
    mkdir -p data/debian
    {{target_dir}}/release/xwx_ime --xml data/debian/xwx.xml --exec /usr/bin/xwx_ime
    cargo deb -p xwx_ime --no-strip