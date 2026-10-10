// Flux Rec Setup & Launcher — modern dark-mode UI v0.6.14.
//
// Full redesign:
// - Borderless window, rounded corners, drop shadow, custom title bar
//   (drag to move, minimize + close buttons).
// - Real Flux Rec logo (embedded 32-bit BMP with alpha, drawn with
//   AlphaBlend) instead of the "FR" text monogram.
// - Setup mode: sidebar with logo, wordmark, version pill and a 4-step
//   progress checklist; main area with headline, 2x2 feature grid and a
//   modern rounded progress bar.
// - Launcher mode (--play): compact centered window with logo, live
//   status, detail line and a shimmer/determinate progress bar.
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
    const SETUP_W: i32 = 980;
    const SETUP_H: i32 = 660;
    const LAUNCH_W: i32 = 540;
    const LAUNCH_H: i32 = 400;
    const CORNER: i32 = 36; // round-rect ellipse diameter for the window
    const TITLE_H: i32 = 46;

    const TIMER_ID: usize = 1;
    const TIMER_MS: u32 = 50;

    // Palette (COLORREF = 0x00BBGGRR).
    const BG: COLORREF = COLORREF(0x00170E0B); // #0B0E17 deep navy-black
    const BG_TITLE: COLORREF = COLORREF(0x0019110D); // #0D1119 title bar
    const BG_SIDE: COLORREF = COLORREF(0x001F1411); // #11141F sidebar
    const BG_CARD: COLORREF = COLORREF(0x00281B16); // #161B28 cards
    const BLUE: COLORREF = COLORREF(0x00FF9B2E); // Flux blue #2E9BFF
    const BLUE_LIGHT: COLORREF = COLORREF(0x00FFC76F); // #6FC7FF highlight
    const BLUE_DIM: COLORREF = COLORREF(0x0080501A); // dim blue
    const WHITE: COLORREF = COLORREF(0x00FFFFFF);
    const GRAY: COLORREF = COLORREF(0x00B2A39A); // #9AA3B2 secondary text
    const DIM: COLORREF = COLORREF(0x0070625A); // #5A6270 dim text
    const TRACK: COLORREF = COLORREF(0x003A2A23); // #232A3A progress track
    const BORDER: COLORREF = COLORREF(0x003D2B23); // #232B3D card border
    const GREEN: COLORREF = COLORREF(0x0084DC3D); // #3DDC84 checkmarks

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
        (
            "Customize",
            "Thousands of avatar items, outfits and skins.",
        ),
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
        let (win_w, win_h) = match mode {
            GuiMode::Setup => (SETUP_W, SETUP_H),
            GuiMode::Launcher => (LAUNCH_W, LAUNCH_H),
        };
        let class_name: PCWSTR = match mode {
            GuiMode::Setup => w!("FluxRecSetupGui"),
            GuiMode::Launcher => w!("FluxRecLauncherGui"),
        };
        let title: PCWSTR = match mode {
            GuiMode::Setup => w!("Flux Rec Setup"),
            GuiMode::Launcher => w!("Flux Rec"),
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
        let sx = GetSystemMetrics(SM_CXSCREEN);
        let sy = GetSystemMetrics(SM_CYSCREEN);
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
            WS_EX_APPWINDOW,
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
            let dc = GetDC(Some(hwnd));
            if !dc.is_invalid() {
                let (bmp, w, h) = create_logo_dib(dc);
                s.logo_bmp = bmp;
                s.logo_w = w;
                s.logo_h = h;
                let _ = ReleaseDC(Some(hwnd), dc);
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

    unsafe fn create_font(px: i32, weight: FW_WEIGHT) -> HFONT {
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
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(
            hdc,
            &bmi as *const BITMAPINFO,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE(std::ptr::null_mut()),
            0,
        );
        if hbmp.is_invalid() || bits.is_null() {
            return (HBITMAP::default(), 0, 0);
        }
        // Copy rows (BMP is bottom-up when biHeight > 0) and premultiply.
        let dst = bits as *mut u8;
        for row in 0..ha {
            let src_row = if h > 0 { ha - 1 - row } else { row };
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
                    let s = &*ptr;
                    let pt = POINT {
                        x: loword(lparam.0),
                        y: hiword(lparam.0),
                    };
                    let (min_r, close_r) = title_buttons(s);
                    if pt_in(pt, close_r) {
                        let _ = DestroyWindow(hwnd);
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
                    loop {
                        match s.rx.try_recv() {
                            Ok(m) => {
                                if m.stage.trim().eq_ignore_ascii_case("done") {
                                    let _ = DestroyWindow(hwnd);
                                    break;
                                }
                                s.percent = m.percent.min(100);
                                if s.percent >= 100 {
                                    s.finished = true;
                                }
                                if !m.stage.is_empty() {
                                    s.stage = m.stage;
                                }
                                if !m.detail.is_empty() {
                                    s.detail = m.detail;
                                }
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                    // Shimmer animation for indeterminate progress.
                    s.shimmer = (s.shimmer + 6) % (s.win_w + 200);
                    let _ = InvalidateRect(hwnd, None, false);
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
        let _ = DrawTextW(hdc, &mut buf, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
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
        if s.logo_bmp.is_invalid() || s.logo_w <= 0 || s.logo_h <= 0 || size <= 0 {
            // Fallback: plain blue ring so the layout never looks broken.
            let pen = CreatePen(PS_SOLID, 6, BLUE);
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
        let mem = CreateCompatibleDC(Some(hdc));
        if mem.is_invalid() {
            return;
        }
        let old = SelectObject(mem, s.logo_bmp);
        let bf = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA,
        };
        let _ = AlphaBlend(hdc, x, y, size, size, mem, 0, 0, s.logo_w, s.logo_h, bf);
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
            GuiMode::Setup => "Flux Rec Setup",
            GuiMode::Launcher => "Flux Rec",
        };
        let rc = RECT {
            left: 20,
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
                COLORREF(0x003D2B23) // card border tone
            };
            rr_fill(hdc, rc.left, rc.top, w, h, 12, bg);
        }
        // Glyphs drawn with lines (no font-coverage risk).
        let fg = if hover && is_close { WHITE } else { GRAY };
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
            GuiMode::Launcher => draw_launcher(hdc, s),
        }
    }

    // ------------------------------------------------------------------
    // Setup mode: sidebar + content
    // ------------------------------------------------------------------

    const SIDE_W: i32 = 300;

    unsafe fn draw_setup(hdc: HDC, s: &GuiState) {
        // Sidebar panel.
        rr_fill(hdc, 0, TITLE_H, SIDE_W, s.win_h - TITLE_H, 0, BG_SIDE);
        // Hairline between sidebar and content.
        let pen = CreatePen(PS_SOLID, 1, BORDER);
        if !pen.is_invalid() {
            let old = SelectObject(hdc, pen);
            let _ = MoveToEx(hdc, SIDE_W, TITLE_H, None);
            let _ = LineTo(hdc, SIDE_W, s.win_h);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(pen);
        }

        draw_setup_sidebar(hdc, s);
        draw_setup_content(hdc, s);
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
        text_center(hdc, s.f_tiny, BLUE_LIGHT, &ver, cx, py, pw, ph);

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
                text_center(hdc, s.f_small, WHITE, "✓", x, cy - 11, 40, 22);
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
        let cx0 = SIDE_W + 40; // content left margin

        // Headline.
        text_at(hdc, s.f_title, WHITE, "Welcome to Flux Rec", cx0, 76);
        text_at(
            hdc,
            s.f_body,
            GRAY,
            "Your private Rec Room revival is setting up.",
            cx0,
            118,
        );

        // 2x2 feature cards.
        let cw = 300;
        let ch = 118;
        let gx = cx0;
        let gy = 166;
        let gap = 20;
        for (i, (title, desc)) in CARDS.iter().enumerate() {
            let col = (i % 2) as i32;
            let row = (i / 2) as i32;
            let x = gx + col * (cw + gap);
            let y = gy + row * (ch + gap);
            rr_fill(hdc, x, y, cw, ch, 20, BG_CARD);
            rr_outline(hdc, x, y, cw, ch, 20, BORDER);
            // Blue accent bar.
            rr_fill(hdc, x + 18, y + 22, 4, ch - 44, 4, BLUE);
            text_at(hdc, s.f_h2, WHITE, title, x + 36, y + 16);
            text_block(
                hdc,
                s.f_small,
                GRAY,
                desc,
                RECT {
                    left: x + 36,
                    top: y + 48,
                    right: x + cw - 18,
                    bottom: y + ch - 12,
                },
            );
        }

        // Progress block.
        let py = 446;
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
                left: cx0,
                top: py,
                right: s.win_w - 170,
                bottom: py + 28,
            },
        );
        // Big percentage, right aligned.
        let pct = format!("{}%", s.percent);
        let _ = SelectObject(hdc, s.f_big);
        let pw = text_width(hdc, &pct);
        text_at(hdc, s.f_big, WHITE, &pct, s.win_w - 40 - pw, py - 8);

        draw_progress_bar(hdc, cx0, py + 40, s.win_w - cx0 - 40, 14, s.percent);

        if !s.detail.is_empty() {
            text_ellipsis(
                hdc,
                s.f_small,
                GRAY,
                &s.detail,
                RECT {
                    left: cx0,
                    top: py + 62,
                    right: s.win_w - 40,
                    bottom: py + 84,
                },
            );
        }

        // Footer strip.
        let fy = s.win_h - 46;
        let pen = CreatePen(PS_SOLID, 1, BORDER);
        if !pen.is_invalid() {
            let old = SelectObject(hdc, pen);
            let _ = MoveToEx(hdc, SIDE_W, fy, None);
            let _ = LineTo(hdc, s.win_w, fy);
            let _ = SelectObject(hdc, old);
            let _ = DeleteObject(pen);
        }
        text_center(
            hdc,
            s.f_tiny,
            DIM,
            "Automatic updates   •   flux.sitey.my   •   Safe & secure",
            (SIDE_W + s.win_w) / 2,
            fy + 13,
            s.win_w - SIDE_W,
            20,
        );
    }

    // ------------------------------------------------------------------
    // Launcher mode: compact centered window
    // ------------------------------------------------------------------

    unsafe fn draw_launcher(hdc: HDC, s: &GuiState) {
        let cx = s.win_w / 2;

        // Logo.
        draw_logo(hdc, s, cx - 52, 62, 104);

        // Wordmark.
        draw_wordmark(hdc, s, cx, 182);

        // Status.
        let stage = if s.stage.is_empty() {
            "Starting…"
        } else {
            s.stage.as_str()
        };
        text_center(hdc, s.f_h2, WHITE, stage, cx, 226, s.win_w - 80, 28);

        // Detail.
        if !s.detail.is_empty() {
            text_center(hdc, s.f_small, GRAY, &s.detail, cx, 256, s.win_w - 80, 22);
        }

        // Progress: determinate when we have a value, shimmer otherwise.
        let bx = 70;
        let bw = s.win_w - 140;
        let by = 296;
        if s.percent > 0 {
            draw_progress_bar(hdc, bx, by, bw, 12, s.percent);
            let pct = format!("{}%", s.percent);
            text_center(hdc, s.f_tiny, DIM, &pct, cx, by + 20, 80, 18);
        } else {
            draw_shimmer_bar(hdc, s, bx, by, bw, 12);
        }

        // Footer.
        let ver = format!("v{}   •   flux.sitey.my", env!("CARGO_PKG_VERSION"));
        text_center(hdc, s.f_tiny, DIM, &ver, cx, s.win_h - 34, s.win_w - 40, 20);
    }
}
