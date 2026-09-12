//! THE SP1 acceptance gate: a Rhai SMA-cross script and a Rust twin — both driving the SAME
//! streaming `vike_indicators` SMA — produce a BYTE-IDENTICAL `BacktestResult` over the same
//! `StrategyEngine` run. This proves the whole Rhai->Strategy binding (indicator bridge +
//! order-verb intents + snapshot reads) is faithful: if the script's arithmetic/branch order or
//! its indicator feed sequence ever drifted from a hand-written Rust strategy, this test would
//! catch it at the bit level, not just "roughly the same P&L".

use vike_backtest::{BacktestResult, EngineParams, StrategyEngine};
use vike_model::{Bar, Broker, Fill, Strategy};
use vike_script::RhaiStrategy;

const FAST: usize = 5;
const SLOW: usize = 20;
const QTY: f64 = 1.0;
const SYMBOL: &str = "BTCUSDT";

// --- Rust twin: same streaming SMA + same decision as the Rhai script ---
struct SmaCrossRef {
    fast: Box<dyn vike_indicators::Indicator>,
    slow: Box<dyn vike_indicators::Indicator>,
}
impl Default for SmaCrossRef {
    fn default() -> Self {
        Self {
            fast: vike_indicators::make_with("sma", &[FAST as f64]).unwrap(),
            slow: vike_indicators::make_with("sma", &[SLOW as f64]).unwrap(),
        }
    }
}
impl<B: Broker> Strategy<B> for SmaCrossRef {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let f = self.fast.on_bar(bar)[0];
        let s = self.slow.on_bar(bar)[0];
        if s.is_nan() {
            return;
        }
        let target = if f > s { QTY } else { -QTY };
        let sym = bar.symbol.clone().unwrap_or_default();
        let delta = target - broker.position(&sym);
        if delta.abs() > 1e-12 {
            broker.submit_market(&sym, if delta > 0.0 { 1 } else { -1 }, delta.abs());
        }
    }
    fn on_fill(&mut self, _b: &mut B, _f: &Fill) {}
}

// NOTE on the NaN guard: the brief's first draft used the common `s != s` idiom for "NaN ->
// warmup, hold". That DIVERGED from the Rust twin's `s.is_nan()` — empirically verified (a
// throwaway probe against `rhai` 1.25.1) that Rhai's `!=`/`==` operators special-case a
// self-comparison (`s != s`) to the algebraically "obvious" `false` regardless of runtime value,
// which is correct for totally-ordered types but WRONG for IEEE-754 float NaN (`NaN != NaN` must
// be `true`). With that idiom the script's warmup guard never fires, `f > s` (both operands NaN)
// silently takes the `else` branch, and the script opens a short position ~19 bars before the
// Rust twin's first real signal — a genuine binding-fidelity bug, not a tolerance issue. Rhai's
// `.is_nan()` method call *does* follow IEEE-754 (verified true for a NaN indicator value), so
// the script below uses that instead — the two now gate warmup identically.
const SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5);
    let s = sma(20);
    if s.is_nan() { return; }                    // NaN -> warmup, hold
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 {
        market(if delta > 0.0 { 1 } else { -1 }, abs(delta));
    }
}
"#;

// seeded LCG walk (mirrors r7_gate::gen_bars) so the fixture is self-contained.
//
// NOTE: shift by 32, not 33. `seed >> 32` keeps the full top 32 bits (range [0, u32::MAX]), so
// dividing by `u32::MAX` gives a uniform fraction over the CLOSED [0.0, 1.0] and `(frac - 0.5) *
// 2.0` is a proper symmetric step over [-1.0, 1.0] — the same transform r7_gate.rs's sibling
// `gen_bars` uses (`(state >> 32) as f64 / (1u64 << 32) as f64 * 2.0 - 1.0`, algebraically
// identical). An earlier `>> 33` draft only kept the top 31 bits, so the same division by
// `u32::MAX` under-filled the numerator's range to [0, ~0.5) — `step` was ALWAYS <= 0, and the
// walk monotonically decayed into the `.max(1.0)` floor and stuck there, so the SMA(5)/SMA(20)
// cross never reversed after its first entry (n_trades stayed 0 for both sides — a broken
// fixture, not a binding bug: empirically confirmed via a standalone probe of both shift amounts
// before changing this).
fn gen_bars(n: usize) -> Vec<Bar> {
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    (0..n)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let step = ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0;
            px = (px + step).max(1.0);
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some(SYMBOL.into()),
            }
        })
        .collect()
}

