// Keeps Windows from sleeping while downloads/updates run.
// On other platforms this is a no-op.

#[cfg(windows)]
pub struct PreventSleep;

#[cfg(windows)]
impl PreventSleep {
    pub fn new() -> Self {
        unsafe {
            // ES_CONTINUOUS | ES_SYSTEM_REQUIRED
            SetThreadExecutionState(0x8000_0000 | 0x0000_0001);
        }
        Self
    }
}

#[cfg(windows)]
impl Drop for PreventSleep {
    fn drop(&mut self) {
        unsafe {
            SetThreadExecutionState(0x8000_0000);
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetThreadExecutionState(flags: u32) -> u32;
}

#[cfg(not(windows))]
pub struct PreventSleep;

#[cfg(not(windows))]
impl PreventSleep {
    pub fn new() -> Self {
        Self
    }
}
