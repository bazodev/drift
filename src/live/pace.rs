//! Debug only: how evenly frames come. Every few seconds one line goes to the dev log,
//! so a stutter shows up as numbers rather than as a feeling.

use std::time::{Duration, Instant};

const REPORT_EVERY: Duration = Duration::from_secs(5);

pub struct Pace {
    label: &'static str,
    since: Instant,
    last: Option<Instant>,
    count: u32,
    longest: Duration,
    late: u32,
}

impl Pace {
    pub fn new(label: &'static str) -> Self {
        Self { label, since: Instant::now(), last: None, count: 0, longest: Duration::ZERO, late: 0 }
    }

    /// One more frame. `aimed` is the spacing it was meant to come at.
    pub fn frame(&mut self, aimed: Duration) {
        let now = Instant::now();
        if let Some(last) = self.last {
            let gap = now - last;
            self.longest = self.longest.max(gap);
            if gap > aimed * 3 / 2 {
                self.late += 1;
            }
        }
        self.last = Some(now);
        self.count += 1;
        let elapsed = now - self.since;
        if elapsed >= REPORT_EVERY {
            if std::env::var_os("DRIFT_DEV_FIFO").is_some() {
                eprintln!(
                    "{}: {:.1} a second, longest gap {} ms, {} late",
                    self.label,
                    self.count as f32 / elapsed.as_secs_f32(),
                    self.longest.as_millis(),
                    self.late,
                );
            }
            *self = Self { last: self.last, ..Self::new(self.label) };
        }
    }

    /// Nothing is drawn for a while: that pause is not a stutter.
    pub fn rest(&mut self) {
        self.last = None;
    }
}
