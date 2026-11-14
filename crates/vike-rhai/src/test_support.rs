//! The ONE home of this crate's test fixtures: the `Bar` builders, the throwaway directory, and the
//! `OrderBrokerMock` + `drive` every integration binary shares. Compiled ONCE by the lib — as
//! `crate::test_support` for the unit tests, and behind the `test-support` feature (which only this
//! crate's self-referential dev edge turns on) for every integration binary and `benches/`, which name
//! it as `vike_rhai::test_support` — instead of once per binary that used to include
//! `tests/common/mod.rs`.
//!
//! The integration binaries stay SEPARATE (`crates/vike-rhai/CLAUDE.md`'s "The installed set is process-wide, once"):
//! only these helpers moved, so each binary still installs at most once in its own process.

use std::path::{Path, PathBuf};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement, Strategy};

use crate::{RhaiIndicator, RhaiStrategy};

/// A bar with all five OHLCV numbers explicit, symbol `"X"` and no funding/bid/ask. The series
/// builders in the tests compute their own numbers and come here for the struct only.
pub fn ohlcv(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar {
        timestamp: ts,
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
        let p = parent.join(format!("vike-rhai-{tag}-{pid}-{nanos}", pid = std::process::id()));
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

/// The integration binaries' broker: records each order a script's `strategy` object places as
/// `(tag, side, qty)` in `seam` ([`OrderBrokerMock::orders`] drops the tag). It NEVER FILLS, so an order
/// re-issued under the same name while the first still rests is an amend (inert here), not a second
/// placement: a test counting one order per bar names each bar's (`"b" + index()`). `price()` is the
/// last pushed bar's close and `index()` that bar's index; `position()`, `equity()` and `now()` are
/// constants.
#[derive(Default)]
pub struct OrderBrokerMock {
    pub bars: Vec<Bar>,
    pub seam: Vec<(String, i32, f64)>,
}

impl OrderBrokerMock {
    /// Each placed order's `(side, qty)`, in placement order: `seam` without the tags.
    pub fn orders(&self) -> Vec<(i32, f64)> {
        self.seam.iter().map(|(_, side, qty)| (*side, *qty)).collect()
    }
}

/// The order seam, recorded as `(tag, side, qty)` per placed order: everything a script sends
/// (`seam`), a group leg by leg; amend, cancel and a refusal are inert.
impl OrderBroker for OrderBrokerMock {
    fn place(&mut self, _: &str, tag: &str, side: i32, qty: f64, _: OrderPlacement, _: bool) {
        self.seam.push((tag.to_string(), side, qty));
    }
    fn place_group(&mut self, _symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        self.seam.extend(legs.into_iter().map(|(tag, leg)| (tag, leg.side, leg.qty)));
    }
    fn amend(
        &mut self,
        _tag: &str,
        _quantity: Option<f64>,
        _price: Option<f64>,
        _trigger: Option<f64>,
    ) {
    }
    fn cancel(&mut self, _tag: &str) {}
    fn deny(&mut self, _symbol: &str, _tag: &str, _quantity: f64, _reason: &str) {}
    fn average_price(&self, _symbol: &str) -> f64 {
        0.0
    }
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

/// A strategy placing one buy a bar sized by the Rhai expression `qty`, each under its bar's own
/// name (`b<index>`), so every bar is a placement on [`OrderBrokerMock`], which never fills.
pub fn sized_by(qty: &str) -> String {
    format!(
        r#"fn on_bar(strategy) {{ strategy.order("b" + index(), OrderSide::Long).size({qty}); }}"#
    )
}

/// Pushes each close as a `bar`, then feeds it to the strategy's `on_bar`.
pub fn drive(
    strat: &mut RhaiStrategy<OrderBrokerMock>,
    broker: &mut OrderBrokerMock,
    closes: &[f64],
) {
    for &c in closes {
        broker.bars.push(bar(c));
        let b = broker.bars.last().unwrap().clone();
        strat.on_bar(broker, &b);
    }
}

/// Compiles `src` with `indicators` bound beside the built-ins, drives it over `closes` and hands back the
/// broker, so a test asserts on its orders without repeating the compile-and-drive scaffold.
pub fn run_with_indicators(
    src: &str,
    indicators: &[RhaiIndicator],
    closes: &[f64],
) -> OrderBrokerMock {
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), indicators)
        .expect("compiles");
    let mut b = OrderBrokerMock::default();
    drive(&mut s, &mut b, closes);
    b
}
