//! Where a program's frames go: for each open card that changed what is
//! on screen, the commits (page flips, atomic commits, mode sets) it made
//! and, per commit, the time the program spent between seeing its previous
//! frame on screen and asking for the next one (drawing it), the time the
//! commit took in the kernel, and the time until that frame was on screen
//! (waiting for the display's next frame). Written to the system log when
//! the card closes:
//!
//! ```text
//! drm: kmscube on card0: 300 frames in 6012 ms (49.9 per second); per frame:
//!      drawing 14.1 ms, commit 0.31 ms (max 2.40), until shown 5.6 ms
//! ```

use crate::time;

#[derive(Default)]
pub struct Flips {
    frames: u64,
    first_ns: u64,
    last_ns: u64,
    /// When the previous frame was (to be) shown.
    shown_ns: u64,
    drawing_ns: u64,
    commit_ns: u64,
    commit_max_ns: u64,
    until_shown_ns: u64,
}

impl Flips {
    /// A commit began at `start`, ended at `end`, and its frame is shown at `due`.
    pub fn record(&mut self, start: u64, end: u64, due: u64) {
        if self.frames == 0 {
            self.first_ns = start;
        } else if start > self.shown_ns {
            self.drawing_ns += start - self.shown_ns;
        }
        self.frames += 1;
        self.last_ns = due.max(end);
        self.commit_ns += end - start;
        self.commit_max_ns = self.commit_max_ns.max(end - start);
        self.until_shown_ns += due.saturating_sub(end);
        self.shown_ns = due.max(end);
    }

    /// Writes the summary to the system log (nothing for a file that never
    /// showed a frame).
    pub fn report(&self, program: &str, minor: u32) {
        if self.frames < 2 {
            return;
        }
        let n = self.frames;
        let span = (self.last_ns - self.first_ns).max(1);
        let ms = |ns: u64| alloc::format!("{}.{:02}", ns / 1_000_000, ns / 10_000 % 100);
        log!(
            "drm",
            "{program} on card{minor}: {n} frames in {} ms ({}.{} per second); per frame: drawing {} ms, commit {} ms (max {}), until shown {} ms",
            span / 1_000_000,
            n * 1_000_000_000 / span,
            n * 10_000_000_000 / span % 10,
            ms(self.drawing_ns / (n - 1)),
            ms(self.commit_ns / n),
            ms(self.commit_max_ns),
            ms(self.until_shown_ns / n)
        );
    }
}

/// Now, for timing a commit.
pub fn now() -> u64 {
    time::now_ns()
}
