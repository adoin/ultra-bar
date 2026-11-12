//! Current-user logon startup. Uninstall removes only this installation's entry.
use anyhow::{Result, ensure};
use std::{
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{Foundation::*, System::Registry::*};
const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
fn name(test: bool) -> &'static str {
    if test {
        "Ultra Bar Installer Test"
    } else {
        "Ultra Bar"
    }
}
fn command() -> Result<String> {
    command_for(&std::env::current_exe()?.canonicalize()?)
}
fn command_for(path: &Path) -> Result<String> {
    let path = path.to_string_lossy();
    let path = path.trim_start_matches("\\\\?\\");
    ensure!(!path.contains(['"', '\r', '\n']), "启动路径无效");
    let command = format!("\"{path}\" --from-startup");
    ensure!(
        command.encode_utf16().count() < 260,
        "开机启动路径过长，请使用较短的安装目录"
    );
    Ok(command)
}
fn read(path: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buffer = vec![0u16; 4096];
        let mut bytes = (buffer.len() * 2) as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            crate::visual::wide(path).as_ptr(),
            crate::visual::wide(name).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let n = buffer.iter().position(|c| *c == 0)?;
        Some(String::from_utf16_lossy(&buffer[..n]))
    }
}
fn write(path: &str, name: &str, value: Option<&str>) -> Result<()> {
    unsafe {
        let mut key = null_mut();
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            crate::visual::wide(path).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        ensure!(code == ERROR_SUCCESS, "无法设置开机启动（{code}）");
        let result = if let Some(text) = value {
            let text = crate::visual::wide(text);
            RegSetValueExW(
                key,
                crate::visual::wide(name).as_ptr(),
                0,
                REG_SZ,
                text.as_ptr().cast(),
                (text.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, crate::visual::wide(name).as_ptr())
        };
        RegCloseKey(key);
        ensure!(
            result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND,
            "无法更新开机启动（{result}）"
        );
        Ok(())
    }
}
pub fn set(enabled: bool, test: bool) -> Result<()> {
    let command = command()?;
    if enabled {
        if read(RUN, name(test)).as_deref() != Some(&command) {
            write(RUN, name(test), Some(&command))?;
        }
    } else {
        write(RUN, name(test), None)?;
    }
    ultra_bar_native_ui::diagnostics::event(
        "lifecycle",
        "startup.configured",
        serde_json::json!({"enabled":enabled,"isolated":test}),
    );
    Ok(())
}
fn remove_owned(path: &str, name: &str, expected: &str) -> Result<()> {
    if read(path, name).is_some_and(|value| value.eq_ignore_ascii_case(expected)) {
        write(path, name, None)?;
    }
    Ok(())
}
pub fn unregister(test: bool) -> Result<()> {
    remove_owned(RUN, name(test), &command()?)
}
pub fn sync(test: bool) -> Result<()> {
    set(crate::settings::Settings::load().startup_enabled, test)
}
pub fn smoke(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    let path = format!("Software\\UltraBar\\Tests\\Startup-{}", std::process::id());
    let result = (|| -> Result<()> {
        let cmd = command()?;
        write(&path, "test", Some(&cmd))?;
        ensure!(
            read(&path, "test").as_deref() == Some(&cmd),
            "启动项写入失败"
        );
        remove_owned(&path, "test", "different owner")?;
        ensure!(read(&path, "test").is_some(), "错误删除了其他安装的启动项");
        remove_owned(&path, "test", &cmd)?;
        ensure!(read(&path, "test").is_none(), "启动项未清理");
        std::fs::write(
            directory.join("startup-result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"write":true,"uninstall_ownership":true,"remove":true,"real_run_key_untouched":true}),
            )?,
        )?;
        Ok(())
    })();
    unsafe {
        RegDeleteTreeW(HKEY_CURRENT_USER, crate::visual::wide(&path).as_ptr());
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_quotes_the_program_without_launching_a_build_runner() {
        assert_eq!(
            command_for(Path::new("D:\\Program Files\\Ultra Bar\\ultra-bar.exe")).unwrap(),
            "\"D:\\Program Files\\Ultra Bar\\ultra-bar.exe\" --from-startup"
        );
        assert!(command_for(Path::new(&format!("D:/{}", "a".repeat(300)))).is_err());
        assert_ne!(name(true), name(false));
    }
}
