#![cfg_attr(not(test), windows_subsystem = "windows")]
mod clock;
mod clock_feed;
mod color_picker;
mod data_paths;
mod input_dialog;
mod jump_list;
mod lifecycle;
mod plugin_panels;
mod plugin_services;
mod plugin_system;
mod plugins;
mod settings;
mod shell;
mod task_apps;
mod task_badge;
mod task_click;
mod task_drag;
mod task_layout;
mod task_popup;
mod task_relief;
mod tray;
mod usage_meter;
mod visual;
mod window_commands;

use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug)]
pub struct Options {
    pub plugins: PathBuf,
    pub takeover: bool,
    pub smoke_seconds: u64,
    pub prefer_development: bool,
    pub isolated: bool,
    pub visual_smoke: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--task-window-details") {
        return shell::diagnose_task_windows(
            args.get(2).context("缺少进程 ID")?.parse()?,
            std::path::Path::new(args.get(3).context("缺少诊断路径")?),
        );
    }
    if args.get(1).map(String::as_str) == Some("--tray-sync-smoke") {
        return tray::sync_smoke(std::path::Path::new(args.get(2).context("缺少验证目录")?));
    }
    if args.get(1).map(String::as_str) == Some("--tray-details") {
        return tray::diagnose_details(std::path::Path::new(args.get(2).context("缺少诊断路径")?));
    }
    if args.get(1).map(String::as_str) == Some("--text-render-smoke") {
        return unsafe {
            visual::text::smoke(&PathBuf::from(args.get(2).context("缺少验证目录")?))
        }
        .map_err(Into::into);
    }
    if args.get(1).map(String::as_str) == Some("--usage-gpu-smoke") {
        return unsafe {
            usage_meter::compositor::smoke(&PathBuf::from(args.get(2).context("缺少验证目录")?))
        };
    }
    if args.get(1).map(String::as_str) == Some("--usage-meter-smoke") {
        return unsafe {
            usage_meter::smoke(&PathBuf::from(args.get(2).context("缺少预览目录")?))
        };
    }
    if args.get(1).map(String::as_str) == Some("--plugin-services-smoke") {
        return plugin_services::smoke(&PathBuf::from(args.get(2).context("缺少验证目录")?));
    }
    if args.get(1).map(String::as_str) == Some("--shell-open") {
        return plugin_services::shell_open(
            args.get(2).context("缺少文件路径")?,
            args.get(3).is_some_and(|s| s == "--select-parent"),
        );
    }
    if let Some(index) = args.iter().position(|arg| arg == "--profile-root") {
        data_paths::set(args.get(index + 1).context("缺少配置目录")?.into())?;
    }
    if args.get(1).map(String::as_str) == Some("--request-exit") {
        return lifecycle::request_exit();
    }
    if args.get(1).map(String::as_str) == Some("--task-relief-smoke") {
        return unsafe {
            task_relief::smoke(&PathBuf::from(args.get(2).context("缺少预览目录")?))
        };
    }
    if args.get(1).map(String::as_str) == Some("--jump-list-helper") {
        return jump_list::helper(&PathBuf::from(args.get(2).context("缺少查询文件")?));
    }
    if args.get(1).map(String::as_str) == Some("--jump-list-smoke") {
        return jump_list::smoke(&PathBuf::from(args.get(2).context("缺少验证目录")?));
    }
    if args.get(1).map(String::as_str) == Some("--jump-list-probe") {
        std::fs::write(
            PathBuf::from(args.get(2).context("缺少验证路径")?),
            std::env::current_dir()?.to_string_lossy().as_bytes(),
        )?;
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--jump-list-diagnose") {
        return jump_list::diagnose(
            &PathBuf::from(args.get(2).context("缺少输出目录")?),
            &args[3..],
        );
    }
    if args.get(1).map(String::as_str) == Some("--task-popup-smoke") {
        return task_popup::smoke(&PathBuf::from(args.get(2).context("缺少预览目录")?));
    }
    if args.get(1).map(String::as_str) == Some("--color-picker-smoke") {
        return unsafe {
            color_picker::smoke(&PathBuf::from(args.get(2).context("缺少预览目录")?))
        }
        .map_err(Into::into);
    }
    if args.get(1).map(String::as_str) == Some("--sync-system-pins") {
        let result = task_apps::sync_system_pins()?;
        task_apps::notify_pins_changed();
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--taskbar-smoke") {
        let directory = PathBuf::from(args.get(2).context("缺少预览目录")?);
        anyhow::ensure!(directory.is_absolute(), "预览目录必须是绝对路径");
        std::fs::create_dir_all(directory.join("plugins"))?;
        data_paths::set(directory.join("profile"))?;
        return shell::run(Options {
            plugins: directory.join("plugins"),
            takeover: false,
            smoke_seconds: 0,
            prefer_development: false,
            isolated: true,
            visual_smoke: Some(directory),
        });
    }
    if args.get(1).is_some_and(|arg| arg == "--plugin-hub-smoke") {
        return plugin_system::smoke_hub(args.get(2).context("缺少预览目录")?.into());
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--plugin-repo-diagnose")
    {
        let input = args.get(2).context("缺少仓库 URL")?;
        let root = args.get(3).context("缺少诊断缓存目录")?;
        return plugin_system::diagnose_repository(input, root.into());
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--register-plugin-association")
    {
        return plugin_system::association::register_scoped(
            args.iter().any(|arg| arg == "--installer-test"),
        );
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--unregister-plugin-association")
    {
        return plugin_system::association::unregister_scoped(
            args.iter().any(|arg| arg == "--installer-test"),
        );
    }
    if args.get(1).is_some_and(|arg| {
        arg == "--install"
            || std::path::Path::new(arg)
                .extension()
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case(plugin_system::model::EXTENSION)
                })
    }) {
        let positional = args[1] != "--install";
        let file = args
            .get(if positional { 1 } else { 2 })
            .context("缺少插件包路径")?;
        let force = positional || args.iter().any(|arg| arg == "--force");
        let result = plugin_system::install_file(std::path::Path::new(file), force, None);
        if !args.iter().any(|arg| arg == "--quiet") {
            let (text, flags) = match &result {
                Ok(installed) => (installed.summary(), 0x40),
                Err(error) => (format!("插件安装失败：{error:#}"), 0x10),
            };
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    std::ptr::null_mut(),
                    visual::wide(&text).as_ptr(),
                    visual::wide("Ultra Bar 插件安装").as_ptr(),
                    flags,
                );
            }
        }
        return result.map(|_| ());
    }
    if args.get(1).map(String::as_str) == Some("--restore-watch") {
        return shell::restore_watch(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some("--tray-diagnose") {
        return tray::diagnose();
    }
    let mut options = Options {
        plugins: if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ultra-bar-plugins")
        } else {
            std::env::current_exe()?
                .parent()
                .context("缺少程序目录")?
                .join("plugins")
        },
        takeover: true,
        smoke_seconds: 0,
        prefer_development: false,
        isolated: false,
        visual_smoke: None,
    };
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugins" => {
                options.plugins = args.next().context("--plugins needs a directory")?.into()
            }
            "--takeover" => options.takeover = true,
            "--preview" => options.takeover = false,
            "--installer-test" => {
                options.isolated = true;
                options.takeover = false;
            }
            "--test-modal" => {}
            "--profile-root" => {
                args.next().context("缺少配置目录")?;
            }
            "--prefer-dev-plugins" => options.prefer_development = true,
            "--smoke-seconds" => {
                options.smoke_seconds = args.next().context("missing seconds")?.parse()?
            }
            "--help" | "-h" => {
                println!(
                    "ultra-bar [--plugins DIRECTORY] [--preview] [--smoke-seconds N]\nDefault: take over the native taskbar; --preview leaves it visible."
                );
                return Ok(());
            }
            _ => anyhow::bail!("Unknown argument: {arg}"),
        }
    }
    anyhow::ensure!(
        !options.isolated || (data_paths::is_isolated() && !options.takeover),
        "隔离模式需要 --profile-root"
    );
    shell::run(options)
}
