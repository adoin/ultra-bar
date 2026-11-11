//! Shared native theme contract. Stored colours use RGB; palette fields use GDI COLORREF.
use std::{
    cell::RefCell,
    sync::{OnceLock, RwLock},
};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
pub const DEFAULT_ACCENT: u32 = 0x69b4ff;
#[derive(Clone, Copy, Debug)]
pub struct Hsl {
    pub h: f64,
    pub s: f64,
    pub l: f64,
}
impl Hsl {
    pub fn from_rgb(rgb: u32) -> Self {
        let r = ((rgb >> 16) & 255) as f64 / 255.;
        let g = ((rgb >> 8) & 255) as f64 / 255.;
        let b = (rgb & 255) as f64 / 255.;
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let delta = max - min;
        let l = (max + min) / 2.;
        if delta < 1e-9 {
            return Self { h: 0., s: 0., l };
        }
        let h = if max == r {
            ((g - b) / delta).rem_euclid(6.)
        } else if max == g {
            (b - r) / delta + 2.
        } else {
            (r - g) / delta + 4.
        };
        Self {
            h: h * 60.,
            s: delta / (1. - (2. * l - 1.).abs()),
            l,
        }
    }
    pub fn rgb(self) -> u32 {
        let h = self.h.rem_euclid(360.) / 60.;
        let s = self.s.clamp(0., 1.);
        let l = self.l.clamp(0., 1.);
        let c = (1. - (2. * l - 1.).abs()) * s;
        let x = c * (1. - (h.rem_euclid(2.) - 1.).abs());
        let m = l - c / 2.;
        let (r, g, b) = if h < 1. {
            (c, x, 0.)
        } else if h < 2. {
            (x, c, 0.)
        } else if h < 3. {
            (0., c, x)
        } else if h < 4. {
            (0., x, c)
        } else if h < 5. {
            (x, 0., c)
        } else {
            (c, 0., x)
        };
        let channel = |v: f64| ((v + m) * 255.).round().clamp(0., 255.) as u32;
        channel(r) << 16 | channel(g) << 8 | channel(b)
    }
}
pub fn colorref(rgb: u32) -> u32 {
    (rgb & 255) << 16 | (rgb & 0xff00) | ((rgb >> 16) & 255)
}
pub fn parse_hex(value: &str) -> Option<u32> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(value, 16).ok()
}
pub fn hex(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xffffff)
}
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub accent: u32,
    pub background: u32,
    pub field: u32,
    pub hover: u32,
    pub text: u32,
    pub muted: u32,
    pub disabled: u32,
    pub border: u32,
    pub selection: u32,
    pub active_fill: u32,
    pub active_border: u32,
    pub active_shadow: u32,
    pub running_fill: u32,
    pub running_border: u32,
    pub running_shadow: u32,
    pub attention_fill: u32,
    pub attention_border: u32,
    pub attention_shadow: u32,
}
/// Material colours retain the primary hue; saturation/lightness describe
/// ambient light and occlusion, rather than emissive accent glows.
#[derive(Clone, Copy, Debug)]
pub struct ReliefPalette {
    pub background: Hsl,
    pub raised: Hsl,
    pub recessed: Hsl,
    pub light: Hsl,
    pub shadow: Hsl,
    pub inner_light: Hsl,
    pub inner_shadow: Hsl,
}
pub fn relief(rgb: u32) -> ReliefPalette {
    let base = Hsl::from_rgb(rgb);
    let shade = |s: f64, l: f64| Hsl {
        h: base.h,
        s: base.s * s,
        l,
    };
    let background = shade(0.24, 0.11);
    ReliefPalette {
        background,
        raised: shade(0.25, background.l + 0.053),
        recessed: shade(0.36, background.l - 0.021),
        light: shade(0.12, background.l + 0.12),
        shadow: shade(0.16, (background.l - 0.085).max(0.012)),
        inner_light: shade(0.14, background.l + 0.16),
        inner_shadow: shade(0.18, (background.l - 0.092).max(0.012)),
    }
}
pub fn derive(rgb: u32) -> Palette {
    let base = Hsl::from_rgb(rgb);
    let material = relief(rgb);
    let shade = |shift: f64, s: f64, l: f64| {
        colorref(
            Hsl {
                h: base.h + shift,
                s: (base.s * s).clamp(0., 0.95),
                l,
            }
            .rgb(),
        )
    };
    Palette {
        accent: colorref(rgb),
        background: colorref(material.background.rgb()),
        field: shade(0., 0.25, 0.20),
        hover: shade(0., 0.30, 0.25),
        text: shade(0., 0.10, 0.95),
        muted: shade(0., 0.17, 0.70),
        disabled: shade(0., 0.12, 0.47),
        border: shade(0., 0.32, 0.38),
        selection: shade(0., 0.58, 0.27),
        active_fill: shade(0., 0.58, 0.27),
        active_border: shade(0., 0.82, 0.65),
        active_shadow: colorref(material.inner_shadow.rgb()),
        running_fill: colorref(material.raised.rgb()),
        running_border: colorref(material.raised.rgb()),
        running_shadow: colorref(material.shadow.rgb()),
        attention_fill: shade(180., 0.62, 0.27),
        attention_border: shade(180., 0.88, 0.66),
        attention_shadow: colorref(material.shadow.rgb()),
    }
}
fn store() -> &'static RwLock<Palette> {
    static THEME: OnceLock<RwLock<Palette>> = OnceLock::new();
    THEME.get_or_init(|| RwLock::new(derive(DEFAULT_ACCENT)))
}
pub fn palette() -> Palette {
    *store().read().unwrap_or_else(|error| error.into_inner())
}
pub fn accent_rgb() -> u32 {
    colorref(palette().accent)
}
pub fn set_accent(rgb: u32) {
    *store().write().unwrap_or_else(|error| error.into_inner()) = derive(rgb & 0xffffff);
}
pub fn init_from_env() {
    if let Ok(value) = std::env::var("ULTRA_BAR_ACCENT")
        && let Some(rgb) = parse_hex(&value)
    {
        set_accent(rgb);
    }
}
pub fn changed_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(crate::wide("UltraBar.ThemeChanged.v1").as_ptr())
    })
}
/// # Safety
/// Invoke on the window's UI thread. The message carries a 24-bit RGB accent.
pub unsafe fn handle_message(hwnd: HWND, message: u32, value: usize) -> bool {
    unsafe {
        if message != changed_message() {
            return false;
        }
        set_accent(value as u32);
        RedrawWindow(
            hwnd,
            std::ptr::null(),
            std::ptr::null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
        true
    }
}
pub fn blend(foreground: u32, background: u32, alpha: f64) -> u32 {
    let channel = |shift: u32| {
        let a = ((foreground >> shift) & 255) as f64;
        let b = ((background >> shift) & 255) as f64;
        (a * alpha + b * (1. - alpha)).round() as u32
    };
    channel(0) | (channel(8) << 8) | (channel(16) << 16)
}
struct Brush {
    color: u32,
    handle: isize,
}
impl Drop for Brush {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.handle as HGDIOBJ);
        }
    }
}
thread_local! {static BRUSHES:RefCell<[Option<Brush>;2]>=const{RefCell::new([None,None])};}
pub fn background_brush() -> HBRUSH {
    brush(0, palette().background)
}
pub fn field_brush() -> HBRUSH {
    brush(1, palette().field)
}
fn brush(index: usize, color: u32) -> HBRUSH {
    BRUSHES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache[index]
            .as_ref()
            .is_none_or(|brush| brush.color != color)
        {
            cache[index] = Some(Brush {
                color,
                handle: unsafe { CreateSolidBrush(color) } as isize,
            });
        }
        cache[index].as_ref().unwrap().handle as HBRUSH
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hsl_round_trips_and_wraps_hue() {
        for rgb in [
            0, 0xffffff, 0xff0000, 0x00ff00, 0x0000ff, 0x69b4ff, 0xccaacc,
        ] {
            assert_eq!(Hsl::from_rgb(rgb).rgb(), rgb);
        }
        assert_eq!(
            Hsl {
                h: 720.,
                s: 1.,
                l: 0.5
            }
            .rgb(),
            0xff0000
        );
        assert_eq!(
            Hsl {
                h: -120.,
                s: 1.,
                l: 0.5
            }
            .rgb(),
            0x0000ff
        );
    }
    #[test]
    fn active_and_relief_keep_the_primary_hue_with_relative_lightness() {
        for hue in [0., 45., 120., 210., 300.] {
            let accent = Hsl {
                h: hue,
                s: 0.8,
                l: 0.55,
            }
            .rgb();
            let p = derive(accent);
            let active = Hsl::from_rgb(colorref(p.active_fill));
            let running = Hsl::from_rgb(colorref(p.running_shadow));
            let base = Hsl::from_rgb(accent);
            assert!((active.h - base.h).abs() < 1.);
            let material = relief(accent);
            assert!((material.light.h - base.h).abs() < 0.01);
            assert!((material.shadow.h - base.h).abs() < 0.01);
            assert!(material.light.s < 0.2 && material.shadow.s < 0.2);
            assert!(
                material.light.l > material.raised.l && material.shadow.l < material.recessed.l
            );
            assert!(running.l < 0.04);
        }
    }
    #[test]
    fn hex_validation_and_neutral_accents() {
        assert_eq!(parse_hex("#69B4FF"), Some(DEFAULT_ACCENT));
        assert!(parse_hex("#12345Z").is_none());
        assert_eq!(hex(0xaabbcc), "#AABBCC");
        let p = derive(0xffffff);
        assert!(Hsl::from_rgb(colorref(p.active_fill)).s < 0.01);
    }
}
