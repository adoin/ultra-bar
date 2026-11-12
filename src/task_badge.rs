use crate::visual::{
    Surface,
    text::draw_text,
    theme::{self, Hsl},
    wide,
};
use std::{cell::RefCell, collections::VecDeque};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};
struct Font(HFONT);
impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            ultra_bar_native_ui::fonts::delete_object(self.0);
        }
    }
}
thread_local! {static FONTS:RefCell<VecDeque<(i32,bool,Font)>>=const{RefCell::new(VecDeque::new())};}
pub unsafe fn draw(
    surface: &Surface,
    item: &RECT,
    icon: &RECT,
    count: usize,
    dpi: i32,
    small: bool,
    glass: bool,
) {
    unsafe {
        if count < 2 {
            return;
        }
        let unit = |v| v * dpi / 96;
        let label = if count > 99 {
            "99+".into()
        } else {
            count.to_string()
        };
        FONTS.with(|fonts| {
            let mut fonts = fonts.borrow_mut();
            let i = if let Some(i) = fonts.iter().position(|(d, s, _)| *d == dpi && *s == small) {
                i
            } else {
                if fonts.len() >= 8 {
                    fonts.pop_front();
                }
                let font = ultra_bar_native_ui::fonts::create_font(
                    -unit(if small { 10 } else { 11 }),
                    0,
                    0,
                    0,
                    600,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as _,
                    0,
                    0,
                    CLEARTYPE_NATURAL_QUALITY as _,
                    0,
                    wide("Segoe UI").as_ptr(),
                );
                fonts.push_back((dpi, small, Font(font)));
                fonts.len() - 1
            };
            let dc = surface.dc;
            let old = SelectObject(dc, fonts[i].2.0);
            let color = SetTextColor(dc, 0xffffff);
            let height = unit(if small { 13 } else { 15 }).max(11);
            let mut measure = RECT {
                left: 0,
                top: 0,
                right: 100,
                bottom: height,
            };
            draw_text(
                dc,
                wide(&label).as_ptr(),
                -1,
                &mut measure,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
            let width = (measure.right - measure.left + unit(4)).max(height);
            let right = (icon.right + unit(4)).min(item.right - unit(1));
            let top = (icon.top - unit(2)).max(item.top);
            let rect = RECT {
                left: right - width,
                top,
                right,
                bottom: top + height,
            };
            let base = Hsl::from_rgb(theme::accent_rgb());
            let fill = Hsl {
                h: base.h,
                s: base.s * 0.95,
                l: base.l.clamp(0.30, 0.42),
            }
            .rgb();
            let mut pixels = vec![0; (width * height) as usize];
            let radius = height as f64 / 2.;
            for y in 0..height {
                for x in 0..width {
                    let qx =
                        (x as f64 + 0.5 - width as f64 / 2.).abs() - (width as f64 / 2. - radius);
                    let qy = (y as f64 + 0.5 - height as f64 / 2.).abs();
                    let distance = qx.max(0.).hypot(qy) - radius;
                    let alpha = ((0.5 - distance).clamp(0., 1.) * 255.).round() as u32;
                    pixels[(y * width + x) as usize] = (alpha << 24) | fill;
                }
            }
            surface.overlay_effect(rect.left, rect.top, width, height, &pixels, glass);
            let mut text = rect;
            draw_text(
                dc,
                wide(&label).as_ptr(),
                -1,
                &mut text,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SetTextColor(dc, color);
            SelectObject(dc, old);
        });
    }
}
