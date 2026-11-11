//! Bounded, path-scoped shutdown for installation. Never terminates processes.
use crate::visual::wide;
use anyhow::{Result, ensure};
use std::{
    mem::{size_of, zeroed},
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Diagnostics::ToolHelp::*, Threading::*},
    UI::{Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};

pub fn exit_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE
        .get_or_init(|| unsafe { RegisterWindowMessageW(wide("UltraBar.RequestExit.v1").as_ptr()) })
}
fn own_path() -> Result<String> {
    Ok(std::env::current_exe()?
        .to_string_lossy()
        .trim_start_matches("\\\\?\\")
        .to_lowercase())
}
unsafe fn matches(process: HANDLE, target: &str) -> bool {
    unsafe {
        let mut path = [0u16; 32768];
        let mut len = path.len() as u32;
        QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) != 0
            && String::from_utf16_lossy(&path[..len as usize])
                .trim_start_matches("\\\\?\\")
                .to_lowercase()
                == target
    }
}
struct Target {
    pid: u32,
    handle: HANDLE,
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
unsafe extern "system" fn exit_window(hwnd: HWND, param: LPARAM) -> i32 {
    unsafe {
        let target = &*(param as *const Target);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let mut class = [0u16; 128];
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        if pid == target.pid
            && String::from_utf16_lossy(&class[..len.max(0) as usize]) == "UltraBar.Window"
        {
            let mut response = 0;
            let delivered = SendMessageTimeoutW(
                hwnd,
                exit_message(),
                0,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                3000,
                &mut response,
            );
            eprintln!(
                "[installer-exit] pid={} window={:#x} delivered={} response={}",
                target.pid, hwnd as usize, delivered, response
            );
            // The main function waits for actual process handles, including recovery.
        }
        1
    }
}
pub fn request_exit() -> Result<()> {
    let target_path = own_path()?;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        ensure!(snapshot != INVALID_HANDLE_VALUE, "无法枚举进程");
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut targets = vec![];
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            if entry.th32ProcessID != std::process::id() {
                let process = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    entry.th32ProcessID,
                );
                if !process.is_null() {
                    if matches(process, &target_path) {
                        targets.push(Target {
                            pid: entry.th32ProcessID,
                            handle: process,
                        });
                    } else {
                        CloseHandle(process);
                    }
                }
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
        eprintln!(
            "[installer-exit] matched {} process(es) at {}",
            targets.len(),
            target_path
        );
        for target in &targets {
            EnumWindows(Some(exit_window), (target as *const Target) as isize);
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while targets
            .iter()
            .any(|target| WaitForSingleObject(target.handle, 0) != WAIT_OBJECT_0)
        {
            ensure!(
                Instant::now() < deadline,
                "请先保存并关闭日程编辑器、设置对话框或插件安装，然后重试。"
            );
            std::thread::sleep(Duration::from_millis(80));
        }
        Ok(())
    }
}
unsafe extern "system" fn modal_owned(hwnd: HWND, param: LPARAM) -> i32 {
    unsafe {
        let state = &mut *(param as *mut (HWND, bool));
        let owner = GetWindow(hwnd, GW_OWNER);
        // Tooltips are disabled owned windows too. A modal dialog is an enabled,
        // visible window whose owner is disabled; do not mistake helper windows
        // for an editor with unsaved content.
        if !owner.is_null()
            && GetAncestor(owner, GA_ROOTOWNER) == state.0
            && IsWindowVisible(hwnd) != 0
            && IsWindowEnabled(hwnd) != 0
            && IsWindowEnabled(owner) == 0
        {
            state.1 = true;
        }
        1
    }
}
pub unsafe fn modal_open(owner: HWND) -> bool {
    unsafe {
        let mut state = (owner, IsWindowEnabled(owner) == 0);
        EnumWindows(
            Some(modal_owned),
            (&mut state as *mut (HWND, bool)) as isize,
        );
        state.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        System::LibraryLoader::GetModuleHandleW, UI::Input::KeyboardAndMouse::EnableWindow,
    };
    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn disabled_helpers_do_not_block_exit_but_owned_modal_dialogs_do() {
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = wide("UltraBar.LifecycleTest");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            let create = |owner, style| {
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class.as_ptr(),
                    wide("Lifecycle test").as_ptr(),
                    WS_POPUP | style,
                    -32000,
                    -32000,
                    20,
                    20,
                    owner,
                    null_mut(),
                    instance,
                    null(),
                );
                assert!(!hwnd.is_null());
                Window(hwnd)
            };
            let root = create(null_mut(), 0);
            let helper = create(root.0, WS_DISABLED);
            assert!(
                !modal_open(root.0),
                "disabled helper/tooltip must not look like an editor"
            );
            let panel = create(root.0, 0);
            EnableWindow(panel.0, 0);
            let dialog = create(panel.0, WS_VISIBLE);
            assert!(
                modal_open(root.0),
                "cross-owner modal dialog must block upgrade"
            );
            drop(dialog);
            assert!(!modal_open(root.0));
            EnableWindow(root.0, 0);
            assert!(
                modal_open(root.0),
                "directly disabled root must block upgrade"
            );
            drop(helper);
            drop(panel);
            drop(root);
            UnregisterClassW(class.as_ptr(), instance);
        }
    }
}
