//! Shell COM objects stay in the disposable query process. No Explorer injection.
//! Private read-only ABI references: Open-Shell JumpLists.cpp; Windhawk
//! taskbar-background-helper IAppResolver_8. Unknown interface versions fall back
//! to documented IApplicationDocumentLists. Never begin/commit a real app's list.
use super::{Entry, Query, Wire};
use crate::visual::wide;
use anyhow::{Result, ensure};
use std::{
    ffi::c_void,
    mem::zeroed,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::Com::*,
        UI::{
            Shell::{PropertiesSystem::*, *},
            WindowsAndMessaging::*,
        },
    },
    core::GUID,
};
const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const LINK: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
const ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const PROPERTIES: GUID = GUID::from_u128(0x886d8eeb_8cf2_4446_8d02_cdba1dbdcf99);
const ARRAY: GUID = GUID::from_u128(0x92ca9dcd_5622_4bba_a805_5e9f541bd8c9);
const PERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
const APP_KEY: GUID = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
const TITLE_KEY: GUID = GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9);
pub struct Apartment(bool);
impl Apartment {
    pub fn new() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
        ensure!(
            hr >= 0 || hr == 0x80010106u32 as i32,
            "Shell COM initialization failed"
        );
        Ok(Self(hr >= 0))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}
struct Object(*mut c_void);
impl Object {
    unsafe fn method(&self, index: usize) -> *const c_void {
        unsafe { *(*(self.0 as *const *const *const c_void)).add(index) }
    }
    fn create(class: GUID, iid: GUID) -> Option<Self> {
        unsafe {
            let mut pointer = null_mut();
            (CoCreateInstance(
                &class,
                null_mut(),
                CLSCTX_INPROC_SERVER | CLSCTX_INPROC_HANDLER,
                &iid,
                &mut pointer,
            ) >= 0
                && !pointer.is_null())
            .then_some(Self(pointer))
        }
    }
    fn query(&self, iid: GUID) -> Option<Self> {
        unsafe {
            let get: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
                std::mem::transmute(self.method(0));
            let mut pointer = null_mut();
            (get(self.0, &iid, &mut pointer) >= 0 && !pointer.is_null()).then_some(Self(pointer))
        }
    }
    fn text(&self, format: GUID, id: u32) -> Option<String> {
        let properties = self.query(PROPERTIES)?;
        unsafe {
            let get: unsafe extern "system" fn(*mut c_void, *const Key, *mut Value) -> i32 =
                std::mem::transmute(properties.method(5));
            let mut value: Value = zeroed();
            let hr = get(properties.0, &Key { format, id }, &mut value);
            let result = if hr >= 0 && (value.kind == 31 || value.kind == 8) && value.data[0] != 0 {
                Some(copy_text(value.data[0] as *const u16))
            } else {
                None
            };
            PropVariantClear(&mut value);
            result.filter(|text| !text.is_empty())
        }
    }
    fn separator(&self) -> bool {
        let Some(properties) = self.query(PROPERTIES) else {
            return false;
        };
        unsafe {
            let get: unsafe extern "system" fn(*mut c_void, *const Key, *mut Value) -> i32 =
                std::mem::transmute(properties.method(5));
            let mut value: Value = zeroed();
            let hr = get(
                properties.0,
                &Key {
                    format: APP_KEY,
                    id: 6,
                },
                &mut value,
            );
            let result = hr >= 0 && value.kind == 11 && (value.data[0] & 0xffff) != 0;
            PropVariantClear(&mut value);
            result
        }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(self.method(2));
                release(self.0);
            }
        }
    }
}
#[repr(C)]
struct Key {
    format: GUID,
    id: u32,
}
#[repr(C)]
struct Value {
    kind: u16,
    reserved: [u16; 3],
    data: [usize; 2],
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn PropVariantClear(value: *mut Value) -> i32;
}
unsafe fn copy_text(pointer: *const u16) -> String {
    unsafe {
        if pointer.is_null() {
            return String::new();
        }
        let mut length = 0;
        while length < 32768 && *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}
unsafe fn owned_text(pointer: *mut u16) -> String {
    unsafe {
        let text = copy_text(pointer);
        CoTaskMemFree(pointer.cast());
        text
    }
}
fn localized(value: &str) -> String {
    unsafe {
        if !value.starts_with('@') {
            return value.chars().take(1024).collect();
        }
        let mut text = [0u16; 4096];
        if SHLoadIndirectString(
            wide(value).as_ptr(),
            text.as_mut_ptr(),
            text.len() as u32,
            null(),
        ) >= 0
        {
            copy_text(text.as_ptr())
        } else {
            value.chars().take(1024).collect()
        }
    }
}
fn load_link(path: &Path) -> Option<Object> {
    let link = Object::create(
        GUID::from_u128(0x00021401_0000_0000_c000_000000000046),
        LINK,
    )?;
    let file = link.query(PERSIST_FILE)?;
    unsafe {
        let load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32 =
            std::mem::transmute(file.method(5));
        if load(file.0, wide(&path.to_string_lossy()).as_ptr(), 0) < 0 {
            return None;
        }
    }
    Some(link)
}
fn shell_item(path: &Path) -> Option<Object> {
    unsafe {
        let mut pointer = null_mut();
        (SHCreateItemFromParsingName(
            wide(&path.to_string_lossy()).as_ptr(),
            null_mut(),
            &ITEM,
            &mut pointer,
        ) >= 0
            && !pointer.is_null())
        .then_some(Object(pointer))
    }
}
fn resolver() -> Option<Object> {
    Object::create(
        GUID::from_u128(0x660b90c8_73a9_4b58_8cae_355b7f55341b),
        GUID::from_u128(0xde25675a_72de_44b4_9373_05170450c140),
    )
}
fn app_id(query: &Query) -> Option<String> {
    unsafe {
        if let Some(id) = &query.app_id {
            return Some(id.clone());
        }
        let hwnd = query.hwnd as HWND;
        let mut pid = 0;
        let window =
            !hwnd.is_null() && GetWindowThreadProcessId(hwnd, &mut pid) != 0 && pid == query.pid;
        if window {
            let mut pointer = null_mut();
            if SHGetPropertyStoreForWindow(hwnd, &PROPERTIES, &mut pointer) >= 0
                && !pointer.is_null()
                && let Some(id) = Object(pointer).text(APP_KEY, 5)
            {
                return Some(id);
            }
            if let Some(resolver) = resolver() {
                let get: unsafe extern "system" fn(
                    *mut c_void,
                    HWND,
                    *mut *mut u16,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> i32 = std::mem::transmute(resolver.method(5));
                let mut pointer = null_mut();
                if get(
                    resolver.0,
                    hwnd,
                    &mut pointer,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                ) >= 0
                    && !pointer.is_null()
                {
                    let id = owned_text(pointer);
                    if !id.is_empty() {
                        return Some(id);
                    }
                }
            }
        }
        let path = query
            .shortcut
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&query.executable));
        if let Some(link) = load_link(&path)
            && let Some(id) = link.text(APP_KEY, 5)
        {
            return Some(id);
        }
        let item = shell_item(&path)?;
        let resolver = resolver()?;
        let get: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut u16) -> i32 =
            std::mem::transmute(resolver.method(3));
        let mut pointer = null_mut();
        if get(resolver.0, item.0, &mut pointer) >= 0 && !pointer.is_null() {
            let id = owned_text(pointer);
            if !id.is_empty() {
                return Some(id);
            }
        }
        None
    }
}
#[repr(C)]
struct Category {
    kind: i32,
    value: usize,
    count: i32,
    padding: [i32; 10],
}
fn custom(app_id: &str) -> Option<Object> {
    let unknown = Object::create(
        GUID::from_u128(0x77f10cf0_3db5_4966_b520_b7c54fd35ed6),
        UNKNOWN,
    )?;
    for iid in [
        0xfebd543d_1f7b_4b38_940b_5933bd2cb21bu128,
        0x507101cd_f6ad_46c8_8e20_eeb9e6bac47f,
        0x03f1eed2_8676_430b_abe1_765c1d8fe147,
    ] {
        if let Some(list) = unknown.query(GUID::from_u128(iid)) {
            unsafe {
                let set: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                    std::mem::transmute(list.method(4));
                if set(list.0, wide(app_id).as_ptr()) >= 0 {
                    return Some(list);
                }
            }
        }
    }
    None
}
unsafe fn collection(list: &Object, index: u32) -> Option<Object> {
    unsafe {
        let get: unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> i32 =
            std::mem::transmute(list.method(9));
        let mut pointer = null_mut();
        (get(list.0, index, &ARRAY, &mut pointer) >= 0 && !pointer.is_null())
            .then_some(Object(pointer))
    }
}
fn recent(app_id: &str, kind: i32) -> Option<Object> {
    let list = Object::create(
        GUID::from_u128(0x86bec222_30f2_47e0_9f25_60d11cd75c28),
        GUID::from_u128(0x3c594f9f_9f30_47a1_979a_c9e83d3d0a06),
    )?;
    unsafe {
        let set: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
            std::mem::transmute(list.method(3));
        if set(list.0, wide(app_id).as_ptr()) < 0 {
            return None;
        }
        let get: unsafe extern "system" fn(
            *mut c_void,
            i32,
            u32,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = std::mem::transmute(list.method(4));
        let mut pointer = null_mut();
        (get(list.0, kind, 10, &ARRAY, &mut pointer) >= 0 && !pointer.is_null())
            .then_some(Object(pointer))
    }
}
fn pinned(app_id: &str) -> Option<Object> {
    let list = Object::create(
        GUID::from_u128(0xf0ae1542_f497_484b_a175_a20db09144ba),
        GUID::from_u128(0xe9c5ef8d_fd41_4f72_ba87_eb03bad5817c),
    )?;
    unsafe {
        let init: unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *const u16,
            *const u16,
        ) -> i32 = std::mem::transmute(list.method(3));
        if init(list.0, wide(app_id).as_ptr(), null(), null()) < 0 {
            return None;
        }
        let get: unsafe extern "system" fn(
            *mut c_void,
            i32,
            u32,
            u32,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = std::mem::transmute(list.method(5));
        let mut pointer = null_mut();
        (get(list.0, 0, 10, 1, &ARRAY, &mut pointer) >= 0 && !pointer.is_null())
            .then_some(Object(pointer))
    }
}
fn link_string(link: &Object, slot: usize) -> String {
    unsafe {
        let get: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32 =
            std::mem::transmute(link.method(slot));
        let mut buffer = [0u16; 32768];
        if get(link.0, buffer.as_mut_ptr(), buffer.len() as i32) >= 0 {
            copy_text(buffer.as_ptr())
        } else {
            String::new()
        }
    }
}
fn entry(object: &Object, category: &str, query: &Query, index: usize) -> Option<Entry> {
    unsafe {
        if let Some(link) = object.query(LINK) {
            if link.separator() {
                return None;
            }
            let title = link
                .text(TITLE_KEY, 2)
                .map(|title| localized(&title))
                .unwrap_or_else(|| link_string(&link, 6));
            if title.is_empty() {
                return None;
            }
            let file = link.query(PERSIST_FILE)?;
            let save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32 =
                std::mem::transmute(file.method(6));
            let path = query
                .output
                .with_file_name(format!("{}-{index}.lnk", query.prefix));
            if save(file.0, wide(&path.to_string_lossy()).as_ptr(), 0) < 0 {
                return None;
            }
            let icon_get: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut i32) -> i32 =
                std::mem::transmute(link.method(16));
            let mut icon = [0u16; 32768];
            let mut icon_index = 0;
            icon_get(
                link.0,
                icon.as_mut_ptr(),
                icon.len() as i32,
                &mut icon_index,
            );
            DoEnvironmentSubstW(icon.as_mut_ptr(), icon.len() as u32);
            let mut icon = copy_text(icon.as_ptr());
            if icon.is_empty() {
                icon = query.executable.clone();
            }
            Some(Entry {
                title,
                category: category.into(),
                path,
                kind: "link".into(),
                icon,
                icon_index,
            })
        } else if let Some(item) = object.query(ITEM) {
            let get: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32 =
                std::mem::transmute(item.method(5));
            let mut name = null_mut();
            if get(item.0, SIGDN_NORMALDISPLAY, &mut name) < 0 || name.is_null() {
                return None;
            }
            let title = owned_text(name);
            let mut pointer = null_mut();
            if SHGetIDListFromObject(item.0, &mut pointer) < 0 || pointer.is_null() {
                return None;
            }
            let size = ILGetSize(pointer) as usize;
            if size > 1024 * 1024 {
                CoTaskMemFree(pointer.cast());
                return None;
            }
            let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), size).to_vec();
            CoTaskMemFree(pointer.cast());
            if !super::valid_pidl(&bytes) {
                return None;
            }
            let path = query
                .output
                .with_file_name(format!("{}-{index}.pidl", query.prefix));
            if std::fs::write(&path, bytes).is_err() {
                return None;
            }
            let mut name = null_mut();
            let icon =
                if get(item.0, SIGDN_DESKTOPABSOLUTEPARSING, &mut name) >= 0 && !name.is_null() {
                    owned_text(name)
                } else {
                    query.executable.clone()
                };
            Some(Entry {
                title,
                category: category.into(),
                path,
                kind: "pidl".into(),
                icon,
                icon_index: 0,
            })
        } else {
            None
        }
    }
}
fn append(array: Option<Object>, category: &str, query: &Query, entries: &mut Vec<Entry>) {
    let Some(array) = array else { return };
    unsafe {
        let count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
            std::mem::transmute(array.method(3));
        let get: unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> i32 =
            std::mem::transmute(array.method(4));
        let mut length = 0;
        if count(array.0, &mut length) < 0 {
            return;
        }
        for index in 0..length.min(32) {
            if entries.len() >= 32 {
                break;
            }
            let mut pointer = null_mut();
            if get(array.0, index, &UNKNOWN, &mut pointer) >= 0
                && !pointer.is_null()
                && let Some(entry) = entry(&Object(pointer), category, query, entries.len())
            {
                let data = std::fs::read(&entry.path).ok();
                let duplicate = entries.iter().any(|previous| {
                    previous.kind == entry.kind
                        && previous.title == entry.title
                        && data.is_some()
                        && std::fs::read(&previous.path).ok() == data
                });
                if duplicate {
                    let _ = std::fs::remove_file(&entry.path);
                } else {
                    entries.push(entry);
                }
            }
        }
    }
}
pub fn read(query: &Query) -> Result<Wire> {
    let _apartment = Apartment::new()?;
    let Some(id) = app_id(query) else {
        return Ok(Wire {
            app_id: None,
            custom_supported: false,
            entries: vec![],
        });
    };
    let mut entries = vec![];
    let list = custom(&id);
    let custom_supported = list.is_some();
    // Common actions first, followed by the app's own categories and destinations.
    if let Some(list) = list {
        unsafe {
            let get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                std::mem::transmute(list.method(6));
            let get: unsafe extern "system" fn(*mut c_void, u32, i32, *mut Category) -> i32 =
                std::mem::transmute(list.method(7));
            let mut count = 0;
            if get_count(list.0, &mut count) >= 0 {
                let mut categories = vec![];
                for index in 0..count.min(16) {
                    let mut category: Category = zeroed();
                    if get(list.0, index, 1, &mut category) >= 0 {
                        let name = if category.kind == 0 && category.value != 0 {
                            localized(&owned_text(category.value as *mut u16))
                        } else if category.kind == 2 {
                            "任务".into()
                        } else {
                            String::new()
                        };
                        categories.push((index, category.kind, name));
                    }
                }
                for (index, kind, name) in categories.iter().filter(|(_, kind, _)| *kind == 2) {
                    let _ = kind;
                    append(collection(&list, *index), name, query, &mut entries);
                }
                append(pinned(&id), "已固定", query, &mut entries);
                for (index, kind, name) in categories {
                    if kind == 0 {
                        append(collection(&list, index), &name, query, &mut entries);
                    }
                }
            }
        }
    }
    append(recent(&id, 0), "最近", query, &mut entries);
    if entries.len() < 12 {
        append(recent(&id, 1), "常用", query, &mut entries);
    }
    Ok(Wire {
        app_id: Some(id),
        custom_supported,
        entries,
    })
}

