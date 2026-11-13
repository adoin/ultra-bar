use std::{
    cell::RefCell,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    rc::Rc,
};
use windows_sys::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{Com::*, LibraryLoader::*, Threading::*},
        UI::{Controls::*, Shell::*, WindowsAndMessaging::*},
    },
    core::BOOL,
};
pub mod diagnostics;
pub mod editor_bridge;
pub mod fonts;
pub mod form;
pub mod geometry;
pub mod input;
pub mod packaged_icon;
pub mod popover;
pub mod settings_bridge;
pub mod text;
pub mod theme;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Owned copies: windows are free to replace or destroy their original icons.
#[derive(Debug)]
pub struct AppIcon {
    original: usize,
    pub handle: HICON,
}
impl PartialEq for AppIcon {
    fn eq(&self, other: &Self) -> bool {
        self.original == other.original
    }
}
impl Drop for AppIcon {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                DestroyIcon(self.handle);
            }
        }
    }
}
impl AppIcon {
    /// # Safety
    /// Call from a Windows UI thread; the returned icon owns its native handle.
    pub unsafe fn from_file(path: &str, dpi: i32, logical_size: i32) -> Self {
        unsafe {
            if let Some(id) = path.strip_prefix("shell:AppsFolder\\") {
                if let Some(handle) =
                    packaged_icon::load(id, (logical_size * dpi / 96).max(1) as u32)
                {
                    return Self {
                        original: 0,
                        handle,
                    };
                }
                let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
                let mut pidl = null_mut();
                let mut info: SHFILEINFOW = zeroed();
                if SHParseDisplayName(wide(path).as_ptr(), null_mut(), &mut pidl, 0, null_mut())
                    >= 0
                    && !pidl.is_null()
                {
                    SHGetFileInfoW(
                        pidl.cast(),
                        0,
                        &mut info,
                        size_of::<SHFILEINFOW>() as u32,
                        SHGFI_PIDL
                            | SHGFI_ICON
                            | if logical_size * dpi / 96 <= 16 {
                                SHGFI_SMALLICON
                            } else {
                                SHGFI_LARGEICON
                            },
                    );
                    CoTaskMemFree(pidl.cast());
                }
                if hr >= 0 {
                    CoUninitialize();
                }
                if !info.hIcon.is_null() {
                    return Self {
                        original: 0,
                        handle: info.hIcon,
                    };
                }
            }
            Self::from_resource(path, 0, dpi, logical_size)
        }
    }
    /// # Safety
    /// Call from a Windows UI thread; the resource is extracted without link overlays.
    pub unsafe fn from_resource(path: &str, index: i32, dpi: i32, logical_size: i32) -> Self {
        unsafe {
            let mut icon = null_mut();
            if !path.to_lowercase().ends_with(".lnk") {
                SHDefExtractIconW(
                    wide(path).as_ptr(),
                    index,
                    0,
                    &mut icon,
                    null_mut(),
                    (logical_size * dpi / 96) as u32,
                );
            }
            if icon.is_null() {
                let mut info: SHFILEINFOW = zeroed();
                SHGetFileInfoW(
                    wide(path).as_ptr(),
                    0,
                    &mut info,
                    size_of::<SHFILEINFOW>() as u32,
                    SHGFI_ICON
                        | if logical_size <= 16 {
                            SHGFI_SMALLICON
                        } else {
                            SHGFI_LARGEICON
                        },
                );
                icon = info.hIcon;
            }
            if icon.is_null() {
                icon = CopyIcon(LoadIconW(null_mut(), IDI_APPLICATION));
            }
            Self {
                original: 0,
                handle: icon,
            }
        }
    }
    /// # Safety
    /// Call from a Windows UI thread with a live window whose icon can be copied.
    pub unsafe fn read(hwnd: HWND, dpi: i32, logical_size: i32) -> Self {
        unsafe {
            if let Some(handle) =
                packaged_icon::for_window(hwnd, (logical_size * dpi / 96).max(1) as u32)
            {
                return Self {
                    original: 0,
                    handle,
                };
            }
            let mut original = 0;
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                if logical_size <= 16 {
                    ICON_SMALL2
                } else {
                    ICON_BIG
                } as _,
                dpi as _,
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                8,
                &mut original,
            );
            if original == 0 {
                original = GetClassLongPtrW(
                    hwnd,
                    if logical_size <= 16 {
                        GCLP_HICONSM
                    } else {
                        GCLP_HICON
                    },
                );
            }
            if original == 0 {
                original = GetClassLongPtrW(
                    hwnd,
                    if logical_size <= 16 {
                        GCLP_HICON
                    } else {
                        GCLP_HICONSM
                    },
                );
            }
            let pixels = logical_size * dpi / 96;
            if original != 0 && icon_width(original as _) >= pixels {
                return Self {
                    original,
                    handle: CopyIcon(original as _),
                };
            }
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if !process.is_null() {
                let mut path = [0u16; 32768];
                let mut length = path.len() as u32;
                let found =
                    QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
                CloseHandle(process);
                if found {
                    let mut extracted = null_mut();
                    if SHDefExtractIconW(
                        path.as_ptr(),
                        0,
                        0,
                        &mut extracted,
                        null_mut(),
                        pixels as u32,
                    ) >= 0
                        && !extracted.is_null()
                    {
                        return Self {
                            original: pid as usize,
                            handle: extracted,
                        };
                    }
                    let mut info: SHFILEINFOW = zeroed();
                    if SHGetFileInfoW(
                        path.as_ptr(),
                        0,
                        &mut info,
                        size_of::<SHFILEINFOW>() as _,
                        SHGFI_ICON
                            | if logical_size <= 16 {
                                SHGFI_SMALLICON
                            } else {
                                SHGFI_LARGEICON
                            },
                    ) != 0
                    {
                        // Stable identity for the executable fallback; the owned handle changes per read.
                        return Self {
                            original: pid as usize,
                            handle: info.hIcon,
                        };
                    }
                }
            }
            if original != 0 {
                return Self {
                    original,
                    handle: CopyIcon(original as _),
                };
            }
            let fallback = LoadIconW(null_mut(), IDI_APPLICATION);
            Self {
                original: fallback as usize,
                handle: CopyIcon(fallback),
            }
        }
    }
}

