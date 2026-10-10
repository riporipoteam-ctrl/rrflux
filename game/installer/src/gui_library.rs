//! Ripo Team Launcher v1.0.0 — game library UI.
//!
//! A game-library style launcher (inspired by Steam/EGS):
//! - Sidebar with Ripo Team branding, Library/Settings nav.
//! - Flux Rec game card: banner, logo, description, live player count,
//!   PLAY button, uninstall.
//! - Settings: light/dark theme toggle (default: light).
//! - PLAY flow: update check -> install update if needed -> launch game ->
//!   close library -> "Launching..." splash.
//!
//! Design rules (same as gui.rs):
//! * Raw Win32 only, no GUI framework.
//! * Never panic, never print; any failure returns and the caller proceeds.
//! * All Win32 code behind `#[cfg(windows)]`; other platforms get stubs.
//! * No `unwrap`/`expect`/`println` anywhere.

use std::sync::mpsc::{Receiver, Sender};

/// UI theme. Default is Light (Armin's order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

/// Config for the library window.
pub struct LibraryConfig {
    pub theme: Theme,
    pub version: String,
    pub cmd_tx: Sender<crate::library::LibCmd>,
}

/// Which view the library window is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Library,
    Detail,   // v1.0.4: game detail view with screenshot carousel
    Settings,
    Working, // PLAY was clicked; showing progress
}

// v1.0.4: embedded screenshots for the detail carousel.
#[cfg(windows)]
const SCREENSHOTS: &[&[u8]] = &[
    include_bytes!("../assets/screenshots/rec-center-1.jpg"),
    include_bytes!("../assets/screenshots/rec-center-2.jpg"),
    include_bytes!("../assets/screenshots/rec-center-3.jpg"),
    include_bytes!("../assets/screenshots/rec-center-4.jpg"),
    include_bytes!("../assets/screenshots/dorm-room.jpg"),
    include_bytes!("../assets/screenshots/paintball.jpg"),
];

#[cfg(windows)]
const SCREENSHOT_NAMES: &[&str] = &[
    "Rec Center",
    "Rec Center Stage",
    "Rec Center Marquee",
    "Rec Center Avatar",
    "Dorm Room",
    "Paintball",
];

// v1.0.5: the real Flux Rec logo (not the drawn approximation).
#[cfg(windows)]
const FLUXREC_LOGO_PNG: &[u8] = include_bytes!("../assets/fluxrec-logo.png");

// ---------------------------------------------------------------------------
// Non-Windows stubs.
// ---------------------------------------------------------------------------
#[cfg(not(windows))]
pub fn run_library_gui(_config: LibraryConfig, rx: Receiver<crate::library::LibMsg>) {
    while rx.recv().is_ok() {}
}

#[cfg(not(windows))]
pub fn run_splash_gui(_theme: Theme, rx: Receiver<()>) {
    let _ = rx.recv();
}

// ---------------------------------------------------------------------------
// Windows implementation.
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub fn run_library_gui(config: LibraryConfig, rx: Receiver<crate::library::LibMsg>) {
    let _ = imp::run_library(config, rx);
}

#[cfg(windows)]
pub fn run_splash_gui(theme: Theme, rx: Receiver<()>) {
    let _ = imp::run_splash(theme, rx);
}

#[cfg(windows)]
mod imp {
    use super::{LibraryConfig, Theme, View, SCREENSHOTS, SCREENSHOT_NAMES, FLUXREC_LOGO_PNG};
    use crate::library::{LibCmd, LibMsg};
    use std::ffi::c_void;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window size.
    const WIN_W: i32 = 1020;
    const WIN_H: i32 = 660;
    const SIDEBAR_W: i32 = 232;
    const TITLE_H: i32 = 48;
    const CORNER: i32 = 28;

    const SPLASH_W: i32 = 480;
    const SPLASH_H: i32 = 320;

    const TIMER_ID: usize = 1;
    const TIMER_MS: u32 = 50;

    // ---- Palettes (COLORREF = 0x00BBGGRR) ----
    struct Palette {
        bg: COLORREF,
        bg_title: COLORREF,
        bg_side: COLORREF,
        bg_card: COLORREF,
        bg_hover: COLORREF,
        text: COLORREF,
        text_dim: COLORREF,
        accent: COLORREF,
        accent_dark: COLORREF,
        track: COLORREF,
        border: COLORREF,
        banner_top: COLORREF,
        banner_bot: COLORREF,
    }

    const LIGHT: Palette = Palette {
        bg: COLORREF(0x00F5F3F0),         // #F0F3F5 warm light gray
        bg_title: COLORREF(0x00FFFFFF),   // white title bar
        bg_side: COLORREF(0x00FFFFFF),    // white sidebar
        bg_card: COLORREF(0x00FFFFFF),    // white cards
        bg_hover: COLORREF(0x00EBE6E0),   // #E0E6EB hover
        text: COLORREF(0x00211A12),       // #121A21 near-black
        text_dim: COLORREF(0x007A6F66),   // #666F7A dim
        accent: COLORREF(0x00FF9B2E),     // Flux blue #2E9BFF
        accent_dark: COLORREF(0x00CC7A1F),// darker blue
        track: COLORREF(0x00E0D8D2),      // #D2D8E0 track
        border: COLORREF(0x00D8D0C8),     // #C8D0D8 border
        banner_top: COLORREF(0x00FF9B2E), // blue gradient
        banner_bot: COLORREF(0x00A04A10), // deep blue
    };

    const DARK: Palette = Palette {
        bg: COLORREF(0x00170E0B),         // #0B0E17
        bg_title: COLORREF(0x0019110D),   // #0D1119
        bg_side: COLORREF(0x001F1411),    // #11141F
        bg_card: COLORREF(0x00281B16),    // #161B28
        bg_hover: COLORREF(0x00302622),   // #222630 hover
        text: COLORREF(0x00FFFFFF),
        text_dim: COLORREF(0x00B2A39A),   // #9AA3B2
        accent: COLORREF(0x00FF9B2E),
        accent_dark: COLORREF(0x00CC7A1F),
        track: COLORREF(0x003A2A23),
        border: COLORREF(0x003D2B23),
        banner_top: COLORREF(0x00FF9B2E),
        banner_bot: COLORREF(0x00803008),
    };

