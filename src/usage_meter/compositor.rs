//! DWM advances a repeating visual transform. No application frame timer or render loop.
use super::*;
use anyhow::{Context, Result};
use ultra_bar_plugin_sdk::Page;
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, POINT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            DirectComposition::*,
            Dxgi::{Common::*, IDXGIDevice, IDXGISurface, IDXGISurface2},
        },
    },
    core::Interface,
};
#[derive(Clone, PartialEq)]
pub struct Meter {
    pub id: String,
    pub bounds: (i32, i32, i32, i32),
    pub progress: Option<f64>,
    pub text: String,
    pub trailing: String,
    pub previous_trailing: String,
    pub previous: Option<String>,
    pub phase: f64,
    pub font: usize,
    pub padding: i32,
    pub accent: u32,
    pub tooltip: String,
}
struct Node {
    spec: Meter,
    visual: IDCompositionVisual,
    _input: InputRegion,
}
/// Input is independent of the layered backing pixels; no redirection bitmap is drawn.
struct InputRegion {
    hwnd: windows_sys::Win32::Foundation::HWND,
    _tips: crate::visual::Tooltips,
}
impl Drop for InputRegion {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
        }
    }
}
impl InputRegion {
    unsafe fn new(
        owner: windows_sys::Win32::Foundation::HWND,
        bounds: (i32, i32, i32, i32),
        tooltip: &str,
    ) -> Result<Self> {
        use windows_sys::Win32::{System::LibraryLoader::*, UI::WindowsAndMessaging::*};
        unsafe {
            let class = crate::visual::wide("UltraBar.TransparentMeter.Input");
            let module = GetModuleHandleW(std::ptr::null());
            RegisterClassW(&WNDCLASSW {
                hInstance: module,
                lpszClassName: class.as_ptr(),
                lpfnWndProc: Some(input_proc),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                ..std::mem::zeroed()
            });
            let (l, t, r, b) = bounds;
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                l,
                t,
                r - l,
                b - t,
                owner,
                std::ptr::null_mut(),
                module,
                std::ptr::null(),
            );
            anyhow::ensure!(
                !hwnd.is_null(),
                "Cannot create transparent meter input region"
            );
            let mut tips = crate::visual::Tooltips::new(hwnd);
            tips.sync(vec![crate::visual::Tip {
                bounds: (0, 0, r - l, b - t),
                text: tooltip.into(),
            }]);
            let input = Self { hwnd, _tips: tips };
            input.position(owner, bounds);
            Ok(input)
        }
    }
    unsafe fn position(
        &self,
        owner: windows_sys::Win32::Foundation::HWND,
        bounds: (i32, i32, i32, i32),
    ) {
        use windows_sys::Win32::{
            Foundation::POINT, Graphics::Gdi::ClientToScreen, UI::WindowsAndMessaging::*,
        };
        unsafe {
            let (l, t, r, b) = bounds;
            let mut pt = POINT { x: l, y: t };
            ClientToScreen(owner, &mut pt);
            let mut current = std::mem::zeroed();
            GetWindowRect(self.hwnd, &mut current);
            if current.left != pt.x
                || current.top != pt.y
                || current.right - current.left != r - l
                || current.bottom - current.top != b - t
            {
                SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    pt.x,
                    pt.y,
                    r - l,
                    b - t,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
            }
            if IsWindowVisible(owner) != IsWindowVisible(self.hwnd) {
                ShowWindow(
                    self.hwnd,
                    if IsWindowVisible(owner) != 0 {
                        SW_SHOWNOACTIVATE
                    } else {
                        SW_HIDE
                    },
                );
            }
        }
    }
}
unsafe extern "system" fn input_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    w: usize,
    l: isize,
) -> isize {
    use windows_sys::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MapWindowPoints, ValidateRect},
        UI::WindowsAndMessaging::*,
    };
    unsafe {
        let owner = GetParent(hwnd);
        match message {
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                ValidateRect(hwnd, std::ptr::null());
                0
            }
            WM_NCHITTEST => HTCLIENT as _,
            WM_MOUSEACTIVATE => SendMessageW(owner, message, w, l),
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN
            | WM_RBUTTONUP => {
                let mut point = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                MapWindowPoints(hwnd, owner, &mut point, 1);
                SendMessageW(
                    owner,
                    message,
                    w,
                    ((point.x as u16 as u32) | ((point.y as u16 as u32) << 16)) as isize,
                )
            }
            WM_MOUSEWHEEL | WM_CONTEXTMENU => SendMessageW(owner, message, w, l),
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
pub struct Compositor {
    owner: windows_sys::Win32::Foundation::HWND,
    pub(crate) device: IDCompositionDevice,
    context: ID3D11DeviceContext,
    _gpu: ID3D11Device,
    _target: IDCompositionTarget,
    pub(crate) root: IDCompositionVisual,
    nodes: Vec<Node>,
    pub uploads: u64,
    pub commits: u64,
}
fn premultiply(pixel: u32) -> u32 {
    let a = pixel >> 24;
    (a << 24)
        | (((pixel >> 16) & 255) * a / 255) << 16
        | (((pixel >> 8) & 255) * a / 255) << 8
        | ((pixel & 255) * a / 255)
}
impl Compositor {
    pub unsafe fn input_visibility(&self, visible: bool) {
        unsafe {
            for node in &self.nodes {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    node._input.hwnd,
                    if visible {
                        windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE
                    } else {
                        windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE
                    },
                );
            }
        }
    }
    pub unsafe fn input_position(&self) {
        unsafe {
            for node in &self.nodes {
                node._input.position(self.owner, node.spec.bounds);
            }
        }
    }
    pub unsafe fn new(hwnd: windows_sys::Win32::Foundation::HWND) -> Result<Self> {
        unsafe { Self::new_layer(hwnd, true) }
    }
    pub(crate) unsafe fn new_layer(
        hwnd: windows_sys::Win32::Foundation::HWND,
        topmost: bool,
    ) -> Result<Self> {
        unsafe {
            let mut gpu = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut gpu),
                None,
                Some(&mut context),
            )?;
            let gpu = gpu.context("No hardware D3D11 device")?;
            let dxgi: IDXGIDevice = gpu.cast()?;
            let device: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = device.CreateTargetForHwnd(HWND(hwnd), topmost)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            device.Commit()?;
            Ok(Self {
                owner: hwnd,
                device,
                context: context.context("No D3D context")?,
                _gpu: gpu,
                _target: target,
                root,
                nodes: vec![],
                uploads: 0,
                commits: 1,
            })
        }
    }
    unsafe fn upload(&mut self, w: i32, h: i32, pixels: &[u32]) -> Result<IDCompositionSurface> {
        unsafe {
            let surface = self.device.CreateSurface(
                w as u32,
                h as u32,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?;
            let mut offset = POINT::default();
            let buffer: IDXGISurface = surface.BeginDraw(None, &mut offset)?;
            let result = (|| -> Result<()> {
                let extended: IDXGISurface2 = buffer.cast()?;
                let mut index = 0;
                let texture: ID3D11Texture2D = extended.GetResource(&mut index)?;
                let region = D3D11_BOX {
                    left: offset.x as u32,
                    top: offset.y as u32,
                    front: 0,
                    right: (offset.x + w) as u32,
                    bottom: (offset.y + h) as u32,
                    back: 1,
                };
                let pixels: Vec<_> = pixels.iter().copied().map(premultiply).collect();
                self.context.UpdateSubresource(
                    &texture,
                    index,
                    Some(&region),
                    pixels.as_ptr().cast(),
                    w as u32 * 4,
                    0,
                );
                Ok(())
            })();
            surface.EndDraw()?;
            result?;
            self.uploads += 1;
            Ok(surface)
        }
    }
    pub(crate) unsafe fn image(
        &mut self,
        w: i32,
        h: i32,
        pixels: &[u32],
    ) -> Result<IDCompositionVisual> {
        unsafe {
            let surface = self.upload(w, h, pixels)?;
            let visual = self.device.CreateVisual()?;
            visual.SetContent(&surface)?;
            Ok(visual)
        }
    }
    unsafe fn clip(&self, w: f32, h: f32) -> Result<IDCompositionRectangleClip> {
        unsafe {
            let clip = self.device.CreateRectangleClip()?;
            clip.SetLeft2(0.)?;
            clip.SetTop2(0.)?;
            clip.SetRight2(w)?;
            clip.SetBottom2(h)?;
            let r = (h / 2.).min(w / 2.);
            clip.SetTopLeftRadiusX2(r)?;
            clip.SetTopLeftRadiusY2(r)?;
            clip.SetTopRightRadiusX2(r)?;
            clip.SetTopRightRadiusY2(r)?;
            clip.SetBottomLeftRadiusX2(r)?;
            clip.SetBottomLeftRadiusY2(r)?;
            clip.SetBottomRightRadiusX2(r)?;
            clip.SetBottomRightRadiusY2(r)?;
            Ok(clip)
        }
    }
    unsafe fn build(&mut self, spec: &Meter) -> Result<IDCompositionVisual> {
        unsafe {
            let (l, t, r, b) = spec.bounds;
            let (w, h) = (r - l, b - t);
            anyhow::ensure!(w > 1 && h > 1, "Invalid meter bounds");
            let fill = spec
                .progress
                .filter(|n| n.is_finite())
                .map(|n| (n.clamp(0., 1.) * FILL_SCALE as f64).round() as i32)
                .unwrap_or(-1);
            let visual = self.device.CreateVisual()?;
            visual.SetOffsetX2(l as f32)?;
            visual.SetOffsetY2(t as f32)?;
            let bounds = fill_bounds(Key {
                width: w,
                height: h,
                fill,
                accent: spec.accent,
            });
            if fill > 0 {
                let background = self.image(
                    w,
                    h,
                    &fill_pixels(Key {
                        width: w,
                        height: h,
                        fill,
                        accent: spec.accent,
                    }),
                )?;
                visual.AddVisual(&background, false, None)?;
            }
            if let Some(bounds) = bounds {
                let container = self.device.CreateVisual()?;
                container.SetOffsetX2(bounds.left as f32)?;
                container.SetOffsetY2(bounds.top as f32)?;
                container
                    .SetClip(&self.clip((w - 2 * bounds.left) as f32, bounds.height as f32)?)?;
                let cropped = self.device.CreateVisual()?;
                let amount = self.device.CreateRectangleClip()?;
                amount.SetLeft2(0.)?;
                amount.SetTop2(0.)?;
                amount.SetRight2(bounds.clip_width as f32)?;
                amount.SetBottom2(bounds.height as f32)?;
                cropped.SetClip(&amount)?;
                let period = bounds.height * 2;
                let alpha = texture_at_scale(bounds.width, period, bounds.height as f64 / 32.);
                let pixels: Vec<_> = alpha
                    .into_iter()
                    .map(|a| (((a * BUBBLE_OPACITY * 255.).round() as u32) << 24) | 0xffffff)
                    .collect();
                let surface = self.upload(bounds.width, period, &pixels)?;
                let moving = self.device.CreateVisual()?;
                for y in [0, period] {
                    let tile = self.device.CreateVisual()?;
                    tile.SetContent(&surface)?;
                    tile.SetOffsetY2(y as f32)?;
                    moving.AddVisual(&tile, false, None)?;
                }
                if spec.progress.is_some_and(|n| n > 0. && n < 1.) {
                    let animation = self.device.CreateAnimation()?;
                    animation.AddCubic(0., 0., -(period as f32) / 2., 0., 0.)?;
                    animation.AddRepeat(2., 2.)?;
                    moving.SetOffsetY(&animation)?;
                }
                cropped.AddVisual(&moving, false, None)?;
                container.AddVisual(&cropped, false, None)?;
                visual.AddVisual(&container, false, None)?;
            }
            let inset = self.image(w, h, &super::inset_pixels(w, h, spec.accent))?;
            visual.AddVisual(&inset, false, None)?;
            let caption = caption_pixels(spec, w, h)?;
            for (radius, dy, opacity) in [(2, 1, 0.55), (1, 0, 0.95)] {
                let shadow = super::text_shadow(&caption, w, h, radius, dy, opacity);
                let shade = self.image(w, h, &shadow)?;
                visual.AddVisual(&shade, false, None)?;
            }
            let label = self.image(w, h, &caption)?;
            visual.AddVisual(&label, false, None)?;
            Ok(visual)
        }
    }
    pub unsafe fn sync(&mut self, specs: Vec<Meter>) -> Result<()> {
        unsafe {
            let mut dirty = false;
            for i in (0..self.nodes.len()).rev() {
                if !specs.iter().any(|s| s.id == self.nodes[i].spec.id) {
                    let old = self.nodes.remove(i);
                    self.root.RemoveVisual(&old.visual)?;
                    dirty = true;
                }
            }
            for spec in specs {
                if let Some(i) = self.nodes.iter().position(|n| n.spec.id == spec.id) {
                    if self.nodes[i].spec == spec {
                        self.nodes[i]._input.position(self.owner, spec.bounds);
                        continue;
                    }
                    let visual = self.build(&spec)?;
                    let old = self.nodes.remove(i);
                    self.root.RemoveVisual(&old.visual)?;
                    self.root.AddVisual(&visual, false, None)?;
                    let input = InputRegion::new(self.owner, spec.bounds, &spec.tooltip)?;
                    self.nodes.push(Node {
                        spec,
                        visual,
                        _input: input,
                    });
                    dirty = true;
                } else {
                    let visual = self.build(&spec)?;
                    self.root.AddVisual(&visual, false, None)?;
                    let input = InputRegion::new(self.owner, spec.bounds, &spec.tooltip)?;
                    self.nodes.push(Node {
                        spec,
                        visual,
                        _input: input,
                    });
                    dirty = true;
                }
            }
            if dirty {
                self.device.Commit()?;
                self.commits += 1;
            }
            Ok(())
        }
    }
}
pub(super) unsafe fn caption_pixels(spec: &Meter, w: i32, h: i32) -> Result<Vec<u32>> {
    unsafe {
        let surface = Surface::new(w, h).context("Cannot rasterize meter label")?;
        let area = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        FillRect(surface.dc, &area, GetStockObject(BLACK_BRUSH) as _);
        let old = SelectObject(surface.dc, spec.font as _);
        SetBkMode(surface.dc, TRANSPARENT as _);
        SetTextColor(surface.dc, 0xffffff);
        let displacement = (h as f64 * (1. - spec.phase)) as i32;
        let mut rect = RECT {
            left: spec.padding,
            top: displacement,
            right: w - spec.padding,
            bottom: h + displacement,
        };
        let flags = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
        super::caption(surface.dc, &rect, &spec.text, &spec.trailing, flags);
        if let Some(old) = &spec.previous {
            rect.top -= h;
            rect.bottom -= h;
            super::caption(surface.dc, &rect, old, &spec.previous_trailing, flags);
        }
        SelectObject(surface.dc, old);
        let pixels = surface.copy_pixels();
        let color = theme::colorref(theme::palette().text);
        let mut result = vec![0; pixels.len()];
        for (i, p) in pixels.into_iter().enumerate() {
            let a = (p & 255).max((p >> 8) & 255).max((p >> 16) & 255);
            result[i] = (a << 24) | color;
        }
        Ok(result)
    }
}
pub fn spec(
    id: &str,
    rect: &RECT,
    page: &Page,
    old: Option<&Page>,
    phase: f64,
    font: HFONT,
    padding: i32,
) -> Meter {
    Meter {
        id: id.into(),
        bounds: (rect.left, rect.top, rect.right, rect.bottom),
        progress: page.progress,
        text: page.text.clone(),
        trailing: page.trailing.clone(),
        previous_trailing: old.map(|p| p.trailing.clone()).unwrap_or_default(),
        previous: old.map(|p| p.text.clone()),
        phase,
        font: font as usize,
        padding,
        accent: theme::accent_rgb(),
        tooltip: page.tooltip.clone(),
    }
}

