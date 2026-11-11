use anyhow::{Context, Result, ensure};
use std::{path::PathBuf, sync::OnceLock};

static PROFILE: OnceLock<PathBuf> = OnceLock::new();
pub fn set(path: PathBuf) -> Result<()> {
    ensure!(path.is_absolute(), "配置目录必须是绝对路径");
    PROFILE
        .set(path)
        .map_err(|_| anyhow::anyhow!("配置目录已初始化"))
}
pub fn is_isolated() -> bool {
    PROFILE.get().is_some()
}
pub fn root() -> Result<PathBuf> {
    Ok(match PROFILE.get() {
        Some(path) => path.clone(),
        None => PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA 不可用")?)
            .join("UltraBar"),
    })
}
