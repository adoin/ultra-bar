//! Sax Design progress/bubbles material adapted to the native taskbar.
//! Texture parameters match progress-textures.ts; DirectComposition owns the animation timeline.
pub mod compositor;
const BUBBLE_OPACITY: f64 = 0.95;
const FILL_OPACITY: f64 = 0.78;
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
    let bubbles = texture(key.width, key.height);
    for y in 0..key.height {
        for x in 0..key.width {
            let i = (y * key.width + x) as usize;
            let fill = if key.fill > 0 {
                coverage(
                    x as f64 + 0.5,
                    y as f64 + 0.5,
                    key.fill as f64,
                    key.height as f64,
                )
            } else {
                0.
            };
            let bubble = bubbles[i] * BUBBLE_OPACITY * fill;
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
    if key.fill <= 0 {
        return pixels;
    }
    let tint = fill_color(key.accent);
    for y in 0..key.height {
        for x in 0..key.width {
            let a = coverage(
                x as f64 + 0.5,
                y as f64 + 0.5,
                key.fill as f64,
                key.height as f64,
            ) * FILL_OPACITY;
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
            .map(|v| (v.clamp(0., 1.) * width as f64).round() as i32)
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
pub unsafe fn label(dc: HDC, rect: &RECT, text: &str) {
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
        let text = crate::visual::wide(text);
        let mut shadow = *rect;
        OffsetRect(&mut shadow, 0, 1);
        DrawTextW(dc, text.as_ptr(), -1, &mut shadow, flags);
        SetTextColor(dc, theme::palette().text);
        let mut foreground = *rect;
        DrawTextW(dc, text.as_ptr(), -1, &mut foreground, flags);
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
            DeleteObject(brush);
            let font = CreateFontW(
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
                    ("Claude 78%", Some(0.78)),
                    ("Codex 100%", Some(1.)),
                    ("Claude —", None),
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
                    label(surface.dc, &text_rect, text);
                }
            }
            SelectObject(surface.dc, old);
            DeleteObject(font);
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
    fn unknown_and_zero_never_receive_a_filled_texture() {
        let key = Key {
            width: 100,
            height: 22,
            fill: 0,
            accent: 0x69b4ff,
        };
        assert_eq!(generate(key), generate(Key { fill: -1, ..key }));
        assert_ne!(generate(key), generate(Key { fill: 36, ..key }));
        assert_eq!(generate(key)[0], 0);
    }
    #[test]
    fn changing_the_limit_never_changes_the_unfilled_track() {
        let key = Key {
            width: 110,
            height: 32,
            fill: 36,
            accent: 0x69b4ff,
        };
        let partial = generate(key);
        let empty = generate(Key { fill: 0, ..key });
        for y in 0..key.height {
            for x in key.fill..key.width {
                let i = (y * key.width + x) as usize;
                assert_eq!(partial[i], empty[i]);
            }
        }
        assert_eq!(texture(110, 32), texture(110, 32));
    }
}
