//! xwx：XWX 拼音输入法 IBus 引擎守护进程。
//!
//! 启动流程：
//! 1. 初始化 tracing；
//! 2. 打开词库与语言模型:
//!    - 词库:`XWX_DICT_PATH` 覆盖,否则 `$XDG_DATA_HOME/xwx/dict.bin`;
//!    - 语言模型:`XWX_LM_PATH` 显式覆盖(原样尊重,即使文件不存在,
//!      让现有逻辑优雅退化为空模型);未设置时按以下优先级探测
//!      `$XDG_DATA_HOME/xwx/` 目录:
//!      - 首选 `wanxiang-lts-zh-hans.gram`(简体万象,最常见);
//!      - 否则取目录下任意 `.gram` 文件(如繁体 `wanxiang-lts-zh-hant.gram`),
//!        多个时按文件名字典序取第一个,保证确定性;
//!      - 都没有时回落到 `bigram.txt`(文本 bigram,旧行为)。
//! 3. 连接 IBus 总线：优先 `IBUS_ADDRESS` 环境变量，其次读
//!    ibus-daemon 留下的 `$XDG_RUNTIME_DIR/ibus/bus` 启动脚本，
//!    最后退回 session bus；
//! 4. 把组件注册进 ibus-daemon（`RegisterComponent`，daemon 不在线时
//!    仅告警，引擎进程照常运行）；
//! 5. 占用 `org.freedesktop.IBus.Engine.xwx` 服务名，导出 Factory，
//!    预导出首个引擎实例，随后等待 daemon 调 `CreateEngine`。
//!
//! 命令行模式：
//! - `xwx_ime` / `xwx_ime --ibus`：以上流程正常启动（连接 daemon）；
//! - `xwx_ime --xml [路径]`：输出 component XML（默认 stdout），用于
//!   快速核对引擎名 / exec 路径，或直接落盘安装。

