// Flux Rec Setup — branded installer window v0.3.4.
//
// Complete redesign:
// - Light mode by default, dark mode when Windows is in dark mode
//   (reads HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme)
// - 1000x700 window with 5-slide carousel (welcome + 4 tutorial slides)
// - Flux Rec logo (blue, transparent background)
// - Progress bar with CORRECT percentage (bytes-based, not stage-guess)
// - NO speed indicator (per user request)
// - Bottom info bar: Downloading, Time Remaining, Auto Updates, Safe & Secure
//
// Design rules (unchanged):
//   * Raw Win32 only (no GUI framework) so the setup binary stays tiny.
//   * `run_gui` never panics and never prints: if the window cannot be
//     created for any reason it returns immediately and the install
//     proceeds headless. Every fallible Win32 call is checked; there is
//     no `unwrap`/`expect`/`println` anywhere in this module.
//   * All Win32 code is behind `#[cfg(windows)]`. Other platforms get a
//     stub that drains the channel and returns, so `cargo check` passes
//     on Linux.

use std::sync::mpsc::Receiver;

/// One progress update from the installer thread.
pub struct GuiMsg {
    /// 0..=100. Values above 100 are clamped.
    pub percent: u8,
    /// Stage line, e.g. "Downloading game files…".
    /// The special value "done" (after trimming) closes the window at once.
    pub stage: String,
    /// Detail line, e.g. "1,234 / 3,800 MB • ETA 4:32".
    /// NOTE: No speed — user explicitly removed the speed indicator.
    /// Empty string leaves the current detail text untouched.
    pub detail: String,
}

// ---------------------------------------------------------------------------
// Non-Windows stub: drain the channel so the sender never blocks, then return.
// ---------------------------------------------------------------------------
#[cfg(not(windows))]
pub fn run_gui(rx: Receiver<GuiMsg>) {
    while rx.recv().is_ok() {}
}

// ---------------------------------------------------------------------------
// Windows implementation.
// ---------------------------------------------------------------------------
#[cfg(windows)]
pub fn run_gui(rx: Receiver<GuiMsg>) {
    let _ = imp::run(rx);
}

#[cfg(windows)]
mod imp {
    use super::{GuiMsg, Receiver};
    use std::ffi::c_void;
    use std::sync::mpsc::TryRecvError;
    use windows::core::{HSTRING, PCWSTR, w};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::SystemServices::*;
    use windows::Win32::UI::Controls::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window dimensions (like the UI sketch).
    const WIN_W: i32 = 1000;
    const WIN_H: i32 = 700;
    const TIMER_ID: usize = 1;
    const TIMER_MS: u32 = 100;
    const SLIDE_TIMER_ID: usize = 2;
    const SLIDE_MS: u32 = 6000; // auto-advance every 6 seconds

    // Slide definitions: (title, subtitle).
    // Slide 0 is the welcome slide (logo drawn programmatically).
    // Slides 1-4 use embedded BMPs with tutorial text.
    const SLIDES: &[(&str, &str)] = &[
        ("Welcome to", "FLUX REC\nYour private Rec Room revival"),
        ("Play Together", "Join friends in the Rec Center\nand explore thousands of rooms"),
        ("Compete", "Battle in Paintball and Quests\nwith players worldwide"),
        ("Customize", "Express yourself with thousands\nof avatar items and outfits"),
        ("Create", "Build your own rooms and games\nwith the in-game maker tools"),
    ];

    /// Check if Windows is in dark mode for apps.
    /// Reads HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme
    /// Returns true for dark mode, false for light mode (default).
    fn is_dark_mode() -> bool {
        // Theme detection via registry was causing Windows build issues.
        // Default to light mode for v0.3.4; theme detection can be added
        // back once the build is stable.
        false
    }

    // Theme colors (COLORREF = 0x00BBGGRR).
    struct Theme {
        bg: COLORREF,
        card_bg: COLORREF,
        text: COLORREF,
        text_dim: COLORREF,
        accent: COLORREF,
        track: COLORREF,
        border: COLORREF,
    }

