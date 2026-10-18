// Flux Rec Setup & Launcher — Ripo Team's light install and update flow.
//
// Ripo Team setup and update UI:
// - Borderless, rounded windows with a custom draggable title bar.
// - Setup pairs real Flux Rec artwork with an install timeline and live status.
// - The automatic update window uses the same product frame before the game opens.
// - Embedded logos and screenshots keep the UI self-contained and work offline.
//
// Design rules:
//   * Raw Win32 only (no GUI framework) so the binary stays tiny.
//   * `run_gui` / `run_gui_launcher` never panic and never print: if the
//     window cannot be created for any reason they return immediately and
//     the install/launch proceeds headless. Every fallible Win32 call is
//     checked; there is no `unwrap`/`expect`/`println` anywhere here.
//   * All Win32 code is behind `#[cfg(windows)]`. Other platforms get a
//     stub that drains the channel and returns, so `cargo check` passes
//     on Linux.
//   * NO Registry API (caused Windows build failures in v0.3.4-0.3.6).

use std::sync::mpsc::Receiver;

/// One progress update from the installer/launcher thread.
pub struct GuiMsg {
    /// 0..=100. Values above 100 are clamped.
    pub percent: u8,
    /// Stage line, e.g. "Downloading game files…".
    /// The special value "done" (after trimming) closes the window at once.
    pub stage: String,
    /// Detail line, e.g. "128 MB / 206 MB".
    /// Empty string leaves the current detail text untouched.
    pub detail: String,
}

/// Which window to show. The setup installer and the `--play` launcher
/// share this module but want very different layouts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GuiMode {
    Setup,
    Launcher,
}

// ---------------------------------------------------------------------------
// Non-Windows stubs: drain the channel so the sender never blocks.
// ---------------------------------------------------------------------------
#[cfg(not(windows))]
pub fn run_gui(rx: Receiver<GuiMsg>) {
    while rx.recv().is_ok() {}
}

#[cfg(not(windows))]
pub fn run_gui_launcher(rx: Receiver<GuiMsg>) {
    while rx.recv().is_ok() {}
}

// ---------------------------------------------------------------------------
// Windows entry points.
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub fn run_gui(rx: Receiver<GuiMsg>) {
    // The install must never die or stall because of the GUI: any failure
    // below is swallowed and the installer proceeds headless/hidden.
    let _ = imp::run(rx, GuiMode::Setup);
}

#[cfg(windows)]
pub fn run_gui_launcher(rx: Receiver<GuiMsg>) {
    let _ = imp::run(rx, GuiMode::Launcher);
}

#[cfg(windows)]
mod imp {
    use super::{GuiMode, GuiMsg, Receiver};
    use std::ffi::c_void;
    use std::sync::mpsc::TryRecvError;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window dimensions (client area == window area: WS_POPUP has no frame).
    const SETUP_W: i32 = 1240;
    const SETUP_H: i32 = 780;
    const LAUNCH_W: i32 = 1040;
    const LAUNCH_H: i32 = 620;
    const CORNER: i32 = 24; // round-rect ellipse diameter for the window
    const TITLE_H: i32 = 46;

    const TIMER_ID: usize = 1;
    // Poll installer progress at 10 Hz. The setup window used to repaint its
    // entire 1100x700 surface 20 times per second even when progress had not
    // changed, which wasted CPU on slower PCs.
    const TIMER_MS: u32 = 100;

    // Palette (COLORREF = 0x00BBGGRR), shared with the light launcher shell.
    const BG: COLORREF = COLORREF(0x00F8F6F3); // cool paper
    const BG_TITLE: COLORREF = COLORREF(0x00FFFFFF);
    const BG_SIDE: COLORREF = COLORREF(0x00FFFFFF);
    const BG_CARD: COLORREF = COLORREF(0x00FFFFFF);
    const BLUE: COLORREF = COLORREF(0x00FF9B2E); // Ripo blue, shared with the launcher shell
    const BLUE_LIGHT: COLORREF = COLORREF(0x00FFB869);
    const BLUE_DIM: COLORREF = COLORREF(0x00FFEEDC);
    const WHITE: COLORREF = COLORREF(0x002D241E); // primary text
    const GRAY: COLORREF = COLORREF(0x00675A50); // secondary text
    const DIM: COLORREF = COLORREF(0x00978A7E); // muted text
    const TRACK: COLORREF = COLORREF(0x00EAE4DE);
    const BORDER: COLORREF = COLORREF(0x00DFD8D1);
    const GREEN: COLORREF = COLORREF(0x005A8C2D);
    const PURE_WHITE: COLORREF = COLORREF(0x00FFFFFF); // actual white (on colored bg)

    // Setup sidebar steps: (title, subtitle).
    const STEPS: &[(&str, &str)] = &[
        ("Download", "Game files & updates"),
        ("Install", "Patch & configure client"),
        ("Verify", "Files & security check"),
        ("Play", "Ready to launch"),
    ];

    // Setup feature cards: (title, description).
    const CARDS: &[(&str, &str)] = &[
        (
            "Play Together",
            "Join friends in the Rec Center and explore thousands of rooms.",
        ),
        (
            "Compete",
            "Paintball, bowling and quests with players worldwide.",
        ),
        ("Customize", "Thousands of avatar items, outfits and skins."),
        (
            "Create",
            "Build your own rooms and games with the maker tools.",
        ),
    ];

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum StepState {
        Todo,
        Active,
        Done,
    }

    struct GuiState {
        hwnd: HWND,
        mode: GuiMode,
        win_w: i32,
        win_h: i32,
        percent: u8,
        stage: String,
        detail: String,
        finished: bool,
        shimmer: i32,
        hover_btn: u8, // 0 = none, 1 = minimize, 2 = close
        bg_brush: HBRUSH,
        logo_bmp: HBITMAP,
        logo_w: i32,
        logo_h: i32,
        team_logo_bmp: HBITMAP,
        team_logo_w: i32,
        team_logo_h: i32,
        game_art_bmp: HBITMAP,
        game_art_w: i32,
        game_art_h: i32,
        f_big: HFONT,
        f_title: HFONT,
        f_word: HFONT,
        f_h2: HFONT,
        f_body: HFONT,
        f_small: HFONT,
        f_tiny: HFONT,
        rx: Receiver<GuiMsg>,
    }

    pub(super) fn run(rx: Receiver<GuiMsg>, mode: GuiMode) -> Silent {
        unsafe { run_inner(rx, mode) }
    }

    unsafe fn run_inner(rx: Receiver<GuiMsg>, mode: GuiMode) -> Silent {
        let (preferred_w, preferred_h) = match mode {
            GuiMode::Setup => (SETUP_W, SETUP_H),
            GuiMode::Launcher => (LAUNCH_W, LAUNCH_H),
        };
        // Keep the complete install/update flow visible on laptop displays. The
        // content uses these client dimensions for its card and progress layout.
        let sx = GetSystemMetrics(SM_CXSCREEN);
        let sy = GetSystemMetrics(SM_CYSCREEN);
        let win_w = preferred_w.min((sx - 48).max(880));
        let win_h = preferred_h.min((sy - 80).max(620));
        let class_name: PCWSTR = match mode {
            GuiMode::Setup => w!("FluxRecSetupGui"),
            GuiMode::Launcher => w!("FluxRecLauncherGui"),
        };
        let title: PCWSTR = match mode {
            GuiMode::Setup => w!("Ripo Team Launcher Setup"),
            GuiMode::Launcher => w!("Ripo Team Launcher"),
        };

        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(|_| ())?.0);
        let bg_brush = CreateSolidBrush(BG);
        if bg_brush.is_invalid() {
            return Err(());
        }

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hIcon: LoadIconW(hinstance, PCWSTR(1usize as *const u16)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: bg_brush,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            let _ = DeleteObject(bg_brush);
            return Err(());
        }

