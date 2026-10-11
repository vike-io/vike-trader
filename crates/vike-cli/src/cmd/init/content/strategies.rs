//! The three shipped Rhai strategies, their parameter presets, and the Rust strategy template.

// ── the three shipped Rhai strategies ────────────────────────────────────────────────────────
//
// Three SHAPES, not three indicators: always-in reversal, mean-reversion entered from flat, and a
// long-only filter whose flat periods are real. They are deliberately NOT a tour of the indicator
// catalog — the host binds nearly all of it and `vike-cli indicators` prints the roster, so three
// examples could never be a sample of it. What an example can teach is how a number becomes a
// POSITION, and each of these does it differently.
//
// Each reads its indicators on the first lines of `on_bar()` — see this module's doc and the ⚠ rule
// in `RHAI_README`, and `crates/vike-cli/tests/init_cli.rs`'s
// `every_shipped_rhai_strategy_reads_its_indicators_before_it_branches`, which gates it.

/// `sma_cross/sma_cross.rhai` — two moving averages, always in the market, long or short.
pub const SMA_CROSS_RHAI: &str = r#"// sma_cross — the classic two-moving-average crossover.
//
// Long while the fast average is above the slow one, short while it is below. Always in the
// market: it reverses rather than flattening.
//
// Knobs (override from a profile's [strategy.params], or grid them in a [paramscan]):
let fast = param("fast", 10.0);
let slow = param("slow", 30.0);
let qty  = param("qty", 1.0);

fn on_bar() {
    // ⚠ BOTH indicators are read here, on EVERY bar, before any branch or return. An indicator
    // only advances when the script calls it, so a call inside an `if` would skip bars and
    // quietly stop being an average of the last N. This ordering is the rule, not a style.
    let f = sma(fast.to_int());
    let s = sma(slow.to_int());

    // Warm-up: the slow average is NaN until it has `slow` bars. Nothing to decide yet.
    if s.is_nan() { return; }

    // Target position, then trade the DIFFERENCE — so a bar that agrees with the position we
    // already hold sends no order at all.
    let target = if f > s { qty } else { -qty };
    let delta  = target - position();
    if delta >  1e-9 { market( 1,  delta); }
    if delta < -1e-9 { market(-1, -delta); }
}
"#;

/// `rsi_meanrev/rsi_meanrev.rhai` — fade the extremes.
pub const RSI_MEANREV_RHAI: &str = r#"// rsi_meanrev — buy oversold, sell overbought.
//
// The opposite instinct to sma_cross: this one bets that a stretched market snaps back. Running
// both over the same slice is a quick way to see what a market has been doing lately.
//
// Knobs:
let len = param("len", 14.0);
let lo  = param("lo", 30.0);   // below this: oversold, go long
let hi  = param("hi", 70.0);   // above this: overbought, go short
let qty = param("qty", 1.0);

fn on_bar() {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let r = rsi(len.to_int());
    if r.is_nan() { return; }

    // `position()` guards keep this from stacking size: enter only from flat or the other side.
    if r < lo && position() <= 0.0 { market(1, qty); }
    if r > hi && position() >= 0.0 { market(-1, qty); }
}
"#;

/// `ema_trend/ema_trend.rhai` — long-only trend filter.
pub const EMA_TREND_RHAI: &str = r#"// ema_trend — hold while price is above its moving average, otherwise hold nothing.
//
// LONG-ONLY, which makes it the useful contrast to sma_cross: its flat periods are real. If a
// long-only rule beats an always-in one, the short side was costing you money.
//
// Knobs:
let len = param("len", 20.0);
let qty = param("qty", 1.0);

fn on_bar() {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let e = ema(len.to_int());
    if e.is_nan() { return; }

    // Target is `qty` or nothing — never negative.
    let target = if close() > e { qty } else { 0.0 };
    let delta  = target - position();
    if delta >  1e-9 { market( 1,  delta); }
    if delta < -1e-9 { market(-1, -delta); }
}
"#;

/// `sma_cross/fast.toml` — a preset. Presets are param tables, one file each.
pub const PRESET_SMA_FAST: &str = r#"# Preset: fast — a short-horizon sma_cross, for intraday bars (1m–15m).
#
# Load it with: vike-cli backtest run --preset sma_cross/fast
# The keys are FLAT — they are merged into [strategy.params] key for key, so a wrapping
# [params] header would make the strategy receive a table called "params" and nothing else.
fast = 5.0
slow = 15.0
qty = 1.0
"#;

