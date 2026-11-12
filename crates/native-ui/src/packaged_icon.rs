//! Prefer the package's unplated taskbar asset; never edit an installed package.
use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    ptr::{null, null_mut},
    sync::Mutex,
    time::{Duration, Instant},
};
use windows::Win32::{
    Graphics::Imaging::*,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::*, Storage::Packaging::Appx::*, System::Com::*,
    UI::WindowsAndMessaging::*,
};

struct Apartment(bool);
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn roots(family: &str) -> Vec<PathBuf> {
    unsafe {
        let mut count = 0;
        let mut length = 0;
        let family = crate::wide(family);
        if GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            null_mut(),
            &mut length,
            null_mut(),
        ) != ERROR_INSUFFICIENT_BUFFER
            || count == 0
            || count > 64
            || length > 65536
        {
            return vec![];
        }
        let mut names = vec![null_mut(); count as usize];
        let mut buffer = vec![0u16; length as usize];
        if GetPackagesByPackageFamily(
            family.as_ptr(),
            &mut count,
            names.as_mut_ptr(),
            &mut length,
            buffer.as_mut_ptr(),
        ) != ERROR_SUCCESS
        {
            return vec![];
        }
        let mut result = vec![];
        for name in names.into_iter().take(count as usize) {
            let mut size = 0;
            if GetPackagePathByFullName(name, &mut size, null_mut()) != ERROR_INSUFFICIENT_BUFFER
                || size > 32768
            {
                continue;
            }
            let mut path = vec![0u16; size as usize];
            if GetPackagePathByFullName(name, &mut size, path.as_mut_ptr()) == ERROR_SUCCESS {
                result.push(PathBuf::from(String::from_utf16_lossy(
                    &path[..size.saturating_sub(1) as usize],
                )));
            }
        }
        result
    }
}

fn logo(xml: &str, app: &str) -> Option<PathBuf> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_str(xml);
    let mut selected = false;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) => {
                if e.local_name().as_ref() == b"Application" {
                    selected = e.attributes().flatten().any(|a| {
                        a.key.as_ref() == b"Id" && a.unescape_value().is_ok_and(|v| v == app)
                    });
                }
                if selected && e.local_name().as_ref() == b"VisualElements" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"Square44x44Logo" {
                            let value = PathBuf::from(a.unescape_value().ok()?.replace('\\', "/"));
                            return (!value.is_absolute()
                                && value
                                    .components()
                                    .all(|c| matches!(c, Component::Normal(_))))
                            .then_some(value);
                        }
                    }
                }
            }
            Event::End(e) if e.local_name().as_ref() == b"Application" => selected = false,
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn rank(name: &str, stem: &str, pixels: u32, light: bool) -> Option<(bool, bool, u32)> {
    let name = name.to_ascii_lowercase();
    let prefix = format!("{}.", stem.to_ascii_lowercase());
    if !name.starts_with(&prefix) || !name.ends_with(".png") {
        return None;
    }
    let qualifiers = name.strip_suffix(".png")?.strip_prefix(&prefix)?;
    let parts: Vec<_> = qualifiers.split('_').collect();
    let is_light = parts.contains(&"altform-lightunplated");
    if !is_light && !parts.contains(&"altform-unplated") {
        return None;
    }
    // Explicit high-contrast assets are reserved for their corresponding theme.
    if parts.iter().any(|p| p.starts_with("contrast-")) {
        return None;
    }
    let size = parts
        .iter()
        .find_map(|p| {
            p.strip_prefix("targetsize-")
                .and_then(|s| s.parse::<u32>().ok())
        })
        .or_else(|| {
            parts
                .iter()
                .find_map(|p| p.strip_prefix("scale-").and_then(|s| s.parse::<u32>().ok()))
                .map(|scale| 44 * scale / 100)
        })?;
    Some((is_light != light, size < pixels, size.abs_diff(pixels)))
}

type Cache = VecDeque<((String, u32, bool), Instant, Option<PathBuf>)>;
static CACHE: Mutex<Cache> = Mutex::new(VecDeque::new());

pub fn asset(id: &str, pixels: u32, light: bool) -> Option<PathBuf> {
    let key = (id.to_owned(), pixels, light);
    if let Some((_, _, path)) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, at, _)| *k == key && at.elapsed() < Duration::from_secs(60))
    {
        return path.clone();
    }
    let find = || {
        let (family, app) = id.split_once('!')?;
        for root in roots(family) {
            let manifest = root.join("AppxManifest.xml");
            if std::fs::metadata(&manifest)
                .ok()
                .is_none_or(|meta| meta.len() > 2 * 1024 * 1024)
            {
                continue;
            }
            let Some(logo) = std::fs::read_to_string(manifest)
                .ok()
                .and_then(|xml| logo(&xml, app))
            else {
                continue;
            };
            let parent = root.join(logo.parent()?);
            let stem = logo.file_stem()?.to_str()?;
            let best = std::fs::read_dir(parent)
                .ok()?
                .filter_map(Result::ok)
                .take(4096)
                .filter_map(|e| {
                    Some((
                        rank(e.file_name().to_str()?, stem, pixels, light)?,
                        e.path(),
                    ))
                })
                .min_by_key(|(rank, _)| *rank)
                .map(|(_, path)| path);
            if let Some(path) = best {
                let canonical = path.canonicalize().ok()?;
                if canonical.starts_with(root.canonicalize().ok()?) {
                    return Some(canonical);
                }
            }
        }
        None
    };
    let path = find();
    crate::diagnostics::event(
        "render",
        "icon.packaged_asset",
        serde_json::json!({"app_id":id,"pixels":pixels,"light":light,"asset":path,"fallback":path.is_none()}),
    );
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _, _)| *k != key);
    if cache.len() >= 64 {
        cache.pop_front();
    }
    cache.push_back((key, Instant::now(), path.clone()));
    path
}

