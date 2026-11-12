//! Shared DirectWrite grayscale text. No LCD subpixel fringes on translucent windows.
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    mem::{size_of, zeroed},
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::RECT as WRect,
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::HDC as WDC,
        },
    },
    core::{PCWSTR, Result},
};
use windows_sys::Win32::{Foundation::RECT, Graphics::Gdi::*};

#[derive(Clone)]
struct Target {
    pixels: *mut u32,
    width: i32,
    height: i32,
    effects: Rc<RefCell<Vec<u32>>>,
    cutouts: Rc<RefCell<Vec<u8>>>,
    foreground: Rc<RefCell<Vec<bool>>>,
}
thread_local! {
    static TARGETS:RefCell<HashMap<usize,Target>>=RefCell::new(HashMap::new());
    static RENDERER:RefCell<Option<Renderer>>=const{RefCell::new(None)};
    static COUNTS:std::cell::Cell<(u64,u64)>=const{std::cell::Cell::new((0,0))};
}
pub(crate) fn register(
    dc: HDC,
    pixels: *mut u32,
    width: i32,
    height: i32,
    effects: Rc<RefCell<Vec<u32>>>,
    cutouts: Rc<RefCell<Vec<u8>>>,
    foreground: Rc<RefCell<Vec<bool>>>,
) {
    TARGETS.with(|t| {
        t.borrow_mut().insert(
            dc as usize,
            Target {
                pixels,
                width,
                height,
                effects,
                cutouts,
                foreground,
            },
        );
    });
}
pub(crate) fn unregister(dc: HDC) {
    TARGETS.with(|t| {
        t.borrow_mut().remove(&(dc as usize));
    });
}
#[derive(Clone, PartialEq)]
struct Font {
    name: Vec<u16>,
    size: i32,
    weight: i32,
    italic: bool,
}
struct Renderer {
    write: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    formats: VecDeque<(Font, IDWriteTextFormat)>,
}
impl Renderer {
    unsafe fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let target = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.,
                dpiY: 96.,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            })?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Self {
                write,
                target,
                formats: VecDeque::new(),
            })
        }
    }
    unsafe fn render(&mut self, dc: HDC, text: &[u16], rect: &RECT, flags: u32) -> Result<i32> {
        unsafe {
            let mut log: LOGFONTW = zeroed();
            GetObjectW(
                GetCurrentObject(dc, OBJ_FONT as u32),
                size_of::<LOGFONTW>() as _,
                (&mut log as *mut LOGFONTW).cast(),
            );
            let length = log.lfFaceName.iter().position(|v| *v == 0).unwrap_or(31);
            let name = if length == 0 {
                crate::wide("Segoe UI")
            } else {
                log.lfFaceName[..=length].to_vec()
            };
            let key = Font {
                name: crate::wide(&crate::fonts::resolve_font(
                    GetCurrentObject(dc, OBJ_FONT as u32),
                    &String::from_utf16_lossy(&name[..name.len().saturating_sub(1)]),
                )),
                size: log.lfHeight.abs().max(1),
                weight: log.lfWeight,
                italic: log.lfItalic != 0,
            };
            let format = if let Some(i) = self.formats.iter().position(|(f, _)| f == &key) {
                let entry = self.formats.remove(i).unwrap();
                let format = entry.1.clone();
                self.formats.push_back(entry);
                format
            } else {
                let format = self.write.CreateTextFormat(
                    PCWSTR(key.name.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(key.weight),
                    if key.italic {
                        DWRITE_FONT_STYLE_ITALIC
                    } else {
                        DWRITE_FONT_STYLE_NORMAL
                    },
                    DWRITE_FONT_STRETCH_NORMAL,
                    key.size as f32,
                    windows::core::w!("zh-CN"),
                )?;
                if self.formats.len() >= 24 {
                    self.formats.pop_front();
                }
                self.formats.push_back((key, format.clone()));
                format
            };
            format.SetTextAlignment(if flags & DT_CENTER != 0 {
                DWRITE_TEXT_ALIGNMENT_CENTER
            } else if flags & DT_RIGHT != 0 {
                DWRITE_TEXT_ALIGNMENT_TRAILING
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            })?;
            format.SetParagraphAlignment(if flags & DT_VCENTER != 0 {
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER
            } else if flags & DT_BOTTOM != 0 {
                DWRITE_PARAGRAPH_ALIGNMENT_FAR
            } else {
                DWRITE_PARAGRAPH_ALIGNMENT_NEAR
            })?;
            format.SetWordWrapping(if flags & DT_SINGLELINE != 0 || flags & DT_WORDBREAK == 0 {
                DWRITE_WORD_WRAPPING_NO_WRAP
            } else {
                DWRITE_WORD_WRAPPING_WRAP
            })?;
            if flags & (DT_END_ELLIPSIS | DT_PATH_ELLIPSIS | DT_WORD_ELLIPSIS) != 0 {
                let sign = self.write.CreateEllipsisTrimmingSign(&format)?;
                format.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    &sign,
                )?;
            } else {
                format.SetTrimming(&DWRITE_TRIMMING::default(), None)?;
            }
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            self.target.BindDC(
                WDC(dc),
                &WRect {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                },
            )?;
            let color = GetTextColor(dc);
            let brush = self.target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: (color & 255) as f32 / 255.,
                    g: ((color >> 8) & 255) as f32 / 255.,
                    b: ((color >> 16) & 255) as f32 / 255.,
                    a: 1.,
                },
                None,
            )?;
            self.target.BeginDraw();
            self.target.DrawText(
                text,
                &format,
                &D2D_RECT_F {
                    left: 0.,
                    top: 0.,
                    right: width as f32,
                    bottom: height as f32,
                },
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            self.target.EndDraw(None, None)?;
            Ok(height)
        }
    }
}
struct Before {
    target: Target,
    pixels: Vec<(usize, u32, u32)>,
}
unsafe fn before(dc: HDC, rect: &RECT) -> Option<Before> {
    unsafe {
        let target = TARGETS.with(|targets| targets.borrow().get(&(dc as usize)).cloned())?;
        let mut points = [
            windows_sys::Win32::Foundation::POINT {
                x: rect.left,
                y: rect.top,
            },
            windows_sys::Win32::Foundation::POINT {
                x: rect.right,
                y: rect.bottom,
            },
        ];
        LPtoDP(dc, points.as_mut_ptr(), 2);
        let effects = target.effects.borrow();
        let cutouts = target.cutouts.borrow();
        let bg = crate::theme::colorref(crate::theme::palette().background);
        let hover = crate::theme::colorref(crate::theme::palette().hover);
        let mut pixels = Vec::new();
        for y in points[0].y.max(0)..points[1].y.min(target.height) {
            for x in points[0].x.max(0)..points[1].x.min(target.width) {
                let i = (y * target.width + x) as usize;
                let rgb = *target.pixels.add(i) & 0xffffff;
                let effect = effects.get(i).copied().unwrap_or(0);
                let mut alpha = if effect == 1 && rgb == 1 {
                    0
                } else if effect >> 24 != 0 && rgb == effect & 0xffffff {
                    effect >> 24
                } else if rgb == bg {
                    148
                } else if rgb == hover {
                    178
                } else {
                    255
                };
                if rgb == bg && effect == 0 {
                    alpha = alpha * (255 - cutouts.get(i).copied().unwrap_or(0) as u32) / 255;
                }
                pixels.push((i, rgb, alpha));
            }
        }
        drop(effects);
        drop(cutouts);
        Some(Before { target, pixels })
    }
}
unsafe fn finish(before: Before, foreground: u32) {
    unsafe {
        let mut effects = before.target.effects.borrow_mut();
        let mut ink = before.target.foreground.borrow_mut();
        if ink.is_empty() {
            ink.resize((before.target.width * before.target.height) as usize, false);
        }
        if effects.is_empty() {
            effects.resize((before.target.width * before.target.height) as usize, 0);
        }
        for (i, old, alpha) in before.pixels {
            let new = *before.target.pixels.add(i) & 0xffffff;
            *before.target.pixels.add(i) = new;
            if new == old {
                continue;
            }
            let shift = [0, 8, 16]
                .into_iter()
                .max_by_key(|s| {
                    (((foreground >> s) & 255) as i32 - ((old >> s) & 255) as i32).abs()
                })
                .unwrap();
            let denominator = ((foreground >> shift) & 255) as f64 - ((old >> shift) & 255) as f64;
            if denominator.abs() < 1. {
                continue;
            }
            let coverage = ((((new >> shift) & 255) as f64 - ((old >> shift) & 255) as f64)
                / denominator)
                .clamp(0., 1.);
            let a = (255. * coverage + alpha as f64 * (1. - coverage)).round() as u32;
            let rgb = if alpha == 0 { foreground } else { new };
            *before.target.pixels.add(i) = rgb;
            effects[i] = (a << 24) | rgb;
            ink[i] = true;
        }
    }
}
/// DrawText-compatible entry point. Native GDI remains the measurement/error fallback.
/// # Safety
/// The HDC, UTF-16 buffer and rectangle must be valid on the calling UI thread.
pub unsafe fn draw_text(
    dc: HDC,
    value: *const u16,
    count: i32,
    rect: *mut RECT,
    flags: u32,
) -> i32 {
    unsafe {
        if value.is_null() || rect.is_null() {
            return 0;
        }
        if flags & DT_CALCRECT != 0
            || flags & DT_MODIFYSTRING != 0
            || (*rect).right <= (*rect).left
            || (*rect).bottom <= (*rect).top
        {
            return DrawTextW(dc, value, count, rect, flags);
        }
        let length = if count >= 0 {
            count as usize
        } else {
            let mut n = 0;
            while n < 65536 && *value.add(n) != 0 {
                n += 1;
            }
            n
        };
        if length == 0 {
            return 0;
        }
        GdiFlush();
        let snapshot = before(dc, &*rect);
        let color = crate::theme::colorref(GetTextColor(dc));
        let result = RENDERER.with(|slot| {
            let Ok(mut renderer) = slot.try_borrow_mut() else {
                return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x80004005u32 as i32,
                )));
            };
            if renderer.is_none() {
                *renderer = Renderer::new().ok();
            }
            match renderer.as_mut() {
                Some(r) => r.render(dc, std::slice::from_raw_parts(value, length), &*rect, flags),
                None => Err(windows::core::Error::from_win32()),
            }
        });
        GdiFlush();
        if let Some(snapshot) = snapshot {
            finish(snapshot, color);
        }
        match result {
            Ok(height) => {
                COUNTS.with(|c| {
                    let (ok, fail) = c.get();
                    c.set((ok + 1, fail));
                });
                height
            }
            Err(_) => {
                COUNTS.with(|c| {
                    let (ok, fail) = c.get();
                    c.set((ok, fail + 1));
                });
                RENDERER.with(|r| {
                    if let Ok(mut r) = r.try_borrow_mut() {
                        *r = None;
                    }
                });
                DrawTextW(dc, value, count, rect, flags)
            }
        }
    }
}
/// Render typography fixtures without capturing or controlling other applications.
/// # Safety
/// Invoke on a Windows rendering thread.
pub unsafe fn smoke(directory: &std::path::Path) -> std::io::Result<()> {
    unsafe {
        std::fs::create_dir_all(directory)?;
        for dpi in [96, 120, 144, 192] {
            let scale = |n| n * dpi / 96;
            let surface = crate::Surface::new(scale(720), scale(190))
                .ok_or_else(std::io::Error::last_os_error)?;
            let area = RECT {
                left: 0,
                top: 0,
                right: scale(720),
                bottom: scale(190),
            };
            FillRect(surface.dc, &area, crate::theme::background_brush());
            SetBkMode(surface.dc, TRANSPARENT as _);
            SetTextColor(surface.dc, crate::theme::palette().text);
            let mut old: HGDIOBJ = std::ptr::null_mut();
            for (row, face, size) in [(0, "Segoe UI", 13), (1, "Segoe MDL2 Assets", 18)] {
                let font = crate::fonts::create_font(
                    -scale(size),
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
                    crate::wide(face).as_ptr(),
                );
                let previous = SelectObject(surface.dc, font);
                if old.is_null() {
                    old = previous;
                }
                for (column, value) in [
                    "主色  日历  新建日程",
                    "H 216°  S 79%  L 54%",
                    "#2D79E7  Codex 35%",
                ]
                .into_iter()
                .enumerate()
                {
                    let text = crate::wide(if row == 1 {
                        "\u{e73e}  \u{e711}  \u{e721}"
                    } else {
                        value
                    });
                    let rect = RECT {
                        left: scale(12 + column as i32 * 238),
                        top: scale(12 + row * 86),
                        right: scale(240 + column as i32 * 238),
                        bottom: scale(42 + row * 86),
                    };
                    let mut old_rect = rect;
                    DrawTextW(
                        surface.dc,
                        text.as_ptr(),
                        -1,
                        &mut old_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                    );
                    let mut new_rect = RECT {
                        top: rect.top + scale(32),
                        bottom: rect.bottom + scale(32),
                        ..rect
                    };
                    draw_text(
                        surface.dc,
                        text.as_ptr(),
                        -1,
                        &mut new_rect,
                        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                    );
                }
                SelectObject(surface.dc, previous);
                crate::fonts::delete_object(font);
            }
            SelectObject(surface.dc, old);
            surface.save_bmp(&directory.join(format!("text-aa-{dpi}.bmp")))?;
            let edges = surface
                .effects
                .borrow()
                .iter()
                .filter(|p| {
                    let a = **p >> 24;
                    a > 148 && a < 255
                })
                .count();
            if edges < 20 {
                return Err(std::io::Error::other(
                    "Text antialias coverage was not preserved",
                ));
            }
        }
        let (ok, fallback) = COUNTS.with(|c| c.get());
        std::fs::write(
            directory.join("text-aa.txt"),
            format!("DirectWrite draws: {ok}\nGDI fallbacks: {fallback}\n"),
        )?;
        if ok == 0 || fallback != 0 {
            return Err(std::io::Error::other(
                "DirectWrite rendering did not initialize",
            ));
        }
        Ok(())
    }
}