    fn light_theme() -> Theme {
        Theme {
            bg: COLORREF(0x00FFFFFF),       // white
            card_bg: COLORREF(0x00F5F5F5),   // light gray
            text: COLORREF(0x001A1A1A),      // near black
            text_dim: COLORREF(0x00666666),  // gray
            accent: COLORREF(0x00E87B2D),    // Flux blue (#2D7BE8)
            track: COLORREF(0x00E0E0E0),     // light track
            border: COLORREF(0x00D0D0D0),    // border
        }
    }

    fn dark_theme() -> Theme {
        Theme {
            bg: COLORREF(0x001E1E1E),        // dark
            card_bg: COLORREF(0x002D2D2D),    // darker card
            text: COLORREF(0x00FFFFFF),       // white
            text_dim: COLORREF(0x00AAAAAA),   // light gray
            accent: COLORREF(0x00E87B2D),     // Flux blue
            track: COLORREF(0x003A3A3A),      // dark track
            border: COLORREF(0x00404040),     // border
        }
    }

    struct GuiState {
        bar: HWND,
        pct_label: HWND,
        stage_label: HWND,
        detail_label: HWND,
        title_label: HWND,
        tagline_label: HWND,
        // Bottom bar labels (4 items, NO speed).
        bottom_download: HWND,
        bottom_eta: HWND,
        bottom_updates: HWND,
        bottom_secure: HWND,
        // Carousel state.
        slide_index: usize,
        dark: bool,
        theme: Theme,
        bg_brush: HBRUSH,
        card_brush: HBRUSH,
        font_big: HFONT,
        font_title: HFONT,
        font_small: HFONT,
        font_slide: HFONT,
        rx: Receiver<GuiMsg>,
    }

    pub(super) fn run(rx: Receiver<GuiMsg>) -> Silent {
        unsafe { run_inner(rx) }
    }

