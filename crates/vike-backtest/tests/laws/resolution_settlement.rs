//! pm-economics lane gate: the opt-in `EngineParams::resolution` binary-resolution settlement.
//! A resolved symbol has its resting orders canceled and any open position closed at the terminal
//! payout by a fee-free settlement fill (recorded distinctly in `SimBroker::settlements`);
//! `resolution: None` — and even a configured source that never fires — leaves the run
//! byte-identical. Mirrors the run-construction pattern in `properties_fills.rs`.

use vike_backtest::{EngineParams, ResolutionSource, SimBroker, StrategyEngine, Tick};
use vike_model::{Bar, QuoteTick, Strategy};

const SYM: &str = "YESTOKEN";
const T0: i64 = 1_700_000_000_000;
const STEP_MS: i64 = 60_000;

fn mk_bars(opens: &[f64]) -> Vec<Bar> {
    opens
        .iter()
        .enumerate()
        .map(|(i, &o)| Bar {
            ts: T0 + i as i64 * STEP_MS,
            open: o,
            high: o + 0.01,
            low: o - 0.01,
            close: o,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(SYM.to_string()),
        })
        .collect()
}

/// Buys `size` YES at step `open_at` (raw market, fills at the NEXT bar's open) and additionally
/// rests a far-away limit buy that never fills — the resting order the settlement must cancel.
struct BuyAndRest {
    open_at: usize,
    size: f64,
    rest_limit: bool,
    fills_seen: Vec<vike_model::Fill>,
}

impl Strategy<SimBroker> for BuyAndRest {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.open_at {
            ctx.submit(SYM, 1, self.size, 0.0, true, None);
            if self.rest_limit {
                // a resting bid far below the market: never crossed, must survive until resolution
                SimBroker::submit_limit(ctx, SYM, 1, 5.0, 0.01, 0.0, true, None);
            }
        }
    }

    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &vike_model::Fill) {
        self.fills_seen.push(fill.clone());
    }
}

fn resolution_at(res_ts: i64, payout: f64) -> Option<ResolutionSource> {
    Some(Box::new(move |sym: &str, ts: i64| (sym == SYM && ts >= res_ts).then_some(payout)))
}

