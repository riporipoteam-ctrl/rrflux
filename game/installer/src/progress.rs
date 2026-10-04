//! Progress reporting for the Flux Rec installer GUI.
//!
//! The install code calls [`Progress`] methods as stages advance; this
//! module translates them into [`gui::GuiMsg`] values on an mpsc channel.
//! Sending never blocks and never panics: if the GUI is gone, updates are
//! silently dropped and the install continues.
//!
//! Pure Rust — compiles on every platform, no `cfg` needed.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

/// (stage text, percent range start, range width). Ranges must be
/// non-overlapping and ascending; the last one ends at 100.
const STAGES: &[(&str, f64, f64)] = &[
    ("Preparing\u{2026}", 0.0, 2.0),
    ("Downloading game files\u{2026}", 2.0, 40.0),
    ("Extracting game files\u{2026}", 42.0, 10.0),
    ("Applying Steam bypass\u{2026}", 52.0, 3.0),
    ("Installing BepInEx\u{2026}", 55.0, 8.0),
    ("Applying Flux Rec branding\u{2026}", 63.0, 15.0),
    ("Writing configuration\u{2026}", 78.0, 5.0),
    ("Creating shortcuts\u{2026}", 83.0, 4.0),
    ("Final checks\u{2026}", 87.0, 8.0),
    ("Committing installation\u{2026}", 95.0, 5.0),
];

struct Inner {
    tx: mpsc::Sender<crate::gui::GuiMsg>,
    /// (range base, range width) for the current stage.
    range: Mutex<(f64, f64)>,
    /// Last (percent, stage) actually sent — identical updates are dropped.
    last: Mutex<(u8, String)>,
    /// Last detail line sent — identical updates are dropped.
    last_detail: Mutex<String>,
    /// Rolling throughput tracker for the detail line.
    throughput: Mutex<Throughput>,
}

/// Cloneable progress handle. All methods are infallible and non-blocking.
#[derive(Clone)]
pub struct Progress {
    inner: Arc<Inner>,
}

/// Create a (sender handle, receiver) pair. Hand the receiver to
/// `gui::run_gui` on its own thread.
pub fn channel() -> (Progress, mpsc::Receiver<crate::gui::GuiMsg>) {
    let (tx, rx) = mpsc::channel();
    let p = Progress {
        inner: Arc::new(Inner {
            tx,
            range: Mutex::new((0.0, 0.0)),
            last: Mutex::new((0, String::new())),
            last_detail: Mutex::new(String::new()),
            throughput: Mutex::new(Throughput::new()),
        }),
    };
    (p, rx)
}

/// Rolling byte counter → "done / total MB • ETA M:SS" strings.
/// NOTE: No speed — user explicitly removed the speed indicator from the UI.
struct Throughput {
    started: Instant,
    last_tick: Instant,
    last_bytes: u64,
    window_bytes: u64,
}

impl Throughput {
    fn new() -> Self {
        let now = Instant::now();
        Throughput {
            started: now,
            last_tick: now,
            last_bytes: 0,
            window_bytes: 0,
        }
    }

    fn reset(&mut self) {
        *self = Throughput::new();
    }

    /// Feed the absolute byte count; returns a formatted detail string.
    /// `label` prefixes the line (e.g. "Downloading").
    fn update(&mut self, label: &str, done: u64, total: u64) -> String {
        let now = Instant::now();
        if done < self.last_bytes {
            // Counter went backwards (new phase) — restart the window.
            self.last_bytes = done;
            self.last_tick = now;
            self.window_bytes = 0;
        }
        self.window_bytes += done.saturating_sub(self.last_bytes);
        self.last_bytes = done;

        let tick_dt = now.duration_since(self.last_tick).as_secs_f64();
        let mbps = if tick_dt >= 0.5 {
            let v = self.window_bytes as f64 / tick_dt / 1_048_576.0;
            self.window_bytes = 0;
            self.last_tick = now;
            v
        } else {
            // Not enough time for a fresh sample — extrapolate from average.
            let elapsed = now.duration_since(self.started).as_secs_f64().max(0.001);
            done as f64 / elapsed / 1_048_576.0
        };

        let have_mb = done as f64 / 1_048_576.0;
        let total_mb = total as f64 / 1_048_576.0;
        let eta = if mbps > 0.05 && total > done {
            let s = ((total - done) as f64 / (mbps * 1_048_576.0)) as u64;
            format!(" \u{2022} ETA {}", fmt_eta(s))
        } else {
            String::new()
        };
        format!(
            "{label}: {have_mb:.0} / {total_mb:.0} MB{eta}"
        )
    }
}