use std::path::PathBuf;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use xwx_engine::ImeEngine;
use xwx_ibus::engine::{EngineFactory, XwxEngineServer};
use xwx_ibus::ibus_proto::{
    self, component_value, FACTORY_PATH, IBUS_INTERFACE, IBUS_PATH, IBUS_SERVICE,
};
use zbus::connection::Builder as ZbusBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // —— 命令行模式：--xml 输出 component XML 后立即退出 ——
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--xml") => {
            // 允许 `--xml [路径] [--exec PATH]`：deb 打包时 exec 需指向
            // 安装后的二进制（/usr/bin/xwx_ime），而非构建期 current_exe。
            let out_path = args.iter().find(|a| !a.starts_with("--") && *a != "--xml");
            let exec = args
                .windows(2)
                .find(|w| w[0] == "--exec")
                .map(|w| w[1].clone())
                .or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .map(|p| p.display().to_string())
                })
                .unwrap_or_default();
            let xml = ibus_proto::component_xml(&exec);
            match out_path.map(String::as_str) {
                Some(path) if path != "-" => {
                    std::fs::write(path, &xml)?;
                    tracing::info!(path, "component XML 已写入");
                }
                _ => print!("{xml}"),
            }
            return Ok(());
        }
        Some("--help") | Some("-h") => {
            println!("XWX 拼音输入法（IBus 引擎）");
            println!();
            println!("用法:");
            println!("  xwx_ime            以 --ibus 模式启动引擎（连接 ibus-daemon）");
            println!("  xwx_ime --ibus     同上，显式指定");
            println!("  xwx_ime --xml [文件] [--exec PATH]  输出 component XML");
            println!("        （默认 stdout；--exec 指定 XML 里的引擎路径,\n         deb 打包时用 /usr/bin/xwx_ime）");
            return Ok(());
        }
        _ => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "XWX 拼音输入法启动");

    // —— 词库 / 语言模型 ——
    // 词库是 dict_to_bin.py 生成的只读 mmap 二进制（dict.bin）：
    // 打开失败（缺失/损坏）时引擎退回内存空词库，不会创建任何文件。
    let dict_path = resolve_data_path("XWX_DICT_PATH", "dict.bin");
    // 简拼副索引:`XWX_ABBREV_PATH` 覆盖,默认与 dict.bin 同目录的
    // `abbrev.bin`。缺失/损坏时 `with_abbrev` 仅告警、引擎退回精确路径,
    // 绝不 panic(见 `MmapDict::with_abbrev`)。
    let abbrev_path = std::env::var("XWX_ABBREV_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dict_path.with_file_name("abbrev.bin"));
    // 语言模型:`XWX_LM_PATH` 显式覆盖(原样尊重,即使文件不存在,后续
    // 逻辑优雅退化为空模型);未设置时按优先级探测数据目录里的
    // .gram / bigram.txt(详见 resolve_lm_path 注释)。
    let lm_choice = resolve_lm_path(std::env::var("XWX_LM_PATH").ok().as_deref());
    let lm_path = lm_choice.path;
    let lm_is_gram = lm_path.is_file() && lm_path.extension().is_some_and(|e| e == "gram");
    let mut engine = if lm_is_gram {
        ImeEngine::new_with_lm(Some(&dict_path), Some(&lm_path))?
    } else {
        ImeEngine::new(Some(&dict_path))?
    };
    engine.dict = engine.dict.with_abbrev(&abbrev_path);
    if engine.dict.has_abbrev() {
        tracing::info!(path = %abbrev_path.display(), "简拼副索引已装配");
    } else {
        tracing::info!(path = %abbrev_path.display(), "简拼副索引未装配(简拼查询退化为空)");
    }
    tracing::info!(path = %dict_path.display(), entries = engine.dict_count(), "词库已就绪");

    // —— 模糊拼音(配置开关)——
    // 此前 `fuzzy_enabled` 在生产进程里从未被置 true:能力实现了、
    // 单测是绿的、用户却用不到(README 却把它列为已完成特性)。
    // 这里接成真正的开关,默认值由 `DEFAULT_FUZZY_ENABLED` 裁决——
    // 默认**关闭**,依据是 `examples/fuzzy_bench.rs` 的实测:
    // 开启后最长全拼串延迟 3.4ms → 22.0ms(6.48x,突破本项目 12ms 上限)、
    // `diannao` 前 10 有一处候选换血,而收益只有 2 组新召回(名次 16~27)。
    // 完整表格见 `DEFAULT_FUZZY_ENABLED` 的文档注释。
    let fuzzy_raw = std::env::var(xwx_engine::FUZZY_ENV).ok();
    engine.fuzzy_enabled = xwx_engine::parse_fuzzy_env(fuzzy_raw.as_deref());
    // 无法识别的取值按「未设置」处理(不报错、不启动失败),但要让用户
    // 知道自己的设置被忽略了——静默吞掉配置是排查噩梦。
    // 「认不认得」的定义复用引擎侧的 `fuzzy_env_value_recognized`,
    // 不在这一层另抄一份取值词表(两份迟早漂移,而漂移的症状正是
    // 「静默吞掉用户配置」,恰好是这里想避免的事)。
    if !xwx_engine::fuzzy_env_value_recognized(fuzzy_raw.as_deref()) {
        tracing::warn!(
            env = xwx_engine::FUZZY_ENV,
            value = fuzzy_raw.as_deref().unwrap_or(""),
            default = xwx_engine::DEFAULT_FUZZY_ENABLED,
            "无法识别的模糊拼音取值,已按默认值处理"
        );
    }
    tracing::info!(
        enabled = engine.fuzzy_enabled,
        pairs = engine.fuzzy_pairs.len(),
        env = xwx_engine::FUZZY_ENV,
        "模糊拼音配置(未设置时取默认值)"
    );

    if lm_is_gram {
        tracing::info!(
            path = %lm_path.display(),
            source = %lm_choice.source.label(),
            "语言模型:万象 .gram(字符级窗口)"
        );
    } else if lm_path.is_file() {
        let file = std::fs::File::open(&lm_path)?;
        engine.lm.load(std::io::BufReader::new(file))?;
        tracing::info!(
            path = %lm_path.display(),
            source = %lm_choice.source.label(),
            vocab = engine.lm.vocab_size(),
            "语言模型已加载(bigram 文本)"
        );
    } else {
        tracing::warn!(
            path = %lm_path.display(),
            source = %lm_choice.source.label(),
            "未找到语言模型,使用空模型"
        );
    }

    // —— 用户学习模型(第 5 步)——
    // 把 UserLM 叠到通用模型之上(MixedLM 用户层)。`with_user_lm` 按值
    // 接收 self 并返回 Self,而上面 bigram 分支的 `engine.lm.load(...)`
    // 需要 `&mut engine`,因此接线必须放在那之后、`Arc::new(engine)` 之前。
    // 路径沿用 resolve_data_path 约定(与 dict.bin 同处),文件名
    // `user_bigram.txt`(与 user_lm.rs 的 open_or_create 读写的 bigram
    // 文本格式匹配,见 USER_LM_FILE_NAME)。
    //
    // **开发期隔离**:`debug_assertions` 打开的构建默认改用
    // `user_bigram.dev.txt`——`just dev` 起的调试引擎不该把开发时打的字
    // 混进真实用户模型(计数/造词/屏蔽表三段都在同一个文件里)。
    // 优先级:环境变量 `XWX_USER_LM_PATH` > debug 默认 > release 默认;
    // 集成测试一律显式设环境变量,因此不受这里影响。`just install` 装的是
    // release 二进制,实机用的仍是真实文件。
    let user_lm_path = resolve_data_path("XWX_USER_LM_PATH", user_lm_file_name());
    let engine = engine.with_user_lm(xwx_engine::UserLM::open_or_create(&user_lm_path));
    tracing::info!(path = %user_lm_path.display(), "用户学习模型已接入");
    let engine = Arc::new(engine);

    // —— 连接总线 ——
    //
    // 注意：session bus（`$DBUS_SESSION_BUS_ADDRESS`）总是可连的，连接成功
    // 不代表 ibus-daemon 在跑。真正的判据是 `org.freedesktop.IBus` 这个
    // bus name 有没有 owner，所以 daemon 自启的检测放在连接之后。
    if ibus_address().is_none() {
        start_ibus_daemon().await;
    }
    let conn = match ibus_address() {
        Some(addr) => ZbusBuilder::address(addr.as_str())?.build().await?,
        None => ZbusBuilder::session()?.build().await?,
    };
    tracing::info!("已连接 IBus 总线");

    // 注册组件（daemon 未运行时不致命，引擎继续等待）
    if let Err(e) = register_component(&conn).await {
        tracing::warn!(error = %e, "注册组件失败（ibus-daemon 未运行？）");
    }

    // 导出 Factory 与首个引擎实例
    //
    // **先挂对象、再占名字**——顺序有语义，不能颠倒。`request_name` 一旦
    // 成功，任何客户端（ibus-daemon、测试）看到这个名字就会立刻发方法调用；
    // 此时若对象还没导出，那个调用就会落进「名字有人、路径没人应答」的窗口，
    // 表现为**调用永远不返回**（集成测试抓到过：`FocusOut` 10s 无应答，而引擎
    // 进程 /proc 状态是 `S`、日志干净——即活着但不分发）。先 `at()` 再
    // `request_name` 就没有这个窗口：名字可见时对象一定已在服务。
    let factory = EngineFactory::new(Arc::clone(&engine), conn.clone());
    conn.object_server().at(FACTORY_PATH, factory).await?;
    let first = XwxEngineServer::new(Arc::clone(&engine));
    conn.object_server().at(ibus_proto::ENGINE_PATH, first).await?;

    // 占用引擎服务名（对象已就绪，从这里起保证「可见即可服务」）
    conn.request_name(ibus_proto::ENGINE_SERVICE).await?;
    tracing::info!(
        service = ibus_proto::ENGINE_SERVICE,
        "服务名已占用（对象服务已先行就绪）"
    );
    tracing::info!(path = ibus_proto::ENGINE_PATH, "引擎已就绪，等待 ibus-daemon 激活");

    // —— 常驻运行 + 用户模型落盘调度 ——
    //
    // `learn_committed` 只改内存计数并累加 dirty(见 lib.rs 文档:落盘由
    // 本进程入口调度)。这里做两件小事,不引入 actor/通道:
    //   1. 定时检查:到批量阈值(needs_flush)就 flush 一次;
    //   2. Ctrl+C 退出时兜底 flush 一次,把不足阈值的尾巴也落盘。
    // flush 失败只告警不吞掉(用户词频是统计量,丢一批可接受,但绝不静默)。
    let mut flush_tick = tokio::time::interval(std::time::Duration::from_secs(30));
    flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = flush_tick.tick() => {
                if let Some(user) = engine.user_lm() {
                    if user.needs_flush() {
                        if let Err(e) = user.flush() {
                            tracing::warn!(error = %e, "用户语言模型定时回写失败");
                        }
                    }
                }
            }
            sig = tokio::signal::ctrl_c() => {
                if let Err(e) = sig {
                    tracing::warn!(error = %e, "监听 Ctrl+C 失败");
                }
                if let Some(user) = engine.user_lm() {
                    if let Err(e) = user.flush() {
                        tracing::warn!(error = %e, "用户语言模型退出回写失败");
                    }
                }
                tracing::info!("收到退出信号,已兜底回写用户语言模型");
                break;
            }
        }
    }
    Ok(())
}

