//! Bounded in-memory transport to the host editor provider. No values are logged.
pub use ultra_bar_plugin_sdk::editors::*;
use windows_sys::Win32::{
    Foundation::HWND, System::DataExchange::COPYDATASTRUCT, UI::WindowsAndMessaging::*,
};
/// # Safety
/// Caller is a live plugin window; host is its owning Ultra Bar window.
pub unsafe fn request(
    host: HWND,
    caller: HWND,
    plugin_id: &str,
    operation: Operation,
) -> Result<(), String> {
    unsafe {
        if host.is_null() || IsWindow(host) == 0 {
            return Err("编辑器需要由 Ultra Bar 提供，请先启动或更新主程序".into());
        }
        let bytes = serde_json::to_vec(&Request {
            protocol: PROTOCOL,
            plugin_id: plugin_id.into(),
            operation,
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("编辑器内容过长".into());
        }
        let packet = COPYDATASTRUCT {
            dwData: REQUEST,
            cbData: bytes.len() as u32,
            lpData: bytes.as_ptr() as _,
        };
        let mut accepted = 0;
        if SendMessageTimeoutW(
            host,
            WM_COPYDATA,
            caller as usize,
            (&packet as *const COPYDATASTRUCT) as isize,
            SMTO_ABORTIFHUNG,
            1000,
            &mut accepted,
        ) == 0
            || accepted != 1
        {
            return Err("主程序未接受编辑器请求，请检查主程序版本".into());
        }
        Ok(())
    }
}
