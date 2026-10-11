//! The ONE home of this crate's test fixtures: the `Bar` builders and the throwaway directory. It
//! is compiled three ways from this single file — as `crate::test_support` for the unit tests, and
//! by `#[path]` into `tests/common/mod.rs` (every integration binary) and `benches/` — so it names
//! nothing but `std` and `vike_model`, and it sits in `src/` because `src/` is what always ships.
#![allow(dead_code)] // each of the three includers uses a subset

use std::path::{Path, PathBuf};
use vike_model::Bar;

/// A bar with all five OHLCV numbers explicit, symbol `"X"` and no funding/bid/ask. The series
/// builders in the tests compute their own numbers and come here for the struct only.
pub(crate) fn ohlcv(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

/// A flat bar at `c` (open = high = low = close, no volume) at `ts`.
pub(crate) fn bar_at(ts: i64, c: f64) -> Bar {
    ohlcv(ts, c, c, c, c, 0.0)
}

/// [`bar_at`] at `ts` 0 — for a test that only reads `close`.
pub(crate) fn bar(c: f64) -> Bar {
    bar_at(0, c)
}

/// A throwaway directory, hand-rolled rather than pulled from `tempfile`: this crate has no
/// dev-dependency on it. Removed on drop. The name carries the pid as well as a nanosecond stamp,
/// so two test binaries running at once cannot be handed the same directory by one `tag`.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir()
            .join(format!("vike-script-{tag}-{pid}-{nanos}", pid = std::process::id()));
        std::fs::create_dir_all(&p).expect("scratch");
        Self(p)
    }
    /// Writes `rel` under the directory (parents created) and returns its full path.
    pub(crate) fn write(&self, rel: &str, src: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, src).unwrap();
        p
    }
    /// Writes `<dir>/indicators/<name>.rhai` — the `<user_data>` layout the loader knows.
    pub(crate) fn indicator(&self, name: &str, src: &str) {
        self.write(&format!("indicators/{name}.rhai"), src);
    }
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
