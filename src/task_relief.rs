//! One upper-left light: raised outer shadows and recessed inner shadows.
//! Raster tiles cache shadows and a thin accent outline; no per-frame lighting work.
use crate::settings::ActiveStyle;
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
    dpi: i32,
    accent: u32,
    pressed: bool,
    style: ActiveStyle,
    hover: bool,
    attention: bool,
}
struct Tile {
    width: i32,
    height: i32,
    pad: i32,
    pixels: Vec<u32>,
}
thread_local! {static CACHE:RefCell<VecDeque<(Key,Rc<Tile>)>>=const{RefCell::new(VecDeque::new())};}
#[derive(Clone, Copy)]
struct Material {
    h: f64,
    s: f64,
    background: u32,
    light: u32,
    dark: u32,
    inner_light: u32,
    inner_dark: u32,
    raised: Hsl,
    recessed: Hsl,
}
fn material(accent: u32) -> Material {
    let base = Hsl::from_rgb(accent);
    let colours = theme::relief(accent);
    Material {
        h: base.h,
        s: base.s,
        background: colours.background.rgb(),
        light: colours.light.rgb(),
        dark: colours.shadow.rgb(),
        inner_light: colours.inner_light.rgb(),
        inner_dark: colours.inner_shadow.rgb(),
        raised: colours.raised,
        recessed: colours.recessed,
    }
}
fn distance(x: f64, y: f64, width: f64, height: f64, radius: f64) -> f64 {
    let qx = (x - width / 2.).abs() - (width / 2. - radius);
    let qy = (y - height / 2.).abs() - (height / 2. - radius);
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - radius
}
fn blurred_inside(distance: f64, blur: f64) -> f64 {
    let t = (0.5 - distance / (2. * blur)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}
#[derive(Clone, Copy)]
struct Pixel {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}
impl Pixel {
    fn new(rgb: u32, a: f64) -> Self {
        Self {
            r: ((rgb >> 16) & 255) as f64,
            g: ((rgb >> 8) & 255) as f64,
            b: (rgb & 255) as f64,
            a,
        }
    }
    fn over(&mut self, rgb: u32, coverage: f64) {
        let alpha = coverage.clamp(0., 1.);
        let old_alpha = self.a;
        let next = alpha + old_alpha * (1. - alpha);
        if next == 0. {
            return;
        }
        let source = Self::new(rgb, alpha);
        let blend =
            |front: f64, back: f64| (front * alpha + back * old_alpha * (1. - alpha)) / next;
        self.r = blend(source.r, self.r);
        self.g = blend(source.g, self.g);
        self.b = blend(source.b, self.b);
        self.a = next;
    }
    fn rgba(self) -> u32 {
        ((self.a * 255.).round() as u32) << 24
            | (self.r.round() as u32) << 16
            | (self.g.round() as u32) << 8
            | self.b.round() as u32
    }
}
fn sample(key: Key, mat: Material, x: f64, y: f64) -> u32 {
    let scale = key.dpi as f64 / 96.;
    let width = key.width as f64;
    let height = key.height as f64;
    let radius = (7. * scale).min(height / 2. - scale).max(scale);
    let sdf = |x, y| distance(x, y, width, height, radius);
    let coverage = (0.5 - sdf(x, y)).clamp(0., 1.);
    let size = (height / (32. * scale)).clamp(0.7, 1.);
    let offset = if key.pressed { 2.2 } else { 1.6 } * scale * size;
    let blur = if key.pressed { 3.0 } else { 2.3 } * scale * size;
    let mut pixel = Pixel::new(mat.background, 148. / 255.);
    if !key.pressed {
        let dark = blurred_inside(sdf(x - offset, y - offset), blur) * (1. - coverage) * 0.72;
        let light = blurred_inside(sdf(x + offset, y + offset), blur) * (1. - coverage) * 0.30;
        if dark + light < 0.002 && coverage == 0. {
            return 0;
        }
        pixel.over(mat.dark, dark);
        pixel.over(mat.light, light);
    } else if coverage == 0. {
        if key.style == ActiveStyle::Underline {
            let bar_width = (width * 0.72)
                .min(44. * scale)
                .max(18. * scale)
                .min(width - 4. * scale)
                .max(scale);
            let bar = distance(
                x - (width - bar_width) / 2.,
                y - (height + 3. * scale),
                bar_width,
                2. * scale,
                scale,
            );
            let amount = (0.5 - bar).clamp(0., 1.);
            if amount == 0. {
                return 0;
            }
            return Pixel::new(
                Hsl {
                    h: mat.h,
                    s: (mat.s * 0.9).clamp(0.50, 0.85),
                    l: 0.65,
                }
                .rgb(),
                amount,
            )
            .rgba();
        }
        return 0;
    }
    let direction = ((x / width - 0.5) + (y / height - 0.5)).clamp(-1., 1.);
    let hue = if key.attention { mat.h + 180. } else { mat.h };
    let material = if key.pressed {
        mat.recessed
    } else {
        mat.raised
    };
    let saturation = if key.attention {
        mat.s * 0.48
    } else {
        material.s
    };
    let lightness = material.l
        + direction * if key.pressed { 0.016 } else { -0.018 }
        + if key.hover { 0.010 } else { 0. };
    let face = Hsl {
        h: hue,
        s: saturation,
        l: lightness,
    }
    .rgb();
    pixel.over(face, coverage * 0.92);
    if key.pressed {
        // Same shadow offset as the raised case, applied to the inverted mask:
        // upper-left occlusion and lower-right reflected light inside the cavity.
        let dark = (1. - blurred_inside(sdf(x - offset, y - offset), blur)) * coverage * 0.96;
        let light = (1. - blurred_inside(sdf(x + offset, y + offset), blur)) * coverage * 0.64;
        pixel.over(mat.inner_dark, dark);
        // A tight contact shadow sits beneath the broader cavity shadow.
        let contact = (1. - blurred_inside(sdf(x - 0.7 * scale, y - 0.7 * scale), 0.65 * scale))
            * coverage
            * 0.38;
        pixel.over(mat.inner_dark, contact);
        pixel.over(mat.inner_light, light);
    }
    if key.pressed {
        match key.style {
            ActiveStyle::Inset => {}
            ActiveStyle::Underline => {}
            ActiveStyle::TopDot => {
                let radius = 1.35 * scale;
                let dot = (x - width / 2.).hypot(y - 2.5 * scale) - radius;
                let rim = (0.5 - (dot - 0.65 * scale)).clamp(0., 1.) * coverage;
                pixel.over(mat.inner_light, rim * 0.60);
                pixel.over(
                    Hsl {
                        h: mat.h,
                        s: mat.s * 0.15,
                        l: 0.015,
                    }
                    .rgb(),
                    (0.5 - dot).clamp(0., 1.) * coverage,
                );
            }
        }
    }
    if !key.pressed {
        // Keep the raised outline inside the item, preserving the two-pixel gaps.
        let inner = (0.5 - (sdf(x, y) + scale)).clamp(0., 1.);
        let outline = (coverage - inner).max(0.);
        pixel.over(key.accent, outline * if key.hover { 0.60 } else { 0.48 });
    }
    pixel.rgba()
}
fn generate(key: Key) -> Tile {
    let pad = (5. * key.dpi as f64 / 96.).ceil() as i32;
    let width = key.width + pad * 2;
    let height = key.height + pad * 2;
    let mat = material(key.accent);
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.push(sample(
                key,
                mat,
                (x - pad) as f64 + 0.5,
                (y - pad) as f64 + 0.5,
            ));
        }
    }
    Tile {
        width,
        height,
        pad,
        pixels,
    }
}
pub unsafe fn draw(
    surface: &Surface,
    rect: &RECT,
    dpi: i32,
    pressed: bool,
    style: ActiveStyle,
    attention: bool,
    hover: bool,
) {
    unsafe {
        let width = rect.right - rect.left;
        // Reserve a separate lower lane for the indicator. The icon and hit box
        // keep their original positions; the cavity itself ends above that lane.
        let height = rect.bottom
            - rect.top
            - if pressed && style == ActiveStyle::Underline {
                (2 * dpi / 96).max(1)
            } else {
                0
            };
        if width < 2 || height < 2 {
            return;
        }
        let key = Key {
            width,
            height,
            dpi,
            accent: theme::accent_rgb(),
            pressed,
            style,
            attention,
            hover,
        };
        let tile = CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(index) = cache.iter().position(|(old, _)| *old == key) {
                let entry = cache.remove(index).unwrap();
                let tile = entry.1.clone();
                cache.push_back(entry);
                return tile;
            }
            let tile = Rc::new(generate(key));
            if cache.len() >= 48 {
                cache.pop_front();
            }
            cache.push_back((key, tile.clone()));
            tile
        });
        surface.effect(
            rect.left - tile.pad,
            rect.top - tile.pad,
            tile.width,
            tile.height,
            &tile.pixels,
        );
    }
}
pub unsafe fn smoke(directory: &std::path::Path) -> anyhow::Result<()> {
    unsafe {
        std::fs::create_dir_all(directory)?;
        let original = theme::accent_rgb();
        for (name, accent, style) in [
            ("blue", theme::DEFAULT_ACCENT, ActiveStyle::Underline),
            ("blue-dot", theme::DEFAULT_ACCENT, ActiveStyle::TopDot),
            ("blue-inset", theme::DEFAULT_ACCENT, ActiveStyle::Inset),
            ("purple", 0xb96df0, ActiveStyle::Underline),
            ("green", 0x36c7a1, ActiveStyle::Underline),
        ] {
            theme::set_accent(accent);
            let p = theme::palette();
            let surface = Surface::new(760, 160)
                .ok_or_else(|| anyhow::anyhow!("Cannot create relief preview"))?;
            let area = RECT {
                left: 0,
                top: 0,
                right: 760,
                bottom: 160,
            };
            let brush = CreateSolidBrush(p.background);
            FillRect(surface.dc, &area, brush);
            ultra_bar_native_ui::fonts::delete_object(brush);
            SetBkMode(surface.dc, TRANSPARENT as i32);
            SetTextColor(surface.dc, p.text);
            let old = SelectObject(surface.dc, GetStockObject(DEFAULT_GUI_FONT));
            for (index, title) in ["运行中 · 浮起", "当前激活 · 凹陷", "悬停 · 浮起"]
                .iter()
                .enumerate()
            {
                let x = 24 + index as i32 * 244;
                let mut label = RECT {
                    left: x,
                    top: 12,
                    right: x + 220,
                    bottom: 40,
                };
                DrawTextW(
                    surface.dc,
                    crate::visual::wide(title).as_ptr(),
                    -1,
                    &mut label,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                let rect = RECT {
                    left: x + 6,
                    top: 58,
                    right: x + 214,
                    bottom: 90,
                };
                draw(&surface, &rect, 96, index == 1, style, false, index == 2);
                let small = RECT {
                    left: x + 6,
                    top: 116,
                    right: x + 40,
                    bottom: 138,
                };
                draw(&surface, &small, 96, index == 1, style, false, index == 2);
            }
            SelectObject(surface.dc, old);
            surface.save_bmp(&directory.join(format!("task-relief-{name}.bmp")))?;
        }
        theme::set_accent(original);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key(pressed: bool) -> Key {
        Key {
            width: 100,
            height: 32,
            dpi: 96,
            accent: theme::DEFAULT_ACCENT,
            pressed,
            style: ActiveStyle::Inset,
            attention: false,
            hover: false,
        }
    }
    fn light(pixel: u32) -> f64 {
        Hsl::from_rgb(pixel & 0xffffff).l
    }
    #[test]
    fn raised_and_inset_follow_one_upper_left_light() {
        let raised = key(false);
        let recessed = key(true);
        let mat = material(raised.accent);
        assert!(
            light(sample(raised, mat, -1., 16.)) > light(sample(raised, mat, 101., 16.)),
            "raised left side receives reflected light"
        );
        assert!(
            light(sample(recessed, mat, 1., 16.)) < light(sample(recessed, mat, 99., 16.)),
            "cavity's left wall casts the inner shadow"
        );
        assert!(
            light(sample(raised, mat, 50., 16.)) > light(sample(recessed, mat, 50., 16.)),
            "recessed floor must be darker than raised material"
        );
    }
    #[test]
    fn markers_preserve_the_inset_floor_and_use_a_separate_lower_lane() {
        for dpi in [96, 144, 192] {
            let scale = dpi as f64 / 96.;
            let base = Key {
                dpi,
                width: 40 * dpi / 96,
                height: 24 * dpi / 96,
                ..key(true)
            };
            let mat = material(base.accent);
            let center = (base.width as f64 / 2., base.height as f64 / 2.);
            for style in [ActiveStyle::Underline, ActiveStyle::TopDot] {
                let active = Key { style, ..base };
                assert_eq!(
                    sample(active, mat, center.0, center.1),
                    sample(base, mat, center.0, center.1)
                );
                assert_eq!(sample(active, mat, -1., center.1), 0);
                let raised = Key {
                    pressed: false,
                    style,
                    ..base
                };
                assert_eq!(
                    sample(raised, mat, center.0, center.1),
                    sample(
                        Key {
                            style: ActiveStyle::Inset,
                            ..raised
                        },
                        mat,
                        center.0,
                        center.1
                    )
                );
            }
            let underline = Key {
                style: ActiveStyle::Underline,
                ..base
            };
            assert_eq!(
                sample(underline, mat, center.0, base.height as f64 - 2.5 * scale),
                sample(base, mat, center.0, base.height as f64 - 2.5 * scale)
            );
            assert_eq!(
                sample(underline, mat, center.0, base.height as f64 + 1.5 * scale),
                0,
                "clear gap below the cavity"
            );
            let bar = sample(underline, mat, center.0, base.height as f64 + 4. * scale);
            assert_eq!(bar >> 24, 255);
            assert!(light(bar) > 0.6);
            assert_ne!(
                sample(
                    underline,
                    mat,
                    center.0 + 9. * scale,
                    base.height as f64 + 4. * scale
                ),
                0,
                "indicator is wider than the former twelve-pixel marker"
            );
            let dot = Key {
                style: ActiveStyle::TopDot,
                ..base
            };
            assert!(
                light(sample(dot, mat, center.0, 2.5 * scale))
                    < light(sample(base, mat, center.0, 2.5 * scale))
            );
        }
    }
    #[test]
    fn shadow_colours_keep_primary_hue_and_avoid_luminous_saturation() {
        for accent in [0x69b4ff, 0xb96df0, 0x36c7a1] {
            let mat = material(accent);
            let base = Hsl::from_rgb(accent);
            for rgb in [mat.light, mat.dark, mat.inner_light, mat.inner_dark] {
                let shade = Hsl::from_rgb(rgb);
                assert!(shade.s < 0.3 && shade.l < 0.3);
                if shade.l > 0.05 {
                    let difference = (shade.h - base.h + 180.).rem_euclid(360.) - 180.;
                    assert!(difference.abs() < 16.);
                }
            }
        }
    }
}