/// 拉起 ibus-daemon，并等它写出总线地址文件。
///
/// 只在 [`ibus_address`] 返回 `None`（daemon 没跑过）时调用。这里不检查
/// session bus 是否可连——session bus 常驻，连得上不代表 IBus 在跑。
///
/// 失败一律只告警：daemon 起不来时引擎照常运行，后续
/// `RegisterComponent` 会再报一次。
///
/// 注意：若本机正在跑 fcitx5，它会伪装占用 `org.freedesktop.IBus`
/// （GNOME Wayland 上只认 IBus 协议），但不实现 `RegisterComponent`。
/// 那种情况下要先 `fcitx5 -k` 停掉它。
async fn start_ibus_daemon() {
    tracing::warn!("未找到 IBus 总线地址，尝试启动 ibus-daemon");
    // -r 抢占已有实例，-d 后台化，-x 启动 XIM（X11/XWayland 应用需要）
    //
    // `-d` 下父进程 fork 后立刻退出，所以这里 wait 不会阻塞；必须收退出码，
    // 否则 daemon 启动失败（如 org.freedesktop.IBus 被 fcitx5 占用）会被
    // 静默吞掉，只能干等下面的轮询超时。
    match std::process::Command::new("ibus-daemon")
        .args(["-rdx"])
        .status()
    {
        Ok(st) if st.success() => {}
        Ok(st) => {
            tracing::warn!(
                status = %st,
                "ibus-daemon 启动失败——org.freedesktop.IBus 可能被 fcitx5 占用，\
                 需先执行 `fcitx5 -k` 并移除其 autostart"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "无法执行 ibus-daemon（未安装？）");
            return;
        }
    }
    // 轮询等待地址文件出现：10 × 500ms
    for _ in 0..10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        if ibus_address().is_some() {
            tracing::info!("ibus-daemon 已就绪");
            return;
        }
    }
    tracing::warn!("等待 ibus-daemon 就绪超时，继续启动（组件注册可能失败）");
}