fn params() -> EngineParams {
    EngineParams {
        cash: 10_000.0,
        slippage: 0.0002,
        maker_fee: Some(0.0002),
        taker_fee: Some(0.0007),
        ..EngineParams::default()
    }
}

fn run<S: Strategy<vike_backtest::SimBroker>>(strat: S, bars: Vec<Bar>) -> BacktestResult {
    StrategyEngine::new(vec![(SYMBOL.to_string(), bars)], strat, params()).run()
}

#[track_caller]
fn assert_bits_eq(a: f64, b: f64, ctx: &str) {
    assert_eq!(a.to_bits(), b.to_bits(), "{ctx}");
}

#[test]
fn rhai_sma_cross_is_byte_identical_to_rust_twin() {
    let bars = gen_bars(400);
    let rust = run(SmaCrossRef::default(), bars.clone());
    let rhai = run(RhaiStrategy::<vike_backtest::SimBroker>::compile(SCRIPT).unwrap(), bars);

    assert_eq!(rust.n_trades, rhai.n_trades);
    assert_bits_eq(rust.final_equity, rhai.final_equity, "final_equity");
    assert_eq!(rust.equity_curve.len(), rhai.equity_curve.len());
    for (i, (a, b)) in rust.equity_curve.iter().zip(&rhai.equity_curve).enumerate() {
        assert_bits_eq(*a, *b, &format!("equity_curve[{i}]"));
    }
    assert_eq!(rust.trades.len(), rhai.trades.len());
    for (i, (a, b)) in rust.trades.iter().zip(&rhai.trades).enumerate() {
        assert_bits_eq(a.entry_price, b.entry_price, &format!("trade[{i}].entry"));
        assert_bits_eq(a.exit_price, b.exit_price, &format!("trade[{i}].exit"));
        assert_bits_eq(a.pnl, b.pnl, &format!("trade[{i}].pnl"));
        assert_bits_eq(a.size, b.size, &format!("trade[{i}].size"));
        assert_bits_eq(a.fees, b.fees, &format!("trade[{i}].fees"));
    }
    assert!(rhai.n_trades > 0, "sanity: the cross should trade over 400 bars");
}

/// Fix 1 regression test (final whole-branch review, IMPORTANT): before the fix,
/// `RhaiStrategy::run_hook`'s pre-eval snapshot unconditionally called
/// `broker.position(&sym)`/`broker.price(&sym)`, where `sym` is the CURRENT bar's symbol tag —
/// but before the first bar ever arrives (i.e. during `on_start`), `ctx.cur_bar` is still the
/// zero-bar (`symbol: None`), so `sym()` returns `""`. A real `SimBroker` routes
/// `position("")`/`price("")` through `SimBroker::idx`, which `panic!`s on an unknown symbol
/// (`crates/vike-backtest/src/engine/sim_broker.rs`) — a Rust panic, not an `EvalAltResult`, so
/// the strategy's own fail-safe (which only catches script errors) never sees it. Mounting ANY
/// script that merely DEFINES `on_start` (even an empty body) therefore panicked the whole
/// `StrategyEngine::run()`. This drives a REAL `SimBroker`/`StrategyEngine` (not the
/// `vike-script` crate's own `MockBroker`, which ignores its symbol argument entirely and so
/// cannot reproduce this) over a script with a trivial `on_start`/`on_bar` pair and asserts the
/// run completes without panicking.
///
/// Non-vacuity verified by hand: with the `!sym.is_empty()` guard in `RhaiStrategy::run_hook`
/// (`crates/vike-script/src/strategy.rs`) temporarily reverted back to the unconditional reads,
/// this exact test panics with `unknown symbol ""` (confirmed locally before applying the fix;
/// the revert is not left in the tree since that would defeat the fix — see the fix report).
#[test]
fn on_start_hook_does_not_panic_sim_broker() {
    let bars = gen_bars(50);
    let n = bars.len();
    let strat =
        RhaiStrategy::<vike_backtest::SimBroker>::compile("fn on_start() {}\nfn on_bar() {}")
            .unwrap();
    let result = StrategyEngine::new(vec![(SYMBOL.to_string(), bars)], strat, params()).run();
    assert_eq!(result.equity_curve.len(), n, "the run must complete every bar, not abort early");
}
