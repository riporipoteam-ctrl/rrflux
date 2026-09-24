// Flux Rec Setup — branded installer window.
//
// Replaces the black console window the user used to see while the installer
// downloads / extracts / brands the game. The console logic is kept but
// hidden; this module is the small window the user actually watches: logo,
// brand title, stage line, blue progress bar, big percentage, footer.
//
// Design rules:
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
    // The install must never die or stall because of the GUI: any failure
    // below is swallowed and the installer proceeds headless/hidden.
    let _ = imp::run(rx);
}

#[cfg(windows)]
mod imp {
    use super::{GuiMsg, Receiver};
    use std::ffi::c_void;
    use std::sync::mpsc::TryRecvError;
    // NOTE: `windows::core::*` is deliberately NOT glob-imported: it would
    // shadow `std::result::Result` with `windows_result::Result<T>`.
    use windows::core::{HSTRING, PCWSTR, w};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::SystemServices::*;
    use windows::Win32::UI::Controls::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    /// Silent-failure result: Err(()) just means "no window, carry on".
    type Silent = std::result::Result<(), ()>;

    const WIN_W: i32 = 480;
    const WIN_H: i32 = 344;
    const TIMER_ID: usize = 1;
    /// Channel poll interval: progress feels live without busy-looping.
    const TIMER_MS: u32 = 100;

    // Flux Rec brand palette (COLORREF = 0x00BBGGRR).
    const BG: COLORREF = COLORREF(0x00261A12); // deep navy (#121A26)
    const BLUE: COLORREF = COLORREF(0x00E87B2D); // Flux blue (#2D7BE8)
    const TRACK: COLORREF = COLORREF(0x003A2A24); // dark track (#242A3A)
    const WHITE: COLORREF = COLORREF(0x00FFFFFF);
    const LIGHT: COLORREF = COLORREF(0x00CFCFCF); // stage text
    const DIM: COLORREF = COLORREF(0x008A8A8A); // footer

    struct GuiState {
        bar: HWND,
        pct_label: HWND,
        stage_label: HWND,
        title_label: HWND,
        footer: HWND,
        bg_brush: HBRUSH,
        font_big: HFONT,
        font_title: HFONT,
        font_small: HFONT,
        rx: Receiver<GuiMsg>,
    }

    pub(super) fn run(rx: Receiver<GuiMsg>) -> Silent {
        // SAFETY: all Win32 calls below check their results; failures map to
        // Err(()) and the caller swallows it silently.
        unsafe { run_inner(rx) }
    }

