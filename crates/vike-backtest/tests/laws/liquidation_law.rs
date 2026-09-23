//! Gate for the ONE scope-parameterized liquidation law in the backtest engine
//! (`StrategyEngine::check_liquidation` / `check_liquidation_tick` over
//! `vike_model::{pool_breached, cross_liquidation_plan}`) and its opt-in stress knob:
//!
//! 1. **DEFAULT = the shared law** — the LEAN cross-pool workflow the live watchdog runs:
//!    trigger only past the `liq_buffer` grace line, then PARTIAL losers-first liquidation
//!    (excess only, stop when healthy) instead of the retired whole-account wipe.
//! 2. **`venue_style_liquidation: true` = the pre-law behavior, exactly** — bare
//!    `eq_adv ≤ maint·notional` trigger, total wipe at the adverse marks (bar path) /
//!    triggering-symbol-only full close (tick path).
//! 3. **The buffer divergence is real and deliberate**: a pool AT the line but inside the
//!    buffer holds under the default law and wipes under the knob.
//! 4. `maint_margin: 0.0` (the default) never liquidates on either path.
//!
//! Run-construction pattern mirrors `variation_margin.rs` (bar path) and
//! `controller_harness.rs` (tick path).

use vike_backtest::{EngineParams, SimBroker, StrategyEngine, Tick};
use vike_model::{Bar, QuoteTick, Strategy};

const AAA: &str = "AAA";
const BBB: &str = "BBB";
const T0: i64 = 1_700_000_000_000;

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Flat-priced tape: bar0 @100 (submit), bar1 @100 (entry fills at open), bar2 at `px2`
/// (the liquidation-check bar — degenerate flat bar so the adverse mark IS `px2`).
fn tape(px2: f64) -> Vec<Bar> {
    vec![
        bar(T0, 100.0, 100.0, 100.0, 100.0),
        bar(T0 + 60_000, 100.0, 100.0, 100.0, 100.0),
        bar(T0 + 120_000, px2, px2, px2, px2),
    ]
}

/// Buys 10 of each symbol at step 0 (fills at bar 1's open = 100). `on_bar` fires once per
/// SYMBOL per step, so a `done` latch keeps the submits single-shot.
#[derive(Default)]
struct BuyBoth {
    done: bool,
}

impl Strategy<SimBroker> for BuyBoth {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 && !self.done {
            self.done = true;
            ctx.submit(AAA, 1, 10.0, 0.0, true, None);
            ctx.submit(BBB, 1, 10.0, 0.0, true, None);
        }
    }
}

/// cash + bar2 prices chosen so at bar2's adverse marks (AAA 100, BBB `bbb_px2`):
/// eq_adv = cash − 2000 + 10·100 + 10·bbb_px2, maint_adv = 0.05·(1000 + 10·bbb_px2).
fn run_two_symbol(cash: f64, bbb_px2: f64, venue_style: bool) -> StrategyEngine<BuyBoth> {
    let params = EngineParams {
        cash,
        maint_margin: 0.05,
        venue_style_liquidation: venue_style,
        ..Default::default()
    };
    let mut eng = StrategyEngine::new(
        vec![(AAA.to_string(), tape(100.0)), (BBB.to_string(), tape(bbb_px2))],
        BuyBoth::default(),
        params,
    );
    eng.run();
    eng
}

// --------------------------------------------------------------------------------------------
// 1. DEFAULT: shared law — partial, losers-first, excess-only
// --------------------------------------------------------------------------------------------

#[test]
fn default_law_partially_liquidates_losers_first() {
    // cash 108: eq_adv = 108 − 2000 + 1000 + 980 = 88; maint_adv = 0.05·1980 = 99.
    // pool_breached(88, 99, 0.10): 88−99 ≤ 0 AND 99 > 96.8 → breach. excess = 11.
    // BBB (upnl −20) is the loser → per_unit 98·0.05 = 4.9 → close 11/4.9 ≈ 2.2449 of BBB
    // and STOP — AAA (upnl 0) is never touched.
    let eng = run_two_symbol(108.0, 98.0, false);
    let aaa = eng.core.position_of(AAA).size;
    let bbb = eng.core.position_of(BBB).size;
    assert_eq!(aaa, 10.0, "the flat-PnL position must be untouched");
    let expect_bbb = 10.0 - 11.0 / 4.9;
    assert!(
        (bbb - expect_bbb).abs() < 1e-9,
        "loser partially closed (excess only): got {bbb}, want {expect_bbb}"
    );
    // exactly one closing trade — the partial BBB close at the adverse mark 98
    let closes: Vec<_> = eng.core.trades.iter().collect();
    assert_eq!(closes.len(), 1, "one partial liquidation trade");
    assert_eq!(closes[0].symbol, BBB);
    assert_eq!(closes[0].exit_price, 98.0);
}

