//! Progress reporting for the Flux Rec installer GUI.
//!
//! The install code calls [`Progress`] methods as stages advance; this
//! module translates them into [`gui::GuiMsg`] values on an mpsc channel.
//! Sending never blocks and never panics: if the GUI is gone, updates are
//! silently dropped and the install continues.
//!
//! Pure Rust — compiles on every platform, no `cfg` needed.

use std::sync::{mpsc, Arc, Mutex};

/// (stage text, percent range start, range width). Ranges must be
/// non-overlapping and ascending; the last one ends at 100.
const STAGES: &[(&str, f64, f64)] = &[
    ("Preparing\u{2026}", 0.0, 2.0),
    ("Downloading game files\u{2026}", 2.0, 50.0),
    ("Extracting game files\u{2026}", 52.0, 14.0),
    ("Applying Steam bypass\u{2026}", 66.0, 2.0),
    ("Applying Flux Rec branding\u{2026}", 68.0, 8.0),
    ("Installing BepInEx\u{2026}", 76.0, 8.0),
    ("Installing Flux Rec plugin\u{2026}", 84.0, 5.0),
    ("Writing configuration\u{2026}", 89.0, 5.0),
    ("Creating shortcuts\u{2026}", 94.0, 3.0),
    ("Final checks\u{2026}", 97.0, 3.0),
];

struct Inner {
    tx: mpsc::Sender<crate::gui::GuiMsg>,
    /// (range base, range width) for the current stage.
    range: Mutex<(f64, f64)>,
    /// Last (percent, stage) actually sent — identical updates are dropped.
    last: Mutex<(u8, String)>,
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
        }),
    };
    (p, rx)
}

impl Progress {
    fn send(&self, percent: u8, stage: &str) {
        let pct = percent.min(100);
        {
            let mut last = self.inner.last.lock().unwrap_or_else(|e| e.into_inner());
            if *last == (pct, stage.to_string()) {
                return;
            }
            *last = (pct, stage.to_string());
        }
        let _ = self.inner.tx.send(crate::gui::GuiMsg {
            percent: pct,
            stage: stage.to_string(),
        });
    }

    /// Begin a named stage: looks up its percent range and jumps the bar to
    /// the range start. Unknown names keep the previous range and just
    /// update the text.
    pub fn set_stage(&self, stage: &'static str) {
        match STAGES.iter().find(|(s, _, _)| *s == stage) {
            Some(&(_, base, width)) => {
                if let Ok(mut r) = self.inner.range.lock() {
                    *r = (base, width);
                }
                self.send(base as u8, stage);
            }
            None => {
                let pct = self.inner.last.lock().map(|l| l.0).unwrap_or(0);
                self.send(pct, stage);
            }
        }
    }

    /// Progress within the current stage: 0.0..=1.0 maps onto the stage's
    /// percent range. Identical percents are dropped automatically.
    pub fn set_fraction(&self, frac: f64) {
        let (base, width) = self.inner.range.lock().map(|r| *r).unwrap_or((0.0, 0.0));
        let f = frac.clamp(0.0, 1.0);
        self.send((base + f * width) as u8, "");
    }

    /// Absolute percent with a status line, outside the stage table.
    /// Used by the launcher (update check / launching game).
    pub fn set_status(&self, stage: &str, percent: u8) {
        self.send(percent, stage);
    }

    /// Replace the status-line text without moving the bar.
    /// Used for live download speed / ETA readouts.
    pub fn set_text(&self, text: String) {
        let pct = self.inner.last.lock().map(|l| l.0).unwrap_or(0);
        self.send(pct, &text);
    }

    /// Install finished: tell the GUI to close its window.
    pub fn done(&self) {
        // "done" is the GUI's close sentinel; the (100, "done") pair is
        // distinct from any real update, so it always goes through.
        let _ = self.inner.tx.send(crate::gui::GuiMsg {
            percent: 100,
            stage: "done".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_never_blocks_without_gui() {
        // No receiver: sends are dropped, nothing panics.
        let (p, rx) = channel();
        drop(rx);
        p.set_stage("Downloading game files\u{2026}");
        p.set_fraction(0.5);
        p.set_status("Checking for updates\u{2026}", 5);
        p.done();
    }

    #[test]
    fn stage_ranges_are_sane() {
        for w in STAGES.windows(2) {
            let (_, b1, w1) = w[0];
            let (_, b2, _) = w[1];
            assert!(b1 + w1 <= b2, "overlapping stage ranges");
        }
        let (_, last_base, last_w) = STAGES[STAGES.len() - 1];
        assert!((last_base + last_w - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn messages_flow_to_receiver() {
        let (p, rx) = channel();
        p.set_stage("Preparing\u{2026}");
        p.set_fraction(1.0);
        p.done();
        // Drain: we only assert it doesn't hang and ends with "done".
        let mut last_stage = String::new();
        for m in rx.try_iter() {
            last_stage = m.stage;
        }
        assert_eq!(last_stage, "done");
    }
}
