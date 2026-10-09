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

mod lm_path;
mod english_config;

use lm_path::{resolve_data_path, resolve_lm_path, user_lm_file_name};

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
            //
            // **必须顺序解析**：`--exec` 吃掉它的**值**，输出路径只能来自独立的
            // 位置参数。旧实现用 `args.iter().find(|a| !a.starts_with("--"))` 找
            // 输出路径，于是 `xwx_ime --xml --exec ~/.local/bin/xwx_ime` 会把
            // `--exec` 的**值**当输出文件、用 XML 截断它（可复现，见
            // `tests/cli.rs` 与 docs/HANDOFF.md 第 ㉕ 条）。
            let mut out_path: Option<&str> = None;
            let mut exec: Option<&str> = None;
            let mut i = 1; // 跳过 `--xml`
            while i < args.len() {
                match args[i].as_str() {
                    "--exec" => {
                        let v = args.get(i + 1).ok_or("--exec 缺少路径参数")?;
                        if exec.replace(v.as_str()).is_some() {
                            return Err("--exec 重复".into());
                        }
                        i += 2;
                    }
                    "-" => {
                        if out_path.replace("-").is_some() {
                            return Err("输出路径重复".into());
                        }
                        i += 1;
                    }
                    a if a.starts_with("--") => {
                        return Err(format!("未知参数 {a}").into());
                    }
                    a => {
                        if out_path.replace(a).is_some() {
                            return Err("输出路径重复".into());
                        }
                        i += 1;
                    }
                }
            }
            let exec = exec.map(str::to_string).or_else(|| {
                std::env::current_exe()
                    .ok()
                    .map(|p| p.display().to_string())
            });
            let xml = ibus_proto::component_xml(exec.as_deref().unwrap_or_default());
            match out_path {
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
            println!("  xwx_ime --xml [文件|-] [--exec PATH]  输出 component XML");
            println!("        （未知参数 / 缺值 / 输出路径重复一律报错并退出）");
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
    // 音节级转移索引:`XWX_SYLLABLE_GRAPH_PATH` 覆盖,默认与 dict.bin 同目录的
    // `syllable_graph.bin`(由 `scripts/build_syllable_graph.py` 生成)。缺失/
    // 损坏/不同代只告警,缩写弧回退到逐次主索引二分(功能不变、每次多一次跨页二分)。
    let graph_path = std::env::var("XWX_SYLLABLE_GRAPH_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dict_path.with_file_name("syllable_graph.bin"));
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
    engine.dict = engine.dict.with_syllable_graph(&graph_path);
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

    // —— 英文通道(可选,2026-10 英文输入)——
    // 三件事按顺序:
    //   1. `XWX_ENGLISH=0` 时**什么都不做**:不打开词库、不打开/创建英文
    //      用户文件(关闭功能后不得留下新的路径访问与写文件副作用);
    //   2. 词库打开失败(缺文件/损坏)**只关闭英文**并告警,中文候选、中文
    //      用户模型与按键行为一律不变——这是本通道唯一的降级方式;
    //   3. 英文用户文件是**独立**文件(默认 `user_english[.dev].txt`),
    //      绝不与中文 `user_bigram.txt` 混用:两者计数混在一起会让
    //      「学英文」改变中文候选。
    let english_cfg = english_config::EnglishConfig::from_env();
    let engine = if !english_cfg.enabled {
        tracing::info!(env = english_config::ENGLISH_ENV, "英文通道已关闭");
        engine
    } else {
        match xwx_engine::EnglishDict::open(&english_cfg.dict_path) {
            Ok(dict) => {
                let en_user =
                    xwx_engine::UserLM::open_or_create(&english_cfg.user_path);
                let english = xwx_engine::EnglishEngine::new(dict)
                    .with_user_lm(en_user)
                    .with_predict(english_cfg.predict);
                tracing::info!(
                    dict = %english_cfg.dict_path.display(),
                    user = %english_cfg.user_path.display(),
                    words = english.dict().word_count(),
                    prefixes = english.dict().prefix_count(),
                    predict = english_cfg.predict,
                    // M2 才消费这份数据;M1 只解析路径并在启动日志里报出来
                    // （路径解析错误在启动期就能被发现,而不是等到开预测）。
                    bigram = %english_cfg.bigram_path.display(),
                    "英文通道已接入"
                );
                engine.with_english(english)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %english_cfg.dict_path.display(),
                    "英文词库打开失败,英文通道降级为关闭(中文不受影响)"
                );
                engine
            }
        }
    };
    let engine = Arc::new(engine);

    // 启动期确定性初始化:词频总量线性扫描 + 纠错变体表。
    // 两笔都是懒初始化,不搬走就由用户的前几个按键/第一次打错来付;这里同步
    // 做完,而且早于下面的总线连接与 `request_name`,所以服务名可见时已经付清。
    let prepare_started = std::time::Instant::now();
    engine.prepare_startup();
    tracing::info!(
        ms = prepare_started.elapsed().as_secs_f64() * 1000.0,
        "启动期初始化完成(词频总量 + 纠错变体表)"
    );

    // 首键预热:进程内第一次走到混合/简拼形状要付 ~26ms(见
    // `ImeEngine::prewarm` 的实测数字),放后台线程先付掉,不挡启动。
    // 只读预热,不学习、不碰用户模型文件。
    {
        let warm = Arc::clone(&engine);
        std::thread::spawn(move || warm.prewarm());
    }

    // —— 连接总线 ——
    //
    // 注意：session bus（`$DBUS_SESSION_BUS_ADDRESS`）总是可连的，连接成功
    // 不代表 ibus-daemon 在跑。真正的判据是 `org.freedesktop.IBus` 这个
    // bus name 有没有 owner，所以 daemon 自启的检测放在连接之后。
    // **自启闸门**:`start_ibus_daemon` 跑的是 `ibus-daemon -rdx`,其中 `-r`
    // 会**替换**总线上正在运行的实例。集成测试会大量启动本二进制,任何一条
    // 没有把 `IBUS_ADDRESS` 指到私有总线的 spawn 都会因此把用户会话的输入法
    // 弄停(2026-10-02 实测事故,见 docs/HANDOFF.md ㊲)。测试用它明确关掉自启;
    // 生产默认行为不变(不设这个变量就是原来的自启)。
    if ibus_address().is_none() && std::env::var_os("XWX_NO_IBUS_AUTOSTART").is_none() {
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
    // **用「可接管」的方式占名**：dev 跑的调试引擎必须能顶掉 daemon 按
    // component XML 拉起的 release 引擎，否则调试引擎会因 `NameTaken`
    // 直接退出、打字仍然落到旧二进制上（实测过的坑：`just dev` 看起来在跑，
    // 实际服务的还是安装版）。AllowReplacement + ReplaceExisting 让后来者
    // 接管；旧持有者会收到 NameLost，不再被 daemon 使用。
    conn.request_name_with_flags(
        ibus_proto::ENGINE_SERVICE,
        zbus::fdo::RequestNameFlags::ReplaceExisting
            | zbus::fdo::RequestNameFlags::AllowReplacement,
    )
    .await?;
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
    // **SIGTERM 也要兜底**:`just dev` 停引擎用的是普通 `kill`(SIGTERM),
    // 旧实现只监听 Ctrl+C,开发期的学习尾巴永远不会落盘。
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = flush_tick.tick() => {
                // **任意非零增量都保存**(30s 一次):[`FLUSH_LEARN_THRESHOLD`]
                // 只是额外触发,不是「不到 64 次就不存」——旧实现那样会让不足
                // 阈值的尾巴在退出时被 `flush` 的 `dirty == 0` 早退跳过。
                let zh_dirty = engine.user_lm().is_some_and(|u| u.dirty_count() > 0);
                let en_dirty = engine
                    .english()
                    .is_some_and(|e| e.user().dirty_count() > 0);
                if zh_dirty || en_dirty {
                    flush_user_lm(&engine).await;
                    tracing::info!(zh = zh_dirty, en = en_dirty, "用户语言模型定时回写完成");
                }
            }
            _ = sigterm.recv() => {
                flush_user_lm(&engine).await;
                tracing::info!("收到 SIGTERM,已兜底回写用户语言模型");
                break;
            }
            sig = tokio::signal::ctrl_c() => {
                if let Err(e) = sig {
                    tracing::warn!(error = %e, "监听 Ctrl+C 失败");
                }
                flush_user_lm(&engine).await;
                tracing::info!("收到退出信号,已兜底回写用户语言模型");
                break;
            }
        }
    }
    Ok(())
}

/// 用户模型落盘放到**阻塞线程**上:序列化 + `sync_data` 是同步 IO,
/// 直接在 tokio 运行时里跑会卡住定时器与信号处理。
///
/// 中英两个模型**分别** flush:一个失败不能阻止另一个(英文写不进去时
/// 中文的增量照样要落盘,反之亦然)。
async fn flush_user_lm(engine: &Arc<ImeEngine>) {
    let engine = Arc::clone(engine);
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(user) = engine.user_lm() {
            if let Err(e) = user.flush() {
                tracing::warn!(error = %e, "中文用户语言模型回写失败");
            }
        }
        if let Some(en) = engine.english() {
            if let Err(e) = en.user().flush() {
                tracing::warn!(error = %e, "英文用户语言模型回写失败");
            }
        }
    })
    .await;
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