unsafe fn icon_width(icon: HICON) -> i32 {
    unsafe {
        let mut info: ICONINFO = zeroed();
        if GetIconInfo(icon, &mut info) == 0 {
            return 0;
        }
        let mut bitmap: BITMAP = zeroed();
        let handle = if info.hbmColor.is_null() {
            info.hbmMask
        } else {
            info.hbmColor
        };
        GetObjectW(
            handle,
            size_of::<BITMAP>() as _,
            (&mut bitmap as *mut BITMAP).cast(),
        );
        if !info.hbmColor.is_null() {
            crate::fonts::delete_object(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            crate::fonts::delete_object(info.hbmMask);
        }
        bitmap.bmWidth
    }
}

/// Accent blur remains necessary on Windows 10; resolve it at runtime and fail
/// back to an opaque surface if the compositor does not support it.
/// # Safety
/// The caller must own the live window and invoke this on its UI thread.
pub unsafe fn enable_glass(hwnd: HWND) -> bool {
    #[repr(C)]
    struct Accent {
        state: i32,
        flags: i32,
        tint: u32,
        animation: i32,
    }
    #[repr(C)]
    struct Data {
        attribute: i32,
        data: *mut std::ffi::c_void,
        size: usize,
    }
    unsafe {
        let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
        let Some(address) =
            GetProcAddress(user32, c"SetWindowCompositionAttribute".as_ptr().cast())
        else {
            return false;
        };
        let set: unsafe extern "system" fn(HWND, *mut Data) -> BOOL = std::mem::transmute(address);
        let mut accent = Accent {
            state: 3,
            flags: 2,
            tint: 0,
            animation: 0,
        };
        let mut data = Data {
            attribute: 19,
            data: (&mut accent as *mut Accent).cast(),
            size: size_of::<Accent>(),
        };
        set(hwnd, &mut data) != 0
    }
}

// COLORREF is BGR while DIB pixels below are 0xAARRGGBB.
pub const BACKGROUND_BRUSH: u32 = 0x002c241c;
pub const HOVER_BRUSH: u32 = 0x00443a30;

pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    pixels: *mut u32,
    width: i32,
    height: i32,
    effects: Rc<RefCell<Vec<u32>>>,
    cutouts: Rc<RefCell<Vec<u8>>>,
    foreground: Rc<RefCell<Vec<bool>>>,
}
impl Surface {
    /// Blit a GDI-painted client to a constant-alpha layered window. GDI clears
    /// DIB alpha during fills; leaving those zeros lets blur composition treat
    /// the client as a transparent hole after the desktop underneath changes.
    /// # Safety
    /// Use only for GDI/constant-alpha clients, not per-pixel `present` windows.
    pub unsafe fn blit_opaque(&self, target: HDC) {
        unsafe {
            GdiFlush();
            for pixel in
                std::slice::from_raw_parts_mut(self.pixels, (self.width * self.height) as usize)
            {
                *pixel |= 0xff000000;
            }
            AlphaBlend(
                target,
                0,
                0,
                self.width,
                self.height,
                self.dc,
                0,
                0,
                self.width,
                self.height,
                BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                },
            );
        }
    }
    /// Clear only an antialiased rounded silhouette; leave its corners and surrounding padding intact.
    /// # Safety
    /// Call on the rendering thread before foreground paint.
    pub unsafe fn clear_rounded(&self, rect: &RECT, radius: f64) {
        let w = (rect.right - rect.left) as f64;
        let h = (rect.bottom - rect.top) as f64;
        if w <= 0. || h <= 0. {
            return;
        }
        let r = radius.clamp(0., w.min(h) / 2.);
        let mut mask = self.cutouts.borrow_mut();
        if mask.is_empty() {
            mask.resize((self.width * self.height) as usize, 0);
        }
        for y in rect.top.max(0)..rect.bottom.min(self.height) {
            for x in rect.left.max(0)..rect.right.min(self.width) {
                let qx = (x as f64 + 0.5 - rect.left as f64 - w / 2.).abs() - (w / 2. - r);
                let qy = (y as f64 + 0.5 - rect.top as f64 - h / 2.).abs() - (h / 2. - r);
                let distance = qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - r;
                let coverage = ((0.5 - distance).clamp(0., 1.) * 255.).round() as u8;
                let index = (y * self.width + x) as usize;
                mask[index] = mask[index].max(coverage);
            }
        }
    }
    /// Clear the actual layered-window backing pixels, including any prior effect.
    /// # Safety
    /// Call on the rendering thread before drawing foreground content.
    pub unsafe fn clear_region(&self, rect: &RECT) {
        unsafe {
            GdiFlush();
            let mut effects = self.effects.borrow_mut();
            let mut foreground = self.foreground.borrow_mut();
            if effects.is_empty() {
                effects.resize((self.width * self.height) as usize, 0);
            }
            for y in rect.top.max(0)..rect.bottom.min(self.height) {
                for x in rect.left.max(0)..rect.right.min(self.width) {
                    let i = (y * self.width + x) as usize;
                    // Nonzero metadata distinguishes explicit alpha=0 from an untouched GDI pixel.
                    *self.pixels.add(i) = 1;
                    effects[i] = 1;
                    if let Some(value) = foreground.get_mut(i) {
                        *value = false;
                    }
                }
            }
        }
    }
    /// # Safety
    /// Call on this surface's UI thread, before drawing foreground text/icons.
    /// Pixels are already composed over the theme background, straight-alpha RGB.
    /// Existing and later GDI foreground paint remains intact and opaque.
    pub unsafe fn effect(&self, left: i32, top: i32, width: i32, height: i32, pixels: &[u32]) {
        unsafe {
            if width <= 0
                || height <= 0
                || width
                    .checked_mul(height)
                    .is_none_or(|length| length as usize != pixels.len())
            {
                return;
            }
            GdiFlush();
            let background = theme::colorref(theme::palette().background);
            let mut effects = self.effects.borrow_mut();
            let foreground = self.foreground.borrow();
            if effects.is_empty() {
                effects.resize(self.width as usize * self.height as usize, 0);
            }
            for y in top.max(0)..top.saturating_add(height).min(self.height) {
                for x in left.max(0)..left.saturating_add(width).min(self.width) {
                    let source = pixels[((y - top) * width + x - left) as usize];
                    if source >> 24 == 0 {
                        continue;
                    }
                    let index = (y * self.width + x) as usize;
                    if foreground.get(index).copied().unwrap_or(false) {
                        continue;
                    }
                    let current = *self.pixels.add(index);
                    if current != background
                        && (effects[index] == 0 || current != (effects[index] & 0xffffff))
                    {
                        continue;
                    }
                    *self.pixels.add(index) = source & 0xffffff;
                    effects[index] = source;
                }
            }
        }
    }
    /// # Safety
    /// Use on this surface's UI thread. RGB is written into the owned DIB.
    pub unsafe fn overlay_effect(
        &self,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        pixels: &[u32],
        glass: bool,
    ) {
        unsafe {
            if width <= 0
                || height <= 0
                || width
                    .checked_mul(height)
                    .is_none_or(|n| n as usize != pixels.len())
            {
                return;
            }
            GdiFlush();
            let background = theme::colorref(theme::palette().background);
            let hover = theme::colorref(theme::palette().hover);
            let mut effects = self.effects.borrow_mut();
            let mut foreground = self.foreground.borrow_mut();
            if effects.is_empty() {
                effects.resize((self.width * self.height) as usize, 0);
            }
            if foreground.is_empty() {
                foreground.resize((self.width * self.height) as usize, false);
            }
            for y in top.max(0)..(top + height).min(self.height) {
                for x in left.max(0)..(left + width).min(self.width) {
                    let source = pixels[((y - top) * width + x - left) as usize];
                    let sa = (source >> 24) as f64 / 255.;
                    if sa == 0. {
                        continue;
                    }
                    let i = (y * self.width + x) as usize;
                    let old = *self.pixels.add(i);
                    let da = (premultiply_effect(old, effects[i], glass, background, hover) >> 24)
                        as f64
                        / 255.;
                    let alpha = sa + da * (1. - sa);
                    let channel = |shift: u32| {
                        ((((source >> shift) & 255) as f64 * sa
                            + ((old >> shift) & 255) as f64 * da * (1. - sa))
                            / alpha)
                            .round() as u32
                    };
                    let rgb = (channel(16) << 16) | (channel(8) << 8) | channel(0);
                    *self.pixels.add(i) = rgb;
                    effects[i] = ((alpha * 255.).round() as u32) << 24 | rgb;
                    foreground[i] = true;
                }
            }
        }
    }
    /// # Safety
    /// Use on this surface's UI thread. RGB is written into the owned DIB.
    pub unsafe fn pixel(&self, x: i32, y: i32, rgb: u32) {
        unsafe {
            if x >= 0 && y >= 0 && x < self.width && y < self.height {
                *self.pixels.add((y * self.width + x) as usize) = rgb & 0xffffff;
            }
        }
    }
    /// Write an offscreen application rendering for layout verification.
    /// Copy straight RGB pixels for upload to a compositor-owned texture.
    /// # Safety
    /// Call on the rendering thread after GDI has finished drawing.
    pub unsafe fn copy_pixels(&self) -> Vec<u32> {
        unsafe {
            GdiFlush();
            std::slice::from_raw_parts(self.pixels, (self.width * self.height) as usize).to_vec()
        }
    }
    /// Copy the same straight-alpha image that `present` would compose, without mutating the DIB.
    /// # Safety
    /// Use on the rendering thread after foreground drawing is complete.
    pub unsafe fn straight_pixels(&self, glass: bool) -> Vec<u32> {
        unsafe {
            GdiFlush();
            let background = theme::colorref(theme::palette().background);
            let hover = theme::colorref(theme::palette().hover);
            let effects = self.effects.borrow();
            let cutouts = self.cutouts.borrow();
            std::slice::from_raw_parts(self.pixels, (self.width * self.height) as usize)
                .iter()
                .enumerate()
                .map(|(i, pixel)| {
                    let effect = effects.get(i).copied().unwrap_or(0);
                    let mut rgba = premultiply_effect(*pixel, effect, glass, background, hover);
                    if *pixel == background && effect == 0 {
                        rgba = apply_cutout(rgba, cutouts.get(i).copied().unwrap_or(0));
                    }
                    let alpha = rgba & 0xff000000;
                    if alpha == 0 {
                        0
                    } else {
                        alpha | pixel & 0xffffff
                    }
                })
                .collect()
        }
    }
    /// Write an offscreen application rendering for layout verification.
    /// # Safety
    /// Call on the surface's rendering thread after drawing has completed.
    pub unsafe fn save_bmp(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        unsafe {
            GdiFlush();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(path)?;
            let bytes = (self.width * self.height * 4) as u32;
            file.write_all(b"BM")?;
            file.write_all(&(54 + bytes).to_le_bytes())?;
            file.write_all(&[0; 4])?;
            file.write_all(&54u32.to_le_bytes())?;
            file.write_all(&40u32.to_le_bytes())?;
            file.write_all(&self.width.to_le_bytes())?;
            file.write_all(&(-self.height).to_le_bytes())?;
            file.write_all(&1u16.to_le_bytes())?;
            file.write_all(&32u16.to_le_bytes())?;
            file.write_all(&0u32.to_le_bytes())?;
            file.write_all(&bytes.to_le_bytes())?;
            file.write_all(&[0; 16])?;
            file.write_all(std::slice::from_raw_parts(
                self.pixels.cast::<u8>(),
                bytes as usize,
            ))
        }
    }
    /// Rasterize on physical pixel boundaries. No font glyph, bitmap scaling or
    /// filtered intermediate surface is involved in the Windows mark.
    /// # Safety
    /// Use exclusively on the thread that owns this surface's device context.
    pub unsafe fn windows_logo(&self, rect: &RECT, dpi: i32, _win11: bool, logical_size: i32) {
        unsafe {
            GdiFlush();
            let size = ((logical_size * dpi + 48) / 96 / 2 * 2).max(8);
            let gap = ((2 * dpi + 96) / 192 * 2).max(2);
            let first = (size - gap) / 2;
            let left = (rect.left + rect.right - size) / 2;
            let top = (rect.top + rect.bottom - size) / 2;
            for y in 0..size {
                for x in 0..size {
                    if (first..first + gap).contains(&x) || (first..first + gap).contains(&y) {
                        continue;
                    }
                    let (px, py) = (left + x, top + y);
                    if (0..self.width).contains(&px) && (0..self.height).contains(&py) {
                        *self.pixels.add((py * self.width + px) as usize) = 0x00f4f7fb;
                    }
                }
            }
        }
    }
    /// # Safety
    /// The resulting surface must be rendered and dropped on this UI thread.
    pub unsafe fn new(width: i32, height: i32) -> Option<Self> {
        width.checked_mul(height)?.checked_mul(4)?;
        if width <= 0 || height <= 0 {
            return None;
        }
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as _,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            };
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old_bitmap = SelectObject(dc, bitmap);
            let surface = Self {
                dc,
                bitmap,
                old_bitmap,
                pixels: bits.cast(),
                width,
                height,
                effects: Rc::new(RefCell::new(Vec::new())),
                cutouts: Rc::new(RefCell::new(Vec::new())),
                foreground: Rc::new(RefCell::new(Vec::new())),
            };
            text::register(
                surface.dc,
                surface.pixels,
                width,
                height,
                surface.effects.clone(),
                surface.cutouts.clone(),
                surface.foreground.clone(),
            );
            Some(surface)
        }
    }
    /// # Safety
    /// The destination must be a live layered window on the surface's UI thread.
    pub unsafe fn present(&self, hwnd: HWND, glass: bool) {
        unsafe {
            GdiFlush();
            let palette = theme::palette();
            let background = theme::colorref(palette.background);
            let hover = theme::colorref(palette.hover);
            let effects = self.effects.borrow();
            let cutouts = self.cutouts.borrow();
            for (index, pixel) in std::slice::from_raw_parts_mut(
                self.pixels,
                self.width as usize * self.height as usize,
            )
            .iter_mut()
            .enumerate()
            {
                let backing = *pixel == background && effects.get(index).copied().unwrap_or(0) == 0;
                *pixel = premultiply_effect(
                    *pixel,
                    effects.get(index).copied().unwrap_or(0),
                    glass,
                    background,
                    hover,
                );
                if backing {
                    *pixel = apply_cutout(*pixel, cutouts.get(index).copied().unwrap_or(0));
                }
            }
            let size = SIZE {
                cx: self.width,
                cy: self.height,
            };
            let origin = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as _,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as _,
            };
            if UpdateLayeredWindow(
                hwnd,
                null_mut(),
                null(),
                &size,
                self.dc,
                &origin,
                0,
                &blend,
                ULW_ALPHA,
            ) == 0
            {
                eprintln!("[shell] layered surface update failed: {}", GetLastError());
            }
        }
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            text::unregister(self.dc);
            SelectObject(self.dc, self.old_bitmap);
            crate::fonts::delete_object(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

fn premultiply_effect(pixel: u32, effect: u32, glass: bool, background: u32, hover: u32) -> u32 {
    if effect == 1 && pixel == 1 {
        0
    } else if glass && effect >> 24 != 0 && pixel == (effect & 0xffffff) {
        premultiply_alpha(pixel, effect >> 24)
    } else {
        premultiply(pixel, glass, background, hover)
    }
}
fn premultiply(pixel: u32, glass: bool, background: u32, hover: u32) -> u32 {
    let rgb = pixel & 0x00ff_ffff;
    let alpha = if !glass {
        255
    } else if rgb == background || rgb == 0x001c242c {
        148
    } else if rgb == hover || rgb == 0x00303a44 {
        178
    } else {
        255
    };
    premultiply_alpha(rgb, alpha)
}
fn premultiply_alpha(rgb: u32, alpha: u32) -> u32 {
    let r = ((rgb >> 16) & 255) * alpha / 255;
    let g = ((rgb >> 8) & 255) * alpha / 255;
    let b = (rgb & 255) * alpha / 255;
    (alpha << 24) | (r << 16) | (g << 8) | b
}
fn apply_cutout(pixel: u32, coverage: u8) -> u32 {
    let keep = 255 - coverage as u32;
    let channel = |shift: u32| ((pixel >> shift) & 255u32) * keep / 255;
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

#[derive(PartialEq)]
pub struct Tip {
    pub bounds: (i32, i32, i32, i32),
    pub text: String,
}
pub struct Tooltips {
    window: HWND,
    owner: HWND,
    tips: Vec<Tip>,
    strings: Vec<Vec<u16>>,
}
impl Tooltips {
    /// # Safety
    /// The owner must remain live during use, on the current UI thread.
    pub unsafe fn new(owner: HWND) -> Self {
        unsafe {
            InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as _,
                dwICC: ICC_WIN95_CLASSES,
            });
            let window = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX | TTS_NOANIMATE | TTS_NOFADE,
                0,
                0,
                0,
                0,
                owner,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            SendMessageW(window, TTM_SETMAXTIPWIDTH, 0, 640);
            SendMessageW(window, TTM_SETDELAYTIME, TTDT_INITIAL as _, 450);
            SendMessageW(window, TTM_SETDELAYTIME, TTDT_AUTOPOP as _, 15000);
            crate::fonts::refresh_tree(window);
            Self {
                window,
                owner,
                tips: vec![],
                strings: vec![],
            }
        }
    }
    /// # Safety
    /// Call on the owner's UI thread while its window and tooltip are live.
    pub unsafe fn sync(&mut self, mut tips: Vec<Tip>) {
        tips.retain(|tip| !tip.text.trim().is_empty());
        if self.tips == tips {
            return;
        }
        unsafe {
            for index in tips.len()..self.tips.len() {
                // V2 size also works without a Comctl32 v6 activation context (native plugins).
                let info = TTTOOLINFOW {
                    cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as _,
                    hwnd: self.owner,
                    uId: index + 1,
                    ..zeroed()
                };
                SendMessageW(
                    self.window,
                    TTM_DELTOOLW,
                    0,
                    (&info as *const TTTOOLINFOW) as _,
                );
            }
            self.strings.truncate(tips.len());
            self.strings.resize_with(tips.len(), Vec::new);
            for (index, tip) in tips.iter().enumerate() {
                let previous = self.tips.get(index);
                if previous == Some(tip) {
                    continue;
                }
                let changed_text = previous.is_none_or(|old| old.text != tip.text);
                // Retain the old allocation until the control has accepted its new pointer.
                let _old = changed_text
                    .then(|| std::mem::replace(&mut self.strings[index], wide(&tip.text)));
                let (left, top, right, bottom) = tip.bounds;
                let info = TTTOOLINFOW {
                    cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as _,
                    hwnd: self.owner,
                    uId: index + 1,
                    uFlags: TTF_SUBCLASS | TTF_TRANSPARENT,
                    rect: RECT {
                        left,
                        top,
                        right,
                        bottom,
                    },
                    lpszText: self.strings[index].as_mut_ptr(),
                    ..zeroed()
                };
                if let Some(previous) = previous {
                    if previous.bounds != tip.bounds {
                        SendMessageW(
                            self.window,
                            TTM_NEWTOOLRECTW,
                            0,
                            (&info as *const TTTOOLINFOW) as _,
                        );
                    }
                    if changed_text {
                        SendMessageW(
                            self.window,
                            TTM_UPDATETIPTEXTW,
                            0,
                            (&info as *const TTTOOLINFOW) as _,
                        );
                    }
                } else {
                    SendMessageW(
                        self.window,
                        TTM_ADDTOOLW,
                        0,
                        (&info as *const TTTOOLINFOW) as _,
                    );
                }
            }
            self.tips = tips;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constant_alpha_gdi_clients_keep_their_own_colors_on_black_and_white_backdrops() {
        unsafe {
            let source = Surface::new(16, 12).unwrap();
            let target = Surface::new(16, 12).unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: 16,
                bottom: 12,
            };
            let brush = CreateSolidBrush(0x00352a21);
            FillRect(source.dc, &rect, brush);
            crate::fonts::delete_object(brush);
            let inner = RECT {
                left: 3,
                top: 3,
                right: 12,
                bottom: 9,
            };
            FillRect(source.dc, &inner, GetStockObject(WHITE_BRUSH));
            let before = source.copy_pixels();
            for backdrop in [BLACK_BRUSH, WHITE_BRUSH] {
                FillRect(target.dc, &rect, GetStockObject(backdrop));
                source.blit_opaque(target.dc);
                let pixels = target.copy_pixels();
                assert!(pixels.iter().all(|p| p >> 24 == 255));
                for (p, original) in pixels.iter().zip(&before) {
                    assert_eq!(p & 0xffffff, original & 0xffffff);
                }
            }
        }
    }
    #[test]
    fn whitespace_only_tooltips_do_not_create_a_narrow_empty_popup() {
        unsafe {
            let owner = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                100,
                40,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            let mut tips = Tooltips::new(owner);
            tips.sync(vec![
                Tip {
                    bounds: (0, 0, 20, 20),
                    text: "\n \t\n".into(),
                },
                Tip {
                    bounds: (20, 0, 60, 20),
                    text: "2026-11-11 星期三".into(),
                },
            ]);
            assert_eq!(SendMessageW(tips.window, TTM_GETTOOLCOUNT, 0, 0), 1);
            DestroyWindow(owner);
        }
    }
    #[test]
    fn changing_a_live_tooltip_does_not_delete_its_active_tool() {
        unsafe {
            let owner = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                null(),
                WS_POPUP,
                -32000,
                -32000,
                100,
                50,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            assert!(!owner.is_null());
            let mut tips = Tooltips::new(owner);
            tips.sync(vec![Tip {
                bounds: (0, 0, 32, 32),
                text: "下载速度：0 B/s\n上传速度：0 B/s".into(),
            }]);
            let tool = TTTOOLINFOW {
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as _,
                hwnd: owner,
                uId: 1,
                uFlags: TTF_TRACK | TTF_ABSOLUTE,
                rect: RECT {
                    left: 0,
                    top: 0,
                    right: 32,
                    bottom: 32,
                },
                lpszText: tips.strings[0].as_mut_ptr(),
                ..zeroed()
            };
            SendMessageW(
                tips.window,
                TTM_SETTOOLINFOW,
                0,
                (&tool as *const TTTOOLINFOW) as _,
            );
            SendMessageW(
                tips.window,
                TTM_TRACKACTIVATE,
                1,
                (&tool as *const TTTOOLINFOW) as _,
            );
            let mut active: TTTOOLINFOW = zeroed();
            active.cbSize = std::mem::offset_of!(TTTOOLINFOW, lpReserved) as _;
            assert_ne!(
                SendMessageW(
                    tips.window,
                    TTM_GETCURRENTTOOLW,
                    0,
                    (&mut active as *mut TTTOOLINFOW) as _
                ),
                0
            );
            tips.sync(vec![Tip {
                bounds: (0, 0, 32, 32),
                text: "下载速度：3 MB/s\n上传速度：12 KB/s".into(),
            }]);
            assert_ne!(
                SendMessageW(
                    tips.window,
                    TTM_GETCURRENTTOOLW,
                    0,
                    (&mut active as *mut TTTOOLINFOW) as _
                ),
                0
            );
            assert_eq!(active.uId, 1);
            assert_eq!(SendMessageW(tips.window, TTM_GETTOOLCOUNT, 0, 0), 1);
            let mut text = [0u16; 512];
            let info = TTTOOLINFOW {
                cbSize: std::mem::offset_of!(TTTOOLINFOW, lpReserved) as _,
                hwnd: owner,
                uId: 1,
                lpszText: text.as_mut_ptr(),
                ..zeroed()
            };
            SendMessageW(
                tips.window,
                TTM_GETTEXTW,
                text.len(),
                (&info as *const TTTOOLINFOW) as _,
            );
            assert!(
                String::from_utf16_lossy(&text).starts_with("下载速度：3 MB/s\n上传速度：12 KB/s")
            );
            DestroyWindow(owner);
        }
    }
    #[test]
    fn overlay_can_cover_an_icon_and_is_protected_from_later_background_shadows() {
        unsafe {
            let surface = Surface::new(2, 2).unwrap();
            surface.pixel(0, 0, 0xff0000);
            surface.overlay_effect(0, 0, 1, 1, &[0x80ffffff], true);
            assert_eq!(surface.copy_pixels()[0], 0xff8080);
            surface.effect(0, 0, 2, 2, &[0xff003300; 4]);
            assert_eq!(surface.copy_pixels()[0], 0xff8080);
        }
    }
    #[test]
    fn directwrite_edges_keep_fractional_alpha_and_survive_later_shadows() {
        unsafe {
            let surface = Surface::new(140, 32).unwrap();
            let rect = RECT {
                left: 0,
                top: 0,
                right: 140,
                bottom: 32,
            };
            FillRect(surface.dc, &rect, theme::background_brush());
            SetBkMode(surface.dc, TRANSPARENT as _);
            SetTextColor(surface.dc, 0xffffff);
            let font = crate::fonts::create_font(
                -13,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                0,
                0,
                ANTIALIASED_QUALITY as _,
                0,
                wide("Segoe UI").as_ptr(),
            );
            let old = SelectObject(surface.dc, font);
            let mut text_rect = rect;
            text::draw_text(
                surface.dc,
                wide("主色 Codex 35%").as_ptr(),
                -1,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            let pixels = surface.copy_pixels();
            assert!(surface.effects.borrow().iter().any(|p| {
                let a = p >> 24;
                a > 148 && a < 255
            }));
            surface.effect(0, 0, 140, 32, &vec![0xc8324050; 140 * 32]);
            let after = surface.copy_pixels();
            for (i, is_text) in surface.foreground.borrow().iter().enumerate() {
                if *is_text {
                    assert_eq!(pixels[i], after[i]);
                }
            }
            SelectObject(surface.dc, old);
            crate::fonts::delete_object(font);
        }
    }
    #[test]
    fn rounded_cutout_leaves_padding_and_corners_intact() {
        unsafe {
            let surface = Surface::new(100, 40).unwrap();
            surface.clear_rounded(
                &RECT {
                    left: 10,
                    top: 4,
                    right: 90,
                    bottom: 36,
                },
                16.,
            );
            let mask = surface.cutouts.borrow();
            assert_eq!(mask[4 * 100 + 10], 0);
            assert_eq!(mask[20 * 100 + 50], 255);
            assert_eq!(mask[50], 0);
            assert!(mask.iter().any(|v| *v > 0 && *v < 255));
            assert_eq!(apply_cutout(0xff204060, 0), 0xff204060);
            assert_eq!(apply_cutout(0xff204060, 255), 0);
        }
    }
    #[test]
    fn windows_mark_has_crisp_physical_pixels_at_common_dpi_scales() {
        unsafe {
            for dpi in [96, 120, 144, 168, 192] {
                for logical_size in [16, 20] {
                    let surface = Surface::new(64, 64).unwrap();
                    std::slice::from_raw_parts_mut(surface.pixels, 64 * 64).fill(0);
                    surface.windows_logo(
                        &RECT {
                            left: 0,
                            top: 0,
                            right: 64,
                            bottom: 64,
                        },
                        dpi,
                        true,
                        logical_size,
                    );
                    let pixels = std::slice::from_raw_parts(surface.pixels, 64 * 64);
                    assert!(pixels.iter().any(|pixel| *pixel == 0x00f4f7fb));
                    assert!(
                        pixels
                            .iter()
                            .all(|pixel| *pixel == 0 || *pixel == 0x00f4f7fb)
                    );
                    let rows: Vec<_> = pixels
                        .chunks_exact(64)
                        .map(|row| row.iter().filter(|pixel| **pixel != 0).count())
                        .filter(|count| *count > 0)
                        .collect();
                    assert!(rows.iter().all(|count| *count == rows[0]));
                    let columns: Vec<_> = (0..64)
                        .map(|x| (0..64).filter(|y| pixels[y * 64 + x] != 0).count())
                        .filter(|count| *count > 0)
                        .collect();
                    assert!(columns.iter().all(|count| *count == columns[0]));
                }
            }
        }
    }
    #[test]
    fn glass_keeps_text_opaque_and_background_premultiplied() {
        let p = theme::derive(theme::DEFAULT_ACCENT);
        let background = theme::colorref(p.background);
        let hover = theme::colorref(p.hover);
        assert_eq!(premultiply(0x00ffffff, true, background, hover), 0xffffffff);
        let pixel = premultiply(background, true, background, hover);
        assert_eq!(pixel >> 24, 148);
        for shift in [0, 8, 16] {
            assert!((pixel >> shift) & 255 <= 148);
        }
        assert_eq!(
            premultiply(background, false, background, hover),
            0xff000000 | background
        );
    }
    #[test]
    fn relief_opacity_does_not_dim_foreground_text_or_icon_pixels() {
        let p = theme::derive(theme::DEFAULT_ACCENT);
        let background = theme::colorref(p.background);
        let hover = theme::colorref(p.hover);
        let effect = 0xb4324050;
        assert_eq!(
            premultiply_effect(0x00324050, effect, true, background, hover) >> 24,
            0xb4
        );
        assert_eq!(
            premultiply_effect(0x00ffffff, effect, true, background, hover),
            0xffffffff
        );
        assert_eq!(
            premultiply_effect(0xff324050, effect, true, background, hover),
            0xff324050
        );
        assert_eq!(
            premultiply_effect(0x00324050, effect, false, background, hover),
            0xff324050
        );
    }
    #[test]
    fn explicit_transparency_preserves_zero_alpha_without_erasing_foreground() {
        for glass in [false, true] {
            assert_eq!(premultiply_effect(1, 1, glass, 0x102030, 0x203040), 0);
            assert_eq!(
                premultiply_effect(0xffffff, 1, glass, 0x102030, 0x203040),
                0xffffffff
            );
        }
    }
    #[test]
    fn overlapping_relief_tiles_do_not_overwrite_an_existing_icon() {
        unsafe {
            let surface = Surface::new(8, 6).unwrap();
            let background = theme::background_brush();
            let rect = RECT {
                left: 0,
                top: 0,
                right: 8,
                bottom: 6,
            };
            FillRect(surface.dc, &rect, background);
            surface.effect(0, 0, 5, 5, &[0xc8324050; 25]);
            SetPixel(surface.dc, 3, 2, 0xffffff);
            surface.effect(2, 0, 5, 5, &[0xb4283440; 25]);
            GdiFlush();
            assert_eq!(GetPixel(surface.dc, 3, 2), 0xffffff);
            assert_eq!(GetPixel(surface.dc, 2, 2), theme::colorref(0x283440));
        }
    }
}
