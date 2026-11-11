//! Read Windows 11's per-icon preferences when available. Windows 10 imports
//! its live toolbar split instead of guessing undocumented IconStreams offsets.
use super::store::{Preference, identity};
use crate::visual::wide;
use anyhow::{Result, bail};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{Foundation::*, System::Registry::*};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn string(key: HKEY, name: &str) -> Option<String> {
    unsafe {
        let mut buffer = [0u16; 32768];
        let mut bytes = (buffer.len() * 2) as u32;
        if RegGetValueW(
            key,
            null(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let length = buffer.iter().position(|ch| *ch == 0)?;
        Some(String::from_utf16_lossy(&buffer[..length]))
    }
}
fn dword(key: HKEY, name: &str) -> Option<u32> {
    unsafe {
        let mut value = 0u32;
        let mut bytes = size_of::<u32>() as u32;
        if RegGetValueW(
            key,
            null(),
            wide(name).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut value as *mut u32).cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        Some(value)
    }
}

pub fn read() -> Result<Vec<Preference>> {
    unsafe {
        let mut root = null_mut();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide("Control Panel\\NotifyIconSettings").as_ptr(),
            0,
            KEY_READ,
            &mut root,
        );
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(vec![]);
        }
        if result != ERROR_SUCCESS {
            bail!("无法读取 Windows 托盘设置（错误 {result}）");
        }
        let root = Key(root);
        let mut output = vec![];
        for index in 0..4096 {
            let mut name = [0u16; 256];
            let mut length = name.len() as u32;
            let result = RegEnumKeyExW(
                root.0,
                index,
                name.as_mut_ptr(),
                &mut length,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            if result == ERROR_NO_MORE_ITEMS {
                break;
            }
            if result != ERROR_SUCCESS {
                bail!("Windows 托盘设置枚举失败");
            }
            let mut child = null_mut();
            if RegOpenKeyExW(root.0, name.as_ptr(), 0, KEY_READ, &mut child) != ERROR_SUCCESS {
                continue;
            }
            let child = Key(child);
            let (Some(executable), Some(uid), Some(promoted)) = (
                string(child.0, "ExecutablePath"),
                dword(child.0, "UID"),
                dword(child.0, "IsPromoted"),
            ) else {
                continue;
            };
            let title = string(child.0, "InitialTooltip").unwrap_or_else(|| {
                std::path::Path::new(&executable)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            output.push(Preference {
                id: identity(&executable, uid),
                executable,
                uid,
                title,
                collapsed: promoted == 0,
            });
        }
        Ok(output)
    }
}