/// `sma_cross/slow.toml` — the second preset, so the FOLDER shows that presets are plural.
pub const PRESET_SMA_SLOW: &str = r#"# Preset: slow — a position-trading sma_cross, for daily bars.
#
# Load it with: vike-cli backtest run --preset sma_cross/slow
# Keys are FLAT — see fast.toml.
fast = 20.0
slow = 100.0
qty = 1.0
"#;

/// `rsi_meanrev/default.toml`.
pub const PRESET_RSI_DEFAULT: &str = r#"# Preset: default — the textbook 14/30/70 RSI settings.
#
# Load it with: vike-cli backtest run --preset rsi_meanrev/default
# Keys are FLAT — see sma_cross/fast.toml.
len = 14.0
lo = 30.0
hi = 70.0
qty = 1.0
"#;

/// `ema_trend/default.toml`.
pub const PRESET_EMA_DEFAULT: &str = r#"# Preset: default — a 20-period trend filter.
#
# Load it with: vike-cli backtest run --preset ema_trend/default
# Keys are FLAT — see sma_cross/fast.toml.
len = 20.0
qty = 1.0
"#;

/// `rust/my_experiment/my_experiment.rs` — the template to copy.
///
/// A COMPLETE, compiling strategy rather than a sketch: the point of a template is that the first
/// edit is a change to working code, not a hunt for what is missing.
pub const MY_EXPERIMENT_RS: &str = r##"//! `my_experiment` — the Rust strategy template. Copy this folder and rename it.
//!
//! ⚠ This compiles only in a SOURCE CHECKOUT — see ../README.md's first line. On a release install
//! nothing reads this directory.
//!
//! # Wiring it up: there is none
//!
//! The build scans this tree (`crates/vike-user-strategies`'s build script): a folder whose
//! `<name>.rs` exports the `build` function below is resolvable from any profile BY NAME after
//! the next `cargo build`. No registry edit — the folder is the registration. Built-in names
//! always win over yours, and live mounting additionally needs `live = true` in a `strategy.toml`
//! beside this file (absent = sim-only).
//!
//! # What you are implementing
//!
//! `vike_model::Strategy<B>`, generic over the broker. Generic is load-bearing: a strategy written
//! against `B: Broker` runs on the simulator AND against a live venue with no port. Write
//! `impl Strategy<SimBroker>` instead and you have built something that can only ever be
//! backtested.
//!
//! Every hook has a default no-op, so implement only the ones you use.

use vike_model::{Bar, Broker, Strategy};

/// Hold a fixed position while the close is above the entry price of the run's first bar.
///
/// Deliberately trivial — it exists to be replaced. What it demonstrates is the shape: state on
/// the struct, decisions in `on_bar`, orders through the broker.
pub struct MyExperiment {
    /// Units to hold when long. Read from the profile in a real strategy.
    qty: f64,
    /// The first close this strategy ever saw. `None` until the first bar arrives.
    reference: Option<f64>,
}

impl MyExperiment {
    pub fn new(qty: f64) -> Self {
        MyExperiment { qty, reference: None }
    }
}

impl Default for MyExperiment {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl<B: Broker> Strategy<B> for MyExperiment {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // The symbol comes from the BAR, never from a hard-coded string: one strategy instance can
        // be mounted on any series, and the engine may run several at once.
        let symbol = bar.symbol.clone().unwrap_or_default();
        if symbol.is_empty() {
            return;
        }

        // First bar: record the reference and do nothing else.
        let reference = *self.reference.get_or_insert(bar.close);

        // Trade the DIFFERENCE between where we want to be and where we are, so a bar that agrees
        // with the current position sends no order.
        let target = if bar.close > reference { self.qty } else { 0.0 };
        let delta = target - broker.position(&symbol);
        if delta.abs() > 1e-9 {
            broker.submit_market(&symbol, if delta > 0.0 { 1 } else { -1 }, delta.abs());
        }
    }
}

/// The entry point the build scan resolves — THE contract: exactly this signature, exported from
/// the folder-named file. Params arrive as the profile's `[strategy.params]` table; read them
/// leniently (a missing or mistyped key falls back to your default — nothing in-tree gates a user
/// strategy's keys, so the defaults ARE the safety net).
///
/// ⚠ Lenient about the TYPE too, which is why this reads float-or-integer rather than one of them:
/// a SWEPT knob arrives as a TOML float (a grid point's overrides are `f64`), while the same knob
/// written by hand in a profile is whatever the author typed. A one-type reader silently ignores
/// every value you sweep. The tier README says so at length.
pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(MyExperiment::new(qty))
}
"##;
