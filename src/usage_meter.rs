//! Sax Design progress/bubbles material adapted to the native taskbar.
//! Texture parameters match progress-textures.ts; DirectComposition owns the animation timeline.
pub mod compositor;
const BUBBLE_OPACITY: f64 = 0.95;
const FILL_OPACITY: f64 = 0.78;
const FILL_SCALE: i32 = 10_000;
use crate::visual::text::draw_text as DrawTextW;
use crate::visual::{
    Surface,
    theme::{self, Hsl},
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    width: i32,
    height: i32,
    fill: i32,
    accent: u32,
}
#[derive(Clone, Copy)]
struct Fill {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    clip_width: f64,
}
fn fill_bounds(key: Key) -> Option<Fill> {
    if key.fill <= 0 || key.width < 2 || key.height < 2 {
        return None;
    }
    let inset = (key.height / 10)
        .max(2)
        .min((key.height - 2) / 2)
        .min((key.width - 2) / 2);
    let track_width = key.width - 2 * inset;
    let height = key.height - 2 * inset;
    let clip_width = area_width(
        track_width,
        height,
        key.fill.clamp(0, FILL_SCALE) as f64 / FILL_SCALE as f64,
    );
    let width = clip_width.ceil() as i32;
    (width > 0).then_some(Fill {
        left: inset,
        top: inset,
        width,
        height,
        clip_width,
    })
}
// Geometry only: cache all 101 integer percentages once per size. Animation
// never evaluates area or rebuilds this table.
type AreaTables = VecDeque<((i32, i32), Rc<[f64; 101]>)>;
thread_local! {static AREA_TABLES:RefCell<AreaTables>=const{RefCell::new(VecDeque::new())};}
fn capsule_area_at(width: f64, height: f64, x: f64) -> f64 {
    let radius = (height / 2.).min(width / 2.);
    if radius <= 0. {
        return 0.;
    }
    // Cumulative area of a rounded capsule, including either orientation.
    let total = width * height - (4. - std::f64::consts::PI) * radius * radius;
    let x = x.clamp(0., width);
    if x > width - radius {
        return total - capsule_area_at(width, height, width - x);
    }
    if x >= radius {
        return (height - 2. * radius) * radius
            + std::f64::consts::PI * radius * radius / 2.
            + (x - radius) * height;
    }
    let offset = radius - x;
    (height - 2. * radius) * x + radius * radius * (offset / radius).clamp(-1., 1.).acos()
        - offset * (radius * radius - offset * offset).max(0.).sqrt()
}
fn area_width(width: i32, height: i32, progress: f64) -> f64 {
    AREA_TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        let key = (width, height);
        let table = if let Some(index) = tables.iter().position(|(k, _)| *k == key) {
            let entry = tables.remove(index).unwrap();
            let table = entry.1.clone();
            tables.push_back(entry);
            table
        } else {
            let (w, h) = (width as f64, height as f64);
            let total = capsule_area_at(w, h, w);
            let mut values = [0.; 101];
            values[100] = w;
            for (percent, value) in values.iter_mut().enumerate().take(100).skip(1) {
                let target = total * percent as f64 / 100.;
                let (mut lo, mut hi) = (0., w);
                for _ in 0..36 {
                    let mid = (lo + hi) / 2.;
                    if capsule_area_at(w, h, mid) < target {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                *value = (lo + hi) / 2.;
            }
            let table = Rc::new(values);
            if tables.len() >= 32 {
                tables.pop_front();
            }
            tables.push_back((key, table.clone()));
            table
        };
        let percent = progress.clamp(0., 1.) * 100.;
        let lo = (percent.floor() as usize).min(100);
        let hi = (lo + 1).min(100);
        table[lo] + (table[hi] - table[lo]) * (percent - lo as f64)
    })
}
thread_local! {static CACHE:RefCell<VecDeque<(Key,Rc<Vec<u32>>)>>=const{RefCell::new(VecDeque::new())};}
fn distance(x: f64, y: f64, w: f64, h: f64) -> f64 {
    let r = (h / 2.).min(w / 2.);
    let qx = (x - w / 2.).abs() - (w / 2. - r);
    let qy = (y - h / 2.).abs() - (h / 2. - r);
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - r
}
fn coverage(x: f64, y: f64, w: f64, h: f64) -> f64 {
    (0.5 - distance(x, y, w, h)).clamp(0., 1.)
}
fn fill_coverage(key: Key, bounds: Fill, x: i32, y: i32) -> f64 {
    if x < bounds.left || x >= bounds.left + bounds.width {
        return 0.;
    }
    let horizontal = (bounds.clip_width - (x - bounds.left) as f64).clamp(0., 1.);
    horizontal
        * coverage(
            (x - bounds.left) as f64 + 0.5,
            (y - bounds.top) as f64 + 0.5,
            (key.width - 2 * bounds.left) as f64,
            bounds.height as f64,
        )
}
fn bubble_alpha(t: f64) -> f64 {
    let stops = [(0., 1.), (0.42, 0.9), (0.78, 0.35), (1., 0.)];
    for pair in stops.windows(2) {
        let ((a, av), (b, bv)) = (pair[0], pair[1]);
        if t <= b {
            return av + (bv - av) * ((t - a) / (b - a)).clamp(0., 1.);
        }
    }
    0.
}
#[cfg(test)]
fn texture(width: i32, height: i32) -> Vec<f64> {
    texture_at_scale(width, height, height as f64 / 32.)
}
fn texture_at_scale(width: i32, height: i32, scale: f64) -> Vec<f64> {
    let tw = 110. * scale;
    let th = 64. * scale;
    let mut seed = 47291u32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed as f64 / 4294967296.
    };
    let mut alpha = vec![0.; (width * height) as usize];
    for _ in 0..270 {
        let cx = random() * tw;
        let cy = random() * th;
        let r = (0.55 + 2.65 * random().powf(0.7)) * scale;
        let opacity = 0.3 + random() * 0.6;
        for ix in -1..=(width as f64 / tw).ceil() as i32 {
            for iy in -1..=1 {
                let x = cx + ix as f64 * tw;
                let y = cy + iy as f64 * th;
                let left = ((x - r).floor() as i32).max(0);
                let right = ((x + r).ceil() as i32).min(width);
                let top = ((y - r).floor() as i32).max(0);
                let bottom = ((y + r).ceil() as i32).min(height);
                for py in top..bottom {
                    for px in left..right {
                        let t = (px as f64 + 0.5 - x).hypot(py as f64 + 0.5 - y) / r;
                        if t < 1. {
                            let index = (py * width + px) as usize;
                            let a = bubble_alpha(t) * opacity;
                            alpha[index] = a + alpha[index] * (1. - a);
                        }
                    }
                }
            }
        }
    }
    alpha
}
fn generate(key: Key) -> Vec<u32> {
    let mut pixels = vec![0; (key.width * key.height) as usize];
    let bounds = fill_bounds(key);
    let bubbles = bounds
        .map(|bounds| texture_at_scale(key.width, key.height, bounds.height as f64 / 32.))
        .unwrap_or_default();
    for y in 0..key.height {
        for x in 0..key.width {
            let i = (y * key.width + x) as usize;
            let fill = bounds
                .map(|bounds| fill_coverage(key, bounds, x, y))
                .unwrap_or(0.);
            let bubble = bubbles.get(i).copied().unwrap_or(0.) * BUBBLE_OPACITY * fill;
            let a = bubble + FILL_OPACITY * fill * (1. - bubble);
            if a > 0. {
                pixels[i] = ((a * 255.).round() as u32) << 24
                    | theme::blend(0xffffff, fill_color(key.accent), bubble / a);
            }
        }
    }
    let inset = inset_pixels(key.width, key.height, key.accent);
    for (pixel, shade) in pixels.iter_mut().zip(inset) {
        let a = (shade >> 24) as f64 / 255.;
        if a == 0. {
            continue;
        }
        let base = (*pixel >> 24) as f64 / 255.;
        let alpha = a + base * (1. - a);
        *pixel = ((alpha * 255.).round() as u32) << 24
            | theme::blend(shade & 0xffffff, *pixel & 0xffffff, a / alpha);
    }
    pixels
}
/// Soft inner relief, lit from the upper left; the center remains transparent.
fn inset_pixels(width: i32, height: i32, accent: u32) -> Vec<u32> {
    let mut pixels = vec![0; (width * height) as usize];
    let hsl = Hsl::from_rgb(accent);
    let dark = Hsl {
        h: hsl.h,
        s: hsl.s * 0.42,
        l: 0.045,
    }
    .rgb();
    let light = Hsl {
        h: hsl.h,
        s: hsl.s * 0.24,
        l: 0.82,
    }
    .rgb();
    let scale = (height as f64 / 28.).max(0.7);
    for y in 0..height {
        for x in 0..width {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let d = distance(px, py, width as f64, height as f64);
            let mask = (0.5 - d).clamp(0., 1.);
            if mask == 0. {
                continue;
            }
            let above = distance(
                px - 1.4 * scale,
                py - 1.4 * scale,
                width as f64,
                height as f64,
            );
            let below = distance(
                px + 1.4 * scale,
                py + 1.4 * scale,
                width as f64,
                height as f64,
            );
            let shadow = ((above + 3. * scale) / (4.5 * scale)).clamp(0., 1.).powi(2) * 0.62;
            let glow =
                ((below + 1.8 * scale) / (3. * scale)).clamp(0., 1.).powi(2) * 0.24 * (1. - shadow);
            let a = (shadow + glow) * mask;
            if a > 0. {
                pixels[(y * width + x) as usize] = ((a * 255.).round() as u32) << 24
                    | theme::blend(light, dark, glow / (shadow + glow));
            }
        }
    }
    pixels
}
fn fill_color(accent: u32) -> u32 {
    let mut hsl = Hsl::from_rgb(accent);
    hsl.s = (hsl.s * 1.3).min(1.);
    hsl.l += (1. - hsl.l) * 0.22;
    hsl.rgb()
}
fn fill_pixels(key: Key) -> Vec<u32> {
    let mut pixels = vec![0; (key.width * key.height) as usize];
    let Some(bounds) = fill_bounds(key) else {
        return pixels;
    };
    let tint = fill_color(key.accent);
    for y in 0..key.height {
        for x in 0..key.width {
            let a = fill_coverage(key, bounds, x, y) * FILL_OPACITY;
            if a > 0. {
                pixels[(y * key.width + x) as usize] = ((a * 255.).round() as u32) << 24 | tint;
            }
        }
    }
    pixels
}
pub unsafe fn draw(surface: &Surface, rect: &RECT, progress: Option<f64>) {
    unsafe {
        surface.clear_rounded(rect, (rect.bottom - rect.top) as f64 / 2.);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < 2 || height < 2 {
            return;
        }
        let fill = progress
            .filter(|v| v.is_finite())
            .map(|v| (v.clamp(0., 1.) * FILL_SCALE as f64).round() as i32)
            .unwrap_or(-1);
        let key = Key {
            width,
            height,
            fill,
            accent: theme::accent_rgb(),
        };
        let pixels = CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
                let entry = cache.remove(i).unwrap();
                let pixels = entry.1.clone();
                cache.push_back(entry);
                return pixels;
            }
            let pixels = Rc::new(generate(key));
            if cache.len() >= 32 {
                cache.pop_front();
            }
            cache.push_back((key, pixels.clone()));
            pixels
        });
        surface.effect(rect.left, rect.top, width, height, &pixels);
    }
}
/// Keep labels readable over the bright microbubble centers, including during vertical page flips.
pub(super) fn text_shadow(
    pixels: &[u32],
    w: i32,
    h: i32,
    radius: i32,
    dy: i32,
    opacity: f64,
) -> Vec<u32> {
    let mut result = vec![0; pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let mut a = 0.0f64;
            for oy in -radius..=radius {
                for ox in -radius..=radius {
                    let (sx, sy) = (x + ox, y + oy - dy);
                    if sx >= 0 && sx < w && sy >= 0 && sy < h {
                        let alpha = (pixels[(sy * w + sx) as usize] >> 24) as f64;
                        let falloff = 1.
                            - (ox * ox + oy * oy) as f64 / ((radius + 1) * (radius + 1) * 2) as f64;
                        a = a.max(alpha * falloff);
                    }
                }
            }
            result[(y * w + x) as usize] = ((a * opacity).round() as u32) << 24 | 0x050a10;
        }
    }
    result
}
pub(super) unsafe fn caption(dc: HDC, rect: &RECT, text: &str, trailing: &str, flags: u32) {
    unsafe {
        let mut main = *rect;
        if !trailing.is_empty() {
            let wide = crate::visual::wide(trailing);
            let mut size: windows_sys::Win32::Foundation::SIZE = std::mem::zeroed();
            GetTextExtentPoint32W(dc, wide.as_ptr(), (wide.len() - 1) as _, &mut size);
            let mut right = *rect;
            // GDI and DirectWrite fallback glyph metrics can differ for ↺.
            // Align within the whole label box and never ellipsize the short suffix.
            main.right = (right.right - size.cx - 10).max(main.left);
            DrawTextW(
                dc,
                wide.as_ptr(),
                -1,
                &mut right,
                (flags & !DT_END_ELLIPSIS) | DT_RIGHT,
            );
        }
        DrawTextW(dc, crate::visual::wide(text).as_ptr(), -1, &mut main, flags);
    }
}
pub unsafe fn label(dc: HDC, rect: &RECT, text: &str, trailing: &str) {
    unsafe {
        let base = Hsl::from_rgb(theme::accent_rgb());
        let color = SetTextColor(
            dc,
            theme::colorref(
                Hsl {
                    h: base.h,
                    s: base.s * 0.3,
                    l: 0.06,
                }
                .rgb(),
            ),
        );
        let flags = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
        let mut shadow = *rect;
        OffsetRect(&mut shadow, 0, 2);
        caption(dc, &shadow, text, trailing, flags);
        for (y, x) in [
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ] {
            let mut edge = *rect;
            OffsetRect(&mut edge, x, y);
            caption(dc, &edge, text, trailing, flags);
        }
        SetTextColor(dc, theme::palette().text);
        let foreground = *rect;
        caption(dc, &foreground, text, trailing, flags);
        SetTextColor(dc, color);
    }
}
pub unsafe fn smoke(directory: &std::path::Path) -> anyhow::Result<()> {
    unsafe {
        std::fs::create_dir_all(directory)?;
        let original = theme::accent_rgb();
        for (name, accent) in [
            ("blue", original),
            ("purple", 0xb96df0),
            ("green", 0x36c7a1),
        ] {
            theme::set_accent(accent);
            let surface = Surface::new(640, 150)
                .ok_or_else(|| anyhow::anyhow!("Cannot create meter preview"))?;
            let area = RECT {
                left: 0,
                top: 0,
                right: 640,
                bottom: 150,
            };
            let brush = CreateSolidBrush(theme::palette().background);
            FillRect(surface.dc, &area, brush);
            ultra_bar_native_ui::fonts::delete_object(brush);
            let font = ultra_bar_native_ui::fonts::create_font(
                -13,
                0,
                0,
                0,
                500,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                crate::visual::wide("Segoe UI").as_ptr(),
            );
            let old = SelectObject(surface.dc, font);
            SetBkMode(surface.dc, TRANSPARENT as _);
            SetTextColor(surface.dc, theme::palette().text);
            for (row, height) in [32, 22].into_iter().enumerate() {
                for (column, (text, progress)) in [
                    ("Codex 36%", Some(0.36)),
                    ("Codex 1%", Some(0.01)),
                    ("Codex 100%", Some(1.)),
                    ("Codex 5%", Some(0.05)),
                ]
                .into_iter()
                .enumerate()
                {
                    let rect = RECT {
                        left: 12 + column as i32 * 156,
                        top: 20 + row as i32 * 70,
                        right: 154 + column as i32 * 156,
                        bottom: 20 + row as i32 * 70 + height,
                    };
                    draw(&surface, &rect, progress);
                    let text_rect = RECT {
                        left: rect.left + 8,
                        ..rect
                    };
                    label(surface.dc, &text_rect, text, "");
                }
            }
            SelectObject(surface.dc, old);
            ultra_bar_native_ui::fonts::delete_object(font);
            surface.save_bmp(&directory.join(format!("usage-bubbles-{name}.bmp")))?;
        }
        theme::set_accent(original);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_suffix_is_not_ellipsized_and_the_double_shadow_works_on_light_and_dark() {
        unsafe {
            let font = ultra_bar_native_ui::fonts::create_font(
                -12,
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
                CLEARTYPE_NATURAL_QUALITY as _,
                0,
                crate::visual::wide("Segoe UI").as_ptr(),
            );
            let page = ultra_bar_plugin_sdk::Page {
                id: "Codex".into(),
                text: "Codex 25%".into(),
                trailing: "↺2".into(),
                ..Default::default()
            };
            let rect = RECT {
                left: 0,
                top: 0,
                right: 112,
                bottom: 22,
            };
            let spec = compositor::spec("test", &rect, &page, None, 1., font, 4);
            let caption = compositor::caption_pixels(&spec, 112, 22).unwrap();
            let reference = Surface::new(112, 22).unwrap();
            SelectObject(reference.dc, font);
            SetBkMode(reference.dc, TRANSPARENT as _);
            SetTextColor(reference.dc, 0xffffff);
            let mut right = RECT {
                left: 4,
                top: 0,
                right: 108,
                bottom: 22,
            };
            DrawTextW(
                reference.dc,
                crate::visual::wide("↺2").as_ptr(),
                -1,
                &mut right,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_RIGHT,
            );
            let expected = reference.copy_pixels();
            for y in 0..22 {
                for x in 82..112 {
                    let i = (y * 112 + x) as usize;
                    let p = expected[i];
                    let alpha = (p & 255).max((p >> 8) & 255).max((p >> 16) & 255);
                    assert_eq!(caption[i] >> 24, alpha);
                }
            }
            let atlas = Surface::new(256, 40).unwrap();
            for (column, background) in [0x0c1018, 0xf0f0f0].into_iter().enumerate() {
                let mut pixels = vec![background; caption.len()];
                for (radius, dy, opacity) in [(2, 1, 0.55), (1, 0, 0.95)] {
                    let shade = text_shadow(&caption, 112, 22, radius, dy, opacity);
                    for (p, s) in pixels.iter_mut().zip(shade) {
                        *p = theme::blend(s & 0xffffff, *p, (s >> 24) as f64 / 255.);
                    }
                }
                for (p, s) in pixels.iter_mut().zip(&caption) {
                    *p = theme::blend(s & 0xffffff, *p, (s >> 24) as f64 / 255.);
                }
                assert!(pixels.iter().any(|p| *p > 0xf0f0f0));
                if column == 1 {
                    assert!(pixels.iter().any(|p| *p < 0x303030));
                }
                for y in 0..22 {
                    for x in 0..112 {
                        atlas.pixel(
                            column as i32 * 128 + 8 + x,
                            8 + y,
                            pixels[(y * 112 + x) as usize],
                        );
                    }
                }
            }
            ultra_bar_native_ui::fonts::delete_object(font);
            atlas
                .save_bmp(std::path::Path::new(
                    "D:/tmp/ultra-bar-agent-dashboard-20261111/caption-contrast.bmp",
                ))
                .unwrap();
        }
    }
    #[test]
    fn liquid_leaves_a_recess_above_and_below_at_both_sizes_and_dpi_scales() {
        for dpi in [96, 144, 192] {
            for height in [22, 32] {
                let height = height * dpi / 96;
                let width = 110 * dpi / 96;
                let key = Key {
                    width,
                    height,
                    fill: FILL_SCALE,
                    accent: 0x69b4ff,
                };
                let fill = fill_pixels(key);
                let composed = generate(key);
                let shadow = inset_pixels(width, height, key.accent);
                let gap = 2 * dpi / 96;
                for y in 0..height {
                    for x in 0..width {
                        let i = (y * width + x) as usize;
                        if y < gap || y >= height - gap {
                            assert_eq!(fill[i], 0);
                            assert_eq!(composed[i], shadow[i]);
                        }
                    }
                }
                assert!(fill.iter().any(|pixel| pixel >> 24 > 0));
            }
        }
    }
    #[test]
    fn low_remaining_uses_the_full_capsule_silhouette_instead_of_a_tall_narrow_pill() {
        for dpi in [96, 144, 192] {
            for logical_height in [22, 32] {
                let width = 110 * dpi / 96;
                let height = logical_height * dpi / 96;
                let key = Key {
                    width,
                    height,
                    fill: 100,
                    accent: 0x69b4ff,
                };
                let bounds = fill_bounds(key).unwrap();
                let pixels = fill_pixels(key);
                let rows: Vec<_> = (0..height)
                    .filter(|y| (0..width).any(|x| pixels[(y * width + x) as usize] >> 24 > 0))
                    .collect();
                assert!(!rows.is_empty());
                assert!(
                    rows.len() < bounds.height as usize,
                    "one percent must follow the curved left tip"
                );
                assert_eq!(pixels[(bounds.top * width + bounds.left) as usize], 0);
                for y in 0..height {
                    for x in 0..width {
                        let amount = fill_coverage(key, bounds, x, y);
                        if amount > 0. {
                            assert!(
                                coverage(
                                    (x - bounds.left) as f64 + 0.5,
                                    (y - bounds.top) as f64 + 0.5,
                                    (width - 2 * bounds.left) as f64,
                                    bounds.height as f64
                                ) > 0.
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn integer_percentages_match_capsule_area_and_reuse_the_lookup_table() {
        for (width, height) in [(106, 18), (106, 26), (159, 27), (212, 52), (16, 28)] {
            let total = capsule_area_at(width as f64, height as f64, width as f64);
            let mut previous = 0.;
            for percent in 0..=100 {
                let x = area_width(width, height, percent as f64 / 100.);
                assert!(x >= previous);
                previous = x;
                let actual = capsule_area_at(width as f64, height as f64, x) / total;
                assert!(
                    (actual - percent as f64 / 100.).abs() < 1e-8,
                    "{percent}% area mismatch"
                );
            }
            assert!(area_width(width, height, 0.01) > width as f64 * 0.01);
            let before = AREA_TABLES.with(|t| t.borrow().len());
            for _ in 0..100 {
                area_width(width, height, 0.01);
            }
            assert_eq!(AREA_TABLES.with(|t| t.borrow().len()), before);
        }
    }
    #[test]
    fn rasterized_fill_area_tracks_the_percentage_at_low_and_high_amounts() {
        for (width, height) in [(110, 22), (110, 32), (165, 33), (220, 64)] {
            let mut key = Key {
                width,
                height,
                fill: FILL_SCALE,
                accent: 0x69b4ff,
            };
            let sum = |key: Key| {
                let bounds = fill_bounds(key);
                (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| bounds.map(|b| fill_coverage(key, b, x, y)).unwrap_or(0.))
                    .sum::<f64>()
            };
            let full = sum(key);
            for percent in [0, 1, 5, 10, 25, 50, 75, 99, 100] {
                key.fill = percent * 100;
                let actual = sum(key) / full;
                assert!(
                    (actual - percent as f64 / 100.).abs() < 0.0025,
                    "raster area mismatch at {percent}%: {actual}"
                );
            }
        }
    }
    #[test]
    fn unknown_and_zero_never_receive_a_filled_texture() {
        let key = Key {
            width: 100,
            height: 22,
            fill: 0,
            accent: 0x69b4ff,
        };
        assert_eq!(generate(key), generate(Key { fill: -1, ..key }));
        assert_ne!(generate(key), generate(Key { fill: 3600, ..key }));
        assert_eq!(generate(key)[0], 0);
    }
    #[test]
    fn changing_the_limit_never_changes_the_unfilled_track() {
        let key = Key {
            width: 110,
            height: 32,
            fill: 3600,
            accent: 0x69b4ff,
        };
        let partial = generate(key);
        let empty = generate(Key { fill: 0, ..key });
        for y in 0..key.height {
            for x in fill_bounds(key).map(|b| b.left + b.width).unwrap_or(0)..key.width {
                let i = (y * key.width + x) as usize;
                assert_eq!(partial[i], empty[i]);
            }
        }
        assert_eq!(texture(110, 32), texture(110, 32));
    }
}
