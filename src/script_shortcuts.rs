//! Explicitly pinned script launchers. Dropping files never executes them.
use crate::task_apps::{Shortcut, normalize};
use anyhow::{Context, Result, ensure};
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
#[link(name = "kernel32")]
unsafe extern "system" {
    fn AllocConsole() -> i32;
}
pub const GROUP: &str = "ultra-bar:scripts";
pub fn is_script(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ps1" | "cmd" | "bat" | "py" | "pyw" | "vbs" | "vbe" | "js" | "jse" | "wsf"
            )
        })
}
pub fn group_pin() -> Shortcut {
    Shortcut {
        launch: GROUP.into(),
        executable: GROUP.into(),
        name: "脚本".into(),
    }
}
fn path() -> Result<PathBuf> {
    Ok(crate::data_paths::root()?.join("taskbar-scripts.json"))
}
fn read() -> Result<Vec<Shortcut>> {
    let path = path()?;
    if !path.exists() {
        return Ok(vec![]);
    }
    ensure!(
        std::fs::metadata(&path)?.len() <= 1024 * 1024,
        "脚本列表过大"
    );
    let entries: Vec<Shortcut> = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(entries.len() <= 1024, "脚本列表过大");
    Ok(entries
        .into_iter()
        .filter(|p| is_script(&p.executable) && Path::new(&p.launch).is_absolute())
        .collect())
}
pub fn load() -> Vec<Shortcut> {
    read().unwrap_or_else(|e| {
        eprintln!("[scripts] {e:#}");
        vec![]
    })
}
fn save(entries: &[Shortcut]) -> Result<()> {
    let path = path()?;
    std::fs::create_dir_all(path.parent().context("脚本目录无效")?)?;
    let pending = path.with_extension("json.new");
    std::fs::write(&pending, serde_json::to_vec_pretty(entries)?)?;
    std::fs::rename(pending, path)?;
    ultra_bar_native_ui::diagnostics::event(
        "tasks",
        "scripts.saved",
        serde_json::json!({"count":entries.len(),"entries":entries}),
    );
    Ok(())
}
pub fn add(mut pin: Shortcut) -> Result<()> {
    ensure!(is_script(&pin.executable), "不支持的脚本类型");
    if !Path::new(&pin.launch)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        pin.name = Path::new(&pin.executable)
            .file_name()
            .context("脚本文件名无效")?
            .to_string_lossy()
            .into_owned();
    }
    let pin = crate::task_apps::retain_link(pin)?;
    let mut entries = read()?;
    if !entries
        .iter()
        .any(|p| normalize(&p.launch) == normalize(&pin.launch))
    {
        ensure!(entries.len() < 1024, "脚本列表已满");
        entries.push(pin);
        save(&entries)?;
    }
    Ok(())
}
pub fn icon_path() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_default())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}
pub fn launch(pin: &Shortcut) -> Result<()> {
    ensure!(
        Path::new(&pin.executable).is_file(),
        "脚本不存在：{}",
        pin.executable
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--run-pinned-script", &pin.launch])
        .env("ULTRA_BAR_SCRIPT_DATA_ROOT", crate::data_paths::root()?);
    if let Some(root) = ultra_bar_native_ui::diagnostics::root() {
        command.env("ULTRA_BAR_LOG_DIR", root);
    }
    if let Some(session) = ultra_bar_native_ui::diagnostics::session() {
        command.env("ULTRA_BAR_PARENT_SESSION", session);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .context("无法启动脚本运行器")?;
    Ok(())
}
fn find_program(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
}
fn command(pin: &Shortcut, arguments: &str) -> Result<Command> {
    let ext = Path::new(&pin.executable)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let system =
        PathBuf::from(std::env::var_os("WINDIR").context("WINDIR 不可用")?).join("System32");
    let mut command = match ext.as_str() {
        "ps1" => {
            let program = find_program("pwsh.exe").unwrap_or_else(icon_path);
            let mut c = Command::new(program);
            c.args(["-NoLogo", "-NoProfile", "-File", &pin.executable]);
            c
        }
        "cmd" | "bat" => {
            let mut c = Command::new(system.join("cmd.exe"));
            c.args(["/d", "/v:off", "/s", "/c"])
                .env("ULTRA_BAR_SCRIPT_TARGET", &pin.executable);
            // Expand the path once via an environment variable, preserving literal
            // percent signs, ampersands and spaces in the script filename.
            c.raw_arg(format!("\"\"%ULTRA_BAR_SCRIPT_TARGET%\" {arguments}\""));
            return Ok(c);
        }
        "py" | "pyw" => {
            let program = find_program("py.exe")
                .or_else(|| find_program("python.exe"))
                .context("未找到 Python 运行环境")?;
            let mut c = Command::new(program);
            c.arg(&pin.executable);
            c
        }
        "js" if find_program("node.exe").is_some() => {
            let mut c = Command::new(find_program("node.exe").unwrap());
            c.arg(&pin.executable);
            c
        }
        "vbs" | "vbe" | "js" | "jse" | "wsf" => {
            let mut c = Command::new(system.join("cscript.exe"));
            c.args(["//nologo", &pin.executable]);
            c
        }
        _ => anyhow::bail!("不支持的脚本类型"),
    };
    if !arguments.is_empty() {
        command.raw_arg(arguments);
    }
    Ok(command)
}
pub fn run(launch: &str) -> Result<()> {
    let pin = load()
        .into_iter()
        .find(|p| normalize(&p.launch) == normalize(launch))
        .context("脚本入口已失效")?;
    ensure!(
        Path::new(&pin.executable).is_file(),
        "脚本不存在：{}",
        pin.executable
    );
    let (arguments, directory) = if Path::new(&pin.launch)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        crate::task_apps::link_run_details(Path::new(&pin.launch))?
    } else {
        (String::new(), String::new())
    };
    let mut command = command(&pin, &arguments)?;
    if directory.is_empty() {
        command.current_dir(
            Path::new(&pin.executable)
                .parent()
                .context("脚本目录无效")?,
        );
    } else {
        command.current_dir(directory);
    }
    ultra_bar_native_ui::diagnostics::event(
        "tasks",
        "script.run_requested",
        serde_json::json!({"name":pin.name,"target":pin.executable}),
    );
    // The launcher owns its console, not the taskbar's redirected log handles.
    ensure!(unsafe { AllocConsole() } != 0, "无法创建脚本控制台");
    let input = std::fs::OpenOptions::new().read(true).open("CONIN$")?;
    let output = std::fs::OpenOptions::new().write(true).open("CONOUT$")?;
    command
        .stdin(input)
        .stderr(output.try_clone()?)
        .stdout(output)
        .spawn()
        .context("无法运行脚本")?;
    Ok(())
}
pub fn execution_smoke(directory: &Path) -> Result<()> {
    ensure!(directory.is_absolute(), "验证目录必须为绝对路径");
    let directory = directory.join("脚本 & %TEMP% 测试");
    std::fs::create_dir_all(&directory)?;
    for ext in ["cmd", "bat", "ps1"] {
        let script = directory.join(format!("entry & %TEMP%.{ext}"));
        let output = directory.join(format!("result-{ext}.txt"));
        let body = if ext == "ps1" {
            format!(
                "Set-Content -LiteralPath (Join-Path $PSScriptRoot 'result-{ext}.txt') -Value 'ok'"
            )
        } else {
            format!("@echo off\r\n> \"%~dp0result-{ext}.txt\" echo ok\r\n")
        };
        std::fs::write(&script, body)?;
        let pin = Shortcut {
            name: "fixture".into(),
            executable: script.to_string_lossy().into_owned(),
            launch: script.to_string_lossy().into_owned(),
        };
        let status = command(&pin, "")?
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        ensure!(
            status.success() && std::fs::read_to_string(output)?.trim() == "ok",
            "{ext} 脚本启动验证失败"
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_supported_interpreted_files_are_grouped() {
        for p in [
            "C:/work/a.PS1",
            "C:/work/a.cmd",
            "C:/work/a.bat",
            "C:/work/a.py",
            "C:/work/a.vbs",
        ] {
            assert!(is_script(p));
        }
        for p in [
            "C:/work/a.exe",
            "C:/work/a.lnk",
            "C:/work/a.rs",
            "C:/work/a.txt",
        ] {
            assert!(!is_script(p));
        }
    }
    #[test]
    fn powershell_uses_file_arguments_and_batch_uses_a_single_path_expansion() {
        let pin = |path: &str| Shortcut {
            name: "test".into(),
            executable: path.into(),
            launch: path.into(),
        };
        let ps = command(&pin("C:/work/a & b.ps1"), "").unwrap();
        let args: Vec<_> = ps
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.last().unwrap(), "C:/work/a & b.ps1");
        assert!(args.contains(&"-File".into()));
        let cmd = command(&pin("C:/work/a %TEMP% & b.cmd"), "").unwrap();
        assert!(cmd.get_envs().any(|(k, v)| k == "ULTRA_BAR_SCRIPT_TARGET"
            && v.is_some_and(|s| s == "C:/work/a %TEMP% & b.cmd")));
    }
}