/// 把 xwx 组件注册进 ibus-daemon。
///
/// 组件包在 variant 里（`RegisterComponent(v)`），daemon 用
/// `ibus_serializable_deserialize` 解析并登记到组件列表。
async fn register_component(conn: &zbus::Connection) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = zbus::Proxy::new(conn, IBUS_SERVICE, IBUS_PATH, IBUS_INTERFACE).await?;
    proxy
        .call_method("RegisterComponent", &(component_value()?,))
        .await?;
    tracing::info!("组件已注册到 ibus-daemon");
    Ok(())
}

/// 定位 IBus 总线地址：`IBUS_ADDRESS` 环境变量优先，否则问 `ibus address`。
///
/// 不自己去翻地址文件：文件位置随 IBus 版本变（`~/.config/ibus/bus/`、
/// `$XDG_RUNTIME_DIR/ibus/bus/`），而且 fcitx5 会往同一个目录写一份指向
/// session bus 的假地址（内容带 `fcitx_random_string`）——挑文件必然踩坑。
/// `ibus address` 是官方查询入口，这些差异它自己处理。
fn ibus_address() -> Option<String> {
    if let Ok(addr) = std::env::var("IBUS_ADDRESS") {
        return Some(addr);
    }
    let out = std::process::Command::new("ibus").arg("address").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let addr = String::from_utf8(out.stdout).ok()?.trim().to_string();
    // daemon 没跑时 `ibus address` 打印 "(null)" 而非报错
    if addr.is_empty() || addr == "(null)" {
        None
    } else {
        Some(addr)
    }
}

/// 解析数据文件路径：环境变量覆盖 > `$XDG_DATA_HOME/xwx/<name>` >
/// `~/.local/share/xwx/<name>`。
/// 用户模型**默认**文件名(不含环境变量覆盖):debug 构建走开发文件。
///
/// 抽成独立函数是为了可测——隐式默认值必须能被一条断言钉住,
/// 否则「debug 不污染真实数据」这件事就只剩注释里的一句话。
fn user_lm_file_name() -> &'static str {
    if cfg!(debug_assertions) {
        xwx_engine::USER_LM_DEV_FILE_NAME
    } else {
        xwx_engine::USER_LM_FILE_NAME
    }
}

fn resolve_data_path(env_key: &str, file_name: &str) -> PathBuf {
    if let Ok(p) = std::env::var(env_key) {
        return PathBuf::from(p);
    }
    let base = xdg_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"));
    base.join("xwx").join(file_name)
}

