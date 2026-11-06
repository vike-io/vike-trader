//! The ONE home of this crate's test fixtures: the `Bar` builders, the throwaway directory, and the
//! `MockBroker` + `drive` every integration binary shares. Compiled ONCE by the lib — as
//! `crate::test_support` for the unit tests, and behind the `test-support` feature (which only this
//! crate's self-referential dev edge turns on) for every integration binary and `benches/`, which name
//! it as `vike_script::test_support` — instead of once per binary that used to include
//! `tests/common/mod.rs`.
//!
//! The integration binaries stay SEPARATE (`crates/vike-script/CLAUDE.md`'s "The installed set is process-wide, once"):
//! only these helpers moved, so each binary still installs at most once in its own process.

use std::path::{Path, PathBuf};
use vike_marketdata::Bar;
use vike_model::{Broker, Strategy};

use crate::{RhaiIndicator, RhaiStrategy};

/// A bar with all five OHLCV numbers explicit, symbol `"X"` and no funding/bid/ask. The series
/// builders in the tests compute their own numbers and come here for the struct only.
pub fn ohlcv(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
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
pub fn bar_at(ts: i64, c: f64) -> Bar {
    ohlcv(ts, c, c, c, c, 0.0)
}

/// [`bar_at`] at `ts` 0 — for a test that only reads `close`.
pub fn bar(c: f64) -> Bar {
    bar_at(0, c)
}

/// A throwaway directory, hand-rolled rather than pulled from `tempfile`: this crate has no
/// dev-dependency on it. Removed on drop. The name carries the pid as well as a nanosecond stamp,
/// so two test binaries running at once cannot be handed the same directory by one `tag`.
///
/// The PARENT is the caller's (every test passes the system temp directory): a library resolving its
/// own scratch root is what `crates/vike-ops/tests/hygiene/system_temp_gate.rs` refuses in `src/`,
/// and this file is `src/` under the `test-support` feature.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(parent: &Path, tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = parent.join(format!("vike-script-{tag}-{pid}-{nanos}", pid = std::process::id()));
        std::fs::create_dir_all(&p).expect("scratch");
        Self(p)
    }
    /// Writes `rel` under the directory (parents created) and returns its full path.
    pub fn write(&self, rel: &str, src: &str) -> PathBuf {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, src).unwrap();
        p
    }
    /// Writes `<dir>/indicators/<name>.rhai` — the `<user_data>` layout the loader knows.
    pub fn indicator(&self, name: &str, src: &str) {
        self.write(&format!("indicators/{name}.rhai"), src);
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The integration binaries' broker: records each market order as `(side, qty)`, which is why it is
/// not `vike_model::strategy::MockBroker` (that one records `(symbol, side, qty)`). `price()` is the
/// last pushed bar's close and `index()` that bar's index; `position()`, `equity()` and `now()` are
/// constants. The seven copies it replaced differed only where no assertion looks: three answered
/// `price()` with `0.0` (no script in them calls `price()`), one kept a `pos` field nothing wrote
/// (so `position()` was `0.0` there too), and two declared the fields in the other order.
#[derive(Default)]
pub struct MockBroker {
    pub bars: Vec<Bar>,
    pub submits: Vec<(i32, f64)>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        self.bars.last().map(|b| b.close).unwrap_or(0.0)
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}

/// Pushes each close as a `bar`, then feeds it to the strategy's `on_bar`.
pub fn drive(strat: &mut RhaiStrategy<MockBroker>, broker: &mut MockBroker, closes: &[f64]) {
    for &c in closes {
        broker.bars.push(bar(c));
        let b = broker.bars.last().unwrap().clone();
        strat.on_bar(broker, &b);
    }
}

/// Compiles `src` with `inds` bound beside the built-ins, drives it over `closes` and hands back the
/// broker, so a test asserts on `submits` without repeating the compile-and-drive scaffold.
pub fn run_with_indicators(src: &str, inds: &[RhaiIndicator], closes: &[f64]) -> MockBroker {
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), inds).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, closes);
    b
}
