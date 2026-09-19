// Small native progress window shown while the game downloads/updates.
// Runs its message loop on a dedicated OS thread; the rest of the program
// talks to it through a channel, so it's safe to poke from async tasks.
// On non-Windows this degrades to throttled console output (used for tests).

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
type HWND = *mut c_void;
#[cfg(windows)]
type HINSTANCE = *mut c_void;
#[cfg(windows)]
type LPCWSTR = *const u16;

#[cfg(windows)]
#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: unsafe extern "system" fn(HWND, u32, usize, isize) -> isize,
    cls_extra: i32,
    wnd_extra: i32,
    instance: HINSTANCE,
    icon: *mut c_void,
    cursor: *mut c_void,
    background: *mut c_void,
    menu_name: LPCWSTR,
    class_name: LPCWSTR,
}

#[cfg(windows)]
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[cfg(windows)]
#[repr(C)]
struct Msg {
    hwnd: HWND,
    message: u32,
    w_param: usize,
    l_param: isize,
    time: u32,
    pt: Point,
    l_private: u32,
}

#[cfg(windows)]
#[repr(C)]
struct InitCommonControlsEx {
    size: u32,
    icc: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: LPCWSTR) -> HINSTANCE;
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(cls: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: LPCWSTR,
        window_name: LPCWSTR,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: *mut c_void,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: HWND) -> i32;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: HWND, filter_min: u32, filter_max: u32) -> i32;
    fn PeekMessageW(
        msg: *mut Msg,
        hwnd: HWND,
        filter_min: u32,
        filter_max: u32,
        remove: u32,
    ) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize;
    fn SetWindowTextW(hwnd: HWND, text: LPCWSTR) -> i32;
    fn SendMessageW(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize;
    fn GetSystemMetrics(index: i32) -> i32;
}

#[cfg(windows)]
#[link(name = "comctl32")]
extern "system" {
    fn InitCommonControlsEx(icce: *const InitCommonControlsEx) -> i32;
}

#[cfg(windows)]
const WM_DESTROY: u32 = 0x0002;
#[cfg(windows)]
const WS_POPUP: u32 = 0x8000_0000;
#[cfg(windows)]
const WS_EX_TOPMOST: u32 = 0x0000_0008;
#[cfg(windows)]
const WS_CAPTION: u32 = 0x00C0_0000;
#[cfg(windows)]
const WS_SYSMENU: u32 = 0x0008_0000;
#[cfg(windows)]
const WS_CHILD: u32 = 0x4000_0000;
#[cfg(windows)]
const WS_VISIBLE: u32 = 0x1000_0000;
#[cfg(windows)]
const PM_REMOVE: u32 = 0x0001;
#[cfg(windows)]
const SM_CXSCREEN: i32 = 0;
#[cfg(windows)]
const SM_CYSCREEN: i32 = 1;
#[cfg(windows)]
const ICC_PROGRESS_CLASS: u32 = 0x0000_0020;
#[cfg(windows)]
const PBM_SETRANGE32: u32 = 0x0406;
#[cfg(windows)]
const PBM_SETPOS: u32 = 0x0402;

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize {
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        0
    } else {
        DefWindowProcW(hwnd, msg, w, l)
    }
}

#[cfg(windows)]
enum Cmd {
    Progress { permille: u32, label: String },
    Close,
}

pub struct ProgressWindow {
    #[cfg(windows)]
    tx: Option<std::sync::mpsc::Sender<Cmd>>,
    #[cfg(not(windows))]
    last_permille: std::sync::Mutex<u32>,
    #[cfg(not(windows))]
    title: String,
}

impl ProgressWindow {
    pub fn new(title: &str) -> Self {
        #[cfg(windows)]
        {
            Self { tx: Self::spawn(title) }
        }
        #[cfg(not(windows))]
        {
            eprintln!("[{title}] starting...");
            Self {
                last_permille: std::sync::Mutex::new(0),
                title: title.to_string(),
            }
        }
    }

