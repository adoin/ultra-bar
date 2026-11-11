use crate::visual::{AppIcon, wide};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::{Com::*, Registry::*, Threading::*},
        UI::{
            Controls::Dialogs::*,
            Shell::{PropertiesSystem::SHGetPropertyStoreForWindow, *},
            WindowsAndMessaging::*,
        },
    },
    core::GUID,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub launch: String,
    pub executable: String,
    pub name: String,
}
pub type Group<T> = (String, Vec<T>, Option<Shortcut>);
pub fn group<T>(pins: &[Shortcut], windows: Vec<T>, key: impl Fn(&T) -> String) -> Vec<Group<T>> {
    let mut groups: Vec<Group<T>> = vec![];
    for pin in pins {
        let id = identity(&pin.executable, 0);
        if !groups.iter().any(|(previous, _, _)| previous == &id) {
            groups.push((id, vec![], Some(pin.clone())));
        }
    }
    for window in windows {
        let id = key(&window);
        if let Some((_, members, _)) = groups.iter_mut().find(|(previous, _, _)| previous == &id) {
            members.push(window);
        } else {
            groups.push((id, vec![window], None));
        }
    }
    groups
}
pub fn normalize(path: &str) -> String {
    path.trim_start_matches("\\\\?\\")
        .replace('/', "\\")
        .to_lowercase()
}
pub fn is_explorer(path: &str) -> bool {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .is_some_and(|root| {
            normalize(path) == normalize(&root.join("explorer.exe").to_string_lossy())
        })
}
pub fn open_this_pc(owner: HWND) -> Result<()> {
    unsafe {
        let path = PathBuf::from(std::env::var_os("WINDIR").context("WINDIR 不可用")?)
            .join("explorer.exe");
        ensure!(
            ShellExecuteW(
                owner,
                wide("open").as_ptr(),
                wide(&path.to_string_lossy()).as_ptr(),
                wide("shell:MyComputerFolder").as_ptr(),
                null(),
                SW_SHOWNORMAL
            ) as isize
                > 32,
            "无法打开此电脑"
        );
        Ok(())
    }
}
pub fn identity(executable: &str, window: usize) -> String {
    let path = normalize(executable);
    if path.is_empty()
        || path.ends_with("\\applicationframehost.exe")
        || path.ends_with("\\rundll32.exe")
    {
        format!("window:{window}")
    } else {
        path
    }
}
pub fn task_identity(executable: &str, window: usize, hosted_id: Option<&str>) -> String {
    hosted_id
        .map(|id| format!("hosted:{}:{id}", normalize(executable)))
        .unwrap_or_else(|| identity(executable, window))
}
pub fn hosted_app_id(hwnd: HWND, executable: &str) -> Option<String> {
    let name = Path::new(executable)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    if !(name == "godot.exe" || name.starts_with("godot_v") || name.starts_with("godot.windows.")) {
        return None;
    }
    type Cache = std::collections::HashMap<(usize, u32), Option<Option<String>>>;
    struct Lookup {
        cache: std::sync::Arc<std::sync::Mutex<Cache>>,
        sender: std::sync::mpsc::SyncSender<(usize, u32)>,
    }
    static LOOKUP: std::sync::OnceLock<Lookup> = std::sync::OnceLock::new();
    let lookup = LOOKUP.get_or_init(|| {
        let cache = std::sync::Arc::new(std::sync::Mutex::new(Cache::new()));
        let work = cache.clone();
        let (sender, requests) = std::sync::mpsc::sync_channel::<(usize, u32)>(32);
        std::thread::spawn(move || {
            while let Ok((window, pid)) = requests.recv() {
                let mut current = 0;
                unsafe {
                    GetWindowThreadProcessId(window as HWND, &mut current);
                }
                let id = if current == pid {
                    read_hosted_app_id(window as HWND)
                } else {
                    None
                };
                work.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert((window, pid), Some(id));
                crate::shell::mark_tasks_dirty();
            }
        });
        Lookup { cache, sender }
    });
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    let key = (hwnd as usize, pid);
    let mut cache = lookup.cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = cache.get(&key) {
        return id.clone().flatten();
    }
    if cache.len() >= 256 {
        cache.retain(|(window, _), _| unsafe { IsWindow(*window as HWND) != 0 });
    }
    cache.insert(key, None);
    drop(cache);
    if lookup.sender.try_send(key).is_err() {
        lookup
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
    None
}
pub fn read_hosted_app_id(hwnd: HWND) -> Option<String> {
    unsafe {
        let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let _apartment = Apartment(hr >= 0);
        let mut pointer = null_mut();
        if SHGetPropertyStoreForWindow(
            hwnd,
            &GUID::from_u128(0x886d8eeb_8cf2_4446_8d02_cdba1dbdcf99),
            &mut pointer,
        ) < 0
            || pointer.is_null()
        {
            return None;
        }
        let properties = ComObject(pointer);
        let get: unsafe extern "system" fn(
            *mut c_void,
            *const PropertyKey,
            *mut PropertyValue,
        ) -> i32 = std::mem::transmute(properties.method(5));
        let key = PropertyKey {
            format: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            id: 5,
        };
        let mut value: PropertyValue = zeroed();
        let mut result = None;
        if get(properties.0, &key, &mut value) >= 0 && value.kind == 31 && value.data[0] != 0 {
            let text = value.data[0] as *const u16;
            let mut len = 0;
            while len < 512 && *text.add(len) != 0 {
                len += 1;
            }
            let id = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            // Official Godot builds publish separate IDs for games and the editor.
            if id.starts_with("Godot.") && !id.starts_with("Godot.GodotEditor.") {
                result = Some(id);
            }
        }
        PropVariantClear(&mut value);
        result
    }
}
pub fn process_path(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return String::new();
        }
        let mut path = [0u16; 32768];
        let mut len = path.len() as u32;
        let found = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) != 0;
        CloseHandle(process);
        if found {
            String::from_utf16_lossy(&path[..len as usize])
        } else {
            String::new()
        }
    }
}
pub fn load() -> Vec<Shortcut> {
    let read = || -> Result<Vec<Shortcut>> {
        let path = crate::data_paths::root()?.join("taskbar-apps.json");
        if !path.exists() {
            return Ok(vec![]);
        }
        ensure!(
            std::fs::metadata(&path)?.len() < 1024 * 1024,
            "固定应用设置过大"
        );
        let pins: Vec<Shortcut> = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(pins
            .into_iter()
            .filter(|pin| !pin.launch.is_empty() && !pin.executable.is_empty())
            .collect())
    };
    read().unwrap_or_else(|error| {
        eprintln!("[task-apps] {error:#}");
        vec![]
    })
}
pub fn initial_pins() -> Vec<Shortcut> {
    let absent =
        crate::data_paths::root().is_ok_and(|root| !root.join("taskbar-apps.json").exists());
    if absent && let Err(error) = sync_system_pins() {
        eprintln!("[task-apps] initial import: {error:#}");
    }
    load()
}
pub fn sync_system_pins() -> Result<serde_json::Value> {
    let previous = load();
    let mut next = vec![];
    let mut skipped = vec![];
    for path in system_pins() {
        match from_file(&path).and_then(retain_link) {
            Ok(pin) => {
                if !next
                    .iter()
                    .any(|old: &Shortcut| normalize(&old.executable) == normalize(&pin.executable))
                {
                    next.push(pin);
                }
            }
            Err(error) => skipped.push(format!("{}: {error:#}", path.display())),
        }
    }
    let imported = next.len();
    for pin in previous {
        if !next
            .iter()
            .any(|old| normalize(&old.executable) == normalize(&pin.executable))
        {
            next.push(pin);
        }
    }
    save(&next)?;
    Ok(
        serde_json::json!({"imported":imported,"total":next.len(),"names":next.iter().map(|pin|pin.name.as_str()).collect::<Vec<_>>(),"skipped":skipped}),
    )
}
pub fn pins_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE
        .get_or_init(|| unsafe { RegisterWindowMessageW(wide("UltraBar.PinsChanged.v1").as_ptr()) })
}
pub fn notify_pins_changed() {
    unsafe extern "system" fn notify(hwnd: HWND, param: isize) -> i32 {
        unsafe {
            let target = &*(param as *const String);
            let mut class = [0u16; 64];
            let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
            if String::from_utf16_lossy(&class[..len.max(0) as usize]) == "UltraBar.Window"
                && normalize(&process_path(hwnd)) == *target
            {
                SendNotifyMessageW(hwnd, pins_message(), 0, 0);
            }
            1
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let target = normalize(&exe.to_string_lossy());
        unsafe {
            EnumWindows(Some(notify), (&target as *const String) as isize);
        }
    }
}
pub fn save(pins: &[Shortcut]) -> Result<()> {
    let path = crate::data_paths::root()?.join("taskbar-apps.json");
    if path.exists() {
        let _: Vec<Shortcut> = serde_json::from_slice(&std::fs::read(&path)?)
            .context("原固定应用设置无法读取，未覆盖文件")?;
    }
    std::fs::create_dir_all(path.parent().context("配置目录无效")?)?;
    let pending = path.with_extension("json.new");
    std::fs::write(&pending, serde_json::to_vec_pretty(pins)?)?;
    std::fs::rename(pending, path)?;
    Ok(())
}
pub fn retain_link(pin: Shortcut) -> Result<Shortcut> {
    if !Path::new(&pin.launch)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
    {
        return Ok(pin);
    }
    use sha2::{Digest, Sha256};
    ensure!(
        std::fs::metadata(&pin.launch)?.len() <= 4 * 1024 * 1024,
        "快捷方式文件过大"
    );
    let bytes = std::fs::read(&pin.launch)?;
    let directory = crate::data_paths::root()?.join("shortcuts");
    std::fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{:x}.lnk", Sha256::digest(&bytes)));
    if !destination.exists() {
        let pending = destination.with_extension("lnk.new");
        std::fs::write(&pending, &bytes)?;
        std::fs::rename(pending, &destination)?;
    }
    ensure!(
        std::fs::read(&destination)? == bytes,
        "固定快捷方式副本损坏"
    );
    Ok(Shortcut {
        launch: destination.to_string_lossy().into(),
        ..pin
    })
}
pub fn from_file(path: &Path) -> Result<Shortcut> {
    let path = path.canonicalize()?;
    let executable = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "exe" => path.to_string_lossy().into_owned(),
        "lnk" => link_metadata(&path)?.0,
        _ => anyhow::bail!("请选择 .exe 程序或 .lnk 快捷方式"),
    };
    ensure!(!executable.is_empty(), "快捷方式没有可分组的程序目标");
    Ok(Shortcut {
        launch: path.to_string_lossy().trim_start_matches("\\\\?\\").into(),
        executable: executable.trim_start_matches("\\\\?\\").into(),
        name: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
    })
}
// IShellLinkW/IPersistFile are the native Shell link reader. Keep the original
// .lnk for launch so arguments, working directory and custom icons survive.
struct ComObject(*mut c_void);
impl ComObject {
    unsafe fn method(&self, index: usize) -> *const c_void {
        unsafe { *((*(self.0 as *const *const *const c_void)).add(index)) }
    }
}
impl Drop for ComObject {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(self.method(2));
            release(self.0);
        }
    }
}
struct Apartment(bool);
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}
#[repr(C)]
struct PropertyKey {
    format: GUID,
    id: u32,
}
#[repr(C)]
struct PropertyValue {
    kind: u16,
    reserved: [u16; 3],
    data: [usize; 2],
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn PropVariantClear(value: *mut PropertyValue) -> i32;
}
unsafe fn link_app_id(link: &ComObject) -> Option<String> {
    unsafe {
        let query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
            std::mem::transmute(link.method(0));
        let mut pointer = null_mut();
        if query(
            link.0,
            &GUID::from_u128(0x886d8eeb_8cf2_4446_8d02_cdba1dbdcf99),
            &mut pointer,
        ) < 0
        {
            return None;
        }
        let properties = ComObject(pointer);
        let get: unsafe extern "system" fn(
            *mut c_void,
            *const PropertyKey,
            *mut PropertyValue,
        ) -> i32 = std::mem::transmute(properties.method(5));
        let key = PropertyKey {
            format: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            id: 5,
        };
        let mut value: PropertyValue = zeroed();
        let mut result = None;
        if get(properties.0, &key, &mut value) >= 0 && value.kind == 31 && value.data[0] != 0 {
            let pointer = value.data[0] as *const u16;
            let mut length = 0;
            while length < 32768 && *pointer.add(length) != 0 {
                length += 1;
            }
            result = Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                pointer, length,
            )));
        }
        PropVariantClear(&mut value);
        result
    }
}
fn link_metadata(path: &Path) -> Result<(String, String, i32)> {
    unsafe {
        let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let _apartment = Apartment(hr >= 0);
        ensure!(
            hr >= 0 || hr == 0x80010106u32 as i32,
            "无法初始化快捷方式读取器"
        );
        let clsid = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
        let iid = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
        let persist = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
        let mut pointer = null_mut();
        ensure!(
            CoCreateInstance(&clsid, null_mut(), CLSCTX_INPROC_SERVER, &iid, &mut pointer) >= 0,
            "无法读取快捷方式"
        );
        let link = ComObject(pointer);
        let query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
            std::mem::transmute(link.method(0));
        let mut pointer = null_mut();
        ensure!(
            query(link.0, &persist, &mut pointer) >= 0,
            "无法打开快捷方式"
        );
        let file = ComObject(pointer);
        let load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32 =
            std::mem::transmute(file.method(5));
        ensure!(
            load(file.0, wide(&path.to_string_lossy()).as_ptr(), 0) >= 0,
            "快捷方式读取失败"
        );
        let get: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32 =
            std::mem::transmute(link.method(3));
        let mut buffer = [0u16; 32768];
        ensure!(
            get(
                link.0,
                buffer.as_mut_ptr(),
                buffer.len() as i32,
                null_mut(),
                0
            ) >= 0,
            "无法解析快捷方式目标"
        );
        let mut target = String::from_utf16_lossy(
            &buffer[..buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len())],
        );
        if target.is_empty() && link_app_id(&link).as_deref() == Some("Microsoft.Windows.Explorer")
        {
            target = PathBuf::from(std::env::var_os("WINDIR").context("WINDIR 不可用")?)
                .join("explorer.exe")
                .to_string_lossy()
                .into();
        }
        if target.is_empty() {
            // Explorer pins can target a virtual folder PIDL rather than an EXE.
            // Launch the original link; group the resulting windows with Explorer.
            let get_id_list: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
                std::mem::transmute(link.method(4));
            let mut pidl = null_mut();
            if get_id_list(link.0, &mut pidl) >= 0 && !pidl.is_null() {
                let mut info: SHFILEINFOW = zeroed();
                const FOLDER_ATTRIBUTE: u32 = 0x20000000; // SFGAO_FOLDER
                info.dwAttributes = FOLDER_ATTRIBUTE;
                let folder = SHGetFileInfoW(
                    pidl.cast(),
                    0,
                    &mut info,
                    size_of::<SHFILEINFOW>() as u32,
                    SHGFI_PIDL | SHGFI_ATTRIBUTES | SHGFI_ATTR_SPECIFIED,
                ) != 0
                    && info.dwAttributes & FOLDER_ATTRIBUTE != 0;
                CoTaskMemFree(pidl);
                if folder {
                    target = PathBuf::from(std::env::var_os("WINDIR").context("WINDIR 不可用")?)
                        .join("explorer.exe")
                        .to_string_lossy()
                        .into();
                }
            }
        }
        if Path::new(&target).is_dir() {
            target = PathBuf::from(std::env::var_os("WINDIR").context("WINDIR 不可用")?)
                .join("explorer.exe")
                .to_string_lossy()
                .into();
        }
        let get_icon: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut i32) -> i32 =
            std::mem::transmute(link.method(16));
        let mut icon = [0u16; 32768];
        let mut index = 0;
        if get_icon(link.0, icon.as_mut_ptr(), icon.len() as i32, &mut index) >= 0 {
            DoEnvironmentSubstW(icon.as_mut_ptr(), icon.len() as u32);
        }
        let source = String::from_utf16_lossy(
            &icon[..icon.iter().position(|v| *v == 0).unwrap_or(icon.len())],
        );
        Ok((target, source, index))
    }
}
pub unsafe fn icon(pin: &Shortcut, dpi: i32, size: i32) -> AppIcon {
    unsafe {
        if Path::new(&pin.launch)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
            && let Ok((_, source, index)) = link_metadata(Path::new(&pin.launch))
            && !source.is_empty()
        {
            return AppIcon::from_resource(&source, index, dpi, size);
        }
        AppIcon::from_file(&pin.executable, dpi, size)
    }
}
pub fn launch(pin: &Shortcut) -> Result<()> {
    unsafe {
        ensure!(
            ShellExecuteW(
                null_mut(),
                wide("open").as_ptr(),
                wide(&pin.launch).as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL
            ) as isize
                > 32,
            "无法启动 {}",
            pin.name
        );
    }
    Ok(())
}
pub unsafe fn choose(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let mut buffer = [0u16; 32768];
        let filter: Vec<u16> = "应用与快捷方式\0*.exe;*.lnk\0\0".encode_utf16().collect();
        let mut dialog: OPENFILENAMEW = zeroed();
        dialog.lStructSize = size_of::<OPENFILENAMEW>() as _;
        dialog.hwndOwner = owner;
        dialog.lpstrFilter = filter.as_ptr();
        dialog.lpstrFile = buffer.as_mut_ptr();
        dialog.nMaxFile = buffer.len() as u32;
        dialog.Flags = OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_NODEREFERENCELINKS;
        if GetOpenFileNameW(&mut dialog) == 0 {
            return None;
        }
        Some(
            String::from_utf16_lossy(
                &buffer[..buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len())],
            )
            .into(),
        )
    }
}
pub fn system_pins() -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .and_then(|root| {
            std::fs::read_dir(
                root.join("Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar"),
            )
            .ok()
        })
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        })
        .collect();
    // Taskband is undocumented. Only use matching filenames as a best-effort
    // ordering hint; never parse opaque records or write Explorer's settings.
    if let Some(bytes) = taskband_order() {
        paths.sort_by_key(|path| {
            let pattern: Vec<u8> = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            bytes
                .windows(pattern.len().max(1))
                .position(|part| part == pattern)
                .unwrap_or(usize::MAX)
        });
    }
    paths
}
fn taskband_order() -> Option<Vec<u8>> {
    unsafe {
        let path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
        let name = wide("Favorites");
        let mut size = 0;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            null_mut(),
            &mut size,
        ) != ERROR_SUCCESS
            || size > 2 * 1024 * 1024
        {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            bytes.as_mut_ptr().cast(),
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        bytes.truncate(size as usize);
        Some(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hosted_projects_are_distinct_from_the_engine_pin_and_group_by_project_id() {
        let engine = "D:/Godot/Godot_v4.6.1-stable_win64.exe";
        let a = task_identity(engine, 11, Some("Godot.HeroMahjong.1"));
        assert_eq!(a, task_identity(engine, 99, Some("Godot.HeroMahjong.1")));
        assert_ne!(a, task_identity(engine, 22, Some("Godot.OtherGame.1")));
        assert_ne!(a, identity(engine, 0));
        assert_eq!(task_identity(engine, 33, None), identity(engine, 0));
    }
    #[test]
    fn reads_native_link_with_unicode_path_and_keeps_original_launch_file() {
        let root = PathBuf::from("D:/tmp/ultra-bar-taskbar-tests").join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("测试 shortcut.lnk");
        let target =
            PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("System32/notepad.exe");
        unsafe {
            let initialized = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
            let _apartment = Apartment(initialized >= 0);
            let mut pointer = null_mut();
            assert!(
                CoCreateInstance(
                    &GUID::from_u128(0x00021401_0000_0000_c000_000000000046),
                    null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &GUID::from_u128(0x000214f9_0000_0000_c000_000000000046),
                    &mut pointer
                ) >= 0
            );
            let link = ComObject(pointer);
            let set: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                std::mem::transmute(link.method(20));
            assert!(set(link.0, wide(&target.to_string_lossy()).as_ptr()) >= 0);
            let args: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                std::mem::transmute(link.method(11));
            assert!(args(link.0, wide("/test-argument \"with spaces\"").as_ptr()) >= 0);
            let query: unsafe extern "system" fn(
                *mut c_void,
                *const GUID,
                *mut *mut c_void,
            ) -> i32 = std::mem::transmute(link.method(0));
            let mut pointer = null_mut();
            assert!(
                query(
                    link.0,
                    &GUID::from_u128(0x0000010b_0000_0000_c000_000000000046),
                    &mut pointer
                ) >= 0
            );
            let file = ComObject(pointer);
            let save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32 =
                std::mem::transmute(file.method(6));
            assert!(save(file.0, wide(&path.to_string_lossy()).as_ptr(), 1) >= 0);
        }
        let pin = from_file(&path).unwrap();
        assert_eq!(
            normalize(&pin.executable),
            normalize(&target.to_string_lossy())
        );
        assert_eq!(PathBuf::from(&pin.launch), path);
        assert!(pin.name.contains("测试"));
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn pinned_slot_survives_start_multi_window_and_exit() {
        let pin = Shortcut {
            launch: "C:/Apps/Chat.lnk".into(),
            executable: "C:/Apps/WeChat.exe".into(),
            name: "微信".into(),
        };
        let plan = |windows: Vec<&'static str>| {
            group(std::slice::from_ref(&pin), windows, |path| {
                identity(path, 1)
            })
        };
        let closed = plan(vec![]);
        let started = plan(vec!["c:\\apps\\wechat.exe"]);
        let multiple = plan(vec!["C:/Apps/WeChat.exe", "c:/APPS/wechat.exe"]);
        assert_eq!(closed.len(), 1);
        assert_eq!(started.len(), 1);
        assert_eq!(multiple.len(), 1);
        assert_eq!(closed[0].0, multiple[0].0);
        assert_eq!(multiple[0].1.len(), 2);
        assert_eq!(started[0].2.as_ref().unwrap().launch, pin.launch);
        assert_eq!(plan(vec![])[0].2, Some(pin));
    }
    #[test]
    fn same_filename_in_different_directories_remains_separate_and_duplicate_pins_merge() {
        let pin = Shortcut {
            launch: "C:/A/App.exe".into(),
            executable: "C:/A/App.exe".into(),
            name: "App".into(),
        };
        let groups = group(
            &[pin.clone(), pin],
            vec!["C:/A/App.exe", "C:/B/App.exe"],
            |path| identity(path, 0),
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].1.len(), 1);
        assert!(groups[1].2.is_none());
    }
    #[test]
    fn executable_identity_merges_instances_but_keeps_hosted_apps_separate() {
        assert_eq!(
            identity("C:/Apps/WeChat.exe", 1),
            identity("c:\\apps\\wechat.EXE", 2)
        );
        assert_ne!(identity("", 1), identity("", 2));
        assert_ne!(
            identity("C:/Windows/System32/ApplicationFrameHost.exe", 1),
            identity("C:/Windows/System32/ApplicationFrameHost.exe", 2)
        );
    }
}