/// Long YES settles at payout 1.0: position force-closed full-qty at 1.0, settlement is fee-free,
/// the resting limit order is canceled, and the PnL/cash math is exact.
#[test]
fn long_yes_settles_at_one() {
    // idx0: submit (market fills at idx1 open = 0.60, limit rests). Resolution reports from idx3.
    let bars = mk_bars(&[0.55, 0.60, 0.70, 0.95, 0.97]);
    let res_ts = T0 + 3 * STEP_MS;
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: true, fills_seen: Vec::new() };
    let params = EngineParams {
        cash: 1_000.0,
        // taker fee on the ENTRY only — proves settlement itself adds no fee on top
        taker_fee: Some(0.001),
        resolution: resolution_at(res_ts, 1.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    let r = e.run();

    // the position is gone and the resting limit was canceled by the resolution sweep
    assert_eq!(e.core.position_of(SYM).size, 0.0, "settled position must be flat");
    assert!(e.core.pending_of(SYM).is_empty(), "resting orders canceled at resolution");

    // ONE distinct settlement record: full qty at payout 1.0, closing a long
    assert_eq!(e.core.settlements.len(), 1);
    let s = &e.core.settlements[0];
    assert_eq!((s.symbol.as_str(), s.payout, s.qty, s.side, s.ts), (SYM, 1.0, 10.0, -1, res_ts));

    // ONE closed trade: entry 0.60, exit at the 1.0 payout, fees = entry taker fee only
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    let entry_fee = 10.0 * 0.60 * 0.001; // broker_sim::fee op order (×1.0 mult is identity)
    assert_eq!(t.entry_price, 0.60);
    assert_eq!(t.exit_price, 1.0);
    assert_eq!(t.size, 10.0);
    assert_eq!(t.fees, entry_fee, "settlement adds NO fee — only the entry fee is charged");
    // ClosedTrade.pnl is GROSS price pnl: (exit − entry) × closing × mult, fees tracked separately
    assert_eq!(t.pnl, (1.0 - 0.60) * 10.0);
    assert!(t.is_long);

    // cash, in the engine's exact op order: (1000 − entry_fee − buy notional) + payout proceeds
    assert_eq!(e.core.cash, 1_000.0 - entry_fee - 10.0 * 0.60 + 10.0);
    assert_eq!(r.final_equity, e.core.cash, "flat book: equity == cash");

    // the strategy observed the settlement through on_fill: a fee-free full close
    let last = e.strategy.fills_seen.last().expect("settlement delivered via on_fill");
    assert_eq!(
        (last.side, last.size, last.price, last.fee, last.is_maker),
        (-1, 10.0, 1.0, 0.0, false)
    );
}

/// Long YES resolved worthless (payout 0.0): the whole stake is lost, cash gets no proceeds.
#[test]
fn long_yes_settles_at_zero() {
    let bars = mk_bars(&[0.55, 0.60, 0.30, 0.05, 0.02]);
    let res_ts = T0 + 3 * STEP_MS;
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: false, fills_seen: Vec::new() };
    let params = EngineParams {
        cash: 1_000.0,
        resolution: resolution_at(res_ts, 0.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    let r = e.run();

    assert_eq!(e.core.position_of(SYM).size, 0.0);
    assert_eq!(e.core.settlements.len(), 1);
    assert_eq!(e.core.settlements[0].payout, 0.0);
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 0.0);
    assert_eq!(r.trades[0].pnl, (0.0 - 0.60) * 10.0); // fee-free run: pure (gross) stake loss
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 0.60); // no proceeds from a worthless token
}

/// A configured source that never fires is byte-identical to `resolution: None` — the same
/// trades, equity curve, cash, and (empty) settlements.
#[test]
fn never_firing_source_is_byte_identical_to_none() {
    let bars = mk_bars(&[0.55, 0.60, 0.70, 0.95, 0.97]);
    let mk = |resolution: Option<ResolutionSource>| {
        let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: true, fills_seen: Vec::new() };
        let params = EngineParams {
            cash: 1_000.0,
            taker_fee: Some(0.001),
            resolution,
            ..Default::default()
        };
        StrategyEngine::new(vec![(SYM.to_string(), bars.clone())], strat, params)
    };
    let mut base = mk(None);
    let rb = base.run();
    let mut never = mk(Some(Box::new(|_: &str, _: i64| None)));
    let rn = never.run();

    assert_eq!(
        rb.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        rn.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
    assert_eq!(rb.n_trades, rn.n_trades);
    assert_eq!(base.core.cash.to_bits(), never.core.cash.to_bits());
    assert!(never.core.settlements.is_empty());
    // the un-resolved run keeps its resting limit; both runs agree
    assert_eq!(base.core.pending_of(SYM).len(), never.core.pending_of(SYM).len());
    assert_eq!(base.core.pending_of(SYM).len(), 1);
}

/// Re-enters ONCE from inside `on_fill`, on the settlement fill itself (fee-free, at the payout) —
/// the classic re-hedge/re-enter pattern, and the adversarial case for the resolution invariant.
struct ReenterOnSettlement {
    payout: f64,
    reentry_attempts: usize,
}

impl Strategy<SimBroker> for ReenterOnSettlement {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, 10.0, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, ctx: &mut SimBroker, fill: &vike_model::Fill) {
        // the settlement fill: fee-free, at the payout, closing the long
        if fill.fee == 0.0 && fill.price == self.payout && self.reentry_attempts == 0 {
            self.reentry_attempts += 1;
            // both lanes: the untagged `pending` book and a resting limit
            ctx.submit(SYM, 1, 25.0, 0.0, true, None);
            SimBroker::submit_limit(ctx, SYM, 1, 25.0, 0.5, 0.0, true, None);
        }
    }
}

