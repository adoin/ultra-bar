use crate::plugins::Widget;
use anyhow::{Context, Result};
use std::{
    os::windows::process::CommandExt,
    process::{Command, Stdio},
};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
pub fn save_settings(
    widget: &Widget,
    owner: HWND,
    revision: u64,
    values: Vec<String>,
) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        anyhow::ensure!(
            widget.id == "com.emssion.schedule-calendar",
            "此插件尚未提供嵌入式设置"
        );
        let window = FindWindowW(
            crate::visual::wide(&format!("UltraBar.ScheduleCalendar.{}", std::process::id()))
                .as_ptr(),
            std::ptr::null(),
        );
        anyhow::ensure!(!window.is_null(), "插件面板尚未启动");
        let data = serde_json::to_vec(&ultra_bar_native_ui::settings_bridge::SaveRequest {
            revision,
            values,
        })?;
        anyhow::ensure!(data.len() <= 65536, "设置内容过长");
        let packet = COPYDATASTRUCT {
            dwData: ultra_bar_native_ui::settings_bridge::COPYDATA_SETTINGS,
            cbData: data.len() as _,
            lpData: data.as_ptr() as _,
        };
        let mut result = 0;
        anyhow::ensure!(
            SendMessageTimeoutW(
                window,
                WM_COPYDATA,
                owner as usize,
                (&packet as *const COPYDATASTRUCT) as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                500,
                &mut result
            ) != 0
                && result == 1,
            "插件设置请求未被接受"
        );
    }
    Ok(())
}
pub fn configure(widget: &Widget, _owner: HWND) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let class =
            crate::visual::wide(&format!("UltraBar.ScheduleCalendar.{}", std::process::id()));
        anyhow::ensure!(
            widget.id == "com.emssion.schedule-calendar",
            "此插件未提供设置入口"
        );
        let window = FindWindowW(class.as_ptr(), std::ptr::null());
        anyhow::ensure!(!window.is_null(), "插件面板尚未启动");
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        AllowSetForegroundWindow(pid);
        PostMessageW(
            window,
            RegisterWindowMessageW(crate::visual::wide("UltraBar.Plugin.Settings.v1").as_ptr()),
            0,
            0,
        );
    }
    Ok(())
}

pub fn launch(widget: &Widget, owner: HWND, show: bool) -> Result<()> {
    let executable = widget
        .panel
        .as_ref()
        .context("插件面板尚未构建，请运行 dev.ps1")?;
    let mut command = Command::new(executable);
    ultra_bar_native_ui::fonts::inherit(&mut command);
    let mut child = command
        .env(
            "ULTRA_BAR_ACCENT",
            crate::visual::theme::hex(crate::visual::theme::accent_rgb()),
        )
        .env(
            "ULTRA_BAR_LOG_DIR",
            ultra_bar_native_ui::diagnostics::root().unwrap_or(std::path::Path::new("logs")),
        )
        .env(
            "ULTRA_BAR_PARENT_SESSION",
            ultra_bar_native_ui::diagnostics::session().unwrap_or(""),
        )
        .env(
            "ULTRA_BAR_CLOCK_STATE",
            crate::data_paths::root()?.join("plugins").join(&widget.id),
        )
        .arg("--plugin-root")
        .arg(&widget.root)
        .arg("--owner")
        .arg((owner as usize).to_string())
        .arg("--parent")
        .arg(std::process::id().to_string())
        .arg(if show { "--show" } else { "--background" })
        .current_dir(&widget.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
        .context("无法启动插件面板")?;
    let id = widget.id.clone();
    let pid = child.id();
    ultra_bar_native_ui::diagnostics::event(
        "plugins",
        "panel.start",
        serde_json::json!({"id":id,"pid":pid,"binary":executable,"show":show}),
    );
    std::thread::spawn(move || {
        let status = child.wait();
        ultra_bar_native_ui::diagnostics::event(
            "plugins",
            "panel.exit",
            serde_json::json!({"id":id,"pid":pid,"exit_code":status.as_ref().ok().and_then(|s|s.code()),"error":status.err().map(|e|e.to_string())}),
        );
    });
    Ok(())
}