#[test]
fn default_law_holds_inside_the_buffer() {
    // cash 115: eq_adv = 95, maint_adv = 99. Condition-2 breached (95 ≤ 99) but the LEAN
    // buffer holds: 99 ≤ 95·1.1 = 104.5 → NO liquidation under the shared law…
    let eng = run_two_symbol(115.0, 98.0, false);
    assert_eq!(eng.core.position_of(AAA).size, 10.0);
    assert_eq!(eng.core.position_of(BBB).size, 10.0);
    assert!(eng.core.trades.is_empty(), "inside the buffer nothing liquidates");
}

// --------------------------------------------------------------------------------------------
// 2. STRESS KNOB: the retired venue-style wipe, exactly
// --------------------------------------------------------------------------------------------

#[test]
fn venue_style_knob_wipes_whole_account_at_bare_trigger() {
    // same tape as `default_law_holds_inside_the_buffer`: 95 ≤ 99 → the OLD bare trigger
    // fires and force-closes EVERYTHING at the adverse marks.
    let eng = run_two_symbol(115.0, 98.0, true);
    assert_eq!(eng.core.position_of(AAA).size, 0.0, "venue-style wipe closes AAA");
    assert_eq!(eng.core.position_of(BBB).size, 0.0, "venue-style wipe closes BBB");
    assert_eq!(eng.core.trades.len(), 2, "both positions force-closed");
    for t in &eng.core.trades {
        let want = if t.symbol == AAA { 100.0 } else { 98.0 };
        assert_eq!(t.exit_price, want, "wipe fills at the adverse mark ({})", t.symbol);
    }
}

#[test]
fn knob_and_default_agree_when_margin_off() {
    // maint_margin 0.0 (the default) → both paths return before touching anything.
    for venue_style in [false, true] {
        let params = EngineParams {
            cash: 10.0, // catastrophically under-collateralized — must still never liquidate
            maint_margin: 0.0,
            venue_style_liquidation: venue_style,
            ..Default::default()
        };
        let mut eng = StrategyEngine::new(
            vec![(AAA.to_string(), tape(50.0)), (BBB.to_string(), tape(50.0))],
            BuyBoth::default(),
            params,
        );
        eng.run();
        assert_eq!(eng.core.position_of(AAA).size, 10.0, "venue_style={venue_style}");
        assert_eq!(eng.core.position_of(BBB).size, 10.0, "venue_style={venue_style}");
    }
}

// --------------------------------------------------------------------------------------------
// 3. TICK path: same law/knob split on the quote lane
// --------------------------------------------------------------------------------------------

fn q(ts: i64, mid: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid: mid,
        ask: mid,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: AAA.to_string(),
    })
}

/// Buys 10 AAA on the first quote (fills at the next quote's mid = 100).
struct BuyOnceOnTick {
    done: bool,
}

impl Strategy<SimBroker> for BuyOnceOnTick {
    fn on_quote_tick(&mut self, ctx: &mut SimBroker, _q: &QuoteTick) {
        if !self.done {
            self.done = true;
            ctx.submit(AAA, 1, 10.0, 0.0, true, None);
        }
    }
}

fn run_tick(cash: f64, crash_mid: f64, venue_style: bool) -> StrategyEngine<BuyOnceOnTick> {
    let params = EngineParams {
        cash,
        maint_margin: 0.05,
        venue_style_liquidation: venue_style,
        ..Default::default()
    };
    let mut eng = StrategyEngine::new(
        vec![(AAA.to_string(), Vec::new())],
        BuyOnceOnTick { done: false },
        params,
    );
    // tick 1: submit; tick 2: entry fills @100; tick 3: the crash → liquidation check
    eng.run_ticks(&[(AAA.to_string(), vec![q(1, 100.0), q(2, 100.0), q(3, crash_mid)])]);
    eng
}

#[test]
fn tick_default_law_partially_liquidates() {
    // cash 54, entry 10@100 → cash −946. Crash to 96: eq_adv = −946 + 960 = 14;
    // maint_adv = 0.05·960 = 48. breach: 14−48 ≤ 0, 48 > 15.4 ✓. excess = 34;
    // per_unit = 96·0.05 = 4.8 → close 34/4.8 ≈ 7.083 of 10 — PARTIAL (old code closed all 10).
    let eng = run_tick(54.0, 96.0, false);
    let left = eng.core.position_of(AAA).size;
    let want = 10.0 - 34.0 / 4.8;
    assert!((left - want).abs() < 1e-9, "tick-path partial close: got {left}, want {want}");
}

#[test]
fn tick_venue_style_closes_triggering_symbol_in_full() {
    let eng = run_tick(54.0, 96.0, true);
    assert_eq!(eng.core.position_of(AAA).size, 0.0, "old tick model: full close of the symbol");
}
