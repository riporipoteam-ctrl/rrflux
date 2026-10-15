//! Ripo Team Launcher v1.0.29 — game library UI.
//!
//! A game-library style launcher (inspired by Steam/EGS):
//! - Sidebar with Ripo Team branding, Library/Settings nav.
//! - Flux Rec game card: banner, logo, description, live player count,
//!   PLAY button, uninstall.
//! - Settings: light/dark theme toggle (default: dark).
//! - PLAY flow: update check -> install update if needed -> launch game ->
//!   close library -> "Launching..." splash.
//!
//! Design rules (same as gui.rs):
//! * Raw Win32 only, no GUI framework.
//! * Never panic, never print; any failure returns and the caller proceeds.
//! * All Win32 code behind `#[cfg(windows)]`; other platforms get stubs.
//! * No `unwrap`/`expect`/`println` anywhere.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

#[cfg(any(windows, test))]
fn premultiply_channel(channel: u8, alpha: u8) -> u8 {
    ((channel as u16 * alpha as u16 + 127) / 255) as u8
}

#[cfg(test)]
mod alpha_tests {
    use super::premultiply_channel;

    #[test]
    fn premultiplication_obeys_alpha_blend_contract() {
        assert_eq!(premultiply_channel(200, 255), 200);
        assert_eq!(premultiply_channel(200, 128), 100);
        assert_eq!(premultiply_channel(200, 0), 0);
    }
}

/// UI theme. New installations start with Dark; users can change it in Settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Light,
    Dark,
}

/// Config for the library window.
pub struct LibraryConfig {
    pub theme: Theme,
    pub version: String,
    pub install_dir: PathBuf,
    pub cmd_tx: Sender<crate::library::LibCmd>,
}