unsafe fn decode(path: &Path, pixels: u32) -> Option<HICON> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let filename = crate::wide(&path.to_string_lossy());
        let decoder = factory
            .CreateDecoderFromFilename(
                windows::core::PCWSTR(filename.as_ptr()),
                None,
                windows::Win32::Foundation::GENERIC_READ,
                WICDecodeMetadataCacheOnLoad,
            )
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let mut width = 0;
        let mut height = 0;
        frame.GetSize(&mut width, &mut height).ok()?;
        if width == 0 || width != height || width > 2048 {
            return None;
        }
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&frame, pixels, pixels, WICBitmapInterpolationModeFant)
            .ok()?;
        let converted = factory.CreateFormatConverter().ok()?;
        converted
            .Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let mut data = vec![0u8; (pixels * pixels * 4) as usize];
        converted.CopyPixels(null(), pixels * 4, &mut data).ok()?;
        // Opaque artwork is not an unplated fallback, even if named as one.
        if !data.chunks_exact(4).any(|p| p[3] < 255) || !data.chunks_exact(4).any(|p| p[3] > 0) {
            return None;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = pixels as i32;
        info.bmiHeader.biHeight = -(pixels as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), bits.cast::<u8>(), data.len());
        let mask_data = vec![0u8; (pixels.div_ceil(16) * 2 * pixels) as usize];
        let mask = CreateBitmap(
            pixels as i32,
            pixels as i32,
            1,
            1,
            mask_data.as_ptr().cast(),
        );
        if mask.is_null() {
            DeleteObject(color);
            return None;
        }
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        DeleteObject(mask);
        DeleteObject(color);
        (!icon.is_null()).then_some(icon)
    }
}

pub(crate) unsafe fn load(id: &str, pixels: u32) -> Option<HICON> {
    if !(1..=256).contains(&pixels) {
        return None;
    }
    let light =
        crate::theme::Hsl::from_rgb(crate::theme::colorref(crate::theme::palette().background)).l
            > 0.5;
    let path = asset(id, pixels, light)?;
    unsafe {
        let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        if hr < 0 && hr != 0x80010106u32 as i32 {
            return None;
        }
        let _apartment = Apartment(hr >= 0);
        decode(&path, pixels)
    }
}

pub(crate) unsafe fn for_window(hwnd: HWND, pixels: u32) -> Option<HICON> {
    use windows_sys::Win32::System::Threading::*;
    unsafe fn process_app(hwnd: HWND) -> Option<String> {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut id = [0u16; 512];
            let mut length = id.len() as u32;
            let status = GetApplicationUserModelId(process, &mut length, id.as_mut_ptr());
            windows_sys::Win32::Foundation::CloseHandle(process);
            if status != ERROR_SUCCESS || length < 2 || length as usize > id.len() {
                return None;
            }
            Some(String::from_utf16_lossy(&id[..length as usize - 1]))
        }
    }
    unsafe extern "system" fn child(hwnd: HWND, data: isize) -> i32 {
        unsafe {
            if let Some(id) = process_app(hwnd) {
                *(data as *mut Option<String>) = Some(id);
                0
            } else {
                1
            }
        }
    }
    unsafe {
        let mut id = process_app(hwnd);
        if id.is_none() {
            // Windows 10's ApplicationFrameHost owns the outer HWND; the app
            // process is a child window. Avoid walking ordinary desktop windows.
            let mut class = [0u16; 64];
            let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
            if String::from_utf16_lossy(&class[..len.max(0) as usize]) == "ApplicationFrameWindow" {
                EnumChildWindows(hwnd, Some(child), (&mut id as *mut Option<String>) as isize);
            }
        }
        load(id.as_deref()?, pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chooses_the_application_logo_and_rejects_escape_paths() {
        let xml = r#"<Package><Application Id="Other"><v:VisualElements Square44x44Logo="Other.png"/></Application><Application Id="App"><v:VisualElements Square44x44Logo="Assets\Logo.png"/></Application></Package>"#;
        assert_eq!(logo(xml, "App"), Some(PathBuf::from("Assets/Logo.png")));
        assert!(logo(&xml.replace("Assets\\Logo.png", "../Logo.png"), "App").is_none());
    }
    #[test]
    fn unplated_selection_matches_theme_and_prefers_downscaling_over_upscaling() {
        let score = |name| rank(name, "Logo", 24, false);
        assert!(score("Logo.png").is_none());
        assert!(score("Logo.targetsize-24.png").is_none());
        assert!(score("Other.targetsize-24_altform-unplated.png").is_none());
        assert!(
            score("Logo.targetsize-24_altform-unplated.png")
                < score("Logo.targetsize-32_altform-unplated.png")
        );
        assert!(
            score("Logo.targetsize-32_altform-unplated.png")
                < score("Logo.targetsize-16_altform-unplated.png")
        );
        assert!(
            score("Logo.targetsize-24_altform-unplated.png")
                < score("Logo.targetsize-24_altform-lightunplated.png")
        );
    }
}
