//! The desktop's OWN frame-rate cap, for the one case the GUI library has none: vertical sync off.
//!
//! With `preferences.vsync` on, wgpu's swap chain blocks every present on the display's refresh, and
//! that block IS the cap. With it off (`AutoNoVsync`) present returns at once, and eframe has no
//! limiter of its own. egui is reactive, so this matters exactly when something asks for a repaint
//! straight after every frame (a running animation, a worker's progress wake, a streaming
//! snapshot's wake), and then the loop would spin as fast as the GPU lets it, a CPU core with it.
//! [`FrameCap`] sleeps the UI thread at the top of a frame just long enough to keep frame
//! STARTS at least [`MIN_FRAME_INTERVAL`] apart. Frames that arrive further apart than that, which
//! is every frame of an idle app, are not touched.
//!
//! The cap exists only when vsync is off (`crates/vike-desktop/src/app_state.rs`'s
//! `App::frame_cap` is `None` otherwise), so a vsync'd app pays nothing.
//!
//! # The number
//!
//! [`MIN_FRAME_INTERVAL`] is 1/144 s: the refresh rate of the common high-refresh panel, so turning
//! vsync off for latency still gets everything such a panel can show, and a chart gains nothing
//! perceptible above it. It is a CHOICE, not a measurement: the cost of a repaint loop is
//! per frame (`VIKE_FRAME_LOG=1` prints the achieved rate once a second — see
//! `crates/vike-desktop/src/app_ui.rs`'s `ui` for what it measured), so it falls in proportion to
//! the cap, and lowering the number is the whole remedy if a box shows a core busy.

use std::time::{Duration, Instant};

/// The shortest time between two frame starts while the cap is on: 1/144 s, rounded down to a whole
/// microsecond (6944.4 µs).
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_micros(6_944);

/// The frames per second [`MIN_FRAME_INTERVAL`] allows, for the Settings window's sentence.
pub fn max_frames_per_second() -> u32 {
    u32::try_from(Duration::from_secs(1).as_micros() / MIN_FRAME_INTERVAL.as_micros())
        .unwrap_or(u32::MAX)
}

/// Keeps frame starts at least `min_interval` apart. The decision ([`Self::pace`]) is pure; only
/// [`Self::throttle`] sleeps.
#[derive(Debug, Clone, Copy)]
pub struct FrameCap {
    min_interval: Duration,
    /// When the previous frame started, counting the sleep that preceded it.
    last_start: Option<Instant>,
}

impl Default for FrameCap {
    fn default() -> Self {
        Self::new(MIN_FRAME_INTERVAL)
    }
}

impl FrameCap {
    /// A cap that keeps frame starts `min_interval` apart.
    pub fn new(min_interval: Duration) -> Self {
        Self { min_interval, last_start: None }
    }

    /// How long a frame that begins at `now` must wait first, and the frame is recorded as starting
    /// after that wait. The first frame never waits, and neither does one that begins a whole
    /// interval or more after the last.
    pub fn pace(&mut self, now: Instant) -> Duration {
        let wait = match self.last_start {
            Some(last) => self.min_interval.saturating_sub(now.saturating_duration_since(last)),
            None => Duration::ZERO,
        };
        self.last_start = Some(now + wait);
        wait
    }

    /// [`Self::pace`] against the clock, then the sleep it asks for. Called once at the top of a
    /// frame.
    pub fn throttle(&mut self) {
        let wait = self.pace(Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

#[path = "frame_cap_tests.rs"]
#[cfg(test)]
mod frame_cap_tests;