/// 由 `XDG_DATA_HOME` / `HOME` 算出 XDG 数据根目录。
///
/// 抽成独立函数主要是让测试可以注入路径,避免依赖进程级环境变量
/// (Rust 测试多线程并发,`std::env::set_var` 会互相干扰)。
fn xdg_data_home(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    xdg.map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 语言模型路径来源(用于日志区分显式指定与自动探测)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LmSource {
    /// `XWX_LM_PATH` 显式指定(原样使用,不做存在性检查)
    Explicit,
    /// 未显式指定时,在数据目录里探测到的首选 `.gram`
    ProbedGram,
    /// 未显式指定时,回落到数据目录里的 `bigram.txt`(可能存在也可能不存在)
    FallbackBigramTxt,
}

impl LmSource {
    fn label(self) -> &'static str {
        match self {
            LmSource::Explicit => "显式 XWX_LM_PATH",
            LmSource::ProbedGram => "自动探测(.gram)",
            LmSource::FallbackBigramTxt => "自动回落(bigram.txt)",
        }
    }
}

/// 语言模型路径解析结果。
struct LmChoice {
    path: PathBuf,
    source: LmSource,
}

/// 解析语言模型路径。
///
/// `explicit` 是 `XWX_LM_PATH` 的当前值(`Some` 即用户显式设置):
/// 显式设置时**原样尊重**——即使指向不存在的文件,也由调用方按现有
/// 逻辑优雅退化为空模型,**绝不**偷偷回落到别的文件,否则用户排查
/// 不出自己指定的路径为何没生效。
///
/// 未显式设置时按以下优先级探测 `$XDG_DATA_HOME/xwx/`:
///
/// 1. `wanxiang-lts-zh-hans.gram`(简体万象,最常见);
/// 2. 目录下任意 `.gram` 文件(例如繁体 `wanxiang-lts-zh-hant.gram`);
///    多个时按文件名字典序取第一个,保证行为确定可复现,不依赖
///    目录遍历顺序;
/// 3. `bigram.txt`(文本 bigram,回落到旧行为;不存在也返回此路径,
///    由调用方走"未找到"分支)。
fn resolve_lm_path(explicit: Option<&str>) -> LmChoice {
    resolve_lm_path_in(
        explicit,
        &xdg_data_home(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME")),
    )
}

/// 在指定 XDG 数据根目录下解析语言模型路径(可测试的核心逻辑)。
fn resolve_lm_path_in(explicit: Option<&str>, xdg_data_home: &std::path::Path) -> LmChoice {
    if let Some(p) = explicit {
        return LmChoice {
            path: PathBuf::from(p),
            source: LmSource::Explicit,
        };
    }
    let xwx_dir = xdg_data_home.join("xwx");

    // 1) 首选简体万象
    let preferred = xwx_dir.join("wanxiang-lts-zh-hans.gram");
    if preferred.is_file() {
        return LmChoice {
            path: preferred,
            source: LmSource::ProbedGram,
        };
    }

    // 2) 目录下任意 .gram,按文件名字典序取第一个(确定性)
    if let Some(gram) = first_gram_in(&xwx_dir) {
        return LmChoice {
            path: gram,
            source: LmSource::ProbedGram,
        };
    }

    // 3) 回落 bigram.txt
    LmChoice {
        path: xwx_dir.join("bigram.txt"),
        source: LmSource::FallbackBigramTxt,
    }
}

/// 列出 `dir` 下所有 `.gram` 文件,按文件名字典序返回第一个。
///
/// 读目录失败(不存在/无权限)一律返回 `None`,让上层走 bigram.txt 回落。
fn first_gram_in(dir: &std::path::Path) -> Option<PathBuf> {
    let mut grams: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "gram"))
        .collect();
    grams.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    grams.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 在临时目录里搭一个 `$XDG_DATA_HOME/xwx/`,返回 TempDir 保活。
    fn setup_xdg(files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg");
        let xwx_dir = xdg.join("xwx");
        fs::create_dir_all(&xwx_dir).expect("mkdir xwx");
        for name in files {
            fs::write(xwx_dir.join(name), b"").expect("touch file");
        }
        (tmp, xdg)
    }

    #[test]
    fn explicit_path_returned_verbatim_even_if_missing() {
        let (_tmp, xdg) = setup_xdg(&["wanxiang-lts-zh-hans.gram", "bigram.txt"]);
        let choice = resolve_lm_path_in(Some("/nonexistent/custom.gram"), &xdg);
        assert_eq!(choice.path, PathBuf::from("/nonexistent/custom.gram"));
        assert_eq!(choice.source, LmSource::Explicit);
    }

    #[test]
    fn unset_prefers_wanxiang_hans_gram() {
        let (_tmp, xdg) = setup_xdg(&[
            "wanxiang-lts-zh-hans.gram",
            "wanxiang-lts-zh-hant.gram",
            "bigram.txt",
        ]);
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(
            choice.path,
            xdg.join("xwx").join("wanxiang-lts-zh-hans.gram")
        );
        assert_eq!(choice.source, LmSource::ProbedGram);
    }

    #[test]
    fn debug_builds_default_to_the_dev_user_model_file() {
        // 开发期隔离:debug 构建的**默认**用户模型文件名必须是 `.dev` 那个,
        // release 必须是真实文件名——否则 `just dev` 打的字会混进真实用户
        // 模型(计数/造词/屏蔽表三段同文件,污染后很难清)。
        //
        // 一条测试在同一个 profile 下只能验自己那一半,所以两边都断:
        // 当前 profile 的期望值 + 「两个常量必须不同」(防止有人把它们改成
        // 同一个名字,让整个隔离静默失效)。
        assert_ne!(
            xwx_engine::USER_LM_DEV_FILE_NAME,
            xwx_engine::USER_LM_FILE_NAME,
            "开发文件名与真实文件名不能相同,否则隔离形同虚设"
        );
        let expected = if cfg!(debug_assertions) {
            xwx_engine::USER_LM_DEV_FILE_NAME
        } else {
            xwx_engine::USER_LM_FILE_NAME
        };
        assert_eq!(user_lm_file_name(), expected);
        assert_eq!(expected.ends_with(".dev.txt"), cfg!(debug_assertions));
    }

    #[test]
    fn unset_falls_back_to_any_gram_when_no_hans() {
        let (_tmp, xdg) = setup_xdg(&["wanxiang-lts-zh-hant.gram", "bigram.txt"]);
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(
            choice.path,
            xdg.join("xwx").join("wanxiang-lts-zh-hant.gram")
        );
        assert_eq!(choice.source, LmSource::ProbedGram);
    }

    #[test]
    fn unset_multiple_grams_picks_first_by_name_deterministically() {
        // 多个 .gram 时按文件名字典序取第一个,不依赖目录遍历顺序
        let (_tmp, xdg) = setup_xdg(&[
            "zzz-custom.gram",
            "aaa-custom.gram",
            "mmm-custom.gram",
            "bigram.txt",
        ]);
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(choice.path, xdg.join("xwx").join("aaa-custom.gram"));
        assert_eq!(choice.source, LmSource::ProbedGram);
    }

    #[test]
    fn unset_no_gram_falls_back_to_bigram_txt() {
        let (_tmp, xdg) = setup_xdg(&["bigram.txt", "dict.bin"]);
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(choice.path, xdg.join("xwx").join("bigram.txt"));
        assert_eq!(choice.source, LmSource::FallbackBigramTxt);
    }

    #[test]
    fn unset_empty_dir_still_returns_bigram_txt_path() {
        // 目录是空的(或不存在)时也返回 bigram.txt 路径,
        // 由调用方的 is_file() 检查走"未找到"分支
        let (_tmp, xdg) = setup_xdg(&[]);
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(choice.path, xdg.join("xwx").join("bigram.txt"));
        assert_eq!(choice.source, LmSource::FallbackBigramTxt);
    }

    #[test]
    fn unset_missing_xwx_dir_falls_back_to_bigram_txt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let xdg = tmp.path().join("xdg"); // xdg/xwx 不存在
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(choice.path, xdg.join("xwx").join("bigram.txt"));
        assert_eq!(choice.source, LmSource::FallbackBigramTxt);
    }

    #[test]
    fn gram_files_in_subdirs_are_ignored() {
        // 只扫描顶层,不递归子目录
        let (_tmp, xdg) = setup_xdg(&["bigram.txt"]);
        let sub = xdg.join("xwx").join("subdir");
        fs::create_dir_all(&sub).expect("mkdir sub");
        fs::write(sub.join("nested.gram"), b"").expect("touch nested");
        let choice = resolve_lm_path_in(None, &xdg);
        assert_eq!(choice.path, xdg.join("xwx").join("bigram.txt"));
        assert_eq!(choice.source, LmSource::FallbackBigramTxt);
    }
}
