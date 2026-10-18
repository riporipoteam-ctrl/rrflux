//! Ripo Team Launcher — multi-game storefront and library UI.
//!
//! A game-library style launcher (inspired by Steam/EGS):
//! - Ripo Team shell with Store, Library, Downloads and Settings navigation.
//! - Flux Rec game card: banner, logo, description, live player count,
//!   PLAY button, uninstall.
//! - Settings: install location and light/dark theme (default: light).
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

/// UI theme. New installations start with Light; users can change it in Settings.
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
	Store,
	Downloads,
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
#[cfg(windows)]
const RIPO_TEAM_LOGO_PNG: &[u8] = include_bytes!("../assets/ripo-team-logo.png");

/// Strip any internal tag prefix so it stays out of user-facing version labels.
fn display_update_version(tag: &str) -> &str {
    tag.rsplit_once("-v")
        .map(|(_, version)| version)
        .unwrap_or(tag)
        .trim_start_matches('v')
}

#[cfg(test)]
mod release_label_tests {
    use super::display_update_version;

    #[test]
    fn updater_label_hides_internal_release_prefix() {
        assert_eq!(display_update_version("legacy-launcher-v1.0.31"), "1.0.31");
        assert_eq!(display_update_version("v1.0.31"), "1.0.31");
        assert_eq!(display_update_version("1.0.31"), "1.0.31");
    }
}

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
        display_update_version, premultiply_channel, LibraryConfig, Theme, View, FLUXREC_LOGO_PNG,
        SCREENSHOTS, SCREENSHOT_NAMES, RIPO_TEAM_LOGO_PNG,
    };
    use crate::library::{LibCmd, LibMsg};
    use std::ffi::c_void;
    use std::{cell::RefCell, collections::HashMap};
    use std::sync::mpsc::{Receiver, Sender, TryRecvError};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window size.
    const WIN_W: i32 = 1360;
    const WIN_H: i32 = 820;
    const SIDEBAR_W: i32 = 208;
    const CONTENT_X: i32 = SIDEBAR_W + 24;
    const TITLE_H: i32 = 58;
    const CORNER: i32 = 24;

    const SPLASH_W: i32 = 480;
    const SPLASH_H: i32 = 320;

    const TIMER_ID: usize = 1;
    const TIMER_MS: u32 = 100;

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
        bg: COLORREF(0x001B110C),       // #0C111B
        bg_title: COLORREF(0x00231710), // #101723
        bg_side: COLORREF(0x002B1D15),  // #151D2B
        bg_card: COLORREF(0x0035251B),  // #1B2535
        bg_hover: COLORREF(0x00443220), // #203244 hover
        text: COLORREF(0x00FFFFFF),
        text_dim: COLORREF(0x00D3C3B7), // #B7C3D3
        accent: COLORREF(0x00FFAB4A), // #4AABFF
        accent_dark: COLORREF(0x00D78332), // #3283D7
        track: COLORREF(0x0045352A), // #2A3545
        border: COLORREF(0x004C3A2C), // #2C3A4C
        banner_top: COLORREF(0x00FFAB4A),
        banner_bot: COLORREF(0x008F572A),
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
		NavStore,
        NavLibrary,
		NavDownloads,
        NavSettings,
        Play,
        Uninstall,
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
        win_w: i32,
        win_h: i32,
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
        // Launcher shell mark. Game views keep using `logo_bitmap` above.
        team_logo_bitmap: HBITMAP,
        // v1.0.5: update check error message (empty if none)
        check_error: String,
    }

    // Button rects (computed per paint).
    struct Rects {
        nav_store: RECT,
        nav_library: RECT,
        nav_downloads: RECT,
        nav_settings: RECT,
        play: RECT,
        library_play: RECT,
        downloads_play: RECT,
        uninstall: RECT,
        details: RECT, // v1.0.6: explicit Details button
        theme_light: RECT,
        theme_dark: RECT,
        check_updates: RECT,
        open_folder: RECT,
        close: RECT,
        minimize: RECT,
        card: RECT,
        library_card: RECT,
        downloads_card: RECT,
        downloads_check_updates: RECT,
        downloads_open_folder: RECT,
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
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            let window_w = WIN_W.min((sw - 48).max(1024));
            let window_h = WIN_H.min((sh - 80).max(660));

            let cls = w!("RipoTeamLauncherLib");
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinst.into(),
                hIcon: LoadIconW(hinst, PCWSTR(1usize as *const u16))
                    .unwrap_or_default(),
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
                win_w: window_w,
                win_h: window_h,
                install_dir: config.install_dir.to_string_lossy().into_owned(),
                view: View::Store,
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
                team_logo_bitmap: HBITMAP(std::ptr::null_mut()),
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
            state.team_logo_bitmap = png_to_bitmap(RIPO_TEAM_LOGO_PNG, 128, 128);
            let state_ptr = Box::into_raw(state);

            // Center on screen.
            let x = (sw - window_w) / 2;
            let y = (sh - window_h) / 2;

            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                cls,
                w!("Ripo Team Launcher"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                window_w,
                window_h,
                None,
                None,
                hinst,
                Some(state_ptr as *const c_void),
            )
            .map_err(|_| {
                let failed_state = Box::from_raw(state_ptr);
                for bitmap in [failed_state.logo_bitmap, failed_state.team_logo_bitmap] {
                    if !bitmap.is_invalid() {
                        let _ = DeleteObject(bitmap);
                    }
                }
                for bitmap in failed_state.carousel_bitmaps {
                    let _ = DeleteObject(bitmap);
                }
            })?;

            // Rounded corners.
            let rgn = CreateRoundRectRgn(0, 0, window_w + 1, window_h + 1, CORNER, CORNER);
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
                hIcon: LoadIconW(hinst, PCWSTR(1usize as *const u16))
                    .unwrap_or_default(),
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
    struct UiFontCache(HashMap<(i32, bool), HFONT>);

    impl Drop for UiFontCache {
        fn drop(&mut self) {
            for (_, font) in self.0.drain() {
                unsafe {
                    let _ = DeleteObject(font);
                }
            }
        }
    }

    thread_local! {
        /// The launcher repaints often while checking or installing. Reuse its small,
        /// finite font set instead of creating and deleting a GDI font for every label.
        static UI_FONTS: RefCell<UiFontCache> = RefCell::new(UiFontCache(HashMap::new()));
    }

    unsafe fn cached_font(size: i32, bold: bool) -> HFONT {
        let key = (size.max(1), bold);
        UI_FONTS.with(|fonts| {
            let mut fonts = fonts.borrow_mut();
            if let Some(font) = fonts.0.get(&key) {
                return *font;
            }
            let font = CreateFontW(
                -key.0,
                0,
                0,
                0,
                if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
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
            if !font.is_invalid() {
                fonts.0.insert(key, font);
            }
            font
        })
    }

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
        let font = cached_font(size.max(12), bold);
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
        let font = cached_font(size, false);
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
    fn compute_rects(win_w: i32, win_h: i32) -> Rects {
        let content_x = CONTENT_X + 8;
        let content_right = win_w - 32;
        let content_w = (content_right - content_x).max(420);
        let hero_top = TITLE_H + 82;
        let hero_bottom = hero_top + ((win_h - TITLE_H - 48) * 54 / 100).clamp(320, 430);
        let library_top = TITLE_H + 62;
        let library_bottom = library_top + 380;
        let library_action_y = library_bottom - 62;
        let settings_tools_y = TITLE_H + 430;
        let carousel_left = content_x;
        let carousel_width = ((content_w - 32) * 60 / 100).clamp(400, 680);
        let carousel_top = TITLE_H + 62;
        let downloads_top = TITLE_H + 86;
        let downloads_bottom = win_h - 82;
        Rects {
            nav_store: RECT { left: 16, top: TITLE_H + 54, right: SIDEBAR_W - 16, bottom: TITLE_H + 102 },
            nav_library: RECT { left: 16, top: TITLE_H + 112, right: SIDEBAR_W - 16, bottom: TITLE_H + 160 },
            nav_downloads: RECT { left: 16, top: TITLE_H + 170, right: SIDEBAR_W - 16, bottom: TITLE_H + 218 },
            nav_settings: RECT { left: 16, top: TITLE_H + 228, right: SIDEBAR_W - 16, bottom: TITLE_H + 276 },
            // Home hero.
            card: RECT {
                left: content_x,
                top: hero_top,
                right: content_right,
                bottom: hero_bottom,
            },
            play: RECT {
                left: content_right - 236,
                top: hero_bottom - 76,
                right: content_right,
                bottom: hero_bottom - 20,
            },
            library_play: RECT {
                left: content_right - 224,
                top: library_action_y,
                right: content_right,
                bottom: library_action_y + 48,
            },
            downloads_play: RECT {
                left: content_right - 220,
                top: downloads_bottom - 56,
                right: content_right,
                bottom: downloads_bottom - 8,
            },
            uninstall: RECT {
                left: content_right - 394,
                top: library_action_y,
                right: content_right - 238,
                bottom: library_action_y + 48,
            },
            details: RECT {
                left: content_right - 440,
                top: hero_bottom - 76,
                right: content_right - 248,
                bottom: hero_bottom - 20,
            },
            // Settings view
            theme_light: RECT {
                left: content_x + 24,
                top: TITLE_H + 190,
                right: content_x + 194,
                bottom: TITLE_H + 238,
            },
            theme_dark: RECT {
                left: content_x + 208,
                top: TITLE_H + 190,
                right: content_x + 378,
                bottom: TITLE_H + 238,
            },
            check_updates: RECT {
                left: content_x + 24,
                top: settings_tools_y + 72,
                right: content_x + 374,
                bottom: settings_tools_y + 120,
            },
            open_folder: RECT {
                left: content_x + 398,
                top: settings_tools_y + 72,
                right: content_x + 748,
                bottom: settings_tools_y + 120,
            },
            downloads_check_updates: RECT {
                left: content_x + 24,
                top: downloads_bottom - 56,
                right: content_x + 244,
                bottom: downloads_bottom - 8,
            },
            downloads_open_folder: RECT {
                left: content_x + 258,
                top: downloads_bottom - 56,
                right: content_x + 492,
                bottom: downloads_bottom - 8,
            },
            close: RECT {
                left: win_w - 52,
                top: 6,
                right: win_w - 12,
                bottom: 40,
            },
            minimize: RECT {
                left: win_w - 96,
                top: 6,
                right: win_w - 56,
                bottom: 40,
            },
            library_card: RECT {
                left: content_x,
                top: library_top,
                right: content_right,
                bottom: library_bottom,
            },
            downloads_card: RECT {
                left: content_x,
                top: downloads_top,
                right: content_right,
                bottom: downloads_bottom,
            },
            // v1.0.4: detail view
            back: RECT {
                left: content_x + 4,
                top: TITLE_H + 14,
                right: content_x + 120,
                bottom: TITLE_H + 48,
            },
            carousel: RECT {
                left: carousel_left,
                top: carousel_top,
                right: carousel_left + carousel_width,
                bottom: carousel_top + carousel_width * 9 / 16,
            },
            carousel_prev: RECT {
                left: carousel_left + 12,
                top: carousel_top + (carousel_width * 9 / 16) / 2 - 20,
                right: carousel_left + 52,
                bottom: carousel_top + (carousel_width * 9 / 16) / 2 + 20,
            },
            carousel_next: RECT {
                left: carousel_left + carousel_width - 52,
                top: carousel_top + (carousel_width * 9 / 16) / 2 - 20,
                right: carousel_left + carousel_width - 12,
                bottom: carousel_top + (carousel_width * 9 / 16) / 2 + 20,
            },
            detail_play: RECT {
                left: content_right - 226,
                top: win_h - 108,
                right: content_right,
                bottom: win_h - 56,
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
        if pt_in(&rects.nav_store, x, y) {
            return Hot::NavStore;
        }
        if pt_in(&rects.nav_library, x, y) {
            return Hot::NavLibrary;
        }
        if pt_in(&rects.nav_downloads, x, y) {
            return Hot::NavDownloads;
        }
        if pt_in(&rects.nav_settings, x, y) {
            return Hot::NavSettings;
        }
        match view {
            View::Store => {
                if pt_in(&rects.play, x, y) {
                    return Hot::Play;
                }
                if pt_in(&rects.details, x, y) {
                    return Hot::Details;
                }
                if pt_in(&rects.card, x, y) {
                    return Hot::Card;
                }
            }
            View::Library => {
                if pt_in(&rects.library_play, x, y) {
                    return Hot::Play;
                }
                if pt_in(&rects.uninstall, x, y) {
                    return Hot::Uninstall;
                }
                if pt_in(&rects.library_card, x, y) {
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
            View::Downloads => {
                if pt_in(&rects.downloads_play, x, y) {
                    return Hot::Play;
                }
                if pt_in(&rects.downloads_check_updates, x, y) {
                    return Hot::CheckForUpdates;
                }
                if pt_in(&rects.downloads_open_folder, x, y) {
                    return Hot::OpenInstallFolder;
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
        let rects = compute_rects(st.win_w, st.win_h);

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
        let paint_w = (rc.right - rc.left).max(1);
        let paint_h = (rc.bottom - rc.top).max(1);
        let bmp = CreateCompatibleBitmap(hdc, paint_w, paint_h);
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
            right: paint_w,
            bottom: paint_h,
        };
        fill_rect(mem, &full, p.bg)?;

        // Title bar.
        let title_r = RECT {
            left: 0,
            top: 0,
            right: paint_w,
            bottom: TITLE_H,
        };
        fill_rect(mem, &title_r, p.bg_title)?;
        let sidebar = RECT {
            left: 0,
            top: TITLE_H,
            right: SIDEBAR_W,
            bottom: paint_h,
        };
        fill_rect(mem, &sidebar, p.bg_side)?;
        let divider = RECT {
            left: SIDEBAR_W - 1,
            top: TITLE_H,
            right: SIDEBAR_W,
            bottom: paint_h,
        };
        fill_rect(mem, &divider, p.border)?;
        draw_text(mem, "LAUNCHER", 25, TITLE_H + 22, SIDEBAR_W - 46, 18, p.text_dim, 10, true, false)?;
        if draw_logo_bitmap(mem, st.team_logo_bitmap, 14, 7, 42).is_err() {
            draw_text(mem, "RT", 18, 16, 38, 24, p.accent, 15, true, true)?;
        }
        draw_text(
            mem,
            "Ripo Team Launcher",
            64,
            17,
            300,
            24,
            p.text,
            17,
            true,
            false,
        )?;
        draw_nav(
            mem,
            &rects.nav_store,
            "Store",
            matches!(st.view, View::Store | View::Detail),
            st.hover == Hot::NavStore,
            p,
        )?;

        let footer_y = paint_h - 104;
        let footer = RECT {
            left: 16,
            top: footer_y,
            right: SIDEBAR_W - 16,
            bottom: paint_h - 18,
        };
        round_rect_path(mem, &footer, 12, p.bg)?;
        if draw_logo_bitmap(mem, st.logo_bitmap, footer.left + 12, footer.top + 14, 32).is_err() {
            draw_logo(mem, footer.left + 12, footer.top + 14, 32, p.accent)?;
        }
        draw_text(mem, "FLUX REC", footer.left + 52, footer.top + 13, footer.right - footer.left - 60, 17, p.text, 10, true, false)?;
        let game_state = if st.install_required { "Not installed" } else if st.update_available { "Update available" } else if st.checking { "Checking status" } else { "Ready to play" };
        draw_text_ellipsis(mem, game_state, footer.left + 52, footer.top + 34, footer.right - footer.left - 60, 18, p.text_dim, 9)?;
        draw_nav(
            mem,
            &rects.nav_library,
            "Library",
            st.view == View::Library,
            st.hover == Hot::NavLibrary,
            p,
        )?;
        draw_nav(
            mem,
            &rects.nav_downloads,
            "Downloads",
            st.view == View::Downloads,
            st.hover == Hot::NavDownloads,
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

        // Compact, truthful update status in the top command area.
        let live_label = if st.checking {
            "Checking"
        } else if st.update_available {
            "Update ready"
        } else if st.install_required {
            "Install needed"
        } else if !st.check_error.is_empty() {
            "Check failed"
        } else {
            "Ready"
        };
        let top_status = RECT {
            left: paint_w - 255,
            top: 10,
            right: paint_w - 128,
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

        // Content.
        match st.view {
            View::Store => draw_store_view(mem, st, &rects, p)?,
            View::Library => draw_library_installed(mem, st, &rects, p)?,
            View::Downloads => draw_downloads_view(mem, st, &rects, p)?,
            View::Detail => draw_detail_view(mem, st, &rects, p)?,
            View::Settings => draw_settings_console(mem, st, &rects, p)?,
            View::Working => draw_working_view(mem, st, &rects, p)?,
        }

        BitBlt(hdc, 0, 0, paint_w, paint_h, mem, 0, 0, SRCCOPY);
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
        if active || hover {
            round_rect_path(hdc, r, 10, if active { p.bg_hover } else { p.bg_card })?;
        }
        if active {
            let marker = RECT {
                left: r.left + 1,
                top: r.top + 10,
                right: r.left + 4,
                bottom: r.bottom - 10,
            };
            round_rect_path(hdc, &marker, 3, p.accent)?;
        }
        let icon_color = if active { p.accent } else { p.text_dim };
        draw_nav_icon(hdc, label, r.left + 15, r.top + 15, icon_color)?;
        draw_text(
            hdc,
            label,
            r.left + 48,
            r.top + 11,
            r.right - r.left - 58,
            26,
            if active { p.text } else { p.text_dim },
            14,
            active,
            false,
        )?;
        Ok(())
    }

    unsafe fn draw_nav_icon(hdc: HDC, label: &str, x: i32, y: i32, color: COLORREF) -> Silent {
        let pen = CreatePen(PS_SOLID, 2, color);
        if pen.is_invalid() {
            return Err(());
        }
        let old_pen = SelectObject(hdc, pen);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        match label {
            "Store" => {
                let _ = Rectangle(hdc, x + 1, y + 3, x + 19, y + 19);
                let _ = MoveToEx(hdc, x + 1, y + 7, None);
                let _ = LineTo(hdc, x + 19, y + 7);
            }
            "Library" => {
                let _ = Rectangle(hdc, x + 1, y + 2, x + 19, y + 20);
                let _ = MoveToEx(hdc, x + 10, y + 2, None);
                let _ = LineTo(hdc, x + 10, y + 20);
            }
            "Downloads" => {
                let _ = MoveToEx(hdc, x + 10, y + 1, None);
                let _ = LineTo(hdc, x + 10, y + 14);
                let _ = LineTo(hdc, x + 5, y + 9);
                let _ = MoveToEx(hdc, x + 10, y + 14, None);
                let _ = LineTo(hdc, x + 15, y + 9);
                let _ = MoveToEx(hdc, x + 3, y + 19, None);
                let _ = LineTo(hdc, x + 17, y + 19);
            }
            _ => {
                let _ = Ellipse(hdc, x + 3, y + 3, x + 17, y + 17);
                let _ = MoveToEx(hdc, x + 10, y, None);
                let _ = LineTo(hdc, x + 10, y + 4);
                let _ = MoveToEx(hdc, x + 10, y + 16, None);
                let _ = LineTo(hdc, x + 10, y + 20);
                let _ = MoveToEx(hdc, x, y + 10, None);
                let _ = LineTo(hdc, x + 4, y + 10);
                let _ = MoveToEx(hdc, x + 16, y + 10, None);
                let _ = LineTo(hdc, x + 20, y + 10);
            }
        }
        let _ = SelectObject(hdc, old_pen);
        let _ = SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen);
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

    /// Cover a media frame with one of the embedded 16:9 game screenshots.
    unsafe fn draw_gallery_image(hdc: HDC, st: &State, index: usize, frame: &RECT, p: &Palette) -> Silent {
        let width = frame.right - frame.left;
        let height = frame.bottom - frame.top;
        if width <= 0 || height <= 0 || st.carousel_bitmaps.is_empty() {
            return fill_gradient(hdc, frame, p.banner_top, p.banner_bot);
        }
        let bitmap = st.carousel_bitmaps[index % st.carousel_bitmaps.len()];
        if bitmap.is_invalid() {
            return fill_gradient(hdc, frame, p.banner_top, p.banner_bot);
        }
        let mem = CreateCompatibleDC(hdc);
        if mem.is_invalid() {
            return fill_gradient(hdc, frame, p.banner_top, p.banner_bot);
        }
        let old = SelectObject(mem, bitmap);
        let source_ratio = 16.0f32 / 9.0;
        let target_ratio = width as f32 / height as f32;
        let (sx, sy, sw, sh) = if target_ratio > source_ratio {
            let crop_h = (640.0 / target_ratio) as i32;
            (0, (360 - crop_h) / 2, 640, crop_h)
        } else {
            let crop_w = (360.0 * target_ratio) as i32;
            ((640 - crop_w) / 2, 0, crop_w, 360)
        };
        let old_mode = SetStretchBltMode(hdc, HALFTONE);
        let _ = StretchBlt(hdc, frame.left, frame.top, width, height, mem, sx, sy, sw.max(1), sh.max(1), SRCCOPY);
        if old_mode != 0 {
            let _ = SetStretchBltMode(hdc, STRETCH_BLT_MODE(old_mode));
        }
        let _ = SelectObject(mem, old);
        let _ = DeleteDC(mem);
        Ok(())
    }

    unsafe fn draw_store_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let hero = &rects.card;
        let content_x = hero.left;
        let content_w = hero.right - hero.left;
        draw_text(hdc, "RIPO TEAM STORE", content_x, TITLE_H + 3, 320, 17, p.accent, 9, true, false)?;
        draw_text(hdc, "Discover games", content_x, TITLE_H + 20, 400, 32, p.text, 25, true, false)?;
        draw_text(hdc, "Explore the first game from Ripo Team.", content_x, TITLE_H + 52, 430, 20, p.text_dim, 11, false, false)?;
        draw_gallery_image(hdc, st, 0, hero, p)?;
        round_rect_outline(hdc, hero, 18, p.border)?;

        // Solid lower scrim keeps the game identity and actions readable over
        // real screenshots while leaving the artwork as the dominant surface.
        let scrim = RECT { left: hero.left, top: hero.bottom - 146, right: hero.right, bottom: hero.bottom };
        fill_rect(hdc, &scrim, COLORREF(0x00221914))?;
        let status = if st.checking {
            "CHECKING FOR UPDATES"
        } else if st.install_required {
            "READY TO INSTALL"
        } else if st.update_available {
            "UPDATE AVAILABLE"
        } else if !st.check_error.is_empty() {
            "UPDATE CHECK WILL RETRY AT LAUNCH"
        } else {
            "UP TO DATE"
        };
        let status_chip = RECT { left: hero.left + 20, top: hero.top + 18, right: hero.left + 210, bottom: hero.top + 48 };
        round_rect_path(hdc, &status_chip, 15, COLORREF(0x00221914))?;
        draw_status_pulse(hdc, status_chip.left + 14, status_chip.top + 15, st.anim_t, COLORREF(0x005A8C2D))?;
        draw_text_ellipsis(hdc, status, status_chip.left + 27, status_chip.top + 5, status_chip.right - status_chip.left - 34, 20, COLORREF(0x00FFFFFF), 9)?;

        let players = match (st.players, st.player_count_pending) {
            (Some(count), _) => format!("{count} online"),
            (None, true) => "Checking players".to_string(),
            (None, false) => "Player count unavailable".to_string(),
        };
        let players_chip = RECT { left: hero.right - 246, top: hero.top + 16, right: hero.right - 20, bottom: hero.top + 52 };
        round_rect_path(hdc, &players_chip, 10, COLORREF(0x00221914))?;
        draw_people_icon(hdc, players_chip.left + 12, players_chip.top + 8, COLORREF(0x00FFFFFF))?;
        draw_text_ellipsis(hdc, &players, players_chip.left + 38, players_chip.top + 7, players_chip.right - players_chip.left - 48, 22, COLORREF(0x00FFFFFF), 10)?;

        if draw_logo_bitmap(hdc, st.logo_bitmap, hero.left + 22, hero.bottom - 112, 56).is_err() {
            draw_logo(hdc, hero.left + 22, hero.bottom - 112, 56, p.accent)?;
        }
        draw_text(hdc, "Flux Rec", hero.left + 94, hero.bottom - 112, (content_w - 565).max(180), 38, COLORREF(0x00FFFFFF), 28, true, false)?;
        draw_text(hdc, "Explore rooms, play together and create in Flux Rec.", hero.left + 94, hero.bottom - 72, (content_w - 565).max(180), 30, COLORREF(0x00E9E1DA), 12, false, false)?;
        let play_label = if st.install_required { "Install" } else if st.update_available { "Update & play" } else { "Play now" };
        round_rect_path(hdc, &rects.play, 10, if st.hover == Hot::Play { p.accent_dark } else { p.accent })?;
        draw_text(hdc, play_label, rects.play.left + 8, rects.play.top + 15, rects.play.right - rects.play.left - 16, 25, COLORREF(0x00FFFFFF), 14, true, true)?;
        round_rect_path(hdc, &rects.details, 10, if st.hover == Hot::Details { p.bg_hover } else { COLORREF(0x00FFFFFF) })?;
        round_rect_outline(hdc, &rects.details, 10, p.border)?;
        draw_text(hdc, "Game details", rects.details.left + 7, rects.details.top + 15, rects.details.right - rects.details.left - 14, 24, p.text, 12, true, true)?;

        let section_y = hero.bottom + 20;
        draw_text(hdc, "Take a look inside", content_x, section_y, 380, 25, p.text, 18, true, false)?;
        draw_text(hdc, "Scenes from Flux Rec", content_x, section_y + 25, 390, 20, p.text_dim, 11, false, false)?;
        let gap = 14;
        let tile_w = (content_w - gap * 2) / 3;
        let tile_top = section_y + 53;
        let tile_h = (st.win_h - tile_top - 18).clamp(124, 170);
        let scenes = [(0usize, "Rec Center"), (4usize, "Dorm Room"), (5usize, "Paintball")];
        for (i, (shot, title)) in scenes.iter().enumerate() {
            let x = content_x + i as i32 * (tile_w + gap);
            let card = RECT { left: x, top: tile_top, right: x + tile_w, bottom: tile_top + tile_h };
            round_rect_path(hdc, &card, 12, p.bg_card)?;
            let image = RECT { left: card.left + 1, top: card.top + 1, right: card.right - 1, bottom: card.bottom - 36 };
            draw_gallery_image(hdc, st, *shot, &image, p)?;
            round_rect_outline(hdc, &card, 12, p.border)?;
            draw_text(hdc, title, card.left + 12, card.bottom - 28, tile_w - 24, 20, p.text, 11, true, false)?;
        }
        Ok(())
    }

    unsafe fn draw_dashboard_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let content_x = CONTENT_X;
        let content_right = st.win_w - 28;
        let status_x = content_right - 272;
        let card = &rects.card;

        draw_text(
            hdc,
            "RIPO TEAM STORE",
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
            "Flux Rec is ready"
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
            "Browse Ripo Team games. Flux Rec is available.",
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
            "A Rec Room community",
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
            "Flux Rec",
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
            &format!("Ripo Team Launcher  v{}", st.version),
            card.left + 24,
            banner.bottom + 112,
            banner_w - 48,
            22,
            p.text_dim,
            12,
            false,
            false,
        )?;

        // Live presence panel with an actual drawn person glyph.
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
            "AUTOMATIC UPDATES",
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
        let update_color = if !st.check_error.is_empty() {
            COLORREF(0x005050D8)
        } else if st.update_available || st.install_required {
            p.accent
        } else {
            p.text
        };
        draw_text(
            hdc,
            update_label,
            update_card.left + 18,
            update_card.top + 42,
            update_card.right - update_card.left - 36,
            25,
            update_color,
            16,
            true,
            false,
        )?;
        let update_sub = if st.checking {
            "Checking for the latest game update…".to_string()
        } else if st.update_available {
            format!(
                "Version {} installs when you press Play.",
                display_update_version(&st.update_version)
            )
        } else if !st.check_error.is_empty() {
            "Update check failed. You can still try launching; retry here.".to_string()
        } else {
            "Updates are checked before every launch.".to_string()
        };
        draw_text(
            hdc,
            &update_sub,
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
                "Check the latest setup release",
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

    unsafe fn draw_library_installed(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let x = rects.library_card.left;
        let right = rects.library_card.right;
        draw_text(hdc, "LIBRARY", x, TITLE_H + 13, 260, 18, p.accent, 10, true, false)?;
        draw_text(hdc, "Your games", x, TITLE_H + 34, 360, 36, p.text, 27, true, false)?;
        let count = if st.install_required { "1 game · ready to install" } else { "1 game in your library" };
        draw_text(hdc, count, right - 260, TITLE_H + 47, 260, 22, p.text_dim, 11, false, false)?;

        let card = &rects.library_card;
        round_rect_path(hdc, card, 16, p.bg_card)?;
        round_rect_outline(hdc, card, 16, p.border)?;
        let art = RECT { left: card.left + 1, top: card.top + 1, right: card.left + 416, bottom: card.top + 235 };
        draw_gallery_image(hdc, st, st.carousel_idx, &art, p)?;
        let art_scrim = RECT { left: art.left, top: art.bottom - 64, right: art.right, bottom: art.bottom };
        fill_rect(hdc, &art_scrim, COLORREF(0x00221914))?;
        if draw_logo_bitmap(hdc, st.logo_bitmap, art.left + 15, art.bottom - 53, 40).is_err() {
            draw_logo(hdc, art.left + 15, art.bottom - 53, 40, p.accent)?;
        }
        draw_text(hdc, "Flux Rec", art.left + 65, art.bottom - 50, art.right - art.left - 77, 30, COLORREF(0x00FFFFFF), 20, true, false)?;

        let info_x = art.right + 34;
        let info_w = right - info_x - 28;
        let state = if st.checking {
            "Checking for updates"
        } else if st.install_required {
            "Ready to install"
        } else if st.update_available {
            "Update available"
        } else if !st.check_error.is_empty() {
            "Update check will retry"
        } else {
            "Up to date"
        };
        draw_text(hdc, "RIPO TEAM  ·  FLUX REC", info_x, card.top + 25, info_w, 18, p.accent, 10, true, false)?;
        draw_text(hdc, "Flux Rec", info_x, card.top + 55, info_w, 38, p.text, 26, true, false)?;
        draw_text(hdc, "Explore rooms, meet friends and create together.", info_x, card.top + 102, info_w, 50, p.text_dim, 12, false, false)?;
        draw_status_pulse(hdc, info_x + 5, card.top + 185, st.anim_t, if st.update_available || st.install_required { p.accent } else { COLORREF(0x005A8C2D) })?;
        draw_text_ellipsis(hdc, state, info_x + 20, card.top + 174, info_w - 20, 24, p.text, 11)?;
        let version = if st.update_available {
            format!("Version {} is available", display_update_version(&st.update_version))
        } else {
            "Updates are checked before launch".to_string()
        };
        draw_text_ellipsis(hdc, &version, info_x, card.top + 205, info_w, 21, p.text_dim, 10)?;
        let players = match (st.players, st.player_count_pending) {
            (Some(count), _) => format!("{count} online"),
            (None, true) => "Checking online players".to_string(),
            (None, false) => "Player count unavailable".to_string(),
        };
        draw_people_icon(hdc, info_x, card.top + 247, p.text_dim)?;
        draw_text_ellipsis(hdc, &players, info_x + 31, card.top + 245, info_w - 40, 22, p.text_dim, 10)?;

        let play_label = if st.install_required { "Install" } else if st.update_available { "Update & play" } else { "Play now" };
        round_rect_path(hdc, &rects.library_play, 10, if st.hover == Hot::Play { p.accent_dark } else { p.accent })?;
        draw_text(hdc, play_label, rects.library_play.left + 8, rects.library_play.top + 12, rects.library_play.right - rects.library_play.left - 16, 24, COLORREF(0x00FFFFFF), 13, true, true)?;
        if !st.install_required {
            round_rect_path(hdc, &rects.uninstall, 10, if st.hover == Hot::Uninstall { p.bg_hover } else { p.bg_card })?;
            round_rect_outline(hdc, &rects.uninstall, 10, p.border)?;
            draw_text(hdc, "Uninstall", rects.uninstall.left + 8, rects.uninstall.top + 12, rects.uninstall.right - rects.uninstall.left - 16, 23, p.text_dim, 11, true, true)?;
        }
        draw_text(hdc, "Select the game card to view screenshots and details.", card.left + 16, card.bottom + 18, right - card.left - 32, 22, p.text_dim, 10, false, false)?;
        Ok(())
    }

    unsafe fn draw_library_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = CONTENT_X;
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
            let version = display_update_version(&st.update_version);
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
        if st.hover == Hot::Back {
            round_rect_path(hdc, &rects.back, 10, p.bg_hover)?;
        }
        draw_text(hdc, "‹  Back to Store", rects.back.left + 8, rects.back.top + 6, 108, 22, p.text_dim, 12, true, false)?;

        let car = &rects.carousel;
        let info = RECT { left: car.right + 20, top: car.top, right: st.win_w - 32, bottom: st.win_h - 24 };
        round_rect_path(hdc, &info, 16, p.bg_card)?;
        round_rect_outline(hdc, &info, 16, p.border)?;
        let idx = if st.carousel_bitmaps.is_empty() { 0 } else { st.carousel_idx % st.carousel_bitmaps.len() };
        draw_gallery_image(hdc, st, idx, car, p)?;
        round_rect_outline(hdc, car, 16, p.border)?;

        for (r, label, hot) in [
            (&rects.carousel_prev, "‹", Hot::CarouselPrev),
            (&rects.carousel_next, "›", Hot::CarouselNext),
        ] {
            round_rect_path(hdc, r, 20, if st.hover == hot { p.accent } else { p.bg_card })?;
            draw_text(hdc, label, r.left, r.top + 1, r.right - r.left, r.bottom - r.top, if st.hover == hot { COLORREF(0x00FFFFFF) } else { p.text }, 22, true, true)?;
        }
        let count = st.carousel_bitmaps.len();
        let thumb_count = count.min(6);
        let thumb_gap = 8;
        let thumb_w = if thumb_count > 0 { ((car.right - car.left - 32 - thumb_gap * (thumb_count as i32 - 1)) / thumb_count as i32).clamp(42, 104) } else { 0 };
        let thumb_h = thumb_w * 9 / 16;
        let thumbs_w = thumb_count as i32 * (thumb_w + thumb_gap) - thumb_gap;
        let thumb_x = car.left + ((car.right - car.left) - thumbs_w) / 2;
        for i in 0..thumb_count {
            let shot_index = i;
            let thumb = RECT { left: thumb_x + i as i32 * (thumb_w + thumb_gap), top: car.bottom + 19, right: thumb_x + i as i32 * (thumb_w + thumb_gap) + thumb_w, bottom: car.bottom + 19 + thumb_h };
            draw_gallery_image(hdc, st, shot_index, &thumb, p)?;
            round_rect_outline(hdc, &thumb, 5, if i == idx { p.accent } else { p.border })?;
        }
        let screenshot_name = SCREENSHOT_NAMES.get(idx).copied().unwrap_or("Game screenshot");
        let gallery_label = if count > 0 { format!("{}  ·  SCREENSHOT {} OF {}", screenshot_name.to_uppercase(), idx + 1, count) } else { "GAME SCREENSHOTS".to_string() };
        draw_text(hdc, &gallery_label, car.left, car.bottom + 19 + thumb_h + 8, car.right - car.left, 20, p.text_dim, 9, true, false)?;

        if draw_logo_bitmap(hdc, st.logo_bitmap, info.left + 24, info.top + 26, 54).is_err() {
            draw_logo(hdc, info.left + 24, info.top + 26, 54, p.accent)?;
        }
        draw_text(hdc, "Flux Rec", info.left + 92, info.top + 31, info.right - info.left - 114, 35, p.text, 25, true, false)?;
        draw_text(hdc, "RIPO TEAM  ·  FLUX REC", info.left + 24, info.top + 94, info.right - info.left - 48, 20, p.accent, 10, true, false)?;
        draw_text(hdc, "Explore rooms, meet friends, play together and make your own spaces.", info.left + 24, info.top + 128, info.right - info.left - 48, 74, p.text, 12, false, false)?;

        let players = match (st.players, st.player_count_pending) {
            (Some(count), _) => format!("{count} players online"),
            (None, true) => "Checking online players".to_string(),
            (None, false) => "Player count unavailable".to_string(),
        };
        draw_people_icon(hdc, info.left + 24, info.top + 222, p.accent)?;
        draw_text_ellipsis(hdc, &players, info.left + 56, info.top + 220, info.right - info.left - 80, 24, p.text_dim, 10)?;
        let state = if st.install_required { "Free to install" } else if st.update_available { "Update available" } else if st.checking { "Checking version" } else if !st.check_error.is_empty() { "Will retry update check" } else { "Up to date" };
        draw_status_pulse(hdc, info.left + 29, info.top + 274, st.anim_t, if st.update_available || st.install_required { p.accent } else { COLORREF(0x005A8C2D) })?;
        draw_text_ellipsis(hdc, state, info.left + 44, info.top + 262, info.right - info.left - 68, 24, p.text, 11)?;
        let version = if st.update_available {
            format!("Version {} is ready", display_update_version(&st.update_version))
        } else {
            "Updates are checked before every launch".to_string()
        };
        draw_text_ellipsis(hdc, &version, info.left + 24, info.top + 298, info.right - info.left - 48, 21, p.text_dim, 9)?;

        let dp = &rects.detail_play;
        let label = if st.install_required { "Install" } else if st.update_available { "Update & play" } else { "Play now" };
        round_rect_path(hdc, dp, 10, if st.hover == Hot::DetailPlay { p.accent_dark } else { p.accent })?;
        draw_text(hdc, label, dp.left + 8, dp.top + 13, dp.right - dp.left - 16, 25, COLORREF(0x00FFFFFF), 13, true, true)?;
        draw_text(hdc, "Updates are checked before launch.", info.left + 24, dp.top - 31, info.right - info.left - 48, 22, p.text_dim, 9, false, false)?;
        Ok(())
    }

    unsafe fn draw_settings_console(
        hdc: HDC,
        st: &mut State,
        rects: &Rects,
        p: &Palette,
    ) -> Silent {
        let left = CONTENT_X;
        let right = st.win_w - 36;
        draw_text(
            hdc,
            "SETTINGS",
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
            "Launcher settings",
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
            "Appearance, install location and updates.",
            left + 350,
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
            "Updates & storage",
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
                "Look for a newer Flux Rec build",
                Hot::CheckForUpdates,
            ),
            (
                &rects.open_folder,
                "Open game folder",
                "View installed game files",
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
            &format!("Ripo Team Launcher v{}", st.version),
            left + 2,
            st.win_h - 38,
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
        let cx = CONTENT_X;
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

    unsafe fn draw_downloads_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let card = &rects.downloads_card;
        draw_text(hdc, "DOWNLOADS", card.left, TITLE_H + 13, 300, 18, p.accent, 10, true, false)?;
        draw_text(hdc, "Your queue", card.left, TITLE_H + 34, 360, 36, p.text, 27, true, false)?;
        draw_text(hdc, "Flux Rec installs and updates", card.right - 260, TITLE_H + 47, 260, 22, p.text_dim, 11, false, false)?;

        round_rect_path(hdc, card, 16, p.bg_card)?;
        round_rect_outline(hdc, card, 16, p.border)?;
        let art = RECT { left: card.left + 1, top: card.top + 1, right: card.left + 368, bottom: card.top + 208 };
        draw_gallery_image(hdc, st, 0, &art, p)?;
        let art_scrim = RECT { left: art.left, top: art.bottom - 58, right: art.right, bottom: art.bottom };
        fill_rect(hdc, &art_scrim, COLORREF(0x00221914))?;
        if draw_logo_bitmap(hdc, st.logo_bitmap, art.left + 14, art.bottom - 49, 38).is_err() {
            draw_logo(hdc, art.left + 14, art.bottom - 49, 38, p.accent)?;
        }
        draw_text(hdc, "Flux Rec", art.left + 62, art.bottom - 46, art.right - art.left - 74, 27, COLORREF(0x00FFFFFF), 18, true, false)?;

        let info_x = art.right + 32;
        let info_w = card.right - info_x - 28;
        let active_transfer = st.percent > 0 && st.percent < 100;
        let state = if active_transfer {
            "Installing or updating"
        } else if st.checking {
            "Checking for updates"
        } else if st.install_required {
            "Ready to install"
        } else if st.update_available {
            "Update available"
        } else if !st.check_error.is_empty() {
            "Update check will retry"
        } else {
            "No active downloads"
        };
        draw_text(hdc, if active_transfer { "ACTIVE" } else { "QUEUE" }, info_x, card.top + 28, info_w, 17, p.accent, 10, true, false)?;
        draw_text(hdc, state, info_x, card.top + 54, info_w, 34, p.text, 22, true, false)?;
        if active_transfer {
            let stage = if st.stage.is_empty() { "Working on game files…" } else { st.stage.as_str() };
            draw_text_ellipsis(hdc, stage, info_x, card.top + 98, info_w, 24, p.text_dim, 11)?;
            let bar = RECT { left: info_x, top: card.top + 136, right: info_x + info_w, bottom: card.top + 151 };
            round_rect_path(hdc, &bar, 12, p.track)?;
            let fill = RECT { left: bar.left, top: bar.top, right: bar.left + ((bar.right - bar.left) * st.percent as i32 / 100).max(12), bottom: bar.bottom };
            fill_gradient(hdc, &fill, p.accent, p.accent_dark)?;
            draw_text(hdc, &format!("{}%", st.percent), info_x, card.top + 160, 90, 20, p.text_dim, 10, false, false)?;
            if !st.detail.is_empty() {
                draw_text_ellipsis(hdc, &st.detail, info_x, card.top + 190, info_w, 34, p.text_dim, 10)?;
            }
        } else {
            let message = if st.install_required {
                "No transfer is active. Install Flux Rec to add it to your library."
            } else if st.update_available {
                "An update is ready. It will be applied before the game launches."
            } else if st.checking {
                "The launcher is checking the latest game version."
            } else if !st.check_error.is_empty() {
                "The check will retry automatically before the next launch."
            } else {
                "There are no active downloads. Flux Rec is ready in your library."
            };
            draw_text(hdc, message, info_x, card.top + 101, info_w, 52, p.text_dim, 12, false, false)?;
            if !st.update_available && !st.install_required {
                draw_text(hdc, "Updates are checked automatically before launch.", info_x, card.top + 164, info_w, 22, p.text_dim, 10, false, false)?;
            }
        }

        let status_top = card.top + 250;
        let status_bottom = card.top + 438;
        let status_gap = 16;
        let status_left = card.left + 28;
        let status_right = card.right - 28;
        let status_w = (status_right - status_left - status_gap) / 2;
        let install_tile = RECT { left: status_left, top: status_top, right: status_left + status_w, bottom: status_bottom };
        let update_tile = RECT { left: install_tile.right + status_gap, top: status_top, right: status_right, bottom: status_bottom };
        for tile in [&install_tile, &update_tile] {
            round_rect_path(hdc, tile, 14, p.bg_side)?;
            round_rect_outline(hdc, tile, 14, p.border)?;
        }
        draw_text(hdc, "INSTALL STATUS", install_tile.left + 20, install_tile.top + 18, status_w - 40, 18, p.accent, 9, true, false)?;
        draw_text(hdc, if st.install_required { "Ready to install" } else { "Installed" }, install_tile.left + 20, install_tile.top + 46, status_w - 40, 30, p.text, 19, true, false)?;
        draw_text_ellipsis(hdc, &st.install_dir, install_tile.left + 20, install_tile.top + 91, status_w - 40, 24, p.text_dim, 10)?;
        draw_text(hdc, "AUTOMATIC UPDATES", update_tile.left + 20, update_tile.top + 18, status_w - 40, 18, p.accent, 9, true, false)?;
        draw_text(hdc, "Checked before launch", update_tile.left + 20, update_tile.top + 46, status_w - 40, 30, p.text, 19, true, false)?;
        let update_note = if st.update_available {
            format!("Version {} is ready to apply.", display_update_version(&st.update_version))
        } else if st.checking {
            "Checking the latest game version now.".to_string()
        } else if !st.check_error.is_empty() {
            "The check will retry automatically at launch.".to_string()
        } else {
            "Flux Rec stays current automatically.".to_string()
        };
        draw_text_ellipsis(hdc, &update_note, update_tile.left + 20, update_tile.top + 91, status_w - 40, 24, p.text_dim, 10)?;

        for (r, label, hot) in [
            (&rects.downloads_check_updates, "Check for updates", Hot::CheckForUpdates),
            (&rects.downloads_open_folder, "Open game folder", Hot::OpenInstallFolder),
        ] {
            round_rect_path(hdc, r, 10, if st.hover == hot { p.bg_hover } else { p.bg_card })?;
            round_rect_outline(hdc, r, 10, p.border)?;
            draw_text(hdc, label, r.left + 8, r.top + 13, r.right - r.left - 16, 22, p.text, 11, true, true)?;
        }
        let play_label = if st.install_required { "Install Flux Rec" } else if st.update_available { "Update & play" } else { "Play Flux Rec" };
        round_rect_path(hdc, &rects.downloads_play, 10, if st.hover == Hot::Play { p.accent_dark } else { p.accent })?;
        draw_text(hdc, play_label, rects.downloads_play.left + 8, rects.downloads_play.top + 12, rects.downloads_play.right - rects.downloads_play.left - 16, 23, COLORREF(0x00FFFFFF), 11, true, true)?;
        Ok(())
    }
    unsafe fn draw_working_view(hdc: HDC, st: &mut State, rects: &Rects, p: &Palette) -> Silent {
        let cx = CONTENT_X;
        // Center the progress in the content area.
        let content_w = st.win_w - CONTENT_X;
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
                        // TIMER_MS is 100 ms, so repaint once per tick: 10 fps rather than
                        // the old 5 fps progress animation.
                        repaint = true;
                    }
                    // Auto-advance the carousel every ~4s (50ms timer * 80).
                    if matches!(st.view, View::Library | View::Detail) && !st.carousel_bitmaps.is_empty() {
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
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                if ptr.is_null() {
                    return LRESULT(HTCLIENT as isize);
                }
                let st = &*ptr;
                let rects = compute_rects(st.win_w, st.win_h);
                if pt_in(&rects.close, pt.x, pt.y)
                    || pt_in(&rects.minimize, pt.x, pt.y)
                    || pt_in(&rects.nav_store, pt.x, pt.y)
                    || pt_in(&rects.nav_library, pt.x, pt.y)
                    || pt_in(&rects.nav_downloads, pt.x, pt.y)
                    || pt_in(&rects.nav_settings, pt.x, pt.y)
                {
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
                    let rects = compute_rects(st.win_w, st.win_h);
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
                    let rects = compute_rects(st.win_w, st.win_h);
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
                    if !(*ptr).team_logo_bitmap.is_invalid() {
                        let _ = DeleteObject((*ptr).team_logo_bitmap);
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
            Hot::NavStore => {
                st.view = View::Store;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::NavLibrary => {
                st.view = View::Library;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::NavDownloads => {
                st.view = View::Downloads;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::NavSettings => {
                st.view = View::Settings;
                let _ = InvalidateRect(hwnd, None, false);
            }
            Hot::Play => {
                if matches!(st.view, View::Store | View::Library | View::Downloads) {
                    start_play(hwnd, st);
                }
            }
            Hot::DetailPlay => {
                if matches!(st.view, View::Library | View::Detail) {
                    start_play(hwnd, st);
                }
            }
            Hot::Card | Hot::Details => {
                if matches!(st.view, View::Store | View::Library) {
                    st.view = View::Detail;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::Back => {
                if st.view == View::Detail {
                    st.view = View::Store;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::CarouselPrev => {
                if matches!(st.view, View::Library | View::Detail) && !st.carousel_bitmaps.is_empty() {
                    let n = st.carousel_bitmaps.len();
                    st.carousel_idx = (st.carousel_idx + n - 1) % n;
                    st.carousel_tick = 0;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            Hot::CarouselNext => {
                if matches!(st.view, View::Library | View::Detail) && !st.carousel_bitmaps.is_empty() {
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

