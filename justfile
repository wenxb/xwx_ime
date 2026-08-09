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

# dev：编译并前台跑引擎（debug，连接到 ibus-daemon）
dev:
    cargo build -p xwx_ime
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
    # fcitx5 占用 org.freedesktop.IBus 时给出提示
    if pgrep -x fcitx5 >/dev/null; then echo "警告：fcitx5 正在运行，会占用 IBus 总线名。需要时先 fcitx5 -k 停掉。" >&2; fi
    # 刷新 daemon
    if command -v ibus >/dev/null 2>&1; then ibus restart >/dev/null 2>&1 || true; else pkill -x ibus-daemon >/dev/null 2>&1 || true; fi
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