/// Verify visible compositor motion while the application UI thread does no frame rendering.
pub unsafe fn smoke(directory: &std::path::Path) -> Result<()> {
    use windows_sys::Win32::{
        Graphics::Dwm::*,
        System::{LibraryLoader::*, Threading::*},
        UI::WindowsAndMessaging::*,
    };
    unsafe {
        anyhow::ensure!(
            directory.is_absolute(),
            "Absolute output directory required"
        );
        std::fs::create_dir_all(directory)?;
        let class = crate::visual::wide("UltraBar.GpuMeter.Probe");
        let module = GetModuleHandleW(std::ptr::null());
        RegisterClassW(&WNDCLASSW {
            hInstance: module,
            lpszClassName: class.as_ptr(),
            lpfnWndProc: Some(DefWindowProcW),
            ..std::mem::zeroed()
        });
        // A static owned backdrop avoids sampling the user's changing wallpaper.
        struct Window(windows_sys::Win32::Foundation::HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                unsafe {
                    DestroyWindow(self.0);
                }
            }
        }
        let underneath = 0x00312720u32;
        let backdrop_class = crate::visual::wide("UltraBar.GpuMeter.Backdrop");
        RegisterClassW(&WNDCLASSW {
            hInstance: module,
            lpszClassName: backdrop_class.as_ptr(),
            lpfnWndProc: Some(DefWindowProcW),
            hbrBackground: CreateSolidBrush(underneath),
            ..std::mem::zeroed()
        });
        let backdrop = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            backdrop_class.as_ptr(),
            crate::visual::wide("").as_ptr(),
            WS_POPUP,
            120,
            120,
            260,
            48,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        anyhow::ensure!(!backdrop.is_null(), "Cannot create static probe backdrop");
        let _backdrop = Window(backdrop);
        ShowWindow(backdrop, SW_SHOWNOACTIVATE);
        UpdateWindow(backdrop);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class.as_ptr(),
            crate::visual::wide("Ultra Bar").as_ptr(),
            WS_POPUP,
            120,
            120,
            260,
            48,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        anyhow::ensure!(!hwnd.is_null(), "Cannot create composition probe");
        let _window = Window(hwnd);
        let background = Surface::new(260, 48).context("Cannot create probe background")?;
        let area = RECT {
            left: 0,
            top: 0,
            right: 260,
            bottom: 48,
        };
        let brush = CreateSolidBrush(theme::palette().background);
        FillRect(background.dc, &area, brush);
        DeleteObject(brush);
        background.clear_rounded(
            &RECT {
                left: 10,
                top: 8,
                right: 250,
                bottom: 40,
            },
            16.,
        );
        background.present(hwnd, false);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let mut composition = Compositor::new(hwnd)?;
        let page = Page {
            id: "Codex".into(),
            text: "Codex 63%".into(),
            trailing: "↺2".into(),
            tooltip: String::new(),
            progress: Some(0.63),
        };
        let rect = RECT {
            left: 10,
            top: 8,
            right: 250,
            bottom: 40,
        };
        let spec = spec(
            "probe",
            &rect,
            &page,
            None,
            1.,
            GetStockObject(DEFAULT_GUI_FONT) as _,
            8,
        );
        composition.sync(vec![spec.clone()])?;
        composition.device.WaitForCommitCompletion()?;
        anyhow::ensure!(DwmFlush() >= 0, "DWM flush failed");
        let capture = |name: &str| -> Result<Vec<u32>> {
            let surface = Surface::new(260, 48).context("Cannot capture probe")?;
            let screen = GetDC(std::ptr::null_mut());
            let copied = BitBlt(
                surface.dc,
                0,
                0,
                260,
                48,
                screen,
                120,
                120,
                SRCCOPY | CAPTUREBLT,
            );
            ReleaseDC(std::ptr::null_mut(), screen);
            anyhow::ensure!(copied != 0, "Cannot capture own probe window");
            surface.save_bmp(&directory.join(name))?;
            Ok(surface.copy_pixels())
        };
        std::thread::sleep(std::time::Duration::from_millis(180));
        let before = capture("gpu-before.bmp")?;
        let cpu = || {
            let mut creation = std::mem::zeroed();
            let mut exit = std::mem::zeroed();
            let mut kernel: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
            let mut user: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            );
            ((kernel.dwHighDateTime as u64) << 32 | kernel.dwLowDateTime as u64)
                + ((user.dwHighDateTime as u64) << 32 | user.dwLowDateTime as u64)
        };
        let start = cpu();
        let uploads = composition.uploads;
        let commits = composition.commits;
        // Intentionally no timer, repaint, upload, Commit or message dispatch during this interval.
        std::thread::sleep(std::time::Duration::from_millis(670));
        let idle_cpu_ms = (cpu() - start) as f64 / 10000.;
        anyhow::ensure!(DwmFlush() >= 0, "DWM flush failed");
        let after = capture("gpu-after.bmp")?;
        // Keep the transparency sample away from the right-aligned suffix and its shadow.
        let empty_pixel = after[24 * 260 + 200] & 0xffffff;
        let rounded_corner_intact =
            after[8 * 260 + 10] & 0xffffff == theme::colorref(theme::palette().background);
        let transparent = empty_pixel == theme::colorref(underneath);
        let hit = WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: 350, y: 144 });
        let empty_region_receives_input =
            GetAncestor(hit, GA_ROOT) == hwnd || GetParent(hit) == hwnd;
        let differences = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        let label_bright_pixels = (16..32)
            .flat_map(|y| (18..100).map(move |x| y * 260 + x))
            .filter(|i| {
                let p = after[*i];
                ((p >> 16) & 255) + ((p >> 8) & 255) + (p & 255) > 650
            })
            .count();
        composition.sync(vec![spec.clone()])?;
        let stable = uploads == composition.uploads && commits == composition.commits;
        let record = serde_json::json!({"hardware":true,"changed_pixels":differences,"idle_cpu_ms":idle_cpu_ms,"unchanged_uploads_and_commits":stable,"application_frame_timer":false,"uploads":uploads,"commits":commits,"label_bright_pixels":label_bright_pixels,"empty_pixel":empty_pixel,"backdrop_pixel":theme::colorref(underneath),"empty_pixel_matches_desktop":transparent,"empty_region_receives_input":empty_region_receives_input,"rounded_corner_intact":rounded_corner_intact});
        std::fs::write(
            directory.join("gpu-animation.json"),
            serde_json::to_vec_pretty(&record)?,
        )?;
        anyhow::ensure!(differences > 30, "Compositor did not visibly animate");
        anyhow::ensure!(stable, "Unchanged scene caused GPU uploads");
        anyhow::ensure!(
            label_bright_pixels > 10,
            "Caption was occluded by the fill layer"
        );
        anyhow::ensure!(transparent, "Empty region is not fully transparent");
        anyhow::ensure!(
            rounded_corner_intact,
            "Rounded corner was erased outside the progress silhouette"
        );
        anyhow::ensure!(
            empty_region_receives_input,
            "Transparent region lost mouse input"
        );
        let mut bare = spec.clone();
        bare.text.clear();
        bare.trailing.clear();
        bare.previous = None;
        bare.previous_trailing.clear();
        bare.progress = Some(0.);
        composition.sync(vec![bare.clone()])?;
        composition.device.WaitForCommitCompletion()?;
        DwmFlush();
        let empty = capture("gpu-empty.bmp")?;
        let mut cases = vec![];
        for progress in [0.01, 0.05, 1.0] {
            bare.progress = Some(progress);
            composition.sync(vec![bare.clone()])?;
            composition.device.WaitForCommitCompletion()?;
            DwmFlush();
            let actual = capture(&format!("gpu-fill-{}.bmp", (progress * 100.) as u32))?;
            let key = Key {
                width: 240,
                height: 32,
                fill: (progress * FILL_SCALE as f64).round() as i32,
                accent: bare.accent,
            };
            let bounds = fill_bounds(key).unwrap();
            let mut checked = 0;
            for y in 0..key.height {
                for x in 0..key.width {
                    let outside = distance(
                        (x - bounds.left) as f64 + 0.5,
                        (y - bounds.top) as f64 + 0.5,
                        (key.width - 2 * bounds.left) as f64,
                        bounds.height as f64,
                    ) > 1.2;
                    if outside {
                        let i = ((8 + y) * 260 + 10 + x) as usize;
                        let a = actual[i];
                        let b = empty[i];
                        let delta = [0, 8, 16]
                            .iter()
                            .map(|shift| {
                                (((a >> shift) & 255) as i32 - ((b >> shift) & 255) as i32).abs()
                            })
                            .max()
                            .unwrap();
                        anyhow::ensure!(
                            delta <= 3,
                            "GPU fill leaked outside the fixed inner capsule at {progress}: {x},{y}"
                        );
                        checked += 1;
                    }
                }
            }
            cases.push(serde_json::json!({"progress":progress,"outside_pixels_checked":checked,"fixed_capsule_clip":true}));
        }
        std::fs::write(
            directory.join("gpu-low-fill.json"),
            serde_json::to_vec_pretty(&cases)?,
        )?;
        Ok(())
    }
}