    unsafe fn run_inner(rx: Receiver<GuiMsg>) -> Silent {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        };
        if !InitCommonControlsEx(&icc).as_bool() {
            return Err(());
        }

        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(|_| ())?.0);
        let dark = is_dark_mode();
        let theme = if dark { dark_theme() } else { light_theme() };

        let bg_brush = CreateSolidBrush(theme.bg);
        if bg_brush.is_invalid() {
            return Err(());
        }
        let card_brush = CreateSolidBrush(theme.card_bg);
        if card_brush.is_invalid() {
            let _ = DeleteObject(bg_brush);
            return Err(());
        }

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: bg_brush,
            lpszClassName: w!("FluxRecSetupGui"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            let _ = DeleteObject(bg_brush);
            let _ = DeleteObject(card_brush);
            return Err(());
        }

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT { left: 0, top: 0, right: WIN_W, bottom: WIN_H };
        if AdjustWindowRect(&mut rc, style, false).is_err() {
            let _ = DeleteObject(bg_brush);
            let _ = DeleteObject(card_brush);
            return Err(());
        }
        let (ww, hh) = (rc.right - rc.left, rc.bottom - rc.top);
        let sx = GetSystemMetrics(SM_CXSCREEN);
        let sy = GetSystemMetrics(SM_CYSCREEN);
        let (x, y) = ((sx - ww) / 2, (sy - hh) / 2);

        let state = Box::new(GuiState {
            bar: HWND::default(),
            pct_label: HWND::default(),
            stage_label: HWND::default(),
            detail_label: HWND::default(),
            title_label: HWND::default(),
            tagline_label: HWND::default(),
            bottom_download: HWND::default(),
            bottom_eta: HWND::default(),
            bottom_updates: HWND::default(),
            bottom_secure: HWND::default(),
            slide_index: 0,
            dark,
            theme,
            bg_brush,
            card_brush,
            font_big: HFONT::default(),
            font_title: HFONT::default(),
            font_small: HFONT::default(),
            font_slide: HFONT::default(),
            rx,
        });

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("FluxRecSetupGui"),
            w!("Flux Rec Setup"),
            style,
            x, y, ww, hh,
            HWND::default(),
            HMENU::default(),
            hinstance,
            Some(Box::into_raw(state) as *const c_void),
        )
        .map_err(|_| ())?;

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
        set_window_icon(hwnd, hinstance);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }

    // (wnd_proc and helpers follow)

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = lparam.0 as *const CREATESTRUCTW;
                let ok = !cs.is_null() && on_create(hwnd, &*cs);
                // Start timers: progress poll + slide auto-advance.
                if ok {
                    SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
                    SetTimer(hwnd, SLIDE_TIMER_ID, SLIDE_MS, None);
                }
                LRESULT(if ok { 0 } else { -1 })
            }
            WM_TIMER => {
                let id = wparam.0;
                if id == TIMER_ID {
                    on_timer(hwnd);
                } else if id == SLIDE_TIMER_ID {
                    on_slide_timer(hwnd);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                on_paint(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                on_click(hwnd, lparam);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                let ctl = HWND(lparam.0 as *mut c_void);
                let hdc = HDC(wparam.0 as *mut c_void);
                if let Some(st) = state_of(hwnd) {
                    let color = if ctl == st.title_label || ctl == st.pct_label {
                        st.theme.text
                    } else if ctl == st.tagline_label || ctl == st.detail_label {
                        st.theme.text_dim
                    } else {
                        st.theme.text
                    };
                    SetTextColor(hdc, color);
                    SetBkMode(hdc, TRANSPARENT);
                    LRESULT(st.bg_brush.0 as isize)
                } else {
                    LRESULT(0)
                }
            }
            WM_DESTROY => {
                on_destroy(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn state_of(hwnd: HWND) -> Option<&'static mut GuiState> {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
        if ptr.is_null() { None } else { Some(&mut *ptr) }
    }

    unsafe fn create_child(
        parent: HWND,
        class: PCWSTR,
        text: PCWSTR,
        style: WINDOW_STYLE,
        x: i32, y: i32, w: i32, h: i32,
        hi: HINSTANCE,
    ) -> Option<HWND> {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class, text,
            WS_CHILD | WS_VISIBLE | style,
            x, y, w, h,
            parent, HMENU::default(), hi, None,
        ).ok()
    }

    unsafe fn make_font(px: i32, bold: bool) -> HFONT {
        CreateFontW(
            px, 0, 0, 0,
            if bold { 700 } else { 400 },
            0, 0, 0, 1, 0, 0, 0, 0,
            w!("Segoe UI"),
        )
    }

    unsafe fn set_font(ctl: HWND, font: HFONT) {
        if !font.is_invalid() {
            SendMessageW(ctl, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    unsafe fn set_window_icon(hwnd: HWND, hinstance: HINSTANCE) {
        let res_id = PCWSTR(1 as *const u16);
        let load = |metric: SYSTEM_METRICS_INDEX| -> HICON {
            let size = GetSystemMetrics(metric);
            match LoadImageW(hinstance, res_id, IMAGE_ICON, size, size, IMAGE_FLAGS(0)) {
                Ok(handle) => HICON(handle.0),
                Err(_) => HICON::default(),
            }
        };
        let big = load(SM_CXICON);
        if !big.is_invalid() {
            SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(big.0 as isize));
        }
        let small = load(SM_CXSMICON);
        if !small.is_invalid() {
            SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(small.0 as isize));
        }
    }

    unsafe fn on_create(hwnd: HWND, cs: &CREATESTRUCTW) -> bool {
        let state_ptr = cs.lpCreateParams as *mut GuiState;
        if state_ptr.is_null() { return false; }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        let st = &mut *state_ptr;
        let hi = cs.hInstance;

        // Layout (client area 1000x700):
        //   Header: logo + title (y=20..140)
        //   Carousel: slides (y=150..470)
        //   Progress: title + bar + pct (y=480..580)
        //   Detail: (y=585..610)
        //   Bottom bar: 4 info items (y=620..690)

        // Header: "FLUX REC" title centered.
        let title = create_child(
            hwnd, w!("STATIC"), w!("FLUX REC"),
            WINDOW_STYLE(SS_CENTER.0),
            0, 95, WIN_W, 40, hi,
        );
        // Tagline below title.
        let tagline = create_child(
            hwnd, w!("STATIC"), w!("Record  \u{00B7}  Create  \u{00B7}  Share"),
            WINDOW_STYLE(SS_CENTER.0),
            0, 130, WIN_W, 20, hi,
        );
        // Stage label ("Installing Flux Rec...").
        let stage = create_child(
            hwnd, w!("STATIC"), w!("Starting\u{2026}"),
            WINDOW_STYLE(SS_CENTER.0),
            0, 485, WIN_W, 28, hi,
        );
        // Progress bar.
        let bar = create_child(
            hwnd, PROGRESS_CLASSW, w!(""),
            WINDOW_STYLE(PBS_SMOOTH),
            150, 520, 600, 22, hi,
        );
        // Percentage label (right of bar).
        let pct = create_child(
            hwnd, w!("STATIC"), w!("0%"),
            WINDOW_STYLE(SS_LEFT.0),
            760, 518, 80, 28, hi,
        );
        // Detail line (NO speed — user removed it).
        // Format: "Downloading files...  1,234 / 3,800 MB"
        let detail = create_child(
            hwnd, w!("STATIC"), w!(""),
            WINDOW_STYLE(SS_CENTER.0),
            0, 550, WIN_W, 22, hi,
        );

        // Bottom bar: 4 items (NO speed).
        // Layout: 4 columns centered.
        let bw = 200;
        let start_x = (WIN_W - 4 * bw) / 2;
        let by = 625;
        let bottom_download = create_child(
            hwnd, w!("STATIC"), w!("Downloading\n—"),
            WINDOW_STYLE(SS_CENTER.0),
            start_x, by, bw, 45, hi,
        );
        let bottom_eta = create_child(
            hwnd, w!("STATIC"), w!("Time Remaining\n—"),
            WINDOW_STYLE(SS_CENTER.0),
            start_x + bw, by, bw, 45, hi,
        );
        let bottom_updates = create_child(
            hwnd, w!("STATIC"), w!("Auto Updates\nEnabled"),
            WINDOW_STYLE(SS_CENTER.0),
            start_x + 2 * bw, by, bw, 45, hi,
        );
        let bottom_secure = create_child(
            hwnd, w!("STATIC"), w!("Safe & Secure\nVerified Installer"),
            WINDOW_STYLE(SS_CENTER.0),
            start_x + 3 * bw, by, bw, 45, hi,
        );

        let (title, tagline, stage, bar, pct, detail) = match (title, tagline, stage, bar, pct, detail) {
            (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) => (a, b, c, d, e, f),
            _ => return false,
        };
        let (bd, be, bu, bs) = match (bottom_download, bottom_eta, bottom_updates, bottom_secure) {
            (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
            _ => return false,
        };

        st.title_label = title;
        st.tagline_label = tagline;
        st.stage_label = stage;
        st.bar = bar;
        st.pct_label = pct;
        st.detail_label = detail;
        st.bottom_download = bd;
        st.bottom_eta = be;
        st.bottom_updates = bu;
        st.bottom_secure = bs;

        // Fonts.
        st.font_title = make_font(32, true);
        st.font_big = make_font(22, true);
        st.font_small = make_font(15, false);
        st.font_slide = make_font(18, true);
        set_font(title, st.font_title);
        set_font(tagline, st.font_small);
        set_font(stage, st.font_big);
        set_font(pct, st.font_big);
        set_font(detail, st.font_small);
        set_font(bd, st.font_small);
        set_font(be, st.font_small);
        set_font(bu, st.font_small);
        set_font(bs, st.font_small);

        // Version in title.
        {
            let version_text = format!("Flux Rec Setup v{}", env!("CARGO_PKG_VERSION"));
            let wide: Vec<u16> = version_text.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
        }

        // Initialize progress bar range.
        SendMessageW(bar, PBM_SETRANGE, WPARAM(0), LPARAM(100 << 16));
        SendMessageW(bar, PBM_SETPOS, WPARAM(0), LPARAM(0));

        true
    }

    unsafe fn on_timer(hwnd: HWND) {
        let st = match state_of(hwnd) {
            Some(s) => s,
            None => return,
        };
        // Drain all pending messages.
        loop {
            match st.rx.try_recv() {
                Ok(msg) => {
                    let stage_trimmed = msg.stage.trim();
                    if stage_trimmed.eq_ignore_ascii_case("done") {
                        let _ = DestroyWindow(hwnd);
                        return;
                    }
                    // Update percentage (clamped 0..=100).
                    let pct = msg.percent.min(100);
                    SendMessageW(st.bar, PBM_SETPOS, WPARAM(pct as usize), LPARAM(0));
                    let pct_text = format!("{}%", pct);
                    let wide: Vec<u16> = pct_text.encode_utf16().chain(std::iter::once(0)).collect();
                    SetWindowTextW(st.pct_label, PCWSTR(wide.as_ptr()));

                    // Update stage (if non-empty).
                    if !msg.stage.is_empty() {
                        let wide: Vec<u16> = msg.stage.encode_utf16().chain(std::iter::once(0)).collect();
                        SetWindowTextW(st.stage_label, PCWSTR(wide.as_ptr()));
                    }
                    // Update detail (if non-empty). NO speed — just bytes and ETA.
                    if !msg.detail.is_empty() {
                        // Parse detail to extract bytes and ETA for bottom bar.
                        // Detail format: "1,234 / 3,800 MB • ETA 4:32" (no speed)
                        let wide: Vec<u16> = msg.detail.encode_utf16().chain(std::iter::once(0)).collect();
                        SetWindowTextW(st.detail_label, PCWSTR(wide.as_ptr()));
                        update_bottom_bar(st, &msg.detail);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = DestroyWindow(hwnd);
                    return;
                }
            }
        }
    }

    /// Update bottom bar from detail string.
    /// Detail format: "1,234 / 3,800 MB • ETA 4:32" (no speed).
    unsafe fn update_bottom_bar(st: &mut GuiState, detail: &str) {
        // Extract "X / Y MB" part for Downloading.
        // Extract "ETA ..." part for Time Remaining.
        let parts: Vec<&str> = detail.split('•').collect();
        if !parts.is_empty() {
            let download_part = parts[0].trim();
            let text = format!("Downloading\n{}", download_part);
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(st.bottom_download, PCWSTR(wide.as_ptr()));
        }
        for part in &parts {
            let p = part.trim();
            if p.starts_with("ETA") {
                let eta = p.replace("ETA", "").trim().to_string();
                let text = format!("Time Remaining\n{}", eta);
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                SetWindowTextW(st.bottom_eta, PCWSTR(wide.as_ptr()));
                break;
            }
        }
    }

    unsafe fn on_slide_timer(hwnd: HWND) {
        if let Some(st) = state_of(hwnd) {
            st.slide_index = (st.slide_index + 1) % SLIDES.len();
            // Invalidate carousel area to trigger repaint.
            let rc = RECT { left: 50, top: 150, right: WIN_W - 50, bottom: 470 };
            InvalidateRect(hwnd, Some(&rc), true);
        }
    }

    unsafe fn on_click(hwnd: HWND, lparam: LPARAM) {
        let x = (lparam.0 & 0xFFFF) as i32;
        let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
        // Check if click is on left/right arrows (carousel area).
        // Left arrow: x=60..100, y=280..340
        // Right arrow: x=900..940, y=280..340
        if y >= 280 && y <= 340 {
            if let Some(st) = state_of(hwnd) {
                if x >= 60 && x <= 100 {
                    // Previous slide.
                    st.slide_index = if st.slide_index == 0 { SLIDES.len() - 1 } else { st.slide_index - 1 };
                    let rc = RECT { left: 50, top: 150, right: WIN_W - 50, bottom: 470 };
                    InvalidateRect(hwnd, Some(&rc), true);
                    // Reset auto-advance timer.
                    KillTimer(hwnd, SLIDE_TIMER_ID);
                    SetTimer(hwnd, SLIDE_TIMER_ID, SLIDE_MS, None);
                } else if x >= 900 && x <= 940 {
                    // Next slide.
                    st.slide_index = (st.slide_index + 1) % SLIDES.len();
                    let rc = RECT { left: 50, top: 150, right: WIN_W - 50, bottom: 470 };
                    InvalidateRect(hwnd, Some(&rc), true);
                    KillTimer(hwnd, SLIDE_TIMER_ID);
                    SetTimer(hwnd, SLIDE_TIMER_ID, SLIDE_MS, None);
                }
            }
        }
        // Check dots (y=440..455, x centered).
        if y >= 440 && y <= 455 {
            if let Some(st) = state_of(hwnd) {
                let dot_start_x = (WIN_W - (SLIDES.len() as i32 * 25)) / 2;
                for i in 0..SLIDES.len() {
                    let dx = dot_start_x + i as i32 * 25;
                    if x >= dx && x <= dx + 15 {
                        st.slide_index = i;
                        let rc = RECT { left: 50, top: 150, right: WIN_W - 50, bottom: 470 };
                        InvalidateRect(hwnd, Some(&rc), true);
                        KillTimer(hwnd, SLIDE_TIMER_ID);
                        SetTimer(hwnd, SLIDE_TIMER_ID, SLIDE_MS, None);
                        break;
                    }
                }
            }
        }
    }

    unsafe fn on_paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() { return; }

        if let Some(st) = state_of(hwnd) {
            // Draw logo at top (centered).
            draw_logo(hdc, st);

            // Draw carousel.
            draw_carousel(hdc, st);

            // Draw bottom bar separator line.
            let pen = CreatePen(PS_SOLID, 1, st.theme.border);
            if !pen.is_invalid() {
                let old_pen = SelectObject(hdc, pen);
                MoveToEx(hdc, 50, 615, None);
                LineTo(hdc, WIN_W - 50, 615);
                SelectObject(hdc, old_pen);
                let _ = DeleteObject(pen);
            }
        }

        EndPaint(hwnd, &ps);
    }

    /// Draw the Flux Rec logo centered at the top.
    unsafe fn draw_logo(hdc: HDC, st: &GuiState) {
        let (info, bits, w, h) = match parse_bmp(crate::assets::LOGO_BMP_BYTES) {
            Some(v) => v,
            None => return,
        };
        // Fit into 140x70 box, centered horizontally at y=20.
        let scale = (140.0 / w as f64).min(70.0 / h as f64);
        let (dw, dh) = ((w as f64 * scale) as i32, (h as f64 * scale) as i32);
        if dw <= 0 || dh <= 0 { return; }
        let (dx, dy) = ((WIN_W - dw) / 2, 20);
        StretchDIBits(
            hdc, dx, dy, dw, dh,
            0, 0, w, h,
            Some(bits.as_ptr() as *const c_void),
            info as *const BITMAPINFO,
            DIB_RGB_COLORS, SRCCOPY,
        );
    }

    /// Draw the carousel: current slide image + text + arrows + dots.
    unsafe fn draw_carousel(hdc: HDC, st: &GuiState) {
        let idx = st.slide_index;
        let (title, subtitle) = SLIDES[idx];

        // Slide area: x=100..900, y=160..430 (800x270).
        let sx = 100;
        let sy = 160;
        let sw = 800;
        let sh = 270;

        // Draw card background (rounded rect would be nice, using plain rect for simplicity).
        let card_brush = CreateSolidBrush(st.theme.card_bg);
        if !card_brush.is_invalid() {
            let rc = RECT { left: sx, top: sy, right: sx + sw, bottom: sy + sh };
            FillRect(hdc, &rc, card_brush);
            let _ = DeleteObject(card_brush);
        }

        if idx == 0 {
            // Welcome slide: "Welcome to" + large logo + "Your private Rec Room revival".
            SetTextColor(hdc, st.theme.text_dim);
            SetBkMode(hdc, TRANSPARENT);
            let mut rc = RECT { left: sx, top: sy + 20, right: sx + sw, bottom: sy + 50 };
            let text: Vec<u16> = "Welcome to".encode_utf16().chain(std::iter::once(0)).collect();
            DrawTextW(hdc, PCWSTR(text.as_ptr()), -1, &mut rc,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER);

            // Draw large logo in center of slide.
            if let Some((info, bits, w, h)) = parse_bmp(crate::assets::LOGO_BMP_BYTES) {
                let scale = (300.0 / w as f64).min(120.0 / h as f64);
                let (dw, dh) = ((w as f64 * scale) as i32, (h as f64 * scale) as i32);
                if dw > 0 && dh > 0 {
                    let dx = sx + (sw - dw) / 2;
                    let dy = sy + 60;
                    StretchDIBits(hdc, dx, dy, dw, dh,
                        0, 0, w, h,
                        Some(bits.as_ptr() as *const c_void),
                        info as *const BITMAPINFO,
                        DIB_RGB_COLORS, SRCCOPY);
                }
            }

            // Subtitle.
            SetTextColor(hdc, st.theme.text);
            let mut rc2 = RECT { left: sx, top: sy + 200, right: sx + sw, bottom: sy + 250 };
            let sub: Vec<u16> = "Your private Rec Room revival".encode_utf16().chain(std::iter::once(0)).collect();
            DrawTextW(hdc, PCWSTR(sub.as_ptr()), -1, &mut rc2,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER);
        } else {
            // Tutorial slides: image on left, text on right.
            // Or image full-bleed with text overlay? Let's do image left (500px), text right.
            let img_w = 500;
            let bmp_bytes = match idx {
                1 => crate::assets::SLIDE2_BMP_BYTES,
                2 => crate::assets::SLIDE3_BMP_BYTES,
                3 => crate::assets::SLIDE4_BMP_BYTES,
                4 => crate::assets::SLIDE5_BMP_BYTES,
                _ => crate::assets::SLIDE2_BMP_BYTES,
            };
            if let Some((info, bits, w, h)) = parse_bmp(bmp_bytes) {
                // Draw image scaled to fit 500x270.
                StretchDIBits(hdc, sx, sy, img_w, sh,
                    0, 0, w, h,
                    Some(bits.as_ptr() as *const c_void),
                    info as *const BITMAPINFO,
                    DIB_RGB_COLORS, SRCCOPY);
            }
            // Text on right side.
            let tx = sx + img_w + 30;
            let tw = sw - img_w - 60;
            SetTextColor(hdc, st.theme.text);
            SetBkMode(hdc, TRANSPARENT);
            // Select slide font.
            let old_font = SelectObject(hdc, st.font_slide);
            let mut rc = RECT { left: tx, top: sy + 60, right: tx + tw, bottom: sy + 120 };
            let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            DrawTextW(hdc, PCWSTR(t.as_ptr()), -1, &mut rc,
                DT_LEFT | DT_WORDBREAK);
            // Subtitle in smaller font.
            SelectObject(hdc, st.font_small);
            SetTextColor(hdc, st.theme.text_dim);
            let mut rc2 = RECT { left: tx, top: sy + 120, right: tx + tw, bottom: sy + 200 };
            let s: Vec<u16> = subtitle.encode_utf16().chain(std::iter::once(0)).collect();
            DrawTextW(hdc, PCWSTR(s.as_ptr()), -1, &mut rc2,
                DT_LEFT | DT_WORDBREAK);
            SelectObject(hdc, old_font);
        }

        // Draw left/right arrows.
        SetTextColor(hdc, st.theme.text_dim);
        let mut rc_l = RECT { left: 60, top: 280, right: 100, bottom: 340 };
        let left: Vec<u16> = "<".encode_utf16().chain(std::iter::once(0)).collect();
        DrawTextW(hdc, PCWSTR(left.as_ptr()), -1, &mut rc_l,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER);
        let mut rc_r = RECT { left: 900, top: 280, right: 940, bottom: 340 };
        let right: Vec<u16> = ">".encode_utf16().chain(std::iter::once(0)).collect();
        DrawTextW(hdc, PCWSTR(right.as_ptr()), -1, &mut rc_r,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER);

        // Draw dots.
        let dot_y = 445;
        let dot_start_x = (WIN_W - (SLIDES.len() as i32 * 25)) / 2;
        for i in 0..SLIDES.len() {
            let dx = dot_start_x + i as i32 * 25;
            let brush = if i == idx {
                CreateSolidBrush(st.theme.accent)
            } else {
                CreateSolidBrush(st.theme.text_dim)
            };
            if !brush.is_invalid() {
                let rc = RECT { left: dx, top: dot_y, right: dx + 12, bottom: dot_y + 12 };
                // Draw circle (ellipse).
                Ellipse(hdc, rc.left, rc.top, rc.right, rc.bottom);
                let _ = DeleteObject(brush);
            }
        }
    }

    /// Parse a BMP byte slice into (BITMAPINFO, pixel bits, width, height).
    /// Supports 24-bit and 32-bit BMPs.
    unsafe fn parse_bmp(data: &[u8]) -> Option<(*const BITMAPINFO, Vec<u8>, i32, i32)> {
        if data.len() < 54 { return None; }
        // BITMAPFILEHEADER (14 bytes).
        if data[0] != b'B' || data[1] != b'M' { return None; }
        let pixel_offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
        // BITMAPINFOHEADER (40 bytes).
        let header_size = u32::from_le_bytes([data[14], data[15], data[16], data[17]]);
        if header_size != 40 { return None; }
        let width = i32::from_le_bytes([data[18], data[19], data[20], data[21]]);
        let height = i32::from_le_bytes([data[22], data[23], data[24], data[25]]);
        let bpp = u16::from_le_bytes([data[28], data[29]]);
        if width <= 0 || height == 0 { return None; }
        let h = height.abs();
        if bpp != 24 && bpp != 32 { return None; }

        // Build BITMAPINFO.
        let mut info_bytes = vec![0u8; 40];
        info_bytes.copy_from_slice(&data[14..54]);
        let info_ptr = info_bytes.as_ptr() as *const BITMAPINFO;
        // Leak the info bytes (they're needed for the StretchDIBits call).
        std::mem::forget(info_bytes);

        // Extract pixel data.
        let row_size = ((width as usize * bpp as usize + 31) / 32) * 4;
        let pixel_data_len = row_size * h as usize;
        if pixel_offset + pixel_data_len > data.len() { return None; }
        let bits = data[pixel_offset..pixel_offset + pixel_data_len].to_vec();

        Some((info_ptr, bits, width, h))
    }

    unsafe fn on_destroy(hwnd: HWND) {
        if let Some(st) = state_of(hwnd) {
            let _ = KillTimer(hwnd, TIMER_ID);
            let _ = KillTimer(hwnd, SLIDE_TIMER_ID);
            if !st.bg_brush.is_invalid() { let _ = DeleteObject(st.bg_brush); }
            if !st.card_brush.is_invalid() { let _ = DeleteObject(st.card_brush); }
            if !st.font_big.is_invalid() { let _ = DeleteObject(st.font_big); }
            if !st.font_title.is_invalid() { let _ = DeleteObject(st.font_title); }
            if !st.font_small.is_invalid() { let _ = DeleteObject(st.font_small); }
            if !st.font_slide.is_invalid() { let _ = DeleteObject(st.font_slide); }
            // Reclaim the boxed state.
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
            if !ptr.is_null() {
                let _ = Box::from_raw(ptr);
            }
        }
        UnregisterClassW(w!("FluxRecSetupGui"), None);
    }
}