/// BUG (1) REGRESSION — `on_fill` re-entry must NOT re-open a resolved market. The settlement
/// clears the books and then fires `on_fill`; a strategy that submits from inside it would land a
/// fresh order in `pending` AFTER the clear but BEFORE this same step's fill phase, filling
/// against the resolution bar and manufacturing fee-free PnL. The per-symbol resolved latch must
/// refuse those orders outright, on both the `pending` and the tagged maker lane.
#[test]
fn post_resolution_reentry_from_on_fill_is_refused() {
    // resolution at idx3; bars continue past it, so a re-opened position WOULD keep trading
    let bars = mk_bars(&[0.55, 0.60, 0.70, 0.95, 0.97, 0.98]);
    let res_ts = T0 + 3 * STEP_MS;
    let strat = ReenterOnSettlement { payout: 1.0, reentry_attempts: 0 };
    let params = EngineParams {
        cash: 1_000.0,
        resolution: resolution_at(res_ts, 1.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    let r = e.run();

    // the strategy really did try to re-enter — otherwise this test proves nothing
    assert_eq!(e.strategy.reentry_attempts, 1, "the re-entry path must actually have been taken");

    // ...and every one of those orders was refused: still flat, nothing resting, ONE settlement
    assert!(e.core.is_resolved(SYM), "the symbol is latched resolved");
    assert_eq!(e.core.position_of(SYM).size, 0.0, "a resolved market cannot be re-opened");
    assert!(e.core.pending_of(SYM).is_empty(), "refused orders never rest");
    assert_eq!(e.core.settlements.len(), 1, "settled exactly once — no re-open, no re-settle");

    // exactly the one round trip: entry 0.60 -> payout 1.0. A re-opened leg would show up as a
    // second trade or as a non-flat book at 0.98.
    assert_eq!(r.trades.len(), 1);
    assert_eq!((r.trades[0].entry_price, r.trades[0].exit_price), (0.60, 1.0));
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 0.60 + 10.0);
    assert_eq!(r.final_equity, e.core.cash);
}

/// BUG (2) REGRESSION — the event stream ends BEFORE `res_ts`, which is the normal real-Polymarket
/// shape (trading halts, the resolution posts later). The in-loop probes only ever ask at event
/// timestamps, so without the end-of-run sweep nothing would settle at all: the held position
/// would be marked at the last traded price (0.97) instead of the payout (1.0).
#[test]
fn stream_ending_before_resolution_still_settles() {
    let bars = mk_bars(&[0.55, 0.60, 0.70, 0.95, 0.97]);
    let last_bar_ts = T0 + 4 * STEP_MS;
    // resolution posts a full day after the tape stops
    let res_ts = last_bar_ts + 86_400_000;
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: true, fills_seen: Vec::new() };
    let params = EngineParams {
        cash: 1_000.0,
        resolution: resolution_at(res_ts, 1.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    let r = e.run();

    assert_eq!(e.core.settlements.len(), 1, "the end-of-run sweep settled the held position");
    let s = &e.core.settlements[0];
    // stamped at the symbol's LAST event ts (no `resolution_end_ts` given) — never `i64::MAX`
    assert_eq!((s.payout, s.qty, s.side, s.ts), (1.0, 10.0, -1, last_bar_ts));
    assert_eq!(e.core.position_of(SYM).size, 0.0);

    // the trade exits at the PAYOUT, not at the last traded price
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 1.0);
    assert_eq!(r.trades[0].pnl, (1.0 - 0.60) * 10.0);
    assert_eq!(e.core.cash, 1_000.0 - 10.0 * 0.60 + 10.0);

    // the curve's final sample is re-pointed at the post-settlement equity — same sample COUNT
    assert_eq!(r.equity_curve.len(), 5);
    assert_eq!(*r.equity_curve.last().unwrap(), r.final_equity);
    assert_eq!(r.final_equity, e.core.cash, "flat book: equity == cash");
}

/// The tick path's twin of the sweep: ticks stop before `res_ts`, and the run still settles at the
/// last tick ts. Also pins `resolution_end_ts` as the suppression knob — an as-of BEFORE the
/// resolution leaves the position open and unsettled (a windowed backtest must not settle past its
/// window).
#[test]
fn tick_stream_ending_before_resolution_settles_and_end_ts_can_suppress() {
    let last_tick_ts = T0 + 2 * STEP_MS;
    let res_ts = last_tick_ts + 86_400_000;
    let quote = |ts: i64, px: f64| {
        Tick::Quote(QuoteTick {
            ts,
            local_ts: 0,
            bid: px - 0.01,
            ask: px + 0.01,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: SYM.to_string(),
        })
    };
    let mk = |resolution_end_ts: Option<i64>| {
        let strat =
            BuyAndRest { open_at: 0, size: 10.0, rest_limit: false, fills_seen: Vec::new() };
        let params = EngineParams {
            cash: 1_000.0,
            resolution: resolution_at(res_ts, 1.0),
            resolution_end_ts,
            ..Default::default()
        };
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params)
    };
    let ticks = || vec![quote(T0, 0.60), quote(T0 + STEP_MS, 0.65), quote(last_tick_ts, 0.97)];

    // default (`None` => probe at i64::MAX): settles, stamped at the LAST TICK ts
    let mut e = mk(None);
    e.core.submit(SYM, 1, 10.0, 0.0, true, None);
    let r = e.run_ticks(&[(SYM.to_string(), ticks())]);
    assert_eq!(e.core.settlements.len(), 1, "tick-path end-of-run sweep settled");
    assert_eq!(e.core.settlements[0].ts, last_tick_ts);
    assert_eq!(e.core.position_of(SYM).size, 0.0);
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 1.0);

    // an explicit as-of BEFORE the resolution suppresses it: the window closed while the market
    // was still live, so the position stays open and is marked, not settled
    let mut e2 = mk(Some(res_ts - 1));
    e2.core.submit(SYM, 1, 10.0, 0.0, true, None);
    let r2 = e2.run_ticks(&[(SYM.to_string(), ticks())]);
    assert!(e2.core.settlements.is_empty(), "as-of before res_ts must not settle");
    assert_eq!(e2.core.position_of(SYM).size, 10.0, "position survives the window end");
    assert!(!e2.core.is_resolved(SYM));
    assert!(r2.trades.is_empty());
}