        // Borderless popup; taskbar presence via WS_EX_APPWINDOW.
        let (x, y) = ((sx - win_w) / 2, (sy - win_h) / 2);

        // Fonts (all Segoe UI; semibold via weight, no extra face needed).
        let f_big = create_font(40, FW_BOLD);
        let f_title = create_font(30, FW_BOLD);
        let f_word = create_font(26, FW_BOLD);
        let f_h2 = create_font(17, FW_SEMIBOLD);
        let f_body = create_font(15, FW_NORMAL);
        let f_small = create_font(13, FW_NORMAL);
        let f_tiny = create_font(12, FW_NORMAL);
        if f_big.is_invalid() || f_title.is_invalid() {
            let _ = DeleteObject(bg_brush);
            return Err(());
        }

        let state = Box::new(GuiState {
            hwnd: HWND::default(),
            mode,
            win_w,
            win_h,
            percent: 0,
            stage: String::new(),
            detail: String::new(),
            finished: false,
            shimmer: 0,
            hover_btn: 0,
            bg_brush,
            logo_bmp: HBITMAP::default(),
            logo_w: 0,
            logo_h: 0,
            team_logo_bmp: HBITMAP::default(),
            team_logo_w: 0,
            team_logo_h: 0,
            game_art_bmp: HBITMAP::default(),
            game_art_w: 0,
            game_art_h: 0,
            f_big,
            f_title,
            f_word,
            f_h2,
            f_body,
            f_small,
            f_tiny,
            rx,
        });
        let ptr = Box::into_raw(state);

        let hwnd = match CreateWindowExW(
            WS_EX_APPWINDOW | WS_EX_COMPOSITED,
            class_name,
            title,
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            hinstance,
            Some(ptr as *const c_void),
        ) {
            Ok(h) => h,
            Err(_) => {
                // Free the fonts first (they live on the state), then the box.
                destroy_fonts(ptr);
                let _ = Box::from_raw(ptr);
                let _ = DeleteObject(bg_brush);
                return Err(());
            }
        };

        // Rounded corners. The region is owned by the window after this call.
        let rgn = CreateRoundRectRgn(0, 0, win_w + 1, win_h + 1, CORNER, CORNER);
        if !rgn.is_invalid() {
            let _ = SetWindowRgn(hwnd, rgn, BOOL(1));
        }

