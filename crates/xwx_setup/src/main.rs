//! XWX 首选项面板：GNOME 系统设置 → 键盘 → XWX → 首选项。
//!
//! 入口由 `ibus-setup-xwx.desktop` 拉起（GNOME 按 `ibus-setup-<引擎名>`
//! 的约定找这个文件名）。界面用 GTK4 + libadwaita 原生控件，与系统设置
//! 视觉一致，主题/深色模式/字体缩放/无障碍全部自动跟随。
//!
//! 保存后调用 `ibus restart` 让引擎重新读配置——引擎只在启动时读
//! `config.toml`，改完不重启不会生效。

use adw::prelude::*;
use gtk::glib;

use xwx_config::{Config, Language};

/// GTK 应用 ID。与引擎的 D-Bus 名字无关，只用于去重同一进程的多个实例。
const APP_ID: &str = "org.xwx.Setup";

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

/// 构建并显示首选项窗口。
fn build_ui(app: &adw::Application) {
    let path = xwx_config::config_path();
    let config = Config::load_or_default(&path);

    let page = adw::PreferencesPage::new();
    page.add(&language_group(&config));

    // 必须挂在 Application 上：`adw::ApplicationWindow` 的构造器会注册
    // 窗口，窗口关掉时应用才知道可以退出。手动 `PreferencesWindow::new()`
    // 不经过这条路径，GTK 认为没有窗口，`present()` 之后立刻退出。
    let window = adw::PreferencesWindow::builder()
        .application(app)
        .title("XWX 首选项")
        .search_enabled(false)
        .build();
    window.add(&page);
    window.present();
}

/// 「输入」分组：默认语言。
fn language_group(config: &Config) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("输入").build();

    let row = adw::ComboRow::new();
    row.set_title("默认语言");
    row.set_subtitle("新会话开始时使用的语言");

    let labels: Vec<&str> = Language::ALL.iter().map(|l| l.label()).collect();
    let model = gtk::StringList::new(&labels);
    row.set_model(Some(&model));
    row.set_selected(config.default_language.index());

    // 选完立刻落盘：没有「应用」按钮，避免用户改了却忘记确认。
    //
    // 用 `Cell` 而不是 `let mut`：GTK 的 `connect_*` 要 `Fn`（可被多次
    // 调用且不能可变捕获），状态只能放外部可变槽位。
    let path = xwx_config::config_path();
    let current = std::cell::Cell::new(config.default_language);
    row.connect_selected_notify(move |row| {
        let Some(lang) = Language::from_index(row.selected()) else {
            return;
        };
        if lang == current.get() {
            return;
        }
        current.set(lang);
        let cfg = Config {
            default_language: lang,
        };
        match cfg.save(&path) {
            Ok(()) => {
                tracing::info!(language = lang.as_str(), "默认语言已保存");
                restart_ibus();
            }
            Err(e) => tracing::error!(error = %e, path = %path.display(), "配置保存失败"),
        }
    });

    group.add(&row);
    group
}

/// 让引擎重新读配置。
///
/// 引擎只在启动时读 `config.toml`，所以必须重启。`ibus restart` 会重启
/// 整个 daemon，期间所有输入法短暂不可用（几十毫秒到一秒）。
/// 失败只记日志：配置已经写进文件，用户重启会话后仍会生效。
fn restart_ibus() {
    match std::process::Command::new("ibus").arg("restart").status() {
        Ok(status) if status.success() => tracing::info!("ibus 已重启，新配置生效"),
        Ok(status) => tracing::warn!(%status, "ibus restart 返回非零，配置需重启会话才生效"),
        Err(e) => tracing::warn!(error = %e, "无法执行 ibus restart，配置需重启会话才生效"),
    }
}