pub fn invoke(entry: &Entry, owner: HWND) -> Result<()> {
    let _apartment = Apartment::new()?;
    unsafe {
        if entry.kind == "link" {
            ensure!(
                ShellExecuteW(
                    owner,
                    wide("open").as_ptr(),
                    wide(&entry.path.to_string_lossy()).as_ptr(),
                    null(),
                    null(),
                    SW_SHOWNORMAL
                ) as isize
                    > 32,
                "应用快捷操作启动失败"
            );
        } else {
            let mut bytes = std::fs::read(&entry.path)?;
            ensure!(super::valid_pidl(&bytes), "无效的 Shell 目标");
            let mut info: SHELLEXECUTEINFOW = zeroed();
            info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            info.fMask = SEE_MASK_IDLIST;
            info.hwnd = owner;
            info.lpIDList = bytes.as_mut_ptr().cast();
            info.nShow = SW_SHOWNORMAL;
            ensure!(ShellExecuteExW(&mut info) != 0, "无法打开最近项目");
        }
        Ok(())
    }
}

/// A uniquely identified test list, never an existing application's list.
pub struct Fixture {
    list: Object,
    id: String,
    _apartment: Apartment,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            if self.id.starts_with("UltraBar.JumpListTest.") {
                let delete: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                    std::mem::transmute(self.list.method(10));
                delete(self.list.0, wide(&self.id).as_ptr());
            }
        }
    }
}
pub fn fixture(id: String, marker: &Path, working: &Path) -> Result<Fixture> {
    ensure!(
        id.starts_with("UltraBar.JumpListTest."),
        "Invalid fixture identity"
    );
    let apartment = Apartment::new()?;
    let list = Object::create(
        GUID::from_u128(0x77f10cf0_3db5_4966_b520_b7c54fd35ed6),
        GUID::from_u128(0x6332debf_87b5_4670_90c0_5e57b408a49e),
    )
    .ok_or_else(|| anyhow::anyhow!("DestinationList unavailable"))?;
    let fixture = Fixture {
        list,
        id,
        _apartment: apartment,
    };
    unsafe {
        let set: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
            std::mem::transmute(fixture.list.method(3));
        ensure!(
            set(fixture.list.0, wide(&fixture.id).as_ptr()) >= 0,
            "Cannot set test identity"
        );
        let begin: unsafe extern "system" fn(
            *mut c_void,
            *mut u32,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = std::mem::transmute(fixture.list.method(4));
        let mut slots = 0;
        let mut removed = null_mut();
        ensure!(
            begin(fixture.list.0, &mut slots, &ARRAY, &mut removed) >= 0,
            "Cannot begin test list"
        );
        let _removed = Object(removed);
        let collection = Object::create(
            GUID::from_u128(0x2d3468c1_36a7_43b6_ac24_d3f02fd9607a),
            GUID::from_u128(0x5632b1a4_e38a_400a_928a_d4cd63230295),
        )
        .ok_or_else(|| anyhow::anyhow!("ObjectCollection unavailable"))?;
        let link = Object::create(
            GUID::from_u128(0x00021401_0000_0000_c000_000000000046),
            LINK,
        )
        .ok_or_else(|| anyhow::anyhow!("ShellLink unavailable"))?;
        let set_text = |slot: usize, text: &str| {
            let set: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                std::mem::transmute(link.method(slot));
            set(link.0, wide(text).as_ptr())
        };
        ensure!(
            set_text(20, &std::env::current_exe()?.to_string_lossy()) >= 0,
            "Cannot set test executable"
        );
        ensure!(
            set_text(
                11,
                &format!(
                    "--jump-list-probe {}",
                    super::job::quote(&marker.to_string_lossy())
                )
            ) >= 0,
            "Cannot set arguments"
        );
        ensure!(
            set_text(9, &working.to_string_lossy()) >= 0,
            "Cannot set working directory"
        );
        let properties = link
            .query(PROPERTIES)
            .ok_or_else(|| anyhow::anyhow!("PropertyStore unavailable"))?;
        let set_value: unsafe extern "system" fn(*mut c_void, *const Key, *const Value) -> i32 =
            std::mem::transmute(properties.method(6));
        let title = wide("原生任务参数验证");
        let value = Value {
            kind: 31,
            reserved: [0; 3],
            data: [title.as_ptr() as usize, 0],
        };
        ensure!(
            set_value(
                properties.0,
                &Key {
                    format: TITLE_KEY,
                    id: 2
                },
                &value
            ) >= 0,
            "Cannot set task title"
        );
        let commit: unsafe extern "system" fn(*mut c_void) -> i32 =
            std::mem::transmute(properties.method(7));
        ensure!(commit(properties.0) >= 0, "Cannot commit title");
        let add: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(collection.method(5));
        ensure!(add(collection.0, link.0) >= 0, "Cannot add test task");
        let array = collection
            .query(ARRAY)
            .ok_or_else(|| anyhow::anyhow!("ObjectArray unavailable"))?;
        let tasks: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32 =
            std::mem::transmute(fixture.list.method(7));
        ensure!(
            tasks(fixture.list.0, array.0) >= 0,
            "Cannot publish test task"
        );
        let commit: unsafe extern "system" fn(*mut c_void) -> i32 =
            std::mem::transmute(fixture.list.method(8));
        ensure!(commit(fixture.list.0) >= 0, "Cannot commit test list");
    }
    Ok(fixture)
}
