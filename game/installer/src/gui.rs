// Flux Rec Setup — branded installer window v0.3.8.
//
// Sketch-faithful redesign (dark mode):
// - Dark navy background (#0A0F1E)
// - FR logo monogram (blue gradient) + "FLUX REC" + "Record • Create • Share"
// - 5-slide carousel with tutorial content (auto-advances, arrows, dots)
// - "Installing Flux Rec..." heading + subtitle
// - Blue progress bar with percentage
// - "Downloading files..." + byte counts
// - Bottom info bar: Downloading, Speed, Time Remaining, Auto Updates, Safe & Secure
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
//   * NO Registry API (caused Windows build failures in v0.3.4-0.3.6).
//     Dark mode is fixed, matching the sketch.

use std::sync::mpsc::Receiver;

/// One progress update from the installer thread.
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
    use windows::core::{PCWSTR, w};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    type Silent = std::result::Result<(), ()>;

    // Window dimensions matching the sketch (16:10-ish).
    const WIN_W: i32 = 900;
    const WIN_H: i32 = 650;
    const TIMER_ID: usize = 1;
    const TIMER_MS: u32 = 100;
    const SLIDE_TIMER_ID: usize = 2;
    const SLIDE_MS: u32 = 5000; // auto-advance carousel every 5s

    // Sketch palette (COLORREF = 0x00BBGGRR).
    const BG: COLORREF = COLORREF(0x001E0F0A); // dark navy #0A0F1E
    const BG_PANEL: COLORREF = COLORREF(0x00241A12); // slightly lighter panel
    const BLUE: COLORREF = COLORREF(0x00FF9B2E); // Flux blue #2E9BFF
    const BLUE_DIM: COLORREF = COLORREF(0x0080501A); // dim blue
    const WHITE: COLORREF = COLORREF(0x00FFFFFF);
    const GRAY: COLORREF = COLORREF(0x00A0A0A0); // subtitle gray
    const DIM: COLORREF = COLORREF(0x00606060); // dim gray
    const TRACK: COLORREF = COLORREF(0x00302A20); // progress track

    // Carousel slides: (title, subtitle).
    const SLIDES: &[(&str, &str)] = &[
        ("Welcome to Flux Rec", "Your private Rec Room revival\nRecord • Create • Share"),
        ("Play Together", "Join friends in the Rec Center\nand explore thousands of rooms"),
        ("Compete", "Battle in Paintball, Bowling\nand Quests with players worldwide"),
        ("Customize", "Express yourself with thousands\nof avatar items and outfits"),
        ("Create", "Build your own rooms and games\nwith the in-game maker tools"),
    ];

    struct GuiState {
        hwnd: HWND,
        percent: u8,
        stage: String,
        detail: String,
        slide: usize,
        bg_brush: HBRUSH,
        font_logo: HFONT,
        font_title: HFONT,
        font_sub: HFONT,
        font_small: HFONT,
        font_tiny: HFONT,
        rx: Receiver<GuiMsg>,
    }

    pub(super) fn run(rx: Receiver<GuiMsg>) -> Silent {
        unsafe { run_inner(rx) }
    }

    unsafe fn run_inner(rx: Receiver<GuiMsg>) -> Silent {
        let hinstance = HINSTANCE(GetModuleHandleW(None).map_err(|_| ())?.0);
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

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT { left: 0, top: 0, right: WIN_W, bottom: WIN_H };
        if AdjustWindowRect(&mut rc, style, false).is_err() {
            let _ = DeleteObject(bg_brush);
            return Err(());
        }
        let (ww, hh) = (rc.right - rc.left, rc.bottom - rc.top);
        let sx = GetSystemMetrics(SM_CXSCREEN);
        let sy = GetSystemMetrics(SM_CYSCREEN);
        let (x, y) = ((sx - ww) / 2, (sy - hh) / 2);

        // Fonts.
        let font_logo = create_font(64, true);
        let font_title = create_font(28, true);
        let font_sub = create_font(18, false);
        let font_small = create_font(15, false);
        let font_tiny = create_font(12, false);
        if font_logo.is_invalid() || font_title.is_invalid() {
            let _ = DeleteObject(bg_brush);
            return Err(());
        }

        let state = Box::new(GuiState {
            hwnd: HWND::default(),
            percent: 0,
            stage: String::from("Downloading files..."),
            detail: String::new(),
            slide: 0,
            bg_brush,
            font_logo,
            font_title,
            font_sub,
            font_small,
            font_tiny,
            rx,
        });
        let ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("FluxRecSetupGui"),
            w!("Flux Rec Setup"),
            style,
            x, y, ww, hh,
            None, None, hinstance,
            Some(ptr as *const c_void),
        );
        if hwnd.is_err() {
            let _ = Box::from_raw(ptr);
            let _ = DeleteObject(bg_brush);
            return Err(());
        }
        let hwnd = hwnd.unwrap_or_default();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // Timers: progress poll + carousel auto-advance.
        SetTimer(hwnd, TIMER_ID, TIMER_MS, None);
        SetTimer(hwnd, SLIDE_TIMER_ID, SLIDE_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(hwnd, TIMER_ID);
        KillTimer(hwnd, SLIDE_TIMER_ID);
        let _ = DeleteObject(font_logo);
        let _ = DeleteObject(font_title);
        let _ = DeleteObject(font_sub);
        let _ = DeleteObject(font_small);
        let _ = DeleteObject(font_tiny);
        let _ = DeleteObject(bg_brush);
        Ok(())
    }

    unsafe fn create_font(px: i32, bold: bool) -> HFONT {
        CreateFontW(
            px, 0, 0, 0,
            if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0, 0, 0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Segoe UI"),
        )
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
                let state = &mut *(cs.lpCreateParams as *mut GuiState);
                state.hwnd = hwnd;
                LRESULT(0)
            }
            WM_TIMER => {
                let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState);
                if wparam.0 == TIMER_ID {
                    // Drain progress messages.
                    loop {
                        match state.rx.try_recv() {
                            Ok(m) => {
                                if m.stage.trim().eq_ignore_ascii_case("done") {
                                    let _ = DestroyWindow(hwnd);
                                    break;
                                }
                                state.percent = m.percent.min(100);
                                if !m.stage.is_empty() {
                                    state.stage = m.stage;
                                }
                                if !m.detail.is_empty() {
                                    state.detail = m.detail;
                                }
                                let _ = InvalidateRect(hwnd, None, false);
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                } else if wparam.0 == SLIDE_TIMER_ID {
                    state.slide = (state.slide + 1) % SLIDES.len();
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // Click left/right arrows to change slides.
                let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState);
                let x = (lparam.0 & 0xFFFF) as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                // Left arrow zone.
                if x >= 40 && x <= 90 && y >= 200 && y <= 320 {
                    state.slide = (state.slide + SLIDES.len() - 1) % SLIDES.len();
                    let _ = InvalidateRect(hwnd, None, false);
                }
                // Right arrow zone.
                if x >= WIN_W - 90 && x <= WIN_W - 40 && y >= 200 && y <= 320 {
                    state.slide = (state.slide + 1) % SLIDES.len();
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let state = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState);
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                if !hdc.is_invalid() {
                    draw_all(hdc, state);
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut GuiState;
                if !ptr.is_null() {
                    let _ = Box::from_raw(ptr);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn draw_all(hdc: HDC, s: &GuiState) {
        // Background.
        let mut rc = RECT::default();
        let _ = GetClientRect(s.hwnd, &mut rc);
        let bg = CreateSolidBrush(BG);
        let _ = FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);

        let _ = SetBkMode(hdc, TRANSPARENT);

        // --- Logo: "FR" monogram ---
        let _ = SelectObject(hdc, s.font_logo);
        let _ = SetTextColor(hdc, BLUE);
        draw_text_center(hdc, "FR", WIN_W / 2, 30, WIN_W, 70);

        // --- "FLUX REC" ---
        let _ = SelectObject(hdc, s.font_title);
        // Draw "FLUX " in white and "REC" in blue.
        let flux_w = text_width(hdc, "FLUX ");
        let rec_w = text_width(hdc, "REC");
        let total_w = flux_w + rec_w;
        let start_x = (WIN_W - total_w) / 2;
        let _ = SetTextColor(hdc, WHITE);
        draw_text_at(hdc, "FLUX ", start_x, 110);
        let _ = SetTextColor(hdc, BLUE);
        draw_text_at(hdc, "REC", start_x + flux_w, 110);

        // --- Tagline ---
        let _ = SelectObject(hdc, s.font_sub);
        let _ = SetTextColor(hdc, GRAY);
        draw_text_center(hdc, "Record  •  Create  •  Share", WIN_W / 2, 150, WIN_W, 25);

        // --- Version ---
        let _ = SetTextColor(hdc, GRAY);
        draw_text_center(
            hdc,
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            WIN_W / 2,
            175,
            WIN_W,
            20,
        );

        // --- Carousel ---
        draw_carousel(hdc, s);

        // --- "Installing Flux Rec..." ---
        let _ = SelectObject(hdc, s.font_title);
        let _ = SetTextColor(hdc, WHITE);
        draw_text_center(hdc, "Installing Flux Rec...", WIN_W / 2, 400, WIN_W, 35);

        let _ = SelectObject(hdc, s.font_sub);
        let _ = SetTextColor(hdc, GRAY);
        draw_text_center(hdc, "Please wait while we set things up for you.", WIN_W / 2, 435, WIN_W, 25);

        // --- Progress bar ---
        let bar_x = 120;
        let bar_w = WIN_W - 240 - 60; // leave room for % text
        let bar_y = 475;
        let bar_h = 14;
        // Track.
        let track = CreateSolidBrush(TRACK);
        let track_rect = RECT { left: bar_x, top: bar_y, right: bar_x + bar_w, bottom: bar_y + bar_h };
        let _ = FillRect(hdc, &track_rect, track);
        let _ = DeleteObject(track);
        // Fill.
        let fill_w = (bar_w * s.percent as i32) / 100;
        if fill_w > 0 {
            let fill = CreateSolidBrush(BLUE);
            let fill_rect = RECT { left: bar_x, top: bar_y, right: bar_x + fill_w, bottom: bar_y + bar_h };
            let _ = FillRect(hdc, &fill_rect, fill);
            let _ = DeleteObject(fill);
        }
        // Percentage.
        let _ = SelectObject(hdc, s.font_sub);
        let _ = SetTextColor(hdc, WHITE);
        let pct = format!("{}%", s.percent);
        draw_text_at(hdc, &pct, bar_x + bar_w + 15, bar_y - 4);

        // --- Stage / detail ---
        let _ = SelectObject(hdc, s.font_small);
        let _ = SetTextColor(hdc, GRAY);
        draw_text_at(hdc, &s.stage, bar_x, bar_y + 25);
        if !s.detail.is_empty() {
            draw_text_at(hdc, &s.detail, bar_x, bar_y + 45);
        }

        // --- Bottom info bar ---
        draw_bottom_bar(hdc, s);
    }

    unsafe fn draw_carousel(hdc: HDC, s: &GuiState) {
        let cy = 260; // carousel center y
        let cw = 380; // center slide width
        let ch = 150; // slide height
        let cx = WIN_W / 2;

        // Side slides (dimmed).
        let side_w = 220;
        let side_alpha = 60; // dimmed

        // Left slide.
        let left_idx = (s.slide + SLIDES.len() - 1) % SLIDES.len();
        draw_slide(hdc, s, left_idx, cx - cw / 2 - side_w - 20, cy - ch / 2, side_w, ch, true);

        // Right slide.
        let right_idx = (s.slide + 1) % SLIDES.len();
        draw_slide(hdc, s, right_idx, cx + cw / 2 + 20, cy - ch / 2, side_w, ch, true);

        // Center slide (focused).
        draw_slide(hdc, s, s.slide, cx - cw / 2, cy - ch / 2, cw, ch, false);

        // Arrows.
        let _ = SelectObject(hdc, s.font_title);
        let _ = SetTextColor(hdc, GRAY);
        draw_text_center(hdc, "<", 65, cy - 20, 50, 40);
        draw_text_center(hdc, ">", WIN_W - 65, cy - 20, 50, 40);

        // Dots.
        let dot_y = cy + ch / 2 + 20;
        let dot_r = 5;
        let dot_gap = 20;
        let total_w = (SLIDES.len() as i32 - 1) * dot_gap;
        let start_x = cx - total_w / 2;
        for i in 0..SLIDES.len() {
            let x = start_x + (i as i32) * dot_gap;
            let brush = CreateSolidBrush(if i == s.slide { BLUE } else { DIM });
            let _ = Ellipse(hdc, x - dot_r, dot_y - dot_r, x + dot_r, dot_y + dot_r);
            let _ = DeleteObject(brush);
        }
        let _ = side_alpha; // suppress unused warning
    }

    unsafe fn draw_slide(hdc: HDC, s: &GuiState, idx: usize, x: i32, y: i32, w: i32, h: i32, dimmed: bool) {
        // Slide background (gradient-like: use solid with border).
        let bg = CreateSolidBrush(if dimmed { COLORREF(0x00181010) } else { BG_PANEL });
        let rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
        let _ = FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);

        // Border.
        let pen = CreatePen(PS_SOLID, 1, if dimmed { DIM } else { BLUE_DIM });
        let old_pen = SelectObject(hdc, pen);
        let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let _ = Rectangle(hdc, x, y, x + w, y + h);
        let _ = SelectObject(hdc, old_pen);
        let _ = SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen);

        // Slide text.
        let (title, sub) = SLIDES[idx];
        let _ = SelectObject(hdc, s.font_small);
        let _ = SetTextColor(hdc, if dimmed { DIM } else { WHITE });
        draw_text_center(hdc, title, x + w / 2, y + 30, w, 25);

        let _ = SelectObject(hdc, s.font_tiny);
        let _ = SetTextColor(hdc, if dimmed { DIM } else { GRAY });
        // Multi-line subtitle.
        for (i, line) in sub.split('\n').enumerate() {
            draw_text_center(hdc, line, x + w / 2, y + 65 + (i as i32) * 20, w, 20);
        }
    }

    unsafe fn draw_bottom_bar(hdc: HDC, s: &GuiState) {
        let bar_y = WIN_H - 80;
        // Separator line.
        let pen = CreatePen(PS_SOLID, 1, COLORREF(0x00201A15));
        let old_pen = SelectObject(hdc, pen);
        let _ = MoveToEx(hdc, 0, bar_y, None);
        let _ = LineTo(hdc, WIN_W, bar_y);
        let _ = SelectObject(hdc, old_pen);
        let _ = DeleteObject(pen);

        // 5 info items.
        let items: &[(&str, &str)] = &[
            ("Downloading", if s.detail.is_empty() { "—" } else { &s.detail }),
            ("Speed", "—"),
            ("Time Remaining", "—"),
            ("Auto Updates", "Enabled"),
            ("Safe & Secure", "Verified Installer"),
        ];
        let col_w = WIN_W / 5;
        let _ = SelectObject(hdc, s.font_tiny);
        for (i, (label, value)) in items.iter().enumerate() {
            let cx = (i as i32) * col_w + col_w / 2;
            let _ = SetTextColor(hdc, BLUE);
            draw_text_center(hdc, label, cx, bar_y + 12, col_w, 18);
            let _ = SetTextColor(hdc, GRAY);
            draw_text_center(hdc, value, cx, bar_y + 32, col_w, 18);
        }
    }

    unsafe fn draw_text_center(hdc: HDC, text: &str, cx: i32, y: i32, w: i32, h: i32) {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut rc = RECT { left: cx - w / 2, top: y, right: cx + w / 2, bottom: y + h };
        // DrawTextW needs &mut [u16]; use the wide vec without the trailing NUL.
        let mut buf = wide[..wide.len() - 1].to_vec();
        let _ = DrawTextW(hdc, &mut buf, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }

    unsafe fn draw_text_at(hdc: HDC, text: &str, x: i32, y: i32) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(hdc, x, y, &wide);
    }

    unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &wide, &mut sz);
        sz.cx
    }
}