    fn pal(theme: Theme) -> &'static Palette {
        match theme {
            Theme::Light => &LIGHT,
            Theme::Dark => &DARK,
        }
    }

    // Button ids for hit-testing.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Hot {
        None,
        NavLibrary,
        NavSettings,
        Play,
        Uninstall,
        Details, // v1.0.6
        ThemeLight,
        ThemeDark,
        Close,
        Minimize,
        Card,       // v1.0.4: click game card -> detail view
        Back,       // v1.0.4: back from detail to library
        CarouselPrev,
        CarouselNext,
        DetailPlay, // PLAY button in detail view
    }

    struct State {
        theme: Theme,
        version: String,
        view: View,
        cmd_tx: Sender<LibCmd>,
        // Library state
        ready: bool,
        update_available: bool,
        update_version: String,
        players: Option<u32>,
        checking: bool,
        // Progress (Working view)
        percent: u8,
        stage: String,
        detail: String,
        // Animation
        hover: Hot,
        anim_t: u32,       // ticks for animations
        progress_anim: f32, // smoothed progress 0..100
        // v1.0.4: detail view carousel
        carousel_idx: usize,
        carousel_bitmaps: Vec<HBITMAP>, // decoded screenshots
        carousel_tick: u32,              // for auto-advance
        // v1.0.5: real Flux Rec logo bitmap
        logo_bitmap: HBITMAP,
        // v1.0.5: update check error message (empty if none)
        check_error: String,
    }

    // Button rects (computed per paint).
    struct Rects {
        nav_library: RECT,
        nav_settings: RECT,
        play: RECT,
        uninstall: RECT,
        details: RECT, // v1.0.6: explicit Details button
        theme_light: RECT,
        theme_dark: RECT,
        close: RECT,
        minimize: RECT,
        card: RECT,
        // v1.0.4: detail view
        back: RECT,
        carousel: RECT,
        carousel_prev: RECT,
        carousel_next: RECT,
        detail_play: RECT,
    }

    pub(super) fn run_library(
        config: LibraryConfig,
        rx: Receiver<LibMsg>,
    ) -> Silent {
        unsafe {
            let hinst = GetModuleHandleW(None).map_err(|_| ())?;

            let cls = w!("RipoTeamLauncherLib");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).map_err(|_| ())?,
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                lpszClassName: cls,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return Err(());
            }

            let mut state = Box::new(State {
                theme: config.theme,
                version: config.version,
                view: View::Library,
                cmd_tx: config.cmd_tx,
                ready: false,
                update_available: false,
                update_version: String::new(),
                players: None,
                checking: true,
                percent: 0,
                stage: String::new(),
                detail: String::new(),
                hover: Hot::None,
                anim_t: 0,
                progress_anim: 0.0,
                carousel_idx: 0,
                carousel_bitmaps: Vec::new(),
                carousel_tick: 0,
                logo_bitmap: HBITMAP(std::ptr::null_mut()),
                check_error: String::new(),
            });

            // v1.0.4: decode screenshots for the carousel (fail-soft).
            // Target size: 640x360 (16:9).
            for jpeg in SCREENSHOTS {
                let bmp = jpeg_to_bitmap(jpeg, 640, 360);
                if !bmp.is_invalid() {
                    state.carousel_bitmaps.push(bmp);
                }
            }
            // v1.0.5: decode the real Flux Rec logo (128x128).
            state.logo_bitmap = png_to_bitmap(FLUXREC_LOGO_PNG, 128, 128);
            let state_ptr = Box::into_raw(state);

            // Center on screen.
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let x = (sw - WIN_W) / 2;
            let y = (sh - WIN_H) / 2;

            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                cls,
                w!("Ripo Team Launcher"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                WIN_W,
                WIN_H,
                None,
                None,
                hinst,
                Some(state_ptr as *const c_void),
            )
            .map_err(|_| {
                let _ = Box::from_raw(state_ptr);
            })?;

            // Rounded corners.
            let rgn = CreateRoundRectRgn(0, 0, WIN_W + 1, WIN_H + 1, CORNER, CORNER);
            if !rgn.is_invalid() {
                SetWindowRgn(hwnd, rgn, true);
            }

            SetTimer(hwnd, TIMER_ID, TIMER_MS, None);

            // Pump LibMsg on the timer; run the message loop.
            let mut msg = MSG::default();
            loop {
                // Drain pending LibMsg (non-blocking) before blocking on messages.
                loop {
                    match rx.try_recv() {
                        Ok(m) => {
                            let st = &mut *state_ptr;
                            match m {
                                LibMsg::Ready { update_available, update_version } => {
                                    st.ready = true;
                                    st.checking = false;
                                    st.update_available = update_available;
                                    st.update_version = update_version;
                                    st.check_error.clear();
                                }
                                LibMsg::Checking => {
                                    st.checking = true;
                                    st.check_error.clear();
                                }
                                LibMsg::CheckFailed(e) => {
                                    st.checking = false;
                                    st.ready = true;
                                    st.check_error = e;
                                }
                                LibMsg::PlayerCount(c) => {
                                    st.players = Some(c);
                                }
                                LibMsg::Progress { percent, stage, detail } => {
                                    st.view = View::Working;
                                    st.percent = percent;
                                    if !stage.is_empty() {
                                        st.stage = stage;
                                    }
                                    if !detail.is_empty() {
                                        st.detail = detail;
                                    }
                                }
                                LibMsg::CloseGui => {
                                    KillTimer(hwnd, TIMER_ID);
                                    DestroyWindow(hwnd);
                                }
                            }
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }

                let ret = GetMessageW(&mut msg, None, 0, 0);
                if ret.0 == 0 {
                    break; // WM_QUIT
                }
                if ret.0 == -1 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);

                // Check if window was destroyed.
                if IsWindow(hwnd).as_bool() == false {
                    break;
                }
            }

            KillTimer(hwnd, TIMER_ID);
            let _ = Box::from_raw(state_ptr);
            Ok(())
        }
    }

    pub(super) fn run_splash(theme: Theme, rx: Receiver<()>) -> Silent {
        unsafe {
            let hinst = GetModuleHandleW(None).map_err(|_| ())?;
            let cls = w!("RipoTeamLauncherSplash");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(splash_proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).map_err(|_| ())?,
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                lpszClassName: cls,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return Err(());
            }

            let st = Box::new(SplashState {
                theme,
                t: 0,
            });
            let ptr = Box::into_raw(st);

            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW | WS_EX_TOPMOST,
                cls,
                w!("Launching Flux Rec"),
                WS_POPUP | WS_VISIBLE,
                (sw - SPLASH_W) / 2,
                (sh - SPLASH_H) / 2,
                SPLASH_W,
                SPLASH_H,
                None,
                None,
                hinst,
                Some(ptr as *const c_void),
            )
            .map_err(|_| {
                let _ = Box::from_raw(ptr);
            })?;

            let rgn = CreateRoundRectRgn(0, 0, SPLASH_W + 1, SPLASH_H + 1, 24, 24);
            if !rgn.is_invalid() {
                SetWindowRgn(hwnd, rgn, true);
            }
            SetTimer(hwnd, TIMER_ID, 50, None);

            // Wait for the close signal or 30s max.
            let mut msg = MSG::default();
            let start = std::time::Instant::now();
            loop {
                match rx.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }
                if start.elapsed().as_secs() > 30 {
                    break;
                }
                // Non-blocking message pump.
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                if IsWindow(hwnd).as_bool() == false {
                    break;
                }
            }

            KillTimer(hwnd, TIMER_ID);
            DestroyWindow(hwnd);
            let _ = Box::from_raw(ptr);
            Ok(())
        }
    }

    struct SplashState {
        theme: Theme,
        t: u32,
    }

    // ------------------------------------------------------------------
    // Drawing helpers
    // ------------------------------------------------------------------
    unsafe fn fill_rect(hdc: HDC, r: &RECT, color: COLORREF) -> Silent {
        let br = CreateSolidBrush(color);
        if br.is_invalid() {
            return Err(());
        }
        FillRect(hdc, r, br);
        DeleteObject(br);
        Ok(())
    }

    unsafe fn draw_text(
        hdc: HDC,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: COLORREF,
        size: i32,
        bold: bool,
        center: bool,
    ) -> Silent {
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let weight = if bold { FW_BOLD } else { FW_NORMAL };
        let font = CreateFontW(
            -size,
            0, 0, 0,
            weight.0 as i32,
            0, 0, 0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Segoe UI"),
        );
        if font.is_invalid() {
            return Err(());
        }
        let old = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        let mut r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let fmt = DT_WORDBREAK | if center { DT_CENTER } else { DT_LEFT };
        DrawTextW(hdc, &mut wide, &mut r, fmt);
        SelectObject(hdc, old);
        DeleteObject(font);
        Ok(())
    }

    /// Vertical gradient fill.
    unsafe fn fill_gradient(hdc: HDC, r: &RECT, top: COLORREF, bot: COLORREF) -> Silent {
        let h = (r.bottom - r.top).max(1);
        // Extract RGB (COLORREF is 0x00BBGGRR).
        let tr = (top.0 & 0xFF) as i32;
        let tg = ((top.0 >> 8) & 0xFF) as i32;
        let tb = ((top.0 >> 16) & 0xFF) as i32;
        let br = (bot.0 & 0xFF) as i32;
        let bg = ((bot.0 >> 8) & 0xFF) as i32;
        let bb = ((bot.0 >> 16) & 0xFF) as i32;
        // Draw in strips of 4px for speed.
        let mut y = r.top;
        while y < r.bottom {
            let t = (y - r.top) as f32 / h as f32;
            let cr = (tr as f32 + (br - tr) as f32 * t) as u32;
            let cg = (tg as f32 + (bg - tg) as f32 * t) as u32;
            let cb = (tb as f32 + (bb - tb) as f32 * t) as u32;
            let brush = CreateSolidBrush(COLORREF(cb << 16 | cg << 8 | cr));
            if !brush.is_invalid() {
                let strip = RECT {
                    left: r.left,
                    top: y,
                    right: r.right,
                    bottom: (y + 4).min(r.bottom),
                };
                FillRect(hdc, &strip, brush);
                DeleteObject(brush);
            }
            y += 4;
        }
        Ok(())
    }

    unsafe fn round_rect_path(hdc: HDC, r: &RECT, rad: i32, color: COLORREF) -> Silent {
        let br = CreateSolidBrush(color);
        if br.is_invalid() {
            return Err(());
        }
        let pen = CreatePen(PS_SOLID, 1, color);
        let old_b = SelectObject(hdc, br);
        let old_p = SelectObject(hdc, pen);
        RoundRect(hdc, r.left, r.top, r.right, r.bottom, rad, rad);
        SelectObject(hdc, old_b);
        SelectObject(hdc, old_p);
        DeleteObject(br);
        DeleteObject(pen);
        Ok(())
    }

    /// Decode an embedded JPEG to an HBITMAP (32-bit). Returns invalid on failure.
    unsafe fn jpeg_to_bitmap(jpeg_bytes: &[u8], target_w: i32, target_h: i32) -> HBITMAP {
        let img = match image::load_from_memory_with_format(jpeg_bytes, image::ImageFormat::Jpeg) {
            Ok(i) => i.to_rgba8(),
            Err(_) => return HBITMAP(std::ptr::null_mut()),
        };
        rgba_to_bitmap(&img, target_w, target_h)
    }

    /// Decode an embedded PNG to an HBITMAP (32-bit). Returns invalid on failure.
    unsafe fn png_to_bitmap(png_bytes: &[u8], target_w: i32, target_h: i32) -> HBITMAP {
        let img = match image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png) {
            Ok(i) => i.to_rgba8(),
            Err(_) => return HBITMAP(std::ptr::null_mut()),
        };
        // Reuse the DIB creation logic from jpeg_to_bitmap.
        rgba_to_bitmap(&img, target_w, target_h)
    }

    /// Convert RGBA8 image to HBITMAP (shared by JPEG and PNG loaders).
    unsafe fn rgba_to_bitmap(
        img: &image::RgbaImage,
        target_w: i32,
        target_h: i32,
    ) -> HBITMAP {
        let (sw, sh) = (img.width() as i32, img.height() as i32);
        if sw <= 0 || sh <= 0 {
            return HBITMAP(std::ptr::null_mut());
        }
        // Scale to target.
        let scaled = image::imageops::resize(
            img,
            target_w as u32,
            target_h as u32,
            image::imageops::FilterType::Triangle,
        );
        let (w, h) = (scaled.width() as i32, scaled.height() as i32);

        // Create a 32-bit top-down DIB.
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative = top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD::default()],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hdc = GetDC(None);
        let hbmp = match CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(h) => h,
            Err(_) => {
                ReleaseDC(None, hdc);
                return HBITMAP(std::ptr::null_mut());
            }
        };
        ReleaseDC(None, hdc);
        if bits.is_null() {
            let _ = DeleteObject(hbmp);
            return HBITMAP(std::ptr::null_mut());
        }
        // Copy pixels (RGBA -> BGRA).
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        let src = scaled.as_raw();
        for i in 0..(w * h) as usize {
            dst[i * 4] = src[i * 4 + 2];     // B
            dst[i * 4 + 1] = src[i * 4 + 1]; // G
            dst[i * 4 + 2] = src[i * 4];     // R
            dst[i * 4 + 3] = src[i * 4 + 3]; // A
        }
        hbmp
    }

    /// Draw the Flux Rec logo mark (rounded square with stylized "R").
    /// Simple geometric approximation of the blue logo.
    unsafe fn draw_logo(hdc: HDC, x: i32, y: i32, size: i32, accent: COLORREF) -> Silent {
        let r = RECT {
            left: x,
            top: y,
            right: x + size,
            bottom: y + size,
        };
        round_rect_path(hdc, &r, size / 5, accent)?;
        // Inner "R" — draw with lines.
        let pen = CreatePen(PS_SOLID, (size / 12).max(2), COLORREF(0x00FFFFFF));
        if pen.is_invalid() {
            return Err(());
        }
        let old = SelectObject(hdc, pen);
        let s = size as f32;
        let px = |fx: f32| x + (s * fx) as i32;
        let py = |fy: f32| y + (s * fy) as i32;
        // Vertical stem
        MoveToEx(hdc, px(0.32), py(0.24), None);
        LineTo(hdc, px(0.32), py(0.76));
        // Bowl
        MoveToEx(hdc, px(0.32), py(0.24), None);
        LineTo(hdc, px(0.60), py(0.24));
        LineTo(hdc, px(0.68), py(0.36));
        LineTo(hdc, px(0.60), py(0.48));
        LineTo(hdc, px(0.32), py(0.48));
        // Leg
        MoveToEx(hdc, px(0.52), py(0.48), None);
        LineTo(hdc, px(0.70), py(0.76));
        SelectObject(hdc, old);
        DeleteObject(pen);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Layout: compute button rects.
    // ------------------------------------------------------------------
    fn compute_rects() -> Rects {
        let content_x = SIDEBAR_W;
        let content_w = WIN_W - SIDEBAR_W;
        Rects {
            nav_library: RECT {
                left: 16,
                top: TITLE_H + 80,
                right: SIDEBAR_W - 16,
                bottom: TITLE_H + 120,
            },
            nav_settings: RECT {
                left: 16,
                top: TITLE_H + 128,
                right: SIDEBAR_W - 16,
                bottom: TITLE_H + 168,
            },
            // Game card
            card: RECT {
                left: content_x + 40,
                top: TITLE_H + 60,
                right: content_x + content_w - 40,
                bottom: TITLE_H + 480,
            },
            play: RECT {
                left: content_x + 40 + 360,
                top: TITLE_H + 400,
                right: content_x + 40 + 560,
                bottom: TITLE_H + 448,
            },
            uninstall: RECT {
                left: content_x + 40 + 580,
                top: TITLE_H + 408,
                right: content_x + 40 + 680,
                bottom: TITLE_H + 440,
            },
            details: RECT {
                left: content_x + 40 + 700,
                top: TITLE_H + 408,
                right: content_x + 40 + 800,
                bottom: TITLE_H + 440,
            },
            // Settings view
            theme_light: RECT {
                left: content_x + 60,
                top: TITLE_H + 140,
                right: content_x + 220,
                bottom: TITLE_H + 184,
            },
            theme_dark: RECT {
                left: content_x + 240,
                top: TITLE_H + 140,
                right: content_x + 400,
                bottom: TITLE_H + 184,
            },
            close: RECT {
                left: WIN_W - 52,
                top: 6,
                right: WIN_W - 12,
                bottom: 40,
            },
            minimize: RECT {
                left: WIN_W - 96,
                top: 6,
                right: WIN_W - 56,
                bottom: 40,
            },
            // v1.0.4: detail view
            back: RECT {
                left: content_x + 40,
                top: TITLE_H + 16,
                right: content_x + 140,
                bottom: TITLE_H + 48,
            },
            carousel: RECT {
                left: content_x + 40,
                top: TITLE_H + 100,
                right: content_x + 40 + 640,
                bottom: TITLE_H + 100 + 360,
            },
            carousel_prev: RECT {
                left: content_x + 48,
                top: TITLE_H + 250,
                right: content_x + 88,
                bottom: TITLE_H + 290,
            },
            carousel_next: RECT {
                left: content_x + 40 + 640 - 48,
                top: TITLE_H + 250,
                right: content_x + 40 + 640 - 8,
                bottom: TITLE_H + 290,
            },
            detail_play: RECT {
                left: content_x + 40,
                top: TITLE_H + 500,
                right: content_x + 240,
                bottom: TITLE_H + 548,
            },
        }
    }

    fn pt_in(r: &RECT, x: i32, y: i32) -> bool {
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }

    fn hit_test(rects: &Rects, view: View, x: i32, y: i32) -> Hot {
        if pt_in(&rects.close, x, y) {
            return Hot::Close;
        }
        if pt_in(&rects.minimize, x, y) {
            return Hot::Minimize;
        }
        if pt_in(&rects.nav_library, x, y) {
            return Hot::NavLibrary;
        }
        if pt_in(&rects.nav_settings, x, y) {
            return Hot::NavSettings;
        }
        match view {
            View::Library => {
                if pt_in(&rects.play, x, y) {
                    return Hot::Play;
                }
                if pt_in(&rects.uninstall, x, y) {
                    return Hot::Uninstall;
                }
                if pt_in(&rects.details, x, y) {
                    return Hot::Details;
                }
                if pt_in(&rects.card, x, y) {
                    return Hot::Card;
                }
            }
            View::Detail => {
                if pt_in(&rects.back, x, y) {
                    return Hot::Back;
                }
                if pt_in(&rects.carousel_prev, x, y) {
                    return Hot::CarouselPrev;
                }
                if pt_in(&rects.carousel_next, x, y) {
                    return Hot::CarouselNext;
                }
                if pt_in(&rects.detail_play, x, y) {
                    return Hot::DetailPlay;
                }
            }
            View::Settings => {
                if pt_in(&rects.theme_light, x, y) {
                    return Hot::ThemeLight;
                }
                if pt_in(&rects.theme_dark, x, y) {
                    return Hot::ThemeDark;
                }
            }
            View::Working => {}
        }
        Hot::None
    }

    // ------------------------------------------------------------------
    // Paint
    // ------------------------------------------------------------------
    unsafe fn on_paint(hwnd: HWND, st: &mut State) -> Silent {
        let p = pal(st.theme);
        let rects = compute_rects();

        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return Err(());
        }

        // Double buffer.
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let mem = CreateCompatibleDC(hdc);
        if mem.is_invalid() {
            EndPaint(hwnd, &ps);
            return Err(());
        }
        let bmp = CreateCompatibleBitmap(hdc, WIN_W, WIN_H);
        if bmp.is_invalid() {
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
            return Err(());
        }
        let old_bmp = SelectObject(mem, bmp);

        // Background.
        let full = RECT {
            left: 0,
            top: 0,
            right: WIN_W,
            bottom: WIN_H,
        };
        fill_rect(mem, &full, p.bg)?;

        // Title bar.
        let title_r = RECT {
            left: 0,
            top: 0,
            right: WIN_W,
            bottom: TITLE_H,
        };
        fill_rect(mem, &title_r, p.bg_title)?;
        draw_text(
            mem,
            "Ripo Team Launcher",
            20,
            12,
            300,
            24,
            p.text,
            17,
            true,
            false,
        )?;
        // Window buttons.
        let close_hover = st.hover == Hot::Close;
        if close_hover {
            fill_rect(mem, &rects.close, COLORREF(0x004444E0))?;
        }
        draw_text(
            mem,
            "✕",
            rects.close.left,
            rects.close.top + 6,
            40,
            28,
            if close_hover { COLORREF(0x00FFFFFF) } else { p.text_dim },
            16,
            false,
            true,
        )?;
        if st.hover == Hot::Minimize {
            fill_rect(mem, &rects.minimize, p.bg_hover)?;
        }
        draw_text(
            mem,
            "—",
            rects.minimize.left,
            rects.minimize.top + 6,
            40,
            28,
            p.text_dim,
            16,
            false,
            true,
        )?;

        // Sidebar.
        let side_r = RECT {
            left: 0,
            top: TITLE_H,
            right: SIDEBAR_W,
            bottom: WIN_H,
        };
        fill_rect(mem, &side_r, p.bg_side)?;

        // Sidebar logo + wordmark (v1.0.10: clean text only, no icon).
        // The drawn "R"/"RT" badges looked amateur — just use typography.
        draw_text(
            mem,
            "RIPO TEAM",
            24,
            TITLE_H + 24,
            160,
            22,
            p.text,
            15,
            true,
            false,
        )?;
        draw_text(
            mem,
            "Game Launcher",
            24,
            TITLE_H + 46,
            160,
            18,
            p.text_dim,
            12,
            false,
            false,
        )?;
        // Nav buttons.
        draw_nav(mem, &rects.nav_library, "📚  Library", st.view == View::Library, st.hover == Hot::NavLibrary, p)?;
        draw_nav(mem, &rects.nav_settings, "⚙️  Settings", st.view == View::Settings, st.hover == Hot::NavSettings, p)?;

        // Version at sidebar bottom.
        draw_text(
            mem,
            &format!("v{}", st.version),
            24,
            WIN_H - 40,
            180,
            20,
            p.text_dim,
            12,
            false,
            false,
        )?;
        draw_text(
            mem,
            "© Ripo Team",
            24,
            WIN_H - 22,
            180,
            20,
            p.text_dim,
            11,
            false,
            false,
        )?;

        // Content.
        match st.view {
            View::Library => draw_library_view(mem, st, &rects, p)?,
            View::Detail => draw_detail_view(mem, st, &rects, p)?,
            View::Settings => draw_settings_view(mem, st, &rects, p)?,
            View::Working => draw_working_view(mem, st, &rects, p)?,
        }

        BitBlt(hdc, 0, 0, WIN_W, WIN_H, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        DeleteObject(bmp);
        DeleteDC(mem);
        EndPaint(hwnd, &ps);
        Ok(())
    }

    unsafe fn draw_nav(
        hdc: HDC,
        r: &RECT,
        label: &str,
        active: bool,
        hover: bool,
        p: &Palette,
    ) -> Silent {
        if active {
            round_rect_path(hdc, r, 12, p.accent)?;
            draw_text(hdc, label, r.left + 12, r.top + 10, r.right - r.left - 24, 24, COLORREF(0x00FFFFFF), 14, true, false)?;
        } else {
            if hover {
                round_rect_path(hdc, r, 12, p.bg_hover)?;
            }
            draw_text(hdc, label, r.left + 12, r.top + 10, r.right - r.left - 24, 24, p.text_dim, 14, false, false)?;
        }
        Ok(())
    }

    unsafe fn draw_library_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = SIDEBAR_W;
        draw_text(
            hdc,
            "My Games",
            cx + 40,
            TITLE_H + 16,
            300,
            30,
            p.text,
            22,
            true,
            false,
        )?;

        // Game card.
        let card = &rects.card;
        // Card shadow (simple offset rect).
        let shadow = RECT {
            left: card.left + 4,
            top: card.top + 6,
            right: card.right + 4,
            bottom: card.bottom + 6,
        };
        round_rect_path(hdc, &shadow, 20, if st.theme == Theme::Light { COLORREF(0x00D8D0C8) } else { COLORREF(0x00000000) })?;
        round_rect_path(hdc, card, 20, p.bg_card)?;

        // Banner (v1.0.9): screenshot background.
        let banner = RECT {
            left: card.left + 2,
            top: card.top + 2,
            right: card.right - 2,
            bottom: card.top + 190,
        };
        let banner_w = banner.right - banner.left;
        let banner_h = banner.bottom - banner.top;
        // Draw screenshot background (use first carousel image).
        if !st.carousel_bitmaps.is_empty() {
            let hbmp = st.carousel_bitmaps[0];
            let mem_dc = CreateCompatibleDC(hdc);
            if !mem_dc.is_invalid() {
                let old = SelectObject(mem_dc, hbmp);
                // Stretch the 640x360 screenshot to fill the banner.
                StretchBlt(
                    hdc,
                    banner.left,
                    banner.top,
                    banner_w,
                    banner_h,
                    mem_dc,
                    0,
                    0,
                    640,
                    360,
                    SRCCOPY,
                );
                SelectObject(mem_dc, old);
                DeleteDC(mem_dc);
            }
        } else {
            fill_gradient(hdc, &banner, p.banner_top, p.banner_bot)?;
        }
        // Note: text drawn below uses shadow for readability on the screenshot.
        // Big logo on banner (v1.0.5: real logo bitmap, not drawn approximation).
        // v1.0.9: use AlphaBlend for transparency.
        if !st.logo_bitmap.is_invalid() {
            let mem_dc = CreateCompatibleDC(hdc);
            if !mem_dc.is_invalid() {
                let old = SelectObject(mem_dc, st.logo_bitmap);
                let bf = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                AlphaBlend(
                    hdc,
                    banner.left + 36,
                    banner.top + 44,
                    96,
                    96,
                    mem_dc,
                    0,
                    0,
                    128,
                    128,
                    bf,
                );
                SelectObject(mem_dc, old);
                DeleteDC(mem_dc);
            }
        } else {
            draw_logo(hdc, banner.left + 36, banner.top + 44, 96, COLORREF(0x00FFFFFF))?;
        }
        draw_text(
            hdc,
            "FLUX REC",
            banner.left + 156,
            banner.top + 56,
            400,
            50,
            COLORREF(0x00FFFFFF),
            40,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "The private Rec Room revival",
            banner.left + 158,
            banner.top + 108,
            400,
            24,
            COLORREF(0x00FFFFFF),
            15,
            false,
            false,
        )?;

        // Description.
        let desc = "Hang out with friends, play games, and create your own rooms in the private Rec Room revival by Ripo Team. No Steam required.";
        draw_text(
            hdc,
            desc,
            card.left + 36,
            card.top + 214,
            card.right - card.left - 320,
            60,
            p.text_dim,
            14,
            false,
            false,
        )?;

        // Stats row.
        let stats_y = card.top + 290;
        let players_txt = match st.players {
            Some(c) => format!("👥 {} online", c),
            None => "👥 …".to_string(),
        };
        draw_text(hdc, &players_txt, card.left + 36, stats_y, 200, 24, p.text, 14, true, false)?;
        draw_text(
            hdc,
            &format!("v{}  •  © Ripo Team", st.version),
            card.left + 36,
            stats_y + 26,
            300,
            22,
            p.text_dim,
            12,
            false,
            false,
        )?;

        // Status line.
        let status = if st.checking {
            "Checking for updates…"
        } else if !st.check_error.is_empty() {
            // v1.0.5: show network errors instead of "Ready to play".
            &st.check_error
        } else if st.update_available {
            &format!("Update {} available", st.update_version)
        } else if st.ready {
            "Ready to play"
        } else {
            "Preparing…"
        };
        // (status is a &str borrowed from st; copy to owned to satisfy borrowck)
        let status_owned = status.to_string();
        draw_text(
            hdc,
            &status_owned,
            card.left + 36,
            card.top + 348,
            300,
            22,
            if st.update_available { p.accent } else { p.text_dim },
            13,
            false,
            false,
        )?;

        // PLAY button.
        let play = &rects.play;
        let play_hover = st.hover == Hot::Play;
        let play_label = if st.update_available { "⟳  UPDATE & PLAY" } else { "▶  PLAY" };
        let btn_color = if play_hover { p.accent_dark } else { p.accent };
        // Button pulse animation when ready.
        round_rect_path(hdc, play, 16, btn_color)?;
        draw_text(hdc, play_label, play.left, play.top + 12, play.right - play.left, 28, COLORREF(0x00FFFFFF), 17, true, true)?;

        // Uninstall (subtle text button).
        let un = &rects.uninstall;
        if st.hover == Hot::Uninstall {
            round_rect_path(hdc, un, 10, p.bg_hover)?;
        }
        draw_text(hdc, "Uninstall", un.left, un.top + 6, un.right - un.left, 22, p.text_dim, 13, false, true)?;

        // v1.0.6: Details button (explicit, not just card click).
        let det = &rects.details;
        if st.hover == Hot::Details {
            round_rect_path(hdc, det, 10, p.bg_hover)?;
        } else {
            round_rect_path(hdc, det, 10, p.bg_card)?;
            // Border
            let pen = CreatePen(PS_SOLID, 1, p.border);
            if !pen.is_invalid() {
                let old = SelectObject(hdc, pen);
                // Draw border via round rect outline (simplified: just fill)
                SelectObject(hdc, old);
                DeleteObject(pen);
            }
        }
        draw_text(hdc, "Details →", det.left, det.top + 6, det.right - det.left, 22, p.accent, 13, true, true)?;

        Ok(())
    }

    /// v1.0.4: game detail view with screenshot carousel.
    unsafe fn draw_detail_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = SIDEBAR_W;

        // Back button.
        let back = &rects.back;
        if st.hover == Hot::Back {
            round_rect_path(hdc, back, 10, p.bg_hover)?;
        }
        draw_text(hdc, "←  Back", back.left + 8, back.top + 6, 90, 22, p.text_dim, 14, false, false)?;

        // Title + logo (v1.0.5: real logo bitmap).
        if !st.logo_bitmap.is_invalid() {
            let mem_dc = CreateCompatibleDC(hdc);
            if !mem_dc.is_invalid() {
                let old = SelectObject(mem_dc, st.logo_bitmap);
                BitBlt(hdc, cx + 40, TITLE_H + 52, 48, 48, mem_dc, 0, 0, SRCCOPY);
                SelectObject(mem_dc, old);
                DeleteDC(mem_dc);
            }
        } else {
            draw_logo(hdc, cx + 40, TITLE_H + 52, 48, p.accent)?;
        }
        draw_text(hdc, "Flux Rec", cx + 104, TITLE_H + 52, 400, 36, p.text, 28, true, false)?;
        draw_text(
            hdc,
            "The private Rec Room revival by Ripo Team",
            cx + 104,
            TITLE_H + 88,
            500,
            22,
            p.text_dim,
            14,
            false,
            false,
        )?;

        // Screenshot carousel.
        let car = &rects.carousel;
        round_rect_path(hdc, car, 16, p.bg_card)?;
        if !st.carousel_bitmaps.is_empty() {
            let idx = st.carousel_idx % st.carousel_bitmaps.len();
            let hbmp = st.carousel_bitmaps[idx];
            let mem_dc = CreateCompatibleDC(hdc);
            if !mem_dc.is_invalid() {
                let old = SelectObject(mem_dc, hbmp);
                BitBlt(hdc, car.left, car.top, 640, 360, mem_dc, 0, 0, SRCCOPY);
                SelectObject(mem_dc, old);
                DeleteDC(mem_dc);
            }
            let name = SCREENSHOT_NAMES.get(idx).copied().unwrap_or("");
            draw_text(hdc, name, car.left, car.bottom + 8, 640, 22, p.text_dim, 13, false, true)?;
            // Dots.
            let n = st.carousel_bitmaps.len();
            let dot_y = car.bottom + 34;
            let total_w = (n as i32) * 16;
            let start_x = car.left + (640 - total_w) / 2;
            for i in 0..n {
                let c = if i == idx { p.accent } else { p.track };
                let br = CreateSolidBrush(c);
                if !br.is_invalid() {
                    let pen = CreatePen(PS_SOLID, 1, c);
                    let ob = SelectObject(hdc, br);
                    let op = SelectObject(hdc, pen);
                    Ellipse(hdc, start_x + (i as i32) * 16, dot_y, start_x + (i as i32) * 16 + 8, dot_y + 8);
                    SelectObject(hdc, ob);
                    SelectObject(hdc, op);
                    DeleteObject(br);
                    DeleteObject(pen);
                }
            }
        } else {
            draw_text(hdc, "Screenshots loading…", car.left, car.top + 160, 640, 30, p.text_dim, 14, false, true)?;
        }

        // Prev/Next buttons.
        for (r, label, hot) in [
            (&rects.carousel_prev, "‹", Hot::CarouselPrev),
            (&rects.carousel_next, "›", Hot::CarouselNext),
        ] {
            if st.hover == hot {
                round_rect_path(hdc, r, 20, p.accent)?;
                draw_text(hdc, label, r.left, r.top + 2, 40, 36, COLORREF(0x00FFFFFF), 24, true, true)?;
            } else {
                let br = CreateSolidBrush(p.bg_card);
                if !br.is_invalid() {
                    let pen = CreatePen(PS_SOLID, 1, p.border);
                    let ob = SelectObject(hdc, br);
                    let op = SelectObject(hdc, pen);
                    Ellipse(hdc, r.left, r.top, r.right, r.bottom);
                    SelectObject(hdc, ob);
                    SelectObject(hdc, op);
                    DeleteObject(br);
                    DeleteObject(pen);
                }
                draw_text(hdc, label, r.left, r.top + 2, 40, 36, p.text, 24, true, true)?;
            }
        }

        // Description.
        let desc = "Hang out with friends, explore player-created rooms, and play games like paintball, laser tag, and quests — all in the private Flux Rec universe. Your dorm room is your home base. No Steam required. © 2026 Ripo Team.";
        draw_text(hdc, desc, cx + 40, TITLE_H + 520, 640, 60, p.text_dim, 13, false, false)?;

        // Player count.
        let players_txt = match st.players {
            Some(c) => format!("👥 {} players online now", c),
            None => "👥 …".to_string(),
        };
        draw_text(hdc, &players_txt, cx + 40, TITLE_H + 580, 300, 24, p.text, 14, true, false)?;

        // PLAY button.
        let dp = &rects.detail_play;
        let hover = st.hover == Hot::DetailPlay;
        round_rect_path(hdc, dp, 14, if hover { p.accent_dark } else { p.accent })?;
        let label = if st.update_available { "⟳  UPDATE & PLAY" } else { "▶  PLAY" };
        draw_text(hdc, label, dp.left, dp.top + 12, dp.right - dp.left, 26, COLORREF(0x00FFFFFF), 16, true, true)?;

        Ok(())
    }

    unsafe fn draw_settings_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {        let cx = SIDEBAR_W;
        draw_text(hdc, "Settings", cx + 40, TITLE_H + 16, 300, 30, p.text, 22, true, false)?;

        draw_text(hdc, "Appearance", cx + 60, TITLE_H + 80, 300, 26, p.text, 16, true, false)?;
        draw_text(
            hdc,
            "Choose how the launcher looks.",
            cx + 60,
            TITLE_H + 106,
            400,
            22,
            p.text_dim,
            13,
            false,
            false,
        )?;

        // Theme toggle buttons.
        let light_active = st.theme == Theme::Light;
        let dark_active = st.theme == Theme::Dark;
        // Light button
        let lr = &rects.theme_light;
        let lhover = st.hover == Hot::ThemeLight;
        round_rect_path(
            hdc,
            lr,
            12,
            if light_active { p.accent } else if lhover { p.bg_hover } else { p.bg_card },
        )?;
        draw_text(
            hdc,
            "☀️  Light",
            lr.left,
            lr.top + 10,
            lr.right - lr.left,
            24,
            if light_active { COLORREF(0x00FFFFFF) } else { p.text },
            14,
            light_active,
            true,
        )?;
        // Dark button
        let dr = &rects.theme_dark;
        let dhover = st.hover == Hot::ThemeDark;
        round_rect_path(
            hdc,
            dr,
            12,
            if dark_active { p.accent } else if dhover { p.bg_hover } else { p.bg_card },
        )?;
        draw_text(
            hdc,
            "🌙  Dark",
            dr.left,
            dr.top + 10,
            dr.right - dr.left,
            24,
            if dark_active { COLORREF(0x00FFFFFF) } else { p.text },
            14,
            dark_active,
            true,
        )?;

        // Install location.
        draw_text(hdc, "Install location", cx + 60, TITLE_H + 220, 300, 26, p.text, 16, true, false)?;
        let dir = crate::default_install_dir().to_string_lossy().to_string();
        draw_text(hdc, &dir, cx + 60, TITLE_H + 248, 700, 22, p.text_dim, 13, false, false)?;

        // About.
        draw_text(hdc, "About", cx + 60, TITLE_H + 300, 300, 26, p.text, 16, true, false)?;
        draw_text(
            hdc,
            &format!("Ripo Team Launcher v{}\n© 2026 Ripo Team. All rights reserved.", st.version),
            cx + 60,
            TITLE_H + 328,
            500,
            48,
            p.text_dim,
            13,
            false,
            false,
        )?;
        Ok(())
    }

    unsafe fn draw_working_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = SIDEBAR_W;
        // Center the progress in the content area.
        let content_w = WIN_W - SIDEBAR_W;
        let bx = cx + (content_w - 440) / 2;
        let by = TITLE_H + 200;

        draw_logo(hdc, bx + 188, by - 110, 64, p.accent)?;
        draw_text(hdc, "FLUX REC", bx, by - 36, 440, 34, p.text, 24, true, true)?;

        if !st.stage.is_empty() {
            draw_text(hdc, &st.stage.clone(), bx, by + 10, 440, 28, p.text, 16, false, true)?;
        }
        if !st.detail.is_empty() {
            draw_text(hdc, &st.detail.clone(), bx, by + 38, 440, 24, p.text_dim, 13, false, true)?;
        }

        // Progress bar (smoothed animation).
        let bar = RECT {
            left: bx,
            top: by + 76,
            right: bx + 440,
            bottom: by + 92,
        };
        round_rect_path(hdc, &bar, 16, p.track)?;
        // Smooth toward the target.
        let target = st.percent as f32;
        if (st.progress_anim - target).abs() < 0.5 {
            st.progress_anim = target;
        } else if st.progress_anim < target {
            st.progress_anim += (target - st.progress_anim) * 0.18 + 0.4;
        } else {
            st.progress_anim = target;
        }
        let fill_w = (440.0 * st.progress_anim / 100.0) as i32;
        if fill_w > 4 {
            let fill = RECT {
                left: bx + 2,
                top: by + 78,
                right: bx + 2 + fill_w,
                bottom: by + 90,
            };
            // Gradient fill for the bar.
            fill_gradient(hdc, &fill, p.accent, p.accent_dark)?;
        }
        draw_text(
            hdc,
            &format!("{}%", st.percent),
            bx,
            by + 100,
            440,
            22,
            p.text_dim,
            13,
            false,
            true,
        )?;
        let _ = rects;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Splash paint
    // ------------------------------------------------------------------
    unsafe fn splash_paint(hwnd: HWND, st: &mut SplashState) -> Silent {
        let p = pal(st.theme);
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return Err(());
        }
        let mem = CreateCompatibleDC(hdc);
        if mem.is_invalid() {
            EndPaint(hwnd, &ps);
            return Err(());
        }
        let bmp = CreateCompatibleBitmap(hdc, SPLASH_W, SPLASH_H);
        if bmp.is_invalid() {
            DeleteDC(mem);
            EndPaint(hwnd, &ps);
            return Err(());
        }
        let old = SelectObject(mem, bmp);

        let full = RECT { left: 0, top: 0, right: SPLASH_W, bottom: SPLASH_H };
        fill_gradient(mem, &full, p.bg_title, p.bg)?;

        // Big logo with a subtle pulse.
        let pulse = ((st.t as f32 * 0.06).sin() * 4.0) as i32;
        draw_logo(mem, SPLASH_W / 2 - 48, 52 + pulse / 2, 96, p.accent)?;
        draw_text(mem, "FLUX REC", 0, 168, SPLASH_W, 40, p.text, 30, true, true)?;
        draw_text(mem, "Launching game…", 0, 208, SPLASH_W, 26, p.text_dim, 15, false, true)?;

        // Indeterminate loading bar: a sliding highlight.
        let bar = RECT {
            left: 90,
            top: 252,
            right: SPLASH_W - 90,
            bottom: 264,
        };
        round_rect_path(mem, &bar, 12, p.track)?;
        let bw = bar.right - bar.left;
        let slide = ((st.t * 7) % (bw as u32 + 120)) as i32 - 60;
        let seg_l = (bar.left + slide).max(bar.left);
        let seg_r = (bar.left + slide + 120).min(bar.right);
        if seg_r > seg_l {
            let seg = RECT { left: seg_l + 2, top: bar.top + 2, right: seg_r - 2, bottom: bar.bottom - 2 };
            fill_gradient(mem, &seg, p.accent, p.accent_dark)?;
        }

        BitBlt(hdc, 0, 0, SPLASH_W, SPLASH_H, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        EndPaint(hwnd, &ps);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Window procedures
    // ------------------------------------------------------------------
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = lparam.0 as *const CREATESTRUCTW;
                if cs.is_null() {
                    return LRESULT(-1);
                }
                let ptr = (*cs).lpCreateParams as *mut State;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                LRESULT(0)
            }
            WM_TIMER => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    (*ptr).anim_t = (*ptr).anim_t.wrapping_add(1);
                    // v1.0.4: auto-advance carousel every ~4s (50ms timer * 80).
                    if (*ptr).view == View::Detail && !(*ptr).carousel_bitmaps.is_empty() {
                        (*ptr).carousel_tick += 1;
                        if (*ptr).carousel_tick >= 80 {
                            (*ptr).carousel_tick = 0;
                            (*ptr).carousel_idx =
                                ((*ptr).carousel_idx + 1) % (*ptr).carousel_bitmaps.len();
                        }
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    let _ = on_paint(hwnd, &mut *ptr);
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                // Drag by the title bar (but not on the buttons).
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut pt = POINT { x, y };
                let _ = ScreenToClient(hwnd, &mut pt);
                let rects = compute_rects();
                if pt_in(&rects.close, pt.x, pt.y) || pt_in(&rects.minimize, pt.x, pt.y) {
                    return LRESULT(HTCLIENT as isize);
                }
                if pt.y >= 0 && pt.y < TITLE_H {
                    return LRESULT(HTCAPTION as isize);
                }
                LRESULT(HTCLIENT as isize)
            }
            WM_MOUSEMOVE => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    let st = &mut *ptr;
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let rects = compute_rects();
                    let hot = hit_test(&rects, st.view, x, y);
                    if hot != st.hover {
                        st.hover = hot;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // (Dragging is handled by WM_NCHITTEST + HTCAPTION.)
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    let st = &mut *ptr;
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    let rects = compute_rects();
                    let hot = hit_test(&rects, st.view, x, y);
                    handle_click(hwnd, st, hot, &rects);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if !ptr.is_null() {
                    let _ = (*ptr).cmd_tx.send(LibCmd::Close);
                    // v1.0.4: free carousel bitmaps.
                    for hbmp in (*ptr).carousel_bitmaps.drain(..) {
                        let _ = DeleteObject(hbmp);
                    }
                    // v1.0.5: free logo bitmap.
                    if !(*ptr).logo_bitmap.is_invalid() {
                        let _ = DeleteObject((*ptr).logo_bitmap);
                    }
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn start_play(hwnd: HWND, st: &mut State) {
        st.view = View::Working;
        st.percent = 0;
        st.stage = "Starting…".to_string();
        st.detail = String::new();
        st.progress_anim = 0.0;
        let _ = st.cmd_tx.send(LibCmd::Play);
        let _ = InvalidateRect(hwnd, None, false);
    }

    unsafe fn handle_click(hwnd: HWND, st: &mut State, hot: Hot, _rects: &Rects) {
        match hot {
            Hot::Close => {
                let _ = st.cmd_tx.send(LibCmd::Close);
                let _ = DestroyWindow(hwnd);
            }
            Hot::Minimize => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            Hot::NavLibrary => {
                st.view = View::Library;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::NavSettings => {
                st.view = View::Settings;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::Play => {
                if st.view == View::Library {
                    start_play(hwnd, st);
                }
            }
            Hot::DetailPlay => {
                if st.view == View::Detail {
                    start_play(hwnd, st);
                }
            }
            Hot::Card | Hot::Details => {
                if st.view == View::Library {
                    st.view = View::Detail;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::Back => {
                if st.view == View::Detail {
                    st.view = View::Library;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::CarouselPrev => {
                if st.view == View::Detail && !st.carousel_bitmaps.is_empty() {
                    let n = st.carousel_bitmaps.len();
                    st.carousel_idx = (st.carousel_idx + n - 1) % n;
                    st.carousel_tick = 0;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::CarouselNext => {
                if st.view == View::Detail && !st.carousel_bitmaps.is_empty() {
                    let n = st.carousel_bitmaps.len();
                    st.carousel_idx = (st.carousel_idx + 1) % n;
                    st.carousel_tick = 0;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::Uninstall => {
                // Confirm via a native dialog (simple + reliable).
                let ans = MessageBoxW(
                    hwnd,
                    w!("Are you sure you want to uninstall Flux Rec?\n\nThis will delete the game files."),
                    w!("Ripo Team Launcher"),
                    MB_YESNO | MB_ICONWARNING,
                );
                if ans == IDYES {
                    let _ = st.cmd_tx.send(LibCmd::Uninstall);
                }
            }
            Hot::ThemeLight => {
                if st.theme != Theme::Light {
                    st.theme = Theme::Light;
                    let _ = st.cmd_tx.send(LibCmd::SetTheme(Theme::Light));
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::ThemeDark => {
                if st.theme != Theme::Dark {
                    st.theme = Theme::Dark;
                    let _ = st.cmd_tx.send(LibCmd::SetTheme(Theme::Dark));
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::None => {}
        }
    }

    unsafe extern "system" fn splash_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = lparam.0 as *const CREATESTRUCTW;
                if cs.is_null() {
                    return LRESULT(-1);
                }
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
                LRESULT(0)
            }
            WM_TIMER => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SplashState;
                if !ptr.is_null() {
                    (*ptr).t = (*ptr).t.wrapping_add(1);
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SplashState;
                if !ptr.is_null() {
                    let _ = splash_paint(hwnd, &mut *ptr);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