    unsafe fn run_inner(rx: Receiver<GuiMsg>) -> Silent {
        // The progress-bar window class needs explicit init.
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        };
        if !InitCommonControlsEx(&icc).as_bool() {
            return Err(());
        }

        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(|_| ())?.0);

        // Dark background brush, owned by the window class lifetime.
        let bg_brush = CreateSolidBrush(BG);
        if bg_brush.is_invalid() {
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
            return Err(());
        }

        // Fixed dialog-style window, centered on the primary monitor.
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: WIN_W,
            bottom: WIN_H,
        };
        if AdjustWindowRect(&mut rc, style, false).is_err() {
            let _ = DeleteObject(bg_brush);
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
            title_label: HWND::default(),
            footer: HWND::default(),
            bg_brush,
            font_big: HFONT::default(),
            font_title: HFONT::default(),
            font_small: HFONT::default(),
            rx,
        });

        // If WM_CREATE fails, wnd_proc returns -1, creation reports Err here,
        // and WM_DESTROY reclaims the boxed state — nothing leaks, nothing
        // panics, the install just continues without a window.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("FluxRecSetupGui"),
            w!("Flux Rec Setup"),
            style,
            x,
            y,
            ww,
            hh,
            HWND::default(),
            HMENU::default(),
            hinstance,
            Some(Box::into_raw(state) as *const c_void),
        )
        .map_err(|_| ())?;

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }

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
                LRESULT(if ok { 0 } else { -1 })
            }
            WM_TIMER => {
                on_timer(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                on_paint(hwnd);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                // Dark theme: transparent statics, per-control text color,
                // shared navy brush behind.
                let ctl = HWND(lparam.0 as *mut c_void);
                let hdc = HDC(wparam.0 as *mut c_void);
                let color = state_of(hwnd)
                    .map(|st| {
                        if ctl == st.footer {
                            DIM
                        } else if ctl == st.stage_label {
                            LIGHT
                        } else {
                            WHITE
                        }
                    })
                    .unwrap_or(WHITE);
                SetTextColor(hdc, color);
                SetBkMode(hdc, TRANSPARENT);
                let brush = state_of(hwnd)
                    .map(|st| st.bg_brush)
                    .unwrap_or(HBRUSH::default());
                LRESULT(brush.0 as isize)
            }
            WM_DESTROY => {
                on_destroy(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn create_child(
        parent: HWND,
        class: PCWSTR,
        text: PCWSTR,
        style: WINDOW_STYLE,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        hi: HINSTANCE,
    ) -> Option<HWND> {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            text,
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            w,
            h,
            parent,
            HMENU::default(),
            hi,
            None,
        )
        .ok()
    }

    /// Segoe UI at `px` height; `bold` selects 700 vs 400 weight.
    /// Returns a null HFONT on failure (caller falls back to stock font).
    unsafe fn make_font(px: i32, bold: bool) -> HFONT {
        CreateFontW(
            px,
            0,
            0,
            0,
            if bold { 700 } else { 400 },
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            0,
            0,
            w!("Segoe UI"),
        )
    }

    unsafe fn set_font(ctl: HWND, font: HFONT) {
        if !font.is_invalid() {
            SendMessageW(ctl, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    unsafe fn on_create(hwnd: HWND, cs: &CREATESTRUCTW) -> bool {
        let state_ptr = cs.lpCreateParams as *mut GuiState;
        if state_ptr.is_null() {
            return false;
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
        let st = &mut *state_ptr;
        let hi = cs.hInstance;

        // Layout (client area 480x344):
        //   logo (painted)  y=16..104
        //   title           y=112 h=30
        //   stage line      y=150 h=20
        //   progress bar    y=178 h=20
        //   percent         y=206 h=34
        //   footer          y=300 h=18
        let title = create_child(
            hwnd,
            w!("STATIC"),
            w!("FLUX REC"),
            WINDOW_STYLE(SS_CENTER.0),
            20,
            112,
            440,
            30,
            hi,
        );
        let stage = create_child(
            hwnd,
            w!("STATIC"),
            w!("Starting…"),
            WINDOW_STYLE(SS_CENTER.0),
            20,
            150,
            440,
            20,
            hi,
        );
        let bar = create_child(
            hwnd,
            PROGRESS_CLASSW,
            w!(""),
            WINDOW_STYLE(PBS_SMOOTH),
            50,
            178,
            380,
            20,
            hi,
        );
        let pct = create_child(
            hwnd,
            w!("STATIC"),
            w!("0%"),
            WINDOW_STYLE(SS_CENTER.0),
            20,
            206,
            440,
            34,
            hi,
        );
        let footer = create_child(
            hwnd,
            w!("STATIC"),
            w!("Ripo Team"),
            WINDOW_STYLE(SS_CENTER.0),
            20,
            300,
            440,
            18,
            hi,
        );
        let (title, stage, bar, pct, footer) = match (title, stage, bar, pct, footer) {
            (Some(a), Some(b), Some(c), Some(d), Some(e)) => (a, b, c, d, e),
            _ => return false,
        };
        st.title_label = title;
        st.stage_label = stage;
        st.bar = bar;
        st.pct_label = pct;
        st.footer = footer;

        // Brand typography (fall back to stock font if creation fails).
        st.font_title = make_font(26, true);
        st.font_big = make_font(24, true);
        st.font_small = make_font(15, false);
        set_font(title, st.font_title);
        set_font(stage, st.font_small);
        set_font(pct, st.font_big);
        set_font(footer, st.font_small);

        // Native progress range 0..100 + Flux blue fill on dark track.
        SendMessageW(bar, PBM_SETRANGE, WPARAM(0), LPARAM(0x0064_0000));
        SendMessageW(bar, PBM_SETPOS, WPARAM(0), LPARAM(0));
        SendMessageW(bar, PBM_SETBARCOLOR, WPARAM(0), LPARAM(BLUE.0 as isize));
        SendMessageW(bar, PBM_SETBKCOLOR, WPARAM(0), LPARAM(TRACK.0 as isize));

        // ~100ms channel poll driving the bar + labels.
        if SetTimer(hwnd, TIMER_ID, TIMER_MS, None) == 0 {
            return false;
        }
        true
    }

    unsafe fn state_of(hwnd: HWND) -> Option<&'static mut GuiState> {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if ptr == 0 {
            None
        } else {
            Some(&mut *(ptr as *mut GuiState))
        }
    }

    unsafe fn set_text(hwnd: HWND, text: &str) {
        let hs = HSTRING::from(text);
        let _ = SetWindowTextW(hwnd, &hs);
    }

    unsafe fn on_timer(hwnd: HWND) {
        let st = match state_of(hwnd) {
            Some(s) => s,
            None => return,
        };
        let mut close = false;
        loop {
            match st.rx.try_recv() {
                Ok(m) => {
                    let pct = m.percent.min(100);
                    SendMessageW(st.bar, PBM_SETPOS, WPARAM(pct as usize), LPARAM(0));
                    set_text(st.pct_label, &format!("{pct}%"));
                    if !m.stage.is_empty() {
                        set_text(st.stage_label, &m.stage);
                    }
                    if m.stage.trim() == "done" {
                        close = true;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Sender gone: at 100% the install finished; otherwise the
                // installer thread is gone too — never leave a hung window.
                Err(TryRecvError::Disconnected) => {
                    close = true;
                    break;
                }
            }
        }
        if close {
            let _ = DestroyWindow(hwnd);
        }
    }

    unsafe fn on_destroy(hwnd: HWND) {
        let _ = KillTimer(hwnd, TIMER_ID);
        let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if ptr != 0 {
            let st = Box::from_raw(ptr as *mut GuiState);
            for font in [st.font_big, st.font_title, st.font_small] {
                if !font.is_invalid() {
                    let _ = DeleteObject(font);
                }
            }
            if !st.bg_brush.is_invalid() {
                let _ = DeleteObject(st.bg_brush);
            }
        }
    }

    unsafe fn on_paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if !hdc.is_invalid() {
            draw_logo(hdc);
        }
        let _ = EndPaint(hwnd, &ps);
    }

    /// Minimal BMP parser: accepts 24/32-bit BITMAPINFOHEADER file images.
    /// Returns (info-header pointer, pixel bytes, width, |height|).
    fn parse_bmp(bytes: &[u8]) -> Option<(*const BITMAPINFOHEADER, &[u8], i32, i32)> {
        if bytes.len() < 54 || &bytes[0..2] != b"BM" {
            return None;
        }
        let u32le = |r: std::ops::Range<usize>| -> Option<u32> {
            bytes.get(r)?.try_into().ok().map(u32::from_le_bytes)
        };
        let off = u32le(10..14)? as usize;
        if u32le(14..18)? != 40 {
            return None; // BITMAPINFOHEADER only
        }
        let w = u32le(18..22)? as i32;
        let h = u32le(22..26)? as i32;
        let bpp = bytes.get(28..30)?.try_into().ok().map(u16::from_le_bytes)?;
        if w <= 0 || h == 0 || (bpp != 24 && bpp != 32) {
            return None;
        }
        if off > bytes.len() {
            return None;
        }
        let info = bytes[14..].as_ptr() as *const BITMAPINFOHEADER;
        Some((info, &bytes[off..], w, h.abs()))
    }

    unsafe fn draw_logo(hdc: HDC) {
        let (info, bits, w, h) = match parse_bmp(crate::assets::LOGO_BMP_BYTES) {
            Some(v) => v,
            None => return, // no/invalid logo: paint nothing, stay silent
        };
        // Fit into a 120x88 box, centered horizontally near the top.
        let scale = (120.0 / w as f64).min(88.0 / h as f64);
        let (dw, dh) = ((w as f64 * scale) as i32, (h as f64 * scale) as i32);
        if dw <= 0 || dh <= 0 {
            return;
        }
        let (dx, dy) = ((WIN_W - dw) / 2, 16);
        StretchDIBits(
            hdc,
            dx,
            dy,
            dw,
            dh,
            0,
            0,
            w,
            h,
            Some(bits.as_ptr() as *const c_void),
            info as *const BITMAPINFO,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}
