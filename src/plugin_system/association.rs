use crate::visual::wide;
use anyhow::{Result, ensure};
use std::ptr::{null, null_mut};
use windows_sys::Win32::{Foundation::*, System::Registry::*, UI::Shell::*};
const PROGID: &str = "UltraBar.PluginPackage.1";
fn command() -> Result<String> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let exe = exe
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_owned();
    ensure!(!exe.contains('"'), "程序路径无效");
    Ok(format!("\"{exe}\" --install \"%1\" --force"))
}
fn value(path: &str, name: &str, text: &str) -> Result<()> {
    unsafe {
        let mut key = null_mut();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(path).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        ensure!(status == ERROR_SUCCESS, "无法注册文件关联（{status}）");
        let text = wide(text);
        let status = RegSetValueExW(
            key,
            wide(name).as_ptr(),
            0,
            REG_SZ,
            text.as_ptr().cast(),
            (text.len() * 2) as _,
        );
        RegCloseKey(key);
        ensure!(status == ERROR_SUCCESS, "无法写入文件关联（{status}）");
        Ok(())
    }
}
fn read(path: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buffer = [0u16; 4096];
        let mut bytes = (buffer.len() * 2) as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            wide(path).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let end = buffer.iter().position(|c| *c == 0)?;
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}
pub fn register() -> Result<()> {
    register_scoped(false)
}
pub fn register_scoped(test: bool) -> Result<()> {
    let progid = if test {
        "UltraBar.PluginPackage.InstallerTest.1"
    } else {
        PROGID
    };
    let extension = if test {
        ".ubplugin-ultrabar-test"
    } else {
        ".ubplugin"
    };
    let command = command()?;
    value(&format!("Software\\Classes\\{extension}"), "", progid)?;
    value(
        &format!("Software\\Classes\\{extension}"),
        "Content Type",
        "application/vnd.ultra-bar.plugin",
    )?;
    value(
        &format!("Software\\Classes\\{extension}\\OpenWithProgids"),
        progid,
        "",
    )?;
    value(
        &format!("Software\\Classes\\{progid}"),
        "",
        "Ultra Bar 插件包",
    )?;
    value(
        &format!("Software\\Classes\\{progid}\\shell\\open\\command"),
        "",
        &command,
    )?;
    value(
        &format!("Software\\Classes\\{progid}\\DefaultIcon"),
        "",
        &format!(
            "\"{}\",0",
            std::env::current_exe()?
                .to_string_lossy()
                .trim_start_matches("\\\\?\\")
        ),
    )?;
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null());
    }
    Ok(())
}
pub fn unregister() -> Result<()> {
    unregister_scoped(false)
}
pub fn unregister_scoped(test: bool) -> Result<()> {
    let progid = if test {
        "UltraBar.PluginPackage.InstallerTest.1"
    } else {
        PROGID
    };
    let extension = if test {
        ".ubplugin-ultrabar-test"
    } else {
        ".ubplugin"
    };
    unsafe {
        let path = format!("Software\\Classes\\{progid}\\shell\\open\\command");
        ensure!(
            read(&path, "").is_none_or(|current| command().is_ok_and(|ours| current == ours)),
            "文件关联由其他 Ultra Bar 路径管理，未移除"
        );
        if read(&format!("Software\\Classes\\{extension}"), "").as_deref() == Some(progid) {
            let mut key = null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(&format!("Software\\Classes\\{extension}")).as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut key,
            ) == ERROR_SUCCESS
            {
                RegDeleteValueW(key, null());
                if read(&format!("Software\\Classes\\{extension}"), "Content Type").as_deref()
                    == Some("application/vnd.ultra-bar.plugin")
                {
                    RegDeleteValueW(key, wide("Content Type").as_ptr());
                }
                RegCloseKey(key);
            }
        }
        RegDeleteTreeW(
            HKEY_CURRENT_USER,
            wide(&format!("Software\\Classes\\{progid}")).as_ptr(),
        );
        let mut key = null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(&format!("Software\\Classes\\{extension}\\OpenWithProgids")).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        ) == ERROR_SUCCESS
        {
            RegDeleteValueW(key, wide(progid).as_ptr());
            RegCloseKey(key);
        }
        SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null());
        Ok(())
    }
}