/// The tick path settles too: a long position is closed at the payout on the first price tick
/// at-or-after resolution, before that tick's fill phase (the resting order cannot fill).
#[test]
fn tick_path_settles_and_cancels() {
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: false, fills_seen: Vec::new() };
    let res_ts = T0 + 2 * STEP_MS;
    let params = EngineParams {
        cash: 1_000.0,
        resolution: resolution_at(res_ts, 1.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params);

    let quote = |ts: i64, px: f64| {
        Tick::Quote(QuoteTick {
            ts,
            local_ts: 0,
            bid: px - 0.01,
            ask: px + 0.01,
            bid_size: 100.0,
            ask_size: 100.0,
            symbol: SYM.to_string(),
        })
    };
    // open a position directly (the strategy's on_bar never fires on the tick path), then feed
    // ticks that cross the resolution ts
    e.core.submit(SYM, 1, 10.0, 0.0, true, None);
    let ticks = vec![
        quote(T0, 0.60),
        quote(T0 + STEP_MS, 0.65),
        quote(res_ts, 0.99),
        quote(res_ts + 1, 0.99),
    ];
    let r = e.run_ticks(&[(SYM.to_string(), ticks)]);

    assert_eq!(e.core.position_of(SYM).size, 0.0, "tick-path settlement flattens");
    assert_eq!(e.core.settlements.len(), 1, "settled exactly once (idempotent after flat)");
    assert_eq!(e.core.settlements[0].ts, res_ts);
    assert_eq!(e.core.settlements[0].payout, 1.0);
    assert!(e.core.pending_of(SYM).is_empty());
    assert_eq!(r.trades.len(), 1);
    assert_eq!(r.trades[0].exit_price, 1.0);
}