fn fmt_eta(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Progress {
    fn send(&self, percent: u8, stage: &str, detail: &str) {
        let pct = percent.min(100);
        let detail_owned = detail.to_string();
        let stage_changed = {
            let mut last = self.inner.last.lock().unwrap_or_else(|e| e.into_inner());
            if *last == (pct, stage.to_string()) {
                false
            } else {
                *last = (pct, stage.to_string());
                true
            }
        };
        let detail_changed = {
            let mut last = self
                .inner
                .last_detail
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == detail_owned {
                false
            } else {
                *last = detail_owned.clone();
                true
            }
        };
        if !stage_changed && !detail_changed {
            return;
        }
        let _ = self.inner.tx.send(crate::gui::GuiMsg {
            percent: pct,
            stage: stage.to_string(),
            detail: detail_owned,
        });
    }

    fn current_detail(&self) -> String {
        self.inner
            .last_detail
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }

    /// Begin a named stage: looks up its percent range and jumps the bar to
    /// the range start. Unknown names keep the previous range and just
    /// update the text. Clears the detail line and resets throughput.
    pub fn set_stage(&self, stage: &'static str) {
        if let Ok(mut t) = self.inner.throughput.lock() {
            t.reset();
        }
        if let Ok(mut d) = self.inner.last_detail.lock() {
            d.clear();
        }
        match STAGES.iter().find(|(s, _, _)| *s == stage) {
            Some(&(_, base, _width)) => {
                if let Ok(mut r) = self.inner.range.lock() {
                    *r = (base, _width);
                }
                let pct = base as u8;
                // Force-send: stage text changed even if percent didn't.
                let _ = self.inner.tx.send(crate::gui::GuiMsg {
                    percent: pct.min(100),
                    stage: stage.to_string(),
                    detail: String::new(),
                });
                if let Ok(mut last) = self.inner.last.lock() {
                    *last = (pct.min(100), stage.to_string());
                }
            }
            None => {
                let pct = self.inner.last.lock().map(|l| l.0).unwrap_or(0);
                self.send(pct, stage, "");
            }
        }
    }

    /// Progress within the current stage: 0.0..=1.0 maps onto the stage's
    /// percent range. Identical percents are dropped automatically.
    pub fn set_fraction(&self, frac: f64) {
        let (base, width) = self.inner.range.lock().map(|r| *r).unwrap_or((0.0, 0.0));
        let f = frac.clamp(0.0, 1.0);
        let detail = self.current_detail();
        self.send((base + f * width) as u8, "", &detail);
    }

    /// Absolute percent with a status line, outside the stage table.
    /// Used by the launcher (update check / launching game).
    pub fn set_status(&self, stage: &str, percent: u8) {
        let detail = self.current_detail();
        self.send(percent, stage, &detail);
    }

    /// Set the secondary detail line (speed / ETA / byte counts) without
    /// touching the stage text or the bar position.
    pub fn set_detail(&self, detail: String) {
        let pct = self.inner.last.lock().map(|l| l.0).unwrap_or(0);
        let stage = self
            .inner
            .last
            .lock()
            .map(|l| l.1.clone())
            .unwrap_or_default();
        self.send(pct, &stage, &detail);
    }

    /// Feed absolute byte counts for the current transfer-like stage.
    /// Formats and shows the "done / total MB • ETA" detail line
    /// (no speed — user explicitly removed the speed indicator).
    /// and advances the bar proportionally.
    pub fn set_throughput(&self, label: &str, done: u64, total: u64) {
        let detail = self
            .inner
            .throughput
            .lock()
            .map(|mut t| t.update(label, done, total))
            .unwrap_or_default();
        if total > 0 {
            self.set_fraction(done as f64 / total as f64);
        }
        if !detail.is_empty() {
            self.set_detail(detail);
        }
    }

    /// Install finished: tell the GUI to close its window.
    pub fn done(&self) {
        // "done" is the GUI's close sentinel; the (100, "done") pair is
        // distinct from any real update, so it always goes through.
        let _ = self.inner.tx.send(crate::gui::GuiMsg {
            percent: 100,
            stage: "done".to_string(),
            detail: String::new(),
        });
    }
}