    /// fraction: 0.0..=1.0
    pub fn set(&self, fraction: f64, label: &str) {
        let permille = (fraction.clamp(0.0, 1.0) * 1000.0) as u32;
        #[cfg(windows)]
        {
            if let Some(tx) = &self.tx {
                let _ = tx.send(Cmd::Progress {
                    permille,
                    label: label.to_string(),
                });
            }
        }
        #[cfg(not(windows))]
        {
            let mut last = self.last_permille.lock().unwrap();
            // Throttle console spam: print every 2%.
            if permille >= *last + 20 || (permille == 1000 && *last != 1000) {
                *last = permille;
                eprintln!(
                    "[{}] {:3}% — {label}",
                    self.title,
                    permille / 10,
                );
            }
        }
    }
}

#[cfg(windows)]
impl ProgressWindow {
    fn spawn(title: &str) -> Option<std::sync::mpsc::Sender<Cmd>> {
        let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
        let title = title.to_string();
        std::thread::spawn(move || unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let icce = InitCommonControlsEx {
                size: 8,
                icc: ICC_PROGRESS_CLASS,
            };
            InitCommonControlsEx(&icce);

            let class_name = wide("FluxRecProgress");
            let wnd_class = WndClassW {
                style: 0,
                wnd_proc,
                cls_extra: 0,
                wnd_extra: 0,
                instance: hinstance,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class_name.as_ptr(),
            };
            if RegisterClassW(&wnd_class) == 0 {
                return;
            }

            let w: i32 = 440;
            let h: i32 = 150;
            let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
            let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 2;
            let title_w = wide(&title);
            // Topmost: the installer window is centered on screen too, and the
            // progress popup would otherwise open directly behind it,
            // leaving the user with no visible progress at all.
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST,
                class_name.as_ptr(),
                title_w.as_ptr(),
                WS_POPUP | WS_CAPTION | WS_SYSMENU,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                return;
            }

            let static_class = wide("STATIC");
            let label_hwnd = CreateWindowExW(
                0,
                static_class.as_ptr(),
                wide("Starting…").as_ptr(),
                WS_CHILD | WS_VISIBLE,
                16,
                16,
                408,
                44,
                hwnd,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            let prog_class = wide("msctls_progress32");
            let bar_hwnd = CreateWindowExW(
                0,
                prog_class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE,
                16,
                72,
                408,
                28,
                hwnd,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            if label_hwnd.is_null() || bar_hwnd.is_null() {
                DestroyWindow(hwnd);
                return;
            }

            ShowWindow(hwnd, 1);
            UpdateWindow(hwnd);

            // Pump messages and apply progress commands.
            'outer: loop {
                let mut msg = std::mem::MaybeUninit::<Msg>::uninit();
                while PeekMessageW(
                    msg.as_mut_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    PM_REMOVE,
                ) != 0
                {
                    let m = msg.assume_init_ref();
                    TranslateMessage(m);
                    DispatchMessageW(m);
                }
                match rx.recv_timeout(std::time::Duration::from_millis(30)) {
                    Ok(Cmd::Progress { permille, label }) => {
                        let lw = wide(&label);
                        SetWindowTextW(label_hwnd, lw.as_ptr());
                        SendMessageW(bar_hwnd, PBM_SETRANGE32, 0, 1000);
                        SendMessageW(bar_hwnd, PBM_SETPOS, permille as usize, 0);
                    }
                    Ok(Cmd::Close) => break 'outer,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 'outer,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            DestroyWindow(hwnd);
            // Drain the queue until WM_QUIT so the thread exits cleanly.
            let mut msg = std::mem::MaybeUninit::<Msg>::uninit();
            while GetMessageW(msg.as_mut_ptr(), std::ptr::null_mut(), 0, 0) > 0 {
                let m = msg.assume_init_ref();
                TranslateMessage(m);
                DispatchMessageW(m);
            }
        });
        Some(tx)
    }
}

impl Drop for ProgressWindow {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(tx) = self.tx.take() {
                let _ = tx.send(Cmd::Close);
            }
        }
    }
}
