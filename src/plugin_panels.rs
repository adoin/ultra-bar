use crate::plugins::Widget;
use anyhow::{Context, Result};
use std::{
    os::windows::process::CommandExt,
    process::{Command, Stdio},
};
use windows_sys::Win32::Foundation::HWND;

pub fn launch(widget: &Widget, owner: HWND, show: bool) -> Result<()> {
    let executable = widget
        .panel
        .as_ref()
        .context("插件面板尚未构建，请运行 dev.ps1")?;
    Command::new(executable)
        .env(
            "ULTRA_BAR_ACCENT",
            crate::visual::theme::hex(crate::visual::theme::accent_rgb()),
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
    Ok(())
}
