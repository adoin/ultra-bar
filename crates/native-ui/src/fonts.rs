//! Default text family shared by host controls and SDK-rendered plugin content.
use std::{
    collections::{BTreeSet, HashMap},
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::null_mut,
    sync::{OnceLock, RwLock},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};
#[derive(Default)]
struct Config {
    family: String,
    path: Option<PathBuf>,
}
fn config() -> &'static RwLock<Config> {
    static VALUE: OnceLock<RwLock<Config>> = OnceLock::new();
    VALUE.get_or_init(|| {
        let path = std::env::var_os("ULTRA_BAR_FONT_SETTINGS").map(PathBuf::from);
        let family = read_family(path.as_deref())
            .or_else(|| std::env::var("ULTRA_BAR_FONT_FAMILY").ok())
            .unwrap_or_else(|| "Segoe UI".into());
        RwLock::new(Config { family, path })
    })
}
fn read_family(path: Option<&std::path::Path>) -> Option<String> {
    let path = path?;
    if std::fs::metadata(path).ok()?.len() > 65536 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["font_family"]
        .as_str()
        .filter(|s| valid(s))
        .map(str::to_owned)
}
pub fn valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.encode_utf16().count() < 32
        && !name.chars().any(char::is_control)
}
pub fn configure(path: PathBuf, name: &str) {
    let mut value = config().write().unwrap_or_else(|e| e.into_inner());
    value.path = Some(path);
    value.family = if valid(name) {
        name.into()
    } else {
        "Segoe UI".into()
    };
}
pub fn family() -> String {
    config()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .family
        .clone()
}
pub fn inherit(command: &mut std::process::Command) {
    let value = config().read().unwrap_or_else(|e| e.into_inner());
    command.env("ULTRA_BAR_FONT_FAMILY", &value.family);
    if let Some(path) = &value.path {
        command.env("ULTRA_BAR_FONT_SETTINGS", path);
    }
}
fn special(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("assets")
        || name.contains("icons")
        || name.contains("symbol")
        || name.contains("wingdings")
        || name.contains("webdings")
        || name.contains("emoji")
        || matches!(
            name.as_str(),
            "consolas" | "cascadia code" | "cascadia mono"
        )
}
pub fn resolve(name: &str) -> String {
    if special(name) { name.into() } else { family() }
}
fn roles() -> &'static RwLock<HashMap<usize, bool>> {
    static ROLES: OnceLock<RwLock<HashMap<usize, bool>>> = OnceLock::new();
    ROLES.get_or_init(|| RwLock::new(HashMap::new()))
}
fn choose_face(default_role: Option<bool>, name: &str, current: &str) -> String {
    if default_role == Some(true) || (default_role.is_none() && !special(name)) {
        current.into()
    } else {
        name.into()
    }
}
pub fn resolve_font(font: HFONT, name: &str) -> String {
    let role = roles()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(font as usize))
        .copied();
    choose_face(role, name, &family())
}
/// # Safety
/// Same ownership contract as GDI DeleteObject; also releases tracked font roles.
pub unsafe fn delete_object(object: HGDIOBJ) -> i32 {
    unsafe {
        let result = windows_sys::Win32::Graphics::Gdi::DeleteObject(object);
        if result != 0 {
            roles()
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(object as usize));
        }
        result
    }
}
pub fn message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(crate::wide("UltraBar.FontChanged.v1").as_ptr())
    })
}
pub fn reload() {
    let path = config()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .path
        .clone();
    if let Some(name) = read_family(path.as_deref()) {
        config().write().unwrap_or_else(|e| e.into_inner()).family = name;
    }
}
pub fn installed() -> Vec<String> {
    unsafe extern "system" fn collect(
        font: *const LOGFONTW,
        _: *const TEXTMETRICW,
        _: u32,
        data: isize,
    ) -> i32 {
        unsafe {
            let name = &(*font).lfFaceName;
            let text =
                String::from_utf16_lossy(&name[..name.iter().position(|c| *c == 0).unwrap_or(32)]);
            if !text.starts_with('@') && valid(&text) {
                (*(data as *mut BTreeSet<String>)).insert(text);
            }
            1
        }
    }
    unsafe {
        let dc = GetDC(null_mut());
        let mut font: LOGFONTW = zeroed();
        font.lfCharSet = DEFAULT_CHARSET;
        let mut names = BTreeSet::new();
        EnumFontFamiliesExW(
            dc,
            &font,
            Some(collect),
            (&mut names as *mut BTreeSet<String>) as isize,
            0,
        );
        ReleaseDC(null_mut(), dc);
        names.into_iter().collect()
    }
}
/// # Safety
/// Same contract as CreateFontW; the face pointer must reference a terminated string.
#[allow(clippy::too_many_arguments)]
pub unsafe fn create_font(
    h: i32,
    w: i32,
    e: i32,
    o: i32,
    weight: i32,
    italic: u32,
    underline: u32,
    strike: u32,
    charset: u32,
    out: u32,
    clip: u32,
    quality: u32,
    pitch: u32,
    face: *const u16,
) -> HFONT {
    unsafe {
        let mut length = 0;
        if !face.is_null() {
            while length < 32 && *face.add(length) != 0 {
                length += 1;
            }
        }
        let requested = if face.is_null() {
            String::new()
        } else {
            String::from_utf16_lossy(std::slice::from_raw_parts(face, length))
        };
        let font = CreateFontW(
            h,
            w,
            e,
            o,
            weight,
            italic,
            underline,
            strike,
            charset,
            out,
            clip,
            quality,
            pitch,
            crate::wide(&resolve(&requested)).as_ptr(),
        );
        if !font.is_null() {
            roles()
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(font as usize, !special(&requested));
        }
        font
    }
}
const OVERRIDE: &str = "UltraBar.Font.Override.v1";
unsafe extern "system" fn cleanup(
    hwnd: HWND,
    m: u32,
    w: usize,
    l: isize,
    id: usize,
    _: usize,
) -> isize {
    unsafe {
        if m == WM_NCDESTROY {
            let font = RemovePropW(hwnd, crate::wide(OVERRIDE).as_ptr());
            if !font.is_null() {
                delete_object(font);
            }
            RemoveWindowSubclass(hwnd, Some(cleanup), id);
        }
        DefSubclassProc(hwnd, m, w, l)
    }
}
unsafe extern "system" fn refresh(hwnd: HWND, _: isize) -> i32 {
    unsafe {
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), 64);
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CHILD == 0
            && !class.eq_ignore_ascii_case("tooltips_class32")
            && !class.eq_ignore_ascii_case("ComboLBox")
            && !class.eq_ignore_ascii_case("SysMonthCal32")
        {
            return 1;
        }
        let mut font = SendMessageW(hwnd, WM_GETFONT, 0, 0) as HFONT;
        if font.is_null() {
            let mut class = [0u16; 64];
            let n = GetClassNameW(hwnd, class.as_mut_ptr(), 64);
            if String::from_utf16_lossy(&class[..n.max(0) as usize])
                .eq_ignore_ascii_case("tooltips_class32")
            {
                font = GetStockObject(DEFAULT_GUI_FONT);
            }
        }
        if font.is_null() {
            return 1;
        }
        let mut log: LOGFONTW = zeroed();
        if GetObjectW(
            font,
            size_of::<LOGFONTW>() as i32,
            (&mut log as *mut LOGFONTW).cast(),
        ) == 0
        {
            return 1;
        }
        let n = log.lfFaceName.iter().position(|v| *v == 0).unwrap_or(32);
        let old = String::from_utf16_lossy(&log.lfFaceName[..n]);
        let face = resolve_font(font, &old);
        if face == old {
            return 1;
        }
        log.lfFaceName.fill(0);
        for (to, from) in log.lfFaceName.iter_mut().take(31).zip(face.encode_utf16()) {
            *to = from;
        }
        let next = CreateFontIndirectW(&log);
        if next.is_null() {
            return 1;
        }
        roles()
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(next as usize, true);
        let previous = GetPropW(hwnd, crate::wide(OVERRIDE).as_ptr());
        SendMessageW(hwnd, WM_SETFONT, next as usize, 1);
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), 64);
        if String::from_utf16_lossy(&class[..n.max(0) as usize])
            .eq_ignore_ascii_case("SysDateTimePick32")
        {
            SendMessageW(
                hwnd,
                windows_sys::Win32::UI::Controls::DTM_SETMCFONT,
                next as usize,
                1,
            );
        }
        SetPropW(hwnd, crate::wide(OVERRIDE).as_ptr(), next);
        SetWindowSubclass(hwnd, Some(cleanup), 0x554246, 0);
        if !previous.is_null() {
            delete_object(previous);
        }
        1
    }
}
/// # Safety
/// Call on the window's UI thread.
pub unsafe fn refresh_tree(hwnd: HWND) {
    unsafe {
        refresh(hwnd, 0);
        EnumChildWindows(hwnd, Some(refresh), 0);
    }
}
/// # Safety
/// Refresh only the calling UI thread's windows, including native tooltip popups.
pub unsafe fn refresh_thread() {
    unsafe extern "system" fn each(hwnd: HWND, _: isize) -> i32 {
        unsafe {
            refresh_tree(hwnd);
            InvalidateRect(hwnd, std::ptr::null(), 0);
            1
        }
    }
    unsafe {
        EnumThreadWindows(
            windows_sys::Win32::System::Threading::GetCurrentThreadId(),
            Some(each),
            0,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_monospace_default_can_change_back_without_changing_code_or_icon_roles() {
        assert_eq!(choose_face(Some(true), "Consolas", "Arial"), "Arial");
        assert_eq!(choose_face(Some(false), "Consolas", "Arial"), "Consolas");
        assert_eq!(
            choose_face(Some(false), "Segoe MDL2 Assets", "Arial"),
            "Segoe MDL2 Assets"
        );
    }
    #[test]
    fn system_list_has_real_families_and_special_faces_keep_their_roles() {
        let names = installed();
        assert!(!names.is_empty());
        assert!(names.iter().all(|n| valid(n) && !n.starts_with('@')));
        for face in ["Segoe MDL2 Assets", "Segoe Fluent Icons", "Consolas"] {
            assert_eq!(resolve(face), face);
        }
        assert!(!valid("bad\nfont"));
        assert!(!valid(&"a".repeat(32)));
    }
}