        // Cache the logo as a premultiplied 32-bit DIB for AlphaBlend.
        {
            let s = &mut *ptr;
            s.hwnd = hwnd;
            let dc = GetDC(hwnd);
            if !dc.is_invalid() {
                let (bmp, w, h) = create_logo_dib(dc);
                s.logo_bmp = bmp;
                s.logo_w = w;
                s.logo_h = h;
                let (team_bmp, team_w, team_h) =
                    create_png_dib(dc, crate::assets::RIPO_TEAM_LOGO_PNG);
                s.team_logo_bmp = team_bmp;
                s.team_logo_w = team_w;
                s.team_logo_h = team_h;
                let (art_bmp, art_w, art_h) = create_png_dib(
                    dc,
                    include_bytes!("../assets/screenshots/rec-center-1.jpg"),
                );
                s.game_art_bmp = art_bmp;
                s.game_art_w = art_w;
                s.game_art_h = art_h;
                let _ = ReleaseDC(hwnd, dc);
            }
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        SetTimer(hwnd, TIMER_ID, TIMER_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(hwnd, TIMER_ID);
        // The state box (with its fonts and the logo bitmap) was freed in
        // WM_DESTROY; only the class background brush remains to be freed.
        let _ = DeleteObject(bg_brush);
        Ok(())
    }

    /// Free the fonts when window creation failed before WM_DESTROY ran.
    unsafe fn destroy_fonts(ptr: *mut GuiState) {
        let s = &*ptr;
        let _ = DeleteObject(s.f_big);
        let _ = DeleteObject(s.f_title);
        let _ = DeleteObject(s.f_word);
        let _ = DeleteObject(s.f_h2);
        let _ = DeleteObject(s.f_body);
        let _ = DeleteObject(s.f_small);
        let _ = DeleteObject(s.f_tiny);
    }

    unsafe fn create_font(px: i32, weight: FONT_WEIGHT) -> HFONT {
        CreateFontW(
            px,
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
        )
    }

    /// Build a premultiplied-alpha 32-bit DIB from the embedded logo BMP.
    /// Returns (bitmap, width, height); bitmap is invalid on any failure.
    unsafe fn create_logo_dib(hdc: HDC) -> (HBITMAP, i32, i32) {
        let bytes = crate::assets::LOGO_BMP_BYTES;
        if bytes.len() < 54 {
            return (HBITMAP::default(), 0, 0);
        }
        let off = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
        let w = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
        let h = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
        let bpp = u16::from_le_bytes([bytes[28], bytes[29]]);
        if w <= 0 || h == 0 || bpp != 32 || bytes.len() < off {
            return (HBITMAP::default(), 0, 0);
        }
        let ha = h.abs() as usize;
        let row_bytes = w as usize * 4;
        if bytes.len() < off + ha * row_bytes {
            return (HBITMAP::default(), 0, 0);
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = 40;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = match CreateDIBSection(
            hdc,
            &bmi as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE(std::ptr::null_mut()),
            0,
        ) {
            Ok(h) => h,
            Err(_) => return (HBITMAP::default(), 0, 0),
        };
        if hbmp.is_invalid() || bits.is_null() {
            return (HBITMAP::default(), 0, 0);
        }
        // Copy rows and premultiply. Both the BMP file (biHeight > 0) and the DIB
        // section (biHeight = h > 0) are bottom-up, so rows copy directly
        // without flipping. (The old ha-1-row flip drew the logo upside down.)
        let dst = bits as *mut u8;
        for row in 0..ha {
            let src_row = row;
            let sp = bytes[off + src_row * row_bytes..].as_ptr();
            let dp = dst.add(row * row_bytes);
            for i in 0..w as usize {
                let b = *sp.add(i * 4);
                let g = *sp.add(i * 4 + 1);
                let r = *sp.add(i * 4 + 2);
                let a = *sp.add(i * 4 + 3);
                if a == 0 {
                    *dp.add(i * 4) = 0;
                    *dp.add(i * 4 + 1) = 0;
                    *dp.add(i * 4 + 2) = 0;
                    *dp.add(i * 4 + 3) = 0;
                } else if a < 255 {
                    let aa = a as u32;
                    *dp.add(i * 4) = ((b as u32 * aa) / 255) as u8;
                    *dp.add(i * 4 + 1) = ((g as u32 * aa) / 255) as u8;
                    *dp.add(i * 4 + 2) = ((r as u32 * aa) / 255) as u8;
                    *dp.add(i * 4 + 3) = a;
                } else {
                    *dp.add(i * 4) = b;
                    *dp.add(i * 4 + 1) = g;
                    *dp.add(i * 4 + 2) = r;
                    *dp.add(i * 4 + 3) = a;
                }
            }
        }
        (hbmp, w, ha as i32)
    }

    /// Build a premultiplied 32-bit DIB from the studio PNG mark.
    unsafe fn create_png_dib(hdc: HDC, bytes: &[u8]) -> (HBITMAP, i32, i32) {
        let Ok(image) = image::load_from_memory(bytes) else {
            return (HBITMAP::default(), 0, 0);
        };
        let rgba = image.to_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return (HBITMAP::default(), 0, 0);
        }
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = 40;
        bmi.bmiHeader.biWidth = width as i32;
        bmi.bmiHeader.biHeight = -(height as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = match CreateDIBSection(
            hdc,
            &bmi as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE(std::ptr::null_mut()),
            0,
        ) {
            Ok(h) => h,
            Err(_) => return (HBITMAP::default(), 0, 0),
        };
        if hbmp.is_invalid() || bits.is_null() {
            return (HBITMAP::default(), 0, 0);
        }
        let dst = bits as *mut u8;
        for (index, pixel) in rgba.pixels().enumerate() {
            let alpha = pixel[3] as u32;
            let offset = index * 4;
            *dst.add(offset) = ((pixel[2] as u32 * alpha) / 255) as u8;
            *dst.add(offset + 1) = ((pixel[1] as u32 * alpha) / 255) as u8;
            *dst.add(offset + 2) = ((pixel[0] as u32 * alpha) / 255) as u8;
            *dst.add(offset + 3) = pixel[3];
        }
        (hbmp, width as i32, height as i32)
    }

    fn loword(v: isize) -> i32 {
        ((v as i32) << 16) >> 16
    }
    fn hiword(v: isize) -> i32 {
        (v as i32) >> 16
    }

    // WinUser.h hit-test values, spelled out to avoid depending on how the
    // windows crate types these constants.
    const HT_CLIENT: isize = 1;
    const HT_CAPTION: isize = 2;

    /// Button rectangles in the title bar: (minimize, close).
    unsafe fn title_buttons(s: &GuiState) -> (RECT, RECT) {
        let top = 8;
        let h = TITLE_H - 16;
        let close = RECT {
            left: s.win_w - 44,
            top,
            right: s.win_w - 12,
            bottom: top + h,
        };
        let min = RECT {
            left: s.win_w - 84,
            top,
            right: s.win_w - 52,
            bottom: top + h,
        };
        (min, close)
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
                LRESULT(0)
            }
            WM_NCHITTEST => {
                // Drag by the title bar (but not on the buttons).
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if ptr.is_null() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let s = &*ptr;
                let mut pt = POINT {
                    x: loword(lparam.0),
                    y: hiword(lparam.0),
                };
                let _ = ScreenToClient(hwnd, &mut pt as *mut POINT);
                let (min_r, close_r) = title_buttons(s);
                if pt_in(pt, min_r) || pt_in(pt, close_r) {
                    return LRESULT(HT_CLIENT);
                }
                if pt.y >= 0 && pt.y < TITLE_H {
                    return LRESULT(HT_CAPTION);
                }
                LRESULT(HT_CLIENT)
            }
            WM_MOUSEMOVE => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if !ptr.is_null() {
                    let s = &mut *ptr;
                    let pt = POINT {
                        x: loword(lparam.0),
                        y: hiword(lparam.0),
                    };
                    let (min_r, close_r) = title_buttons(s);
                    let hov = if pt_in(pt, close_r) {
                        2
                    } else if pt_in(pt, min_r) {
                        1
                    } else {
                        0
                    };
                    if hov != s.hover_btn {
                        s.hover_btn = hov;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if !ptr.is_null() {
                    let pt = POINT {
                        x: loword(lparam.0),
                        y: hiword(lparam.0),
                    };
                    let (min_r, close_r, mode) = {
                        let state = &*ptr;
                        let (min_r, close_r) = title_buttons(state);
                        (min_r, close_r, state.mode)
                    };
                    if pt_in(pt, close_r) {
                        let (title, body) = match mode {
                            GuiMode::Setup => (
                                w!("Ripo Team Launcher Setup"),
                                w!("Close this window? Setup will continue in the background."),
                            ),
                            GuiMode::Launcher => (
                                w!("Ripo Team Launcher"),
                                w!("Close this window? Flux Rec will continue starting in the background."),
                            ),
                        };
                        if MessageBoxW(hwnd, body, title, MB_YESNO | MB_ICONINFORMATION) == IDYES {
                            let _ = DestroyWindow(hwnd);
                        }
                    } else if pt_in(pt, min_r) {
                        let _ = ShowWindow(hwnd, SW_MINIMIZE);
                    }
                }
                LRESULT(0)
            }
            WM_TIMER => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if ptr.is_null() {
                    return LRESULT(0);
                }
                let s = &mut *ptr;
                if wparam.0 == TIMER_ID {
                    // Drain progress messages.
                    let mut changed = false;
                    loop {
                        match s.rx.try_recv() {
                            Ok(m) => {
                                if m.stage.trim().eq_ignore_ascii_case("done") {
                                    let _ = DestroyWindow(hwnd);
                                    break;
                                }
                                let percent = m.percent.min(100);
                                if s.percent != percent {
                                    s.percent = percent;
                                    changed = true;
                                }
                                if s.percent >= 100 {
                                    s.finished = true;
                                }
                                if !m.stage.is_empty() && s.stage != m.stage {
                                    s.stage = m.stage;
                                    changed = true;
                                }
                                if !m.detail.is_empty() && s.detail != m.detail {
                                    s.detail = m.detail;
                                    changed = true;
                                }
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                    // Animate only while the installer has no determinate
                    // progress. During downloads, redraw only on new data.
                    if s.percent == 0 && !s.finished {
                        s.shimmer = (s.shimmer + 12) % (s.win_w + 200);
                        changed = true;
                    }
                    if changed {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if !ptr.is_null() {
                    let s = &*ptr;
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    if !hdc.is_invalid() {
                        draw_all(hdc, s);
                        let _ = EndPaint(hwnd, &ps);
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if !ptr.is_null() {
                    // Free GDI objects before dropping the state box.
                    let s = &*ptr;
                    if !s.logo_bmp.is_invalid() {
                        let _ = DeleteObject(s.logo_bmp);
                    }
                    if !s.team_logo_bmp.is_invalid() {
                        let _ = DeleteObject(s.team_logo_bmp);
                    }
                    if !s.game_art_bmp.is_invalid() {
                        let _ = DeleteObject(s.game_art_bmp);
                    }
                    destroy_fonts(ptr);
                    let _ = Box::from_raw(ptr);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn pt_in(pt: POINT, rc: RECT) -> bool {
        pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
    }

    // ------------------------------------------------------------------
    // Drawing helpers
    // ------------------------------------------------------------------

    unsafe fn rr_fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, rad: i32, color: COLORREF) {
        if w <= 0 || h <= 0 {
            return;
        }
        let brush = CreateSolidBrush(color);
        if brush.is_invalid() {
            return;
        }
        let pen = CreatePen(PS_SOLID, 1, color);
        let old_b = SelectObject(hdc, brush);
        let old_p = if pen.is_invalid() {
            HGDIOBJ::default()
        } else {
            SelectObject(hdc, pen)
        };
        let _ = RoundRect(hdc, x, y, x + w, y + h, rad, rad);
        let _ = SelectObject(hdc, old_b);
        if !pen.is_invalid() {
            let _ = SelectObject(hdc, old_p);
            let _ = DeleteObject(pen);
        }
        let _ = DeleteObject(brush);
    }

    unsafe fn rr_outline(hdc: HDC, x: i32, y: i32, w: i32, h: i32, rad: i32, color: COLORREF) {
        if w <= 0 || h <= 0 {
            return;
        }
        let pen = CreatePen(PS_SOLID, 1, color);
        if pen.is_invalid() {
            return;
        }
        let old_p = SelectObject(hdc, pen);
        let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = RoundRect(hdc, x, y, x + w, y + h, rad, rad);
        let _ = SelectObject(hdc, old_p);
        let _ = SelectObject(hdc, old_b);
        let _ = DeleteObject(pen);
    }

    unsafe fn text_at(hdc: HDC, font: HFONT, color: COLORREF, text: &str, x: i32, y: i32) {
        let _ = SelectObject(hdc, font);
        let _ = SetTextColor(hdc, color);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(hdc, x, y, &wide);
    }

    unsafe fn text_center(
        hdc: HDC,
        font: HFONT,
        color: COLORREF,
        text: &str,
        cx: i32,
        y: i32,
        w: i32,
        h: i32,
    ) {
        let _ = SelectObject(hdc, font);
        let _ = SetTextColor(hdc, color);
        let mut rc = RECT {
            left: cx - w / 2,
            top: y,
            right: cx + w / 2,
            bottom: y + h,
        };
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let _ = DrawTextW(
            hdc,
            &mut buf,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }

    unsafe fn text_block(hdc: HDC, font: HFONT, color: COLORREF, text: &str, rc: RECT) {
        let _ = SelectObject(hdc, font);
        let _ = SetTextColor(hdc, color);
        let mut rc = rc;
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let _ = DrawTextW(hdc, &mut buf, &mut rc, DT_WORDBREAK);
    }

    unsafe fn text_ellipsis(hdc: HDC, font: HFONT, color: COLORREF, text: &str, rc: RECT) {
        let _ = SelectObject(hdc, font);
        let _ = SetTextColor(hdc, color);
        let mut rc = rc;
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        let _ = DrawTextW(
            hdc,
            &mut buf,
            &mut rc,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
    }

    unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &wide, &mut sz);
        sz.cx
    }

    /// Draw the real Flux Rec logo with per-pixel alpha.
    unsafe fn draw_logo(hdc: HDC, s: &GuiState, x: i32, y: i32, size: i32) {
        draw_bitmap(hdc, s.logo_bmp, s.logo_w, s.logo_h, x, y, size, BLUE);
    }

    unsafe fn draw_team_logo(hdc: HDC, s: &GuiState, x: i32, y: i32, size: i32) {
        draw_bitmap(hdc, s.team_logo_bmp, s.team_logo_w, s.team_logo_h, x, y, size, BLUE);
    }

    unsafe fn draw_bitmap(
        hdc: HDC,
        bitmap: HBITMAP,
        width: i32,
        height: i32,
        x: i32,
        y: i32,
        size: i32,
        fallback: COLORREF,
    ) {
        if bitmap.is_invalid() || width <= 0 || height <= 0 || size <= 0 {
            // Fallback: plain blue ring so the layout never looks broken.
            let pen = CreatePen(PS_SOLID, 6, fallback);
            if !pen.is_invalid() {
                let old_p = SelectObject(hdc, pen);
                let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                let _ = Ellipse(hdc, x, y, x + size, y + size);
                let _ = SelectObject(hdc, old_p);
                let _ = SelectObject(hdc, old_b);
                let _ = DeleteObject(pen);
            }
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        if mem.is_invalid() {
            return;
        }
        let old = SelectObject(mem, bitmap);
        let bf = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = AlphaBlend(hdc, x, y, size, size, mem, 0, 0, width, height, bf);
        let _ = SelectObject(mem, old);
        let _ = DeleteDC(mem);
    }

    /// Fill a product artwork frame from an embedded game screenshot.
    unsafe fn draw_game_art(hdc: HDC, s: &GuiState, x: i32, y: i32, w: i32, h: i32) {
        if s.game_art_bmp.is_invalid() || s.game_art_w <= 0 || s.game_art_h <= 0 || w <= 0 || h <= 0 {
            rr_fill(hdc, x, y, w, h, 0, BG_SIDE);
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        if mem.is_invalid() {
            return;
        }
        let old = SelectObject(mem, s.game_art_bmp);
        let previous_mode = SetStretchBltMode(hdc, HALFTONE);
        // Crop to fill while preserving the source screenshot's aspect ratio.
        // Stretching a 16:9 game image into a portrait panel made avatars and
        // rooms look visibly squashed in the previous setup/updater UI.
        let target_ratio = w as f64 / h as f64;
        let source_ratio = s.game_art_w as f64 / s.game_art_h as f64;
        let (src_x, src_y, src_w, src_h) = if source_ratio > target_ratio {
            let crop_w = (s.game_art_h as f64 * target_ratio).round() as i32;
            ((s.game_art_w - crop_w) / 2, 0, crop_w.max(1), s.game_art_h)
        } else {
            let crop_h = (s.game_art_w as f64 / target_ratio).round() as i32;
            (0, (s.game_art_h - crop_h) / 2, s.game_art_w, crop_h.max(1))
        };
        let _ = StretchBlt(hdc, x, y, w, h, mem, src_x, src_y, src_w, src_h, SRCCOPY);
        if previous_mode != 0 {
            let _ = SetStretchBltMode(hdc, STRETCH_BLT_MODE(previous_mode));
        }
        let _ = SelectObject(mem, old);
        let _ = DeleteDC(mem);
    }

    /// "FLUX " in white + "REC" in Flux blue, centered at cx.
    unsafe fn draw_wordmark(hdc: HDC, s: &GuiState, cx: i32, y: i32) {
        let _ = SelectObject(hdc, s.f_word);
        let fw = text_width(hdc, "FLUX ");
        let rw = text_width(hdc, "REC");
        let sx = cx - (fw + rw) / 2;
        text_at(hdc, s.f_word, WHITE, "FLUX ", sx, y);
        text_at(hdc, s.f_word, BLUE, "REC", sx + fw, y);
    }

    // ------------------------------------------------------------------
    // Window chrome: custom title bar with minimize/close buttons.
    // ------------------------------------------------------------------

    unsafe fn draw_titlebar(hdc: HDC, s: &GuiState) {
        let w = s.win_w;
        rr_fill(hdc, 0, 0, w, TITLE_H, 0, BG_TITLE);
        // Bottom hairline.
        let pen = CreatePen(PS_SOLID, 1, BORDER);
        if !pen.is_invalid() {
            let old = SelectObject(hdc, pen);
            let _ = MoveToEx(hdc, 0, TITLE_H, None);
            let _ = LineTo(hdc, w, TITLE_H);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(pen);
        }
        let title = match s.mode {
            GuiMode::Setup => "Ripo Team Launcher Setup",
            GuiMode::Launcher => "Ripo Team Launcher",
        };
        draw_team_logo(hdc, s, 18, 9, 28);
        let rc = RECT {
            left: 56,
            top: 0,
            right: w - 100,
            bottom: TITLE_H,
        };
        text_ellipsis(hdc, s.f_small, GRAY, title, rc);

        // Buttons.
        let (min_r, close_r) = title_buttons(s);
        draw_win_button(hdc, min_r, s.hover_btn == 1, false);
        draw_win_button(hdc, close_r, s.hover_btn == 2, true);
    }

    unsafe fn draw_win_button(hdc: HDC, rc: RECT, hover: bool, is_close: bool) {
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        if hover {
            let bg = if is_close {
                COLORREF(0x001C2BC4) // soft red
            } else {
                COLORREF(0x00EBE6E0) // light hover
            };
            rr_fill(hdc, rc.left, rc.top, w, h, 12, bg);
        }
        // Glyphs drawn with lines (no font-coverage risk).
        let fg = if hover && is_close { PURE_WHITE } else { GRAY };
        let pen = CreatePen(PS_SOLID, 2, fg);
        if pen.is_invalid() {
            return;
        }
        let old = SelectObject(hdc, pen);
        let cx = (rc.left + rc.right) / 2;
        let cy = (rc.top + rc.bottom) / 2;
        if is_close {
            let r = 6;
            let _ = MoveToEx(hdc, cx - r, cy - r, None);
            let _ = LineTo(hdc, cx + r, cy + r);
            let _ = MoveToEx(hdc, cx + r, cy - r, None);
            let _ = LineTo(hdc, cx - r, cy + r);
        } else {
            let _ = MoveToEx(hdc, cx - 6, cy, None);
            let _ = LineTo(hdc, cx + 6, cy);
        }
        let _ = SelectObject(hdc, old);
        let _ = DeleteObject(pen);
    }

    // ------------------------------------------------------------------
    // Progress bar (rounded, modern).
    // ------------------------------------------------------------------

    unsafe fn draw_progress_bar(hdc: HDC, x: i32, y: i32, w: i32, h: i32, percent: u8) {
        rr_fill(hdc, x, y, w, h, h, TRACK);
        let mut fw = (w * percent as i32) / 100;
        if percent > 0 && fw < h {
            fw = h; // keep the pill ends round even at 1%
        }
        if fw > 0 {
            if fw > w {
                fw = w;
            }
            rr_fill(hdc, x, y, fw, h, h, BLUE);
            // Soft glow: 1px dim-blue outline around the whole track.
            rr_outline(hdc, x, y, w, h, h, BLUE_DIM);
        }
    }

    /// Indeterminate shimmer used by the launcher before percent is known.
    unsafe fn draw_shimmer_bar(hdc: HDC, s: &GuiState, x: i32, y: i32, w: i32, h: i32) {
        rr_fill(hdc, x, y, w, h, h, TRACK);
        let bw = 120;
        let travel = w + bw * 2;
        let bx = x - bw + (s.shimmer % travel.max(1));
        // Three-tone moving block: dim edges, bright core.
        rr_fill(hdc, bx, y, 30, h, h, BLUE_DIM);
        rr_fill(hdc, bx + 30, y, 60, h, h, BLUE);
        rr_fill(hdc, bx + 90, y, 30, h, h, BLUE_DIM);
    }

    // ------------------------------------------------------------------
    // Frame dispatch
    // ------------------------------------------------------------------

    unsafe fn draw_all(hdc: HDC, s: &GuiState) {
        let _ = SetBkMode(hdc, TRANSPARENT);
        // Base background.
        let bg = CreateSolidBrush(BG);
        if !bg.is_invalid() {
            let rc = RECT {
                left: 0,
                top: 0,
                right: s.win_w,
                bottom: s.win_h,
            };
            let _ = FillRect(hdc, &rc, bg);
            let _ = DeleteObject(bg);
        }

        draw_titlebar(hdc, s);
        match s.mode {
            GuiMode::Setup => draw_setup(hdc, s),
            GuiMode::Launcher => draw_launcher_redesigned(hdc, s),
        }
    }

    // ------------------------------------------------------------------
    // Setup mode: product information + install activity
    // ------------------------------------------------------------------

    const SIDE_W: i32 = 300;

    unsafe fn draw_setup(hdc: HDC, s: &GuiState) {
        draw_setup_redesigned(hdc, s);
    }

    unsafe fn draw_setup_sidebar(hdc: HDC, s: &GuiState) {
        let cx = SIDE_W / 2;

        // Logo.
        draw_logo(hdc, s, cx - 75, 78, 150);

        // Wordmark + tagline.
        draw_wordmark(hdc, s, cx, 244);
        text_center(
            hdc,
            s.f_small,
            GRAY,
            "Record  •  Create  •  Share",
            cx,
            282,
            SIDE_W - 40,
            22,
        );

        // Version pill.
        let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
        let _ = SelectObject(hdc, s.f_tiny);
        let tw = text_width(hdc, &ver);
        let pw = tw + 30;
        let ph = 26;
        let px = cx - pw / 2;
        let py = 312;
        rr_outline(hdc, px, py, pw, ph, ph, BLUE_DIM);
        text_center(hdc, s.f_tiny, BLUE, &ver, cx, py, pw, ph);

        // Steps.
        let states = step_states(s.percent, s.finished);
        let mut y = 372;
        for (i, (title, sub)) in STEPS.iter().enumerate() {
            let st = states.get(i).copied().unwrap_or(StepState::Todo);
            draw_step(hdc, s, 52, y, (i + 1) as i32, title, sub, st);
            y += 62;
        }
    }

    fn step_states(percent: u8, finished: bool) -> [StepState; 4] {
        if finished || percent >= 100 {
            return [StepState::Done; 4];
        }
        let mut st = [StepState::Todo; 4];
        let active = if percent >= 80 {
            2
        } else if percent >= 45 {
            1
        } else {
            0
        };
        let mut i = 0;
        while i < active {
            st[i] = StepState::Done;
            i += 1;
        }
        st[active] = StepState::Active;
        st
    }

    unsafe fn draw_step(
        hdc: HDC,
        s: &GuiState,
        x: i32,
        y: i32,
        num: i32,
        title: &str,
        sub: &str,
        st: StepState,
    ) {
        let cy = y + 18;
        let r = 15;
        match st {
            StepState::Done => {
                // Green filled circle + white check.
                let brush = CreateSolidBrush(GREEN);
                if !brush.is_invalid() {
                    let old = SelectObject(hdc, brush);
                    let _ = Ellipse(hdc, x - r, cy - r, x + r, cy + r);
                    let _ = SelectObject(hdc, old);
                    let _ = DeleteObject(brush);
                }
                text_center(hdc, s.f_small, PURE_WHITE, "✓", x, cy - 11, 40, 22);
            }
            StepState::Active => {
                // Blue ring + white number.
                let pen = CreatePen(PS_SOLID, 2, BLUE);
                if !pen.is_invalid() {
                    let old_p = SelectObject(hdc, pen);
                    let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                    let _ = Ellipse(hdc, x - r, cy - r, x + r, cy + r);
                    let _ = SelectObject(hdc, old_p);
                    let _ = SelectObject(hdc, old_b);
                    let _ = DeleteObject(pen);
                }
            }
            StepState::Todo => {
                let pen = CreatePen(PS_SOLID, 2, DIM);
                if !pen.is_invalid() {
                    let old_p = SelectObject(hdc, pen);
                    let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                    let _ = Ellipse(hdc, x - r, cy - r, x + r, cy + r);
                    let _ = SelectObject(hdc, old_p);
                    let _ = SelectObject(hdc, old_b);
                    let _ = DeleteObject(pen);
                }
            }
        }
        if st != StepState::Done {
            let num = format!("{num}");
            let col = if st == StepState::Active { WHITE } else { DIM };
            text_center(hdc, s.f_small, col, &num, x, cy - 11, 40, 22);
        }
        let tcol = if st == StepState::Todo { DIM } else { WHITE };
        let scol = if st == StepState::Todo { DIM } else { GRAY };
        text_at(hdc, s.f_body, tcol, title, x + 28, y + 2);
        text_at(hdc, s.f_tiny, scol, sub, x + 28, y + 26);
    }

    unsafe fn draw_setup_content(hdc: HDC, s: &GuiState) {
        let margin = 44;
        text_at(hdc, s.f_tiny, BLUE, "FLUX REC  /  INSTALLATION", margin, 64);
        text_at(
            hdc,
            s.f_title,
            WHITE,
            "Install Flux Rec",
            margin,
            82,
        );
        text_at(
            hdc,
            s.f_body,
            GRAY,
            "The 2025 client, ready to play. Follow the live progress below.",
            margin,
            124,
        );

        // Horizontal stage rail replaces the old left-side checklist.
        let states = step_states(s.percent, s.finished);
        let gap = 14;
        let step_w = (s.win_w - margin * 2 - gap * 3) / 4;
        for (i, (title, sub)) in STEPS.iter().enumerate() {
            let x = margin + i as i32 * (step_w + gap);
            let y = 154;
            rr_fill(hdc, x, y, step_w, 72, 14, BG_CARD);
            rr_outline(hdc, x, y, step_w, 72, 14, BORDER);
            let st = states.get(i).copied().unwrap_or(StepState::Todo);
            draw_step(hdc, s, x + 26, y + 17, (i + 1) as i32, title, sub, st);
        }

        let left = margin;
        let top = 246;
        let hero_w = 612;
        let panel_h = 352;
        let right = left + hero_w + 22;
        let status_w = s.win_w - right - margin;

        // Clear product identity and client version, separate from live install status.
        rr_fill(hdc, left, top, hero_w, panel_h, 22, BG_CARD);
        rr_outline(hdc, left, top, hero_w, panel_h, 22, BORDER);
        rr_fill(hdc, left + 32, top + 54, 164, 164, 30, BG_SIDE);
        draw_logo(hdc, s, left + 64, top + 86, 100);
        draw_wordmark(hdc, s, left + 404, top + 70);
        text_center(
            hdc,
            s.f_tiny,
            BLUE,
            "2025 CLIENT  /  RIPO TEAM",
            left + 404,
            top + 116,
            360,
            20,
        );
        text_block(
            hdc,
            s.f_body,
            GRAY,
            "Play with friends, explore rooms and create your own. Setup downloads the client and checks its files before launch.",
            RECT { left: left + 228, top: top + 154, right: left + hero_w - 28, bottom: top + 244 },
        );
        rr_fill(hdc, left + 228, top + 264, 188, 34, 17, BLUE_DIM);
        text_center(
            hdc,
            s.f_tiny,
            BLUE,
            "2025 GAME BUILD",
            left + 322,
            top + 270,
            176,
            20,
        );
        text_at(
            hdc,
            s.f_tiny,
            DIM,
            "RIPO TEAM  ·  FLUX REC",
            left + 28,
            top + panel_h - 31,
        );

        // Live work console: progress, current operation, and the latest detail line.
        rr_fill(hdc, right, top, status_w, panel_h, 22, BG_CARD);
        rr_outline(hdc, right, top, status_w, panel_h, 22, BORDER);
        text_at(
            hdc,
            s.f_tiny,
            BLUE,
            "INSTALL ACTIVITY",
            right + 24,
            top + 24,
        );
        let stage = if s.stage.is_empty() {
            "Preparing…"
        } else {
            s.stage.as_str()
        };
        text_ellipsis(
            hdc,
            s.f_h2,
            WHITE,
            stage,
            RECT {
                left: right + 24,
                top: top + 58,
                right: right + status_w - 118,
                bottom: top + 92,
            },
        );
        let pct = format!("{}%", s.percent);
        let _ = SelectObject(hdc, s.f_title);
        let pw = text_width(hdc, &pct);
        text_at(
            hdc,
            s.f_title,
            BLUE,
            &pct,
            right + status_w - pw - 24,
            top + 48,
        );
        let bx = right + 24;
        let bw = status_w - 48;
        if s.percent == 0 && !s.finished {
            draw_shimmer_bar(hdc, s, bx, top + 112, bw, 14);
        } else {
            draw_progress_bar(hdc, bx, top + 112, bw, 14, s.percent);
        }
        text_at(
            hdc,
            s.f_tiny,
            GRAY,
            if s.finished {
                "Final checks complete"
            } else {
                "Progress updates automatically"
            },
            right + 24,
            top + 142,
        );
        if !s.detail.is_empty() {
            text_block(
                hdc,
                s.f_small,
                GRAY,
                &s.detail,
                RECT {
                    left: right + 24,
                    top: top + 184,
                    right: right + status_w - 24,
                    bottom: top + 252,
                },
            );
        } else {
            text_block(
                hdc,
                s.f_small,
                GRAY,
                "You can minimize this window while the setup continues.",
                RECT {
                    left: right + 24,
                    top: top + 184,
                    right: right + status_w - 24,
                    bottom: top + 252,
                },
            );
        }
        rr_fill(hdc, right + 24, top + 282, status_w - 48, 44, 14, BG_SIDE);
        text_center(
            hdc,
            s.f_tiny,
            GRAY,
            "GAME FILES  ·  PATCHES  ·  FINAL CHECKS",
            right + status_w / 2,
            top + 293,
            status_w - 64,
            22,
        );

        let fy = s.win_h - 38;
        text_at(
            hdc,
            s.f_tiny,
            DIM,
            "FLUX REC  /  2025 CLIENT  /  AUTOMATIC UPDATES",
            margin,
            fy,
        );
    }

    unsafe fn draw_setup_redesigned(hdc: HDC, s: &GuiState) {
        // Setup is an editorial split screen: real game art establishes the
        // product on the left; one focused activity panel explains the work on
        // the right. The screen stays useful at every progress state.
        let margin = 28;
        let top = TITLE_H + 22;
        let bottom = s.win_h - 26;
        let panel_h = bottom - top;
        let gap = 22;
        let content_w = s.win_w - margin * 2;
        let art_w = ((content_w - gap) * 51 / 100).clamp(390, 620);
        let panel_x = margin + art_w + gap;
        let panel_w = s.win_w - margin - panel_x;

        // Full-height art panel with a branded caption band.
        draw_game_art(hdc, s, margin, top, art_w, panel_h);
        rr_fill(hdc, margin, bottom - 174, art_w, 174, 0, COLORREF(0x00241A13));
        draw_logo(hdc, s, margin + 28, bottom - 128, 64);
        text_at(hdc, s.f_title, PURE_WHITE, "Flux Rec", margin + 108, bottom - 132);
        text_at(hdc, s.f_small, COLORREF(0x00F1E5D8), "2025 GAME BUILD  ·  RIPO TEAM", margin + 110, bottom - 96);
        text_block(
            hdc,
            s.f_small,
            COLORREF(0x00F1E5D8),
            "Explore rooms, play with friends and create together.",
            RECT { left: margin + 28, top: bottom - 68, right: margin + art_w - 28, bottom: bottom - 28 },
        );
        rr_outline(hdc, margin, top, art_w, panel_h, 18, BORDER);

        // Installation activity panel.
        rr_fill(hdc, panel_x, top, panel_w, panel_h, 18, BG_CARD);
        rr_outline(hdc, panel_x, top, panel_w, panel_h, 18, BORDER);
        let pad = 28;
        let inner_x = panel_x + pad;
        let inner_w = panel_w - pad * 2;
        text_at(hdc, s.f_tiny, BLUE, "RIPO TEAM LAUNCHER  /  SETUP", inner_x, top + 28);
        text_at(hdc, s.f_title, WHITE, "Getting Flux Rec ready", inner_x, top + 56);
        text_at(hdc, s.f_small, GRAY, "2025 client · Download, install and verify", inner_x, top + 102);

        let display_percent = if s.finished { 100 } else { s.percent };
        let percent = format!("{display_percent}%");
        let _ = SelectObject(hdc, s.f_big);
        let percent_w = text_width(hdc, &percent);
        text_at(hdc, s.f_big, if s.finished { GREEN } else { BLUE }, &percent, panel_x + panel_w - pad - percent_w, top + 140);
        let bar_y = top + 164;
        if display_percent == 0 && !s.finished {
            draw_shimmer_bar(hdc, s, inner_x, bar_y, inner_w, 14);
        } else {
            draw_progress_bar(hdc, inner_x, bar_y, inner_w, 14, display_percent);
        }

        let stage = if s.finished {
            "Installation complete"
        } else if s.stage.is_empty() {
            "Preparing game files…"
        } else {
            s.stage.as_str()
        };
        text_ellipsis(
            hdc,
            s.f_h2,
            WHITE,
            stage,
            RECT { left: inner_x, top: top + 196, right: panel_x + panel_w - pad, bottom: top + 226 },
        );

        // A vertical step list keeps the long operation legible without four
        // tiny cards competing for attention.
        let states = step_states(s.percent, s.finished);
        let step_y = top + 254;
        let step_spacing = ((panel_h - 366) / STEPS.len() as i32).clamp(45, 66);
        for (i, (title, sub)) in STEPS.iter().enumerate() {
            let state = states.get(i).copied().unwrap_or(StepState::Todo);
            draw_step(hdc, s, inner_x + 14, step_y + i as i32 * step_spacing, (i + 1) as i32, title, sub, state);
        }

        let info_y = top + panel_h - 92;
        let detail = if !s.detail.is_empty() {
            s.detail.as_str()
        } else if s.finished {
            "Flux Rec is installed and will appear in your launcher library."
        } else {
            "You can minimize this window while setup continues."
        };
        text_ellipsis(
            hdc,
            s.f_tiny,
            GRAY,
            detail,
            RECT { left: inner_x, top: info_y, right: panel_x + panel_w - pad, bottom: info_y + 24 },
        );
        text_at(hdc, s.f_tiny, DIM, &format!("Launcher v{}", env!("CARGO_PKG_VERSION")), inner_x, bottom - 34);
        text_at(hdc, s.f_tiny, DIM, "Automatic update check", panel_x + panel_w - pad - 150, bottom - 34);
    }
    unsafe fn draw_launcher(hdc: HDC, s: &GuiState) {
        let left = 28;
        let panel_y = TITLE_H + 22;
        let brand_w = 188;
        rr_fill(
            hdc,
            left,
            panel_y,
            brand_w,
            s.win_h - panel_y - 46,
            18,
            BG_CARD,
        );
        rr_outline(
            hdc,
            left,
            panel_y,
            brand_w,
            s.win_h - panel_y - 46,
            18,
            BORDER,
        );

        let cx = left + brand_w / 2;
        let cy = panel_y + 91;
        rr_fill(hdc, cx - 60, cy - 60, 120, 120, 28, BG_SIDE);
        draw_logo(hdc, s, cx - 42, cy - 42, 84);
        draw_wordmark(hdc, s, cx, panel_y + 165);
        text_center(
            hdc,
            s.f_tiny,
            GRAY,
            "2025 CLIENT",
            cx,
            panel_y + 204,
            brand_w - 22,
            20,
        );
        text_center(
            hdc,
            s.f_tiny,
            DIM,
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            cx,
            panel_y + 234,
            brand_w - 22,
            18,
        );

        let content_x = left + brand_w + 28;
        let content_w = s.win_w - content_x - 28;
        rr_fill(
            hdc,
            content_x,
            panel_y,
            content_w,
            s.win_h - panel_y - 46,
            18,
            BG_CARD,
        );
        rr_outline(
            hdc,
            content_x,
            panel_y,
            content_w,
            s.win_h - panel_y - 46,
            18,
            BORDER,
        );
        text_at(
            hdc,
            s.f_tiny,
            BLUE,
            "FLUX REC  /  LAUNCH STATUS",
            content_x + 24,
            panel_y + 23,
        );

        let stage = if s.stage.is_empty() {
            "Starting Flux Rec…"
        } else {
            s.stage.as_str()
        };
        text_ellipsis(
            hdc,
            s.f_title,
            WHITE,
            stage,
            RECT {
                left: content_x + 24,
                top: panel_y + 58,
                right: content_x + content_w - 24,
                bottom: panel_y + 100,
            },
        );
        if !s.detail.is_empty() {
            text_ellipsis(
                hdc,
                s.f_small,
                GRAY,
                &s.detail,
                RECT {
                    left: content_x + 24,
                    top: panel_y + 108,
                    right: content_x + content_w - 24,
                    bottom: panel_y + 136,
                },
            );
        } else {
            text_at(
                hdc,
                s.f_small,
                GRAY,
                "Preparing your game session…",
                content_x + 24,
                panel_y + 108,
            );
        }

        let bx = content_x + 24;
        let bw = content_w - 48;
        let by = panel_y + 166;
        if s.percent > 0 {
            draw_progress_bar(hdc, bx, by, bw, 14, s.percent);
            let pct = format!("{}%", s.percent);
            text_ellipsis(
                hdc,
                s.f_tiny,
                DIM,
                &pct,
                RECT {
                    left: bx,
                    top: by + 20,
                    right: bx + bw,
                    bottom: by + 40,
                },
            );
        } else {
            draw_shimmer_bar(hdc, s, bx, by, bw, 14);
            text_at(hdc, s.f_tiny, DIM, "Checking for changes", bx, by + 20);
        }

        let stage_y = panel_y + 210;
        for (i, (label, xoff)) in [("CHECK", 0), ("UPDATE", 118), ("LAUNCH", 236)]
            .into_iter()
            .enumerate()
        {
            let x = content_x + 24 + xoff;
            let active = s.percent >= [0, 35, 80][i];
            rr_fill(
                hdc,
                x,
                stage_y,
                104,
                34,
                12,
                if active { BLUE_DIM } else { BG_SIDE },
            );
            text_center(
                hdc,
                s.f_tiny,
                if active { BLUE } else { DIM },
                label,
                x + 52,
                stage_y + 8,
                96,
                18,
            );
        }
        let footer = format!("Launcher v{}  ·  2025 game build", env!("CARGO_PKG_VERSION"));
        text_at(hdc, s.f_tiny, DIM, &footer, content_x + 24, s.win_h - 31);
    }

    unsafe fn draw_launcher_redesigned(hdc: HDC, s: &GuiState) {
        // The updater keeps the same recognizable product art as setup, with a
        // compact progress rail beside it so the game launch remains the focus.
        let margin = 26;
        let top = TITLE_H + 22;
        let bottom = s.win_h - 24;
        let panel_h = bottom - top;
        let gap = 20;
        let content_w = s.win_w - margin * 2;
        let art_w = ((content_w - gap) * 48 / 100).clamp(390, 500);
        let panel_x = margin + art_w + gap;
        let panel_w = s.win_w - margin - panel_x;

        draw_game_art(hdc, s, margin, top, art_w, panel_h);
        rr_fill(hdc, margin, bottom - 134, art_w, 134, 0, COLORREF(0x00241A13));
        draw_logo(hdc, s, margin + 24, bottom - 104, 54);
        text_at(hdc, s.f_h2, PURE_WHITE, "Flux Rec", margin + 94, bottom - 103);
        text_at(hdc, s.f_tiny, COLORREF(0x00F1E5D8), "2025 GAME BUILD  ·  RIPO TEAM", margin + 96, bottom - 72);
        text_at(hdc, s.f_tiny, COLORREF(0x00F1E5D8), "Update check before every launch", margin + 24, bottom - 34);
        rr_outline(hdc, margin, top, art_w, panel_h, 18, BORDER);

        rr_fill(hdc, panel_x, top, panel_w, panel_h, 18, BG_CARD);
        rr_outline(hdc, panel_x, top, panel_w, panel_h, 18, BORDER);
        let pad = 28;
        let inner_x = panel_x + pad;
        let inner_w = panel_w - pad * 2;
        text_at(hdc, s.f_tiny, BLUE, "RIPO TEAM LAUNCHER  /  AUTO UPDATE", inner_x, top + 30);
        let state = if s.finished {
            "Update complete"
        } else if s.stage.is_empty() {
            "Checking game files…"
        } else {
            s.stage.as_str()
        };
        text_ellipsis(
            hdc,
            s.f_title,
            WHITE,
            state,
            RECT { left: inner_x, top: top + 62, right: panel_x + panel_w - pad, bottom: top + 108 },
        );
        let display_percent = if s.finished { 100 } else { s.percent };
        let percent = format!("{display_percent}%");
        let _ = SelectObject(hdc, s.f_big);
        let percent_w = text_width(hdc, &percent);
        text_at(hdc, s.f_big, if s.finished { GREEN } else { BLUE }, &percent, panel_x + panel_w - pad - percent_w, top + 132);
        let bar_y = top + 158;
        if display_percent == 0 && !s.finished {
            draw_shimmer_bar(hdc, s, inner_x, bar_y, inner_w, 14);
        } else {
            draw_progress_bar(hdc, inner_x, bar_y, inner_w, 14, display_percent);
        }
        let detail = if !s.detail.is_empty() {
            s.detail.as_str()
        } else if s.finished {
            "Starting Flux Rec now…"
        } else {
            "The game opens as soon as the update check finishes."
        };
        text_ellipsis(
            hdc,
            s.f_small,
            GRAY,
            detail,
            RECT { left: inner_x, top: bar_y + 24, right: panel_x + panel_w - pad, bottom: bar_y + 48 },
        );

        let steps = ["Check files", "Apply update", "Start game"];
        let step_y = top + 264;
        for (i, label) in steps.iter().enumerate() {
            let y = step_y + i as i32 * 78;
            let threshold = [0, 35, 85][i];
            let next_threshold = [35, 85, 100][i];
            let step = if s.finished || s.percent >= next_threshold {
                StepState::Done
            } else if i == 0 || s.percent >= threshold {
                StepState::Active
            } else {
                StepState::Todo
            };
            let color = match step {
                StepState::Done => GREEN,
                StepState::Active => BLUE,
                StepState::Todo => TRACK,
            };
            let cx = inner_x + 14;
            let cy = y + 15;
            let pen = CreatePen(PS_SOLID, 2, color);
            if !pen.is_invalid() {
                let old_pen = SelectObject(hdc, pen);
                if step == StepState::Done {
                    let brush = CreateSolidBrush(color);
                    let old_brush = SelectObject(hdc, brush);
                    let _ = Ellipse(hdc, cx - 11, cy - 11, cx + 11, cy + 11);
                    let _ = SelectObject(hdc, old_brush);
                    let _ = DeleteObject(brush);
                    text_center(hdc, s.f_tiny, PURE_WHITE, "✓", cx, cy - 9, 28, 18);
                } else {
                    let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                    let _ = Ellipse(hdc, cx - 11, cy - 11, cx + 11, cy + 11);
                    let _ = SelectObject(hdc, old_brush);
                }
                let _ = SelectObject(hdc, old_pen);
                let _ = DeleteObject(pen);
            }
            text_at(hdc, s.f_body, if step == StepState::Todo { DIM } else { WHITE }, label, inner_x + 38, y + 5);
            let description = match i {
                0 => "Compare the installed build with the latest release",
                1 => "Download and verify changed files when needed",
                _ => "Open the 2025 client when checks finish",
            };
            text_ellipsis(
                hdc,
                s.f_tiny,
                GRAY,
                description,
                RECT { left: inner_x + 38, top: y + 29, right: panel_x + panel_w - pad, bottom: y + 50 },
            );
            if i < 2 {
                let pen = CreatePen(PS_SOLID, 1, TRACK);
                if !pen.is_invalid() {
                    let old = SelectObject(hdc, pen);
                    let _ = MoveToEx(hdc, cx, cy + 12, None);
                    let _ = LineTo(hdc, cx, y + 77);
                    let _ = SelectObject(hdc, old);
                    let _ = DeleteObject(pen);
                }
            }
        }
        text_at(hdc, s.f_tiny, DIM, &format!("Launcher v{}", env!("CARGO_PKG_VERSION")), inner_x, bottom - 30);
    }
}