/// Which view the library window is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Library,
    Detail, // v1.0.4: game detail view with screenshot carousel
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
    use super::{
        premultiply_channel, LibraryConfig, Theme, View, FLUXREC_LOGO_PNG, SCREENSHOTS,
        SCREENSHOT_NAMES,
    };
    use crate::library::{LibCmd, LibMsg};
    use std::ffi::c_void;
    use std::sync::mpsc::{Receiver, Sender, TryRecvError};
    use windows::core::w;
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window size.
    const WIN_W: i32 = 1120;
    const WIN_H: i32 = 720;
    const SIDEBAR_W: i32 = 206;
    const TITLE_H: i32 = 54;
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
        bg: COLORREF(0x00F5F3F0),          // #F0F3F5 warm light gray
        bg_title: COLORREF(0x00FFFFFF),    // white title bar
        bg_side: COLORREF(0x00FFFFFF),     // white sidebar
        bg_card: COLORREF(0x00FFFFFF),     // white cards
        bg_hover: COLORREF(0x00EBE6E0),    // #E0E6EB hover
        text: COLORREF(0x00211A12),        // #121A21 near-black
        text_dim: COLORREF(0x007A6F66),    // #666F7A dim
        accent: COLORREF(0x00FF9B2E),      // Flux blue #2E9BFF
        accent_dark: COLORREF(0x00CC7A1F), // darker blue
        track: COLORREF(0x00E0D8D2),       // #D2D8E0 track
        border: COLORREF(0x00D8D0C8),      // #C8D0D8 border
        banner_top: COLORREF(0x00FF9B2E),  // blue gradient
        banner_bot: COLORREF(0x00A04A10),  // deep blue
    };

    const DARK: Palette = Palette {
        bg: COLORREF(0x00170E0B),       // #0B0E17
        bg_title: COLORREF(0x0019110D), // #0D1119
        bg_side: COLORREF(0x001F1411),  // #11141F
        bg_card: COLORREF(0x00281B16),  // #161B28
        bg_hover: COLORREF(0x00302622), // #222630 hover
        text: COLORREF(0x00FFFFFF),
        text_dim: COLORREF(0x00B2A39A), // #9AA3B2
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
        RefreshPlayers,
        CheckForUpdates,
        OpenInstallFolder,
        Details, // v1.0.6
        ThemeLight,
        ThemeDark,
        Close,
        Minimize,
        Card, // v1.0.4: click game card -> detail view
        Back, // v1.0.4: back from detail to library
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
        install_required: bool,
        update_available: bool,
        update_version: String,
        install_dir: String,
        players: Option<u32>,
        player_count_pending: bool,
        checking: bool,
        // Progress (Working view)
        percent: u8,
        stage: String,
        detail: String,
        // Animation
        hover: Hot,
        anim_t: u32,        // ticks for animations
        progress_anim: f32, // smoothed progress 0..100
        // v1.0.4: detail view carousel
        carousel_idx: usize,
        carousel_bitmaps: Vec<HBITMAP>, // decoded screenshots
        carousel_tick: u32,             // for auto-advance
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
        refresh_players: RECT,
        check_updates: RECT,
        open_folder: RECT,
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

    pub(super) fn run_library(config: LibraryConfig, rx: Receiver<LibMsg>) -> Silent {
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
                install_dir: config.install_dir.to_string_lossy().into_owned(),
                view: View::Library,
                cmd_tx: config.cmd_tx,
                ready: false,
                install_required: false,
                update_available: false,
                update_version: String::new(),
                players: None,
                player_count_pending: true,
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
                w!("Flux Rec Control Center"),
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
                                LibMsg::Ready {
                                    update_available,
                                    update_version,
                                } => {
                                    st.ready = true;
                                    st.checking = false;
                                    st.install_required = false;
                                    st.update_available = update_available;
                                    st.update_version = update_version;
                                    st.check_error.clear();
                                }
                                LibMsg::Checking => {
                                    st.checking = true;
                                    st.check_error.clear();
                                }
                                LibMsg::InstallRequired => {
                                    st.install_required = true;
                                    st.ready = true;
                                    st.checking = false;
                                    st.update_available = false;
                                    st.update_version.clear();
                                    st.check_error.clear();
                                }
                                LibMsg::CheckFailed(e) => {
                                    st.checking = false;
                                    st.ready = true;
                                    st.check_error = e;
                                }
                                LibMsg::PlayerCount(c) => {
                                    st.players = c;
                                    st.player_count_pending = false;
                                }
                                LibMsg::Progress {
                                    percent,
                                    stage,
                                    detail,
                                } => {
                                    st.view = View::Working;
                                    st.percent = percent;
                                    if !stage.is_empty() {
                                        st.stage = stage;
                                    }
                                    st.detail = detail;
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
                logo_bitmap: png_to_bitmap(FLUXREC_LOGO_PNG, 128, 128),
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
                let failed_state = Box::from_raw(ptr);
                if !failed_state.logo_bitmap.is_invalid() {
                    let _ = DeleteObject(failed_state.logo_bitmap);
                }
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
        logo_bitmap: HBITMAP,
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
            -(size.max(12)),
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
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

    unsafe fn draw_text_ellipsis(
        hdc: HDC,
        text: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: COLORREF,
        size: i32,
    ) -> Silent {
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let font = CreateFontW(
            -size,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
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
        DrawTextW(
            hdc,
            &mut wide,
            &mut r,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_LEFT,
        );
        SelectObject(hdc, old);
        let _ = DeleteObject(font);
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

    unsafe fn round_rect_outline(hdc: HDC, r: &RECT, rad: i32, color: COLORREF) -> Silent {
        let pen = CreatePen(PS_SOLID, 1, color);
        if pen.is_invalid() {
            return Err(());
        }
        let old_pen = SelectObject(hdc, pen);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, rad, rad);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen);
        Ok(())
    }

    unsafe fn draw_logo_bitmap(hdc: HDC, bitmap: HBITMAP, x: i32, y: i32, size: i32) -> Silent {
        if bitmap.is_invalid() || size <= 0 {
            return Err(());
        }
        let mem_dc = CreateCompatibleDC(hdc);
        if mem_dc.is_invalid() {
            return Err(());
        }
        let old = SelectObject(mem_dc, bitmap);
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = AlphaBlend(hdc, x, y, size, size, mem_dc, 0, 0, 128, 128, blend);
        SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
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
    unsafe fn rgba_to_bitmap(img: &image::RgbaImage, target_w: i32, target_h: i32) -> HBITMAP {
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
        // AlphaBlend expects premultiplied BGRA, not straight-alpha RGBA.
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        let src = scaled.as_raw();
        for i in 0..(w * h) as usize {
            let alpha = src[i * 4 + 3];
            dst[i * 4] = premultiply_channel(src[i * 4 + 2], alpha); // B
            dst[i * 4 + 1] = premultiply_channel(src[i * 4 + 1], alpha); // G
            dst[i * 4 + 2] = premultiply_channel(src[i * 4], alpha); // R
            dst[i * 4 + 3] = alpha;
        }
        hbmp
    }

    /// Draw the Flux Rec logo mark with the Rec Room face.
    unsafe fn draw_logo(hdc: HDC, x: i32, y: i32, size: i32, accent: COLORREF) -> Silent {
        let r = RECT {
            left: x,
            top: y,
            right: x + size,
            bottom: y + size,
        };
        round_rect_path(hdc, &r, size / 5, accent)?;
        // Keep the face readable at the sidebar size as well as on the larger
        // game card. The old geometric "R" did not match the desktop shortcut.
        let white = COLORREF(0x00FFFFFF);
        let pen = CreatePen(PS_SOLID, (size / 18).max(1), white);
        if pen.is_invalid() {
            return Err(());
        }
        let old = SelectObject(hdc, pen);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let s = size as f32;
        let px = |fx: f32| x + (s * fx) as i32;
        let py = |fy: f32| y + (s * fy) as i32;
        let face = RECT {
            left: px(0.22),
            top: py(0.19),
            right: px(0.78),
            bottom: py(0.81),
        };
        let _ = RoundRect(
            hdc,
            face.left,
            face.top,
            face.right,
            face.bottom,
            (size / 5).max(3),
            (size / 5).max(3),
        );

        let white_brush = CreateSolidBrush(white);
        if !white_brush.is_invalid() {
            let previous_brush = SelectObject(hdc, white_brush);
            let eye = (size / 11).max(2);
            let _ = Ellipse(hdc, px(0.36), py(0.36), px(0.36) + eye, py(0.36) + eye);
            let _ = Ellipse(hdc, px(0.59), py(0.36), px(0.59) + eye, py(0.36) + eye);
            let _ = SelectObject(hdc, previous_brush);
            let _ = DeleteObject(white_brush);
        }

        // A short, rounded U-shaped smile made from simple line segments.
        MoveToEx(hdc, px(0.36), py(0.55), None);
        LineTo(hdc, px(0.41), py(0.63));
        LineTo(hdc, px(0.49), py(0.67));
        LineTo(hdc, px(0.57), py(0.63));
        LineTo(hdc, px(0.63), py(0.55));
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old);
        DeleteObject(pen);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Layout: compute button rects.
    // ------------------------------------------------------------------
    fn compute_rects() -> Rects {
        let content_x = SIDEBAR_W + 28;
        let content_right = WIN_W - 28;
        let content_w = content_right - content_x;
        let status_x = content_right - 272;
        let hero_right = status_x - 18;
        let action_y = TITLE_H + 500;
        let action_w = (content_w - 36) / 4;
        let action_x = |index: i32| content_x + index * (action_w + 12);
        Rects {
            nav_library: RECT {
                left: 16,
                top: TITLE_H + 88,
                right: SIDEBAR_W - 16,
                bottom: TITLE_H + 130,
            },
            nav_settings: RECT {
                left: 16,
                top: TITLE_H + 140,
                right: SIDEBAR_W - 16,
                bottom: TITLE_H + 182,
            },
            // Home hero.
            card: RECT {
                left: content_x,
                top: TITLE_H + 60,
                right: hero_right,
                bottom: TITLE_H + 486,
            },
            play: RECT {
                left: status_x + 14,
                top: TITLE_H + 395,
                right: content_right - 14,
                bottom: TITLE_H + 449,
            },
            uninstall: RECT {
                left: action_x(3),
                top: action_y,
                right: action_x(3) + action_w,
                bottom: action_y + 58,
            },
            details: RECT {
                left: action_x(2),
                top: action_y,
                right: action_x(2) + action_w,
                bottom: action_y + 58,
            },
            // Settings view
            theme_light: RECT {
                left: content_x + 36,
                top: TITLE_H + 164,
                right: content_x + 196,
                bottom: TITLE_H + 210,
            },
            theme_dark: RECT {
                left: content_x + 208,
                top: TITLE_H + 164,
                right: content_x + 368,
                bottom: TITLE_H + 210,
            },
            refresh_players: RECT {
                left: status_x + 180,
                top: TITLE_H + 78,
                right: content_right - 12,
                bottom: TITLE_H + 110,
            },
            check_updates: RECT {
                left: action_x(0),
                top: action_y,
                right: action_x(0) + action_w,
                bottom: action_y + 58,
            },
            open_folder: RECT {
                left: action_x(1),
                top: action_y,
                right: action_x(1) + action_w,
                bottom: action_y + 58,
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
                left: content_x + 4,
                top: TITLE_H + 16,
                right: content_x + 120,
                bottom: TITLE_H + 48,
            },
            carousel: RECT {
                left: content_x + 4,
                top: TITLE_H + 96,
                right: content_x + 4 + 620,
                bottom: TITLE_H + 96 + 349,
            },
            carousel_prev: RECT {
                left: content_x + 12,
                top: TITLE_H + 96 + 154,
                right: content_x + 52,
                bottom: TITLE_H + 96 + 194,
            },
            carousel_next: RECT {
                left: content_x + 4 + 620 - 52,
                top: TITLE_H + 96 + 154,
                right: content_x + 4 + 620 - 12,
                bottom: TITLE_H + 96 + 194,
            },
            detail_play: RECT {
                left: content_right - 212,
                top: TITLE_H + 500,
                right: content_right,
                bottom: TITLE_H + 554,
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
                if pt_in(&rects.refresh_players, x, y) {
                    return Hot::RefreshPlayers;
                }
                if pt_in(&rects.check_updates, x, y) {
                    return Hot::CheckForUpdates;
                }
                if pt_in(&rects.open_folder, x, y) {
                    return Hot::OpenInstallFolder;
                }
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
                if pt_in(&rects.check_updates, x, y) {
                    return Hot::CheckForUpdates;
                }
                if pt_in(&rects.open_folder, x, y) {
                    return Hot::OpenInstallFolder;
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
            "FLUX REC  /  CONTROL CENTER",
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
            if close_hover {
                COLORREF(0x00FFFFFF)
            } else {
                p.text_dim
            },
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

        // Compact status chip in the command strip.
        let live_label = if st.checking {
            "CHECKING"
        } else if st.update_available {
            "UPDATE READY"
        } else if st.install_required {
            "INSTALL NEEDED"
        } else if !st.check_error.is_empty() {
            "CHECK FAILED"
        } else {
            "READY"
        };
        let top_status = RECT {
            left: WIN_W - 268,
            top: 10,
            right: WIN_W - 122,
            bottom: 42,
        };
        round_rect_path(mem, &top_status, 16, p.bg_hover)?;
        draw_text(
            mem,
            live_label,
            top_status.left + 8,
            top_status.top + 5,
            top_status.right - top_status.left - 16,
            22,
            if st.update_available || st.install_required {
                p.accent
            } else {
                p.text_dim
            },
            11,
            true,
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

        // Brand mark + wordmark.
        draw_logo(mem, 24, TITLE_H + 20, 36, p.accent)?;
        draw_text(
            mem,
            "RIPO TEAM",
            72,
            TITLE_H + 24,
            150,
            22,
            p.text,
            15,
            true,
            false,
        )?;
        draw_text(
            mem,
            "2025 CLIENT",
            72,
            TITLE_H + 46,
            150,
            18,
            p.text_dim,
            12,
            false,
            false,
        )?;
        // Nav buttons.
        draw_nav(
            mem,
            &rects.nav_library,
            "Overview",
            st.view == View::Library,
            st.hover == Hot::NavLibrary,
            p,
        )?;
        draw_nav(
            mem,
            &rects.nav_settings,
            "Settings",
            st.view == View::Settings,
            st.hover == Hot::NavSettings,
            p,
        )?;

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
            View::Library => draw_dashboard_view(mem, st, &rects, p)?,
            View::Detail => draw_detail_view(mem, st, &rects, p)?,
            View::Settings => draw_settings_console(mem, st, &rects, p)?,
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
            draw_text(
                hdc,
                label,
                r.left + 12,
                r.top + 10,
                r.right - r.left - 24,
                24,
                COLORREF(0x00FFFFFF),
                14,
                true,
                false,
            )?;
        } else {
            if hover {
                round_rect_path(hdc, r, 12, p.bg_hover)?;
            }
            draw_text(
                hdc,
                label,
                r.left + 12,
                r.top + 10,
                r.right - r.left - 24,
                24,
                p.text_dim,
                14,
                false,
                false,
            )?;
        }
        Ok(())
    }

    unsafe fn draw_status_pulse(hdc: HDC, x: i32, y: i32, tick: u32, color: COLORREF) -> Silent {
        let phase = (tick % 24) as i32;
        let radius = 4 + phase / 8;
        let pen = CreatePen(PS_SOLID, 1, color);
        if !pen.is_invalid() {
            let old_pen = SelectObject(hdc, pen);
            let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let _ = Ellipse(hdc, x - radius, y - radius, x + radius, y + radius);
            let _ = SelectObject(hdc, old_pen);
            let _ = SelectObject(hdc, old_brush);
            let _ = DeleteObject(pen);
        }
        let brush = CreateSolidBrush(color);
        if !brush.is_invalid() {
            let old = SelectObject(hdc, brush);
            let _ = Ellipse(hdc, x - 3, y - 3, x + 4, y + 4);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(brush);
        }
        Ok(())
    }

    unsafe fn draw_people_icon(hdc: HDC, x: i32, y: i32, color: COLORREF) -> Silent {
        let pen = CreatePen(PS_SOLID, 2, color);
        if pen.is_invalid() {
            return Err(());
        }
        let old_pen = SelectObject(hdc, pen);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = Ellipse(hdc, x + 7, y, x + 19, y + 12);
        let _ = MoveToEx(hdc, x + 2, y + 28, None);
        let _ = LineTo(hdc, x + 4, y + 21);
        let _ = LineTo(hdc, x + 9, y + 16);
        let _ = LineTo(hdc, x + 16, y + 16);
        let _ = LineTo(hdc, x + 21, y + 21);
        let _ = LineTo(hdc, x + 23, y + 28);
        let _ = SelectObject(hdc, old_pen);
        let _ = SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen);
        Ok(())
    }

    unsafe fn draw_dashboard_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let content_x = SIDEBAR_W + 28;
        let content_right = WIN_W - 28;
        let status_x = content_right - 272;
        let card = &rects.card;

        draw_text(
            hdc,
            "FLUX REC  /  2025 CLIENT",
            content_x,
            TITLE_H + 12,
            400,
            18,
            p.accent,
            11,
            true,
            false,
        )?;
        let headline = if st.install_required {
            "Install Flux Rec"
        } else if st.update_available {
            "Update available"
        } else {
            "Ready to play"
        };
        draw_text(
            hdc,
            headline,
            content_x,
            TITLE_H + 31,
            460,
            32,
            p.text,
            24,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Launch your game and keep the client up to date.",
            content_x + 464,
            TITLE_H + 34,
            420,
            20,
            p.text_dim,
            12,
            false,
            false,
        )?;

        // Featured game: a wide media surface instead of the old single-card layout.
        round_rect_path(hdc, card, 22, p.bg_card)?;
        round_rect_outline(hdc, card, 22, p.border)?;
        let banner = RECT {
            left: card.left + 2,
            top: card.top + 2,
            right: card.right - 2,
            bottom: card.top + 242,
        };
        let banner_w = banner.right - banner.left;
        let banner_h = banner.bottom - banner.top;
        if !st.carousel_bitmaps.is_empty() {
            let hbmp = st.carousel_bitmaps[0];
            let mem_dc = CreateCompatibleDC(hdc);
            if !mem_dc.is_invalid() {
                let old = SelectObject(mem_dc, hbmp);
                let source_h = ((640 * banner_h) / banner_w).clamp(1, 360);
                let source_y = (360 - source_h) / 2;
                let _ = StretchBlt(
                    hdc,
                    banner.left,
                    banner.top,
                    banner_w,
                    banner_h,
                    mem_dc,
                    0,
                    source_y,
                    640,
                    source_h,
                    SRCCOPY,
                );
                let _ = SelectObject(mem_dc, old);
                let _ = DeleteDC(mem_dc);
            }
        } else {
            fill_gradient(hdc, &banner, p.banner_top, p.banner_bot)?;
        }
        // Solid brand bars keep the title readable over varied screenshots.
        fill_rect(
            hdc,
            &RECT {
                left: banner.left,
                top: banner.top,
                right: banner.right,
                bottom: banner.top + 112,
            },
            COLORREF(0x0017100B),
        )?;
        fill_rect(
            hdc,
            &RECT {
                left: banner.left,
                top: banner.bottom - 42,
                right: banner.right,
                bottom: banner.bottom,
            },
            COLORREF(0x00A43C12),
        )?;
        draw_logo(hdc, banner.left + 22, banner.top + 22, 70, p.accent)?;
        draw_text(
            hdc,
            "FLUX REC",
            banner.left + 108,
            banner.top + 34,
            340,
            36,
            COLORREF(0x00FFFFFF),
            28,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "THE COMMUNITY IS YOURS",
            banner.left + 110,
            banner.top + 72,
            340,
            20,
            COLORREF(0x00FFFFFF),
            12,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "A private Rec Room revival by Ripo Team",
            banner.left + 20,
            banner.bottom - 33,
            banner_w - 40,
            22,
            COLORREF(0x00FFFFFF),
            12,
            true,
            false,
        )?;

        draw_text(
            hdc,
            "Your game",
            card.left + 24,
            banner.bottom + 18,
            360,
            28,
            p.text,
            20,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Explore player-created rooms, play with friends and build your own worlds.",
            card.left + 24,
            banner.bottom + 52,
            banner_w - 48,
            42,
            p.text_dim,
            13,
            false,
            false,
        )?;
        draw_text(
            hdc,
            &format!("Launcher  v{}  ·  2025 client", st.version),
            card.left + 24,
            banner.bottom + 112,
            banner_w - 48,
            22,
            p.text_dim,
            12,
            false,
            false,
        )?;

        // Live presence panel with an actual drawn person glyph and an explicit refresh target.
        let player_card = RECT {
            left: status_x,
            top: card.top,
            right: content_right,
            bottom: card.top + 150,
        };
        round_rect_path(hdc, &player_card, 18, p.bg_card)?;
        round_rect_outline(hdc, &player_card, 18, p.border)?;
        draw_status_pulse(
            hdc,
            player_card.left + 20,
            player_card.top + 24,
            st.anim_t,
            COLORREF(0x0038A169),
        )?;
        draw_text(
            hdc,
            "LIVE PRESENCE",
            player_card.left + 34,
            player_card.top + 15,
            150,
            18,
            p.text_dim,
            10,
            true,
            false,
        )?;
        draw_people_icon(hdc, player_card.left + 18, player_card.top + 54, p.accent)?;
        let players_text = match (st.players, st.player_count_pending) {
            (Some(count), _) => format!("{count} online"),
            (None, true) => "Checking…".to_string(),
            (None, false) => "Unavailable".to_string(),
        };
        draw_text(
            hdc,
            &players_text,
            player_card.left + 54,
            player_card.top + 57,
            190,
            28,
            p.text,
            20,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Live session count",
            player_card.left + 54,
            player_card.top + 88,
            184,
            20,
            p.text_dim,
            11,
            false,
            false,
        )?;
        let refresh_hover = st.hover == Hot::RefreshPlayers;
        round_rect_path(
            hdc,
            &rects.refresh_players,
            10,
            if refresh_hover { p.bg_hover } else { p.bg_card },
        )?;
        round_rect_outline(hdc, &rects.refresh_players, 10, p.border)?;
        draw_text(
            hdc,
            if st.player_count_pending {
                "Refreshing"
            } else {
                "Refresh"
            },
            rects.refresh_players.left,
            rects.refresh_players.top + 5,
            rects.refresh_players.right - rects.refresh_players.left,
            20,
            p.accent,
            11,
            true,
            true,
        )?;

        // Update/install health card.
        let update_card = RECT {
            left: status_x,
            top: card.top + 166,
            right: content_right,
            bottom: card.top + 296,
        };
        round_rect_path(hdc, &update_card, 18, p.bg_card)?;
        round_rect_outline(hdc, &update_card, 18, p.border)?;
        draw_text(
            hdc,
            "INSTALL & UPDATE",
            update_card.left + 18,
            update_card.top + 16,
            220,
            18,
            p.text_dim,
            10,
            true,
            false,
        )?;
        let update_label = if st.checking {
            "Checking for updates"
        } else if st.install_required {
            "Install required"
        } else if st.update_available {
            "New update available"
        } else if !st.check_error.is_empty() {
            "Check could not complete"
        } else {
            "Ready to play"
        };
        draw_text(
            hdc,
            update_label,
            update_card.left + 18,
            update_card.top + 42,
            update_card.right - update_card.left - 36,
            25,
            if st.update_available || st.install_required {
                p.accent
            } else {
                p.text
            },
            16,
            true,
            false,
        )?;
        let update_sub = if st.update_available {
            st.update_version.trim_start_matches("recflare-installer-v")
        } else if !st.check_error.is_empty() {
            "Could not check updates. Try again."
        } else {
            "Updates are checked before the game launches."
        };
        draw_text(
            hdc,
            update_sub,
            update_card.left + 18,
            update_card.top + 74,
            update_card.right - update_card.left - 36,
            34,
            p.text_dim,
            11,
            false,
            false,
        )?;

        // Primary action.
        let play_hover = st.hover == Hot::Play;
        round_rect_path(
            hdc,
            &rects.play,
            15,
            if play_hover { p.accent_dark } else { p.accent },
        )?;
        let play_label = if st.install_required {
            "INSTALL FLUX REC"
        } else if st.update_available {
            "UPDATE & PLAY"
        } else {
            "PLAY FLUX REC"
        };
        draw_text(
            hdc,
            play_label,
            rects.play.left + 6,
            rects.play.top + 13,
            rects.play.right - rects.play.left - 12,
            28,
            COLORREF(0x00FFFFFF),
            15,
            true,
            true,
        )?;

        // Frequently used controls are separate, visible tiles rather than hidden in a context menu.
        let actions = [
            (
                &rects.check_updates,
                "Check for updates",
                "Run a live release check",
                Hot::CheckForUpdates,
            ),
            (
                &rects.open_folder,
                "Open install folder",
                "Browse local game files",
                Hot::OpenInstallFolder,
            ),
            (
                &rects.details,
                "Game details",
                "Screenshots and overview",
                Hot::Details,
            ),
            (
                &rects.uninstall,
                if st.install_required {
                    "Not installed"
                } else {
                    "Uninstall"
                },
                if st.install_required {
                    "Install first to enable"
                } else {
                    "Remove local game files"
                },
                Hot::Uninstall,
            ),
        ];
        for (r, title, subtitle, hot) in actions {
            let hover = st.hover == hot;
            round_rect_path(hdc, r, 14, if hover { p.bg_hover } else { p.bg_card })?;
            round_rect_outline(hdc, r, 14, p.border)?;
            draw_text(
                hdc,
                title,
                r.left + 8,
                r.top + 10,
                r.right - r.left - 16,
                21,
                if hover { p.accent } else { p.text },
                12,
                true,
                true,
            )?;
            draw_text(
                hdc,
                subtitle,
                r.left + 8,
                r.top + 33,
                r.right - r.left - 16,
                18,
                p.text_dim,
                9,
                false,
                true,
            )?;
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
        round_rect_path(
            hdc,
            &shadow,
            20,
            if st.theme == Theme::Light {
                COLORREF(0x00D8D0C8)
            } else {
                COLORREF(0x00000000)
            },
        )?;
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
                // Crop the 16:9 screenshot to fill the wide banner without
                // distorting the artwork.
                let source_h = ((640 * banner_h) / banner_w).clamp(1, 360);
                let source_y = (360 - source_h) / 2;
                let _ = StretchBlt(
                    hdc,
                    banner.left,
                    banner.top,
                    banner_w,
                    banner_h,
                    mem_dc,
                    0,
                    source_y,
                    640,
                    source_h,
                    SRCCOPY,
                );
                SelectObject(mem_dc, old);
                DeleteDC(mem_dc);
            }
        } else {
            fill_gradient(hdc, &banner, p.banner_top, p.banner_bot)?;
        }
        // Note: text drawn below uses shadow for readability on the screenshot.
        // Big logo on banner; fall back to a drawn mark if decoding failed.
        if draw_logo_bitmap(hdc, st.logo_bitmap, banner.left + 36, banner.top + 44, 96).is_err() {
            draw_logo(
                hdc,
                banner.left + 36,
                banner.top + 44,
                96,
                COLORREF(0x00FFFFFF),
            )?;
        }
        draw_text(
            hdc,
            "FLUX REC",
            banner.left + 158,
            banner.top + 58,
            400,
            50,
            COLORREF(0x00140D08),
            40,
            true,
            false,
        )?;
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
            banner.top + 110,
            400,
            24,
            COLORREF(0x00140D08),
            15,
            false,
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
            Some(c) => format!("{} players online", c),
            None => "Player count unavailable".to_string(),
        };
        draw_text(
            hdc,
            &players_txt,
            card.left + 36,
            stats_y,
            200,
            24,
            p.text,
            14,
            true,
            false,
        )?;
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

        // Status chip: distinguish install, update, offline and ready states.
        let status = if st.checking {
            "Checking for updates…".to_string()
        } else if !st.check_error.is_empty() {
            st.check_error.clone()
        } else if st.install_required {
            "Ready to install".to_string()
        } else if st.update_available {
            let version = st
                .update_version
                .trim_start_matches("recflare-installer-v")
                .trim_start_matches('v');
            format!("Update v{version} available")
        } else if st.ready {
            "Ready to play — updates are checked when you press Play".to_string()
        } else {
            "Preparing your library…".to_string()
        };
        let status_rect = RECT {
            left: card.left + 36,
            top: card.top + 348,
            right: card.right - 36,
            bottom: card.top + 376,
        };
        let highlighted = st.update_available || st.install_required;
        let status_error = !st.check_error.is_empty();
        let status_fill = if highlighted {
            p.accent
        } else if status_error && st.theme == Theme::Light {
            COLORREF(0x00E8E2FF)
        } else {
            p.bg_hover
        };
        let status_text = if highlighted {
            COLORREF(0x00FFFFFF)
        } else if status_error && st.theme == Theme::Light {
            COLORREF(0x002B39C0)
        } else if status_error {
            COLORREF(0x005E7BFF)
        } else {
            p.text_dim
        };
        round_rect_path(hdc, &status_rect, 10, status_fill)?;
        draw_text(
            hdc,
            &status,
            status_rect.left + 12,
            status_rect.top + 2,
            status_rect.right - status_rect.left - 24,
            24,
            status_text,
            13,
            highlighted,
            false,
        )?;

        // PLAY button.
        let play = &rects.play;
        let play_hover = st.hover == Hot::Play;
        let play_label = if st.install_required {
            "INSTALL"
        } else if st.update_available {
            "UPDATE & PLAY"
        } else {
            "PLAY"
        };
        let btn_color = if play_hover { p.accent_dark } else { p.accent };
        // Button pulse animation when ready.
        round_rect_path(hdc, play, 16, btn_color)?;
        draw_text(
            hdc,
            play_label,
            play.left,
            play.top + 12,
            play.right - play.left,
            28,
            COLORREF(0x00FFFFFF),
            16,
            true,
            true,
        )?;

        // Uninstall is only available after game files exist.
        if !st.install_required {
            let un = &rects.uninstall;
            if st.hover == Hot::Uninstall {
                round_rect_path(hdc, un, 10, p.bg_hover)?;
            }
            draw_text(
                hdc,
                "Uninstall",
                un.left,
                un.top + 6,
                un.right - un.left,
                22,
                p.text_dim,
                13,
                false,
                true,
            )?;
        }

        // v1.0.6: Details button (explicit, not just card click).
        let det = &rects.details;
        if st.hover == Hot::Details {
            round_rect_path(hdc, det, 10, p.bg_hover)?;
        } else {
            round_rect_path(hdc, det, 10, p.bg_card)?;
            round_rect_outline(hdc, det, 10, p.border)?;
        }
        draw_text(
            hdc,
            "Details",
            det.left,
            det.top + 14,
            det.right - det.left,
            22,
            p.accent,
            13,
            true,
            true,
        )?;

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
        draw_text(
            hdc,
            "←  Back",
            back.left + 8,
            back.top + 6,
            90,
            22,
            p.text_dim,
            14,
            false,
            false,
        )?;

        // Title + logo. Use the same premultiplied-alpha path as the banner.
        if draw_logo_bitmap(hdc, st.logo_bitmap, cx + 40, TITLE_H + 52, 48).is_err() {
            draw_logo(hdc, cx + 40, TITLE_H + 52, 48, p.accent)?;
        }
        draw_text(
            hdc,
            "Flux Rec",
            cx + 104,
            TITLE_H + 52,
            400,
            36,
            p.text,
            28,
            true,
            false,
        )?;
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
                let _ = StretchBlt(
                    hdc,
                    car.left,
                    car.top,
                    car.right - car.left,
                    car.bottom - car.top,
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
            let name = SCREENSHOT_NAMES.get(idx).copied().unwrap_or("");
            draw_text(
                hdc,
                name,
                car.left,
                car.bottom + 8,
                car.right - car.left,
                22,
                p.text_dim,
                13,
                false,
                true,
            )?;
            // Dots.
            let n = st.carousel_bitmaps.len();
            let dot_y = car.bottom + 34;
            let total_w = (n as i32) * 16;
            let start_x = car.left + ((car.right - car.left) - total_w) / 2;
            for i in 0..n {
                let c = if i == idx { p.accent } else { p.track };
                let br = CreateSolidBrush(c);
                if !br.is_invalid() {
                    let pen = CreatePen(PS_SOLID, 1, c);
                    let ob = SelectObject(hdc, br);
                    let op = SelectObject(hdc, pen);
                    Ellipse(
                        hdc,
                        start_x + (i as i32) * 16,
                        dot_y,
                        start_x + (i as i32) * 16 + 8,
                        dot_y + 8,
                    );
                    SelectObject(hdc, ob);
                    SelectObject(hdc, op);
                    DeleteObject(br);
                    DeleteObject(pen);
                }
            }
        } else {
            draw_text(
                hdc,
                "Screenshots loading…",
                car.left,
                car.top + 150,
                car.right - car.left,
                30,
                p.text_dim,
                14,
                false,
                true,
            )?;
        }

        // Prev/Next buttons.
        for (r, label, hot) in [
            (&rects.carousel_prev, "‹", Hot::CarouselPrev),
            (&rects.carousel_next, "›", Hot::CarouselNext),
        ] {
            if st.hover == hot {
                round_rect_path(hdc, r, 20, p.accent)?;
                draw_text(
                    hdc,
                    label,
                    r.left,
                    r.top + 2,
                    40,
                    36,
                    COLORREF(0x00FFFFFF),
                    24,
                    true,
                    true,
                )?;
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
                draw_text(
                    hdc,
                    label,
                    r.left,
                    r.top + 2,
                    40,
                    36,
                    p.text,
                    24,
                    true,
                    true,
                )?;
            }
        }

        // Description.
        let desc = "Hang out with friends, explore player-created rooms, and play paintball, laser tag, quests and more. No Steam required.";
        draw_text(
            hdc,
            desc,
            cx + 40,
            car.bottom + 46,
            430,
            44,
            p.text_dim,
            13,
            false,
            false,
        )?;

        // Player count.
        draw_people_icon(hdc, cx + 40, car.bottom + 104, p.accent)?;
        let players_txt = match (st.players, st.player_count_pending) {
            (Some(c), _) => format!("{c} online now"),
            (None, true) => "Checking live player count…".to_string(),
            (None, false) => "Player count unavailable".to_string(),
        };
        draw_text(
            hdc,
            &players_txt,
            cx + 72,
            car.bottom + 108,
            398,
            24,
            p.text,
            14,
            true,
            false,
        )?;

        // PLAY button.
        let dp = &rects.detail_play;
        let hover = st.hover == Hot::DetailPlay;
        round_rect_path(hdc, dp, 14, if hover { p.accent_dark } else { p.accent })?;
        let label = if st.install_required {
            "INSTALL"
        } else if st.update_available {
            "UPDATE & PLAY"
        } else {
            "PLAY"
        };
        draw_text(
            hdc,
            label,
            dp.left,
            dp.top + 14,
            dp.right - dp.left,
            26,
            COLORREF(0x00FFFFFF),
            15,
            true,
            true,
        )?;

        Ok(())
    }

    unsafe fn draw_settings_console(
        hdc: HDC,
        st: &mut State,
        rects: &Rects,
        p: &Palette,
    ) -> Silent {
        let left = SIDEBAR_W + 28;
        let right = WIN_W - 28;
        draw_text(
            hdc,
            "PREFERENCES & TOOLS",
            left,
            TITLE_H + 12,
            420,
            18,
            p.accent,
            11,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Make Flux Rec yours",
            left,
            TITLE_H + 31,
            460,
            32,
            p.text,
            24,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Quick settings and safe local actions.",
            left + 500,
            TITLE_H + 42,
            350,
            20,
            p.text_dim,
            12,
            false,
            false,
        )?;

        let theme_card = RECT {
            left,
            top: TITLE_H + 60,
            right,
            bottom: TITLE_H + 268,
        };
        round_rect_path(hdc, &theme_card, 20, p.bg_card)?;
        round_rect_outline(hdc, &theme_card, 20, p.border)?;
        draw_text(
            hdc,
            "Appearance",
            left + 24,
            theme_card.top + 20,
            340,
            24,
            p.text,
            17,
            true,
            false,
        )?;
        draw_text(
            hdc,
            "Choose a theme. Your selection is saved for next time.",
            left + 24,
            theme_card.top + 50,
            600,
            20,
            p.text_dim,
            12,
            false,
            false,
        )?;
        for (r, label, active, hot) in [
            (
                &rects.theme_light,
                "Light theme",
                st.theme == Theme::Light,
                Hot::ThemeLight,
            ),
            (
                &rects.theme_dark,
                "Dark theme",
                st.theme == Theme::Dark,
                Hot::ThemeDark,
            ),
        ] {
            let hover = st.hover == hot;
            round_rect_path(
                hdc,
                r,
                13,
                if active {
                    p.accent
                } else if hover {
                    p.bg_hover
                } else {
                    p.bg_card
                },
            )?;
            round_rect_outline(hdc, r, 13, if active { p.accent } else { p.border })?;
            draw_text(
                hdc,
                label,
                r.left + 8,
                r.top + 12,
                r.right - r.left - 16,
                22,
                if active { COLORREF(0x00FFFFFF) } else { p.text },
                13,
                true,
                true,
            )?;
        }

        let install_card = RECT {
            left,
            top: TITLE_H + 286,
            right,
            bottom: TITLE_H + 416,
        };
        round_rect_path(hdc, &install_card, 20, p.bg_card)?;
        round_rect_outline(hdc, &install_card, 20, p.border)?;
        draw_text(
            hdc,
            "Install location",
            left + 24,
            install_card.top + 18,
            320,
            24,
            p.text,
            16,
            true,
            false,
        )?;
        draw_text_ellipsis(
            hdc,
            &st.install_dir,
            left + 24,
            install_card.top + 52,
            right - left - 48,
            24,
            p.text_dim,
            12,
        )?;
        draw_text(
            hdc,
            "Open this folder in File Explorer to inspect local files.",
            left + 24,
            install_card.top + 82,
            right - left - 48,
            20,
            p.text_dim,
            11,
            false,
            false,
        )?;

        let tools_card = RECT {
            left,
            top: TITLE_H + 434,
            right,
            bottom: TITLE_H + 592,
        };
        round_rect_path(hdc, &tools_card, 20, p.bg_card)?;
        round_rect_outline(hdc, &tools_card, 20, p.border)?;
        draw_text(
            hdc,
            "Quick actions",
            left + 24,
            tools_card.top + 16,
            320,
            22,
            p.text,
            16,
            true,
            false,
        )?;
        for (r, label, subtitle, hot) in [
            (
                &rects.check_updates,
                "Check for updates",
                "Run a fresh GitHub release check",
                Hot::CheckForUpdates,
            ),
            (
                &rects.open_folder,
                "Open install folder",
                "Browse game files",
                Hot::OpenInstallFolder,
            ),
        ] {
            let hover = st.hover == hot;
            round_rect_path(hdc, r, 13, if hover { p.bg_hover } else { p.bg_card })?;
            round_rect_outline(hdc, r, 13, p.border)?;
            draw_text(
                hdc,
                label,
                r.left + 8,
                r.top + 9,
                r.right - r.left - 16,
                20,
                if hover { p.accent } else { p.text },
                12,
                true,
                true,
            )?;
            draw_text(
                hdc,
                subtitle,
                r.left + 8,
                r.top + 32,
                r.right - r.left - 16,
                16,
                p.text_dim,
                9,
                false,
                true,
            )?;
        }
        draw_text(
            hdc,
            &format!("Flux Rec launcher v{}  ·  Ripo Team", st.version),
            left + 2,
            WIN_H - 38,
            right - left,
            20,
            p.text_dim,
            11,
            false,
            false,
        )?;
        Ok(())
    }

    unsafe fn draw_settings_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = SIDEBAR_W;
        draw_text(
            hdc,
            "Settings",
            cx + 40,
            TITLE_H + 16,
            300,
            30,
            p.text,
            22,
            true,
            false,
        )?;

        draw_text(
            hdc,
            "Appearance",
            cx + 60,
            TITLE_H + 80,
            300,
            26,
            p.text,
            16,
            true,
            false,
        )?;
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
            if light_active {
                p.accent
            } else if lhover {
                p.bg_hover
            } else {
                p.bg_card
            },
        )?;
        draw_text(
            hdc,
            "Light",
            lr.left,
            lr.top + 10,
            lr.right - lr.left,
            24,
            if light_active {
                COLORREF(0x00FFFFFF)
            } else {
                p.text
            },
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
            if dark_active {
                p.accent
            } else if dhover {
                p.bg_hover
            } else {
                p.bg_card
            },
        )?;
        draw_text(
            hdc,
            "Dark",
            dr.left,
            dr.top + 10,
            dr.right - dr.left,
            24,
            if dark_active {
                COLORREF(0x00FFFFFF)
            } else {
                p.text
            },
            14,
            dark_active,
            true,
        )?;

        // Install location.
        draw_text(
            hdc,
            "Install location",
            cx + 60,
            TITLE_H + 220,
            300,
            26,
            p.text,
            16,
            true,
            false,
        )?;
        let dir = st.install_dir.clone();
        draw_text_ellipsis(hdc, &dir, cx + 60, TITLE_H + 248, 700, 22, p.text_dim, 13)?;

        // About.
        draw_text(
            hdc,
            "About",
            cx + 60,
            TITLE_H + 300,
            300,
            26,
            p.text,
            16,
            true,
            false,
        )?;
        draw_text(
            hdc,
            &format!(
                "Ripo Team Launcher v{}\n© 2026 Ripo Team. All rights reserved.",
                st.version
            ),
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

        if draw_logo_bitmap(hdc, st.logo_bitmap, bx + 188, by - 110, 64).is_err() {
            draw_logo(hdc, bx + 188, by - 110, 64, p.accent)?;
        }
        draw_text(
            hdc,
            "FLUX REC",
            bx,
            by - 36,
            440,
            34,
            p.text,
            24,
            true,
            true,
        )?;

        if !st.stage.is_empty() {
            draw_text(
                hdc,
                &st.stage.clone(),
                bx,
                by + 10,
                440,
                28,
                p.text,
                16,
                false,
                true,
            )?;
        }
        if !st.detail.is_empty() {
            draw_text(
                hdc,
                &st.detail.clone(),
                bx,
                by + 38,
                440,
                24,
                p.text_dim,
                13,
                false,
                true,
            )?;
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

        let full = RECT {
            left: 0,
            top: 0,
            right: SPLASH_W,
            bottom: SPLASH_H,
        };
        fill_gradient(mem, &full, p.bg_title, p.bg)?;

        // Big logo with a subtle pulse.
        let pulse = ((st.t as f32 * 0.06).sin() * 4.0) as i32;
        if draw_logo_bitmap(mem, st.logo_bitmap, SPLASH_W / 2 - 48, 52 + pulse / 2, 96).is_err() {
            draw_logo(mem, SPLASH_W / 2 - 48, 52 + pulse / 2, 96, p.accent)?;
        }
        draw_text(
            mem, "FLUX REC", 0, 168, SPLASH_W, 40, p.text, 30, true, true,
        )?;
        draw_text(
            mem,
            "Launching game…",
            0,
            208,
            SPLASH_W,
            26,
            p.text_dim,
            15,
            false,
            true,
        )?;

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
            let seg = RECT {
                left: seg_l + 2,
                top: bar.top + 2,
                right: seg_r - 2,
                bottom: bar.bottom - 2,
            };
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
                    let st = &mut *ptr;
                    let mut repaint = false;
                    if st.view == View::Working {
                        st.anim_t = st.anim_t.wrapping_add(1);
                        repaint = st.anim_t % 2 == 0;
                    }
                    // Auto-advance the carousel every ~4s (50ms timer * 80).
                    if st.view == View::Detail && !st.carousel_bitmaps.is_empty() {
                        st.anim_t = st.anim_t.wrapping_add(1);
                        st.carousel_tick += 1;
                        if st.carousel_tick >= 80 {
                            st.carousel_tick = 0;
                            st.carousel_idx = (st.carousel_idx + 1) % st.carousel_bitmaps.len();
                            repaint = true;
                        }
                    }
                    if repaint {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
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
            Hot::Uninstall if !st.install_required => {
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
            Hot::Uninstall => {}
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
            Hot::RefreshPlayers => {
                if !st.player_count_pending {
                    st.players = None;
                    st.player_count_pending = true;
                    let _ = st.cmd_tx.send(LibCmd::RefreshPlayerCount);
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::CheckForUpdates => {
                if !st.checking {
                    st.checking = true;
                    st.check_error.clear();
                    let _ = st.cmd_tx.send(LibCmd::CheckForUpdates);
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::OpenInstallFolder => {
                let _ = st.cmd_tx.send(LibCmd::OpenInstallFolder);
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
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SplashState;
                if !ptr.is_null() && !(*ptr).logo_bitmap.is_invalid() {
                    let _ = DeleteObject((*ptr).logo_bitmap);
                    (*ptr).logo_bitmap = HBITMAP(std::ptr::null_mut());
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