// --- sharp-edge hardening ---

/// The end-of-run sweep must probe at a timestamp a caller can do ARITHMETIC on. A
/// `ResolutionSource` written the natural windowed way (`ts + window`) would panic in debug /
/// wrap in release if the sweep probed at `i64::MAX`; `RESOLUTION_PROBE_SENTINEL` leaves headroom.
/// The source below is exactly that shape — it overflows if the sentinel regresses to `i64::MAX`.
#[test]
fn end_of_run_probe_ts_leaves_headroom_for_caller_arithmetic() {
    // a full day of headroom is far more than any real resolution window needs
    assert!(
        vike_backtest::RESOLUTION_PROBE_SENTINEL.checked_add(86_400_000).is_some(),
        "the probe sentinel must survive a caller's `ts + window`"
    );

    let bars = mk_bars(&[0.55, 0.60, 0.70]);
    let last_bar_ts = T0 + 2 * STEP_MS;
    let res_ts = last_bar_ts + 86_400_000;
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: false, fills_seen: Vec::new() };
    let params = EngineParams {
        cash: 1_000.0,
        // windowed source: does `probe_ts + window` arithmetic unguarded, as a caller naturally
        // would. With an `i64::MAX` probe this is an overflow panic in a debug-profile test run.
        resolution: Some(Box::new(move |sym: &str, ts: i64| {
            let window_end = ts + 86_400_000;
            (sym == SYM && window_end >= res_ts).then_some(1.0)
        })),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    let r = e.run();

    // it settled (so the sweep really did probe at the sentinel), stamped at the last event ts
    assert_eq!(e.core.settlements.len(), 1);
    assert_eq!(e.core.settlements[0].ts, last_bar_ts);
    assert_eq!(r.trades[0].exit_price, 1.0);
}

/// `is_resolved` is public read-only getter surface — TOTAL, never a panic on an unknown symbol.
/// A strategy calls it from inside `on_bar`/`on_fill`, where a panic aborts the whole backtest.
#[test]
fn is_resolved_is_total_for_unknown_symbols() {
    let bars = mk_bars(&[0.55, 0.60, 0.70]);
    let res_ts = T0 + STEP_MS;
    let strat = BuyAndRest { open_at: 0, size: 10.0, rest_limit: false, fills_seen: Vec::new() };
    let params = EngineParams {
        cash: 1_000.0,
        resolution: resolution_at(res_ts, 1.0),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], strat, params);
    e.run();

    // the registered symbol resolved...
    assert!(e.core.is_resolved(SYM));
    // ...and symbols this run never registered answer `false` rather than panicking
    assert!(!e.core.is_resolved("NEVER_REGISTERED"));
    assert!(!e.core.is_resolved(""));

    // and the same holds with NO resolution source configured (the `resolved` vec is empty)
    let bars2 = mk_bars(&[0.55, 0.60]);
    let strat2 = BuyAndRest { open_at: 0, size: 1.0, rest_limit: false, fills_seen: Vec::new() };
    let mut e2 = StrategyEngine::new(
        vec![(SYM.to_string(), bars2)],
        strat2,
        EngineParams { cash: 1_000.0, ..Default::default() },
    );
    e2.run();
    assert!(!e2.core.is_resolved(SYM));
    assert!(!e2.core.is_resolved("NEVER_REGISTERED"));
}
