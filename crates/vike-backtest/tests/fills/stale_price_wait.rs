//! Gate for the opt-in stale-price wait discipline (`EngineParams::max_price_staleness_ms`,
//! `vike_backtest::staleness`) — the LEAN `FutureFillModel` analog.
//!
//! What is proven here:
//! 1. `None` (the default) reproduces the exact PRE-feature outcome on a fill-forwarded tape —
//!    the order fills on the stale bar and nothing is deferred. (The byte-identity of `None` is
//!    established by the untouched parity/golden suites, which all run with `None`; a `None`-vs-
//!    `Default` comparison would run the same code path twice and prove nothing.)
//! 2. Fills against fresh data are unchanged when the discipline is ON — on the plain coarse
//!    lane AND on the shared-cash gated lane, where a spurious deferral would also reshuffle the
//!    gate's `seq` tie-break for the other symbols.
//! 3. A market order that would resolve against a stale (fill-forwarded, zero-volume, quote-less)
//!    bar is DEFERRED — it keeps resting, is never lost — and then fills on the next fresh print,
//!    at THAT print's price, not the stale one. Same for `MarketClose` (a deferred EXIT).
//! 4. Price-conditional kinds (limit) are never gated.
//! 5. Per-symbol independence: a stale symbol defers without disturbing a fresh symbol's fill.
//! 6. Granular sub-bars are FIRST-CLASS prints: a fully-printing granular step never defers, and
//!    a quiet stretch within a step defers and then fills on the next printing sub-bar.
//! 7. The tick lane is inert by construction (each event is the symbol's own fresh tick) —
//!    including a zero-size trade tick, which is a transaction and therefore a print.
//!
//! Bar shape convention below: `volume > 0.0` = a real print; `volume == 0.0` with no bid/ask =
//! the fill-forward shape this discipline refuses.

use vike_backtest::{EngineParams, FillModelKind, SimBroker, StrategyEngine, Tick};
use vike_model::{Bar, QuoteTick, Strategy, TradeTick};

const T0: i64 = 1_700_000_000_000;
const MIN: i64 = 60_000;

/// `(price, volume)` pairs → a one-symbol bar series on a 1-minute grid.
fn series(symbol: &str, pv: &[(f64, f64)]) -> Vec<Bar> {
    pv.iter()
        .enumerate()
        .map(|(i, &(p, v))| Bar {
            ts: T0 + i as i64 * MIN,
            open: p,
            high: p,
            low: p,
            close: p,
            volume: v,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(symbol.to_string()),
        })
        .collect()
}

/// Submits ONE market buy on bar `at`; the fill lands on a later bar's open.
struct BuyOnce {
    symbol: String,
    at: usize,
    size: f64,
}

impl Strategy<SimBroker> for BuyOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.at {
            ctx.submit(&self.symbol, 1, self.size, 0.0, true, None);
        }
    }
}

/// Submits ONE resting buy LIMIT on bar `at` — a price-conditional kind, never gated.
struct LimitOnce {
    symbol: String,
    at: usize,
    price: f64,
    size: f64,
}

impl Strategy<SimBroker> for LimitOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.at {
            ctx.submit_limit(&self.symbol, 1, self.size, self.price, 0.0, true, None);
        }
    }
}

/// Tick-lane twin of [`BuyOnce`] — `on_bar` never fires under `run_ticks`, so the market order is
/// sent from the first tick handler instead.
struct BuyOnFirstTick {
    symbol: String,
    size: f64,
    sent: bool,
}

impl Strategy<SimBroker> for BuyOnFirstTick {
    fn on_trade_tick(&mut self, ctx: &mut SimBroker, _t: &TradeTick) {
        if !self.sent {
            self.sent = true;
            ctx.submit(&self.symbol, 1, self.size, 0.0, true, None);
        }
    }
}

fn params(staleness: Option<i64>) -> EngineParams {
    EngineParams { cash: 1_000_000.0, max_price_staleness_ms: staleness, ..Default::default() }
}

/// The canonical sparse tape: a print at idx0, then THREE fill-forwarded (zero-volume) bars at a
/// drifted price, then a real print again. A market order submitted on idx0 would fill at idx1's
/// stale 101.0 without the discipline, and at idx4's fresh 110.0 with it.
fn sparse_tape(sym: &str) -> Vec<(String, Vec<Bar>)> {
    vec![(
        sym.to_string(),
        series(sym, &[(100.0, 5.0), (101.0, 0.0), (101.0, 0.0), (101.0, 0.0), (110.0, 7.0)]),
    )]
}

#[test]
fn default_none_reproduces_the_pre_feature_stale_fill() {
    // NB this is NOT the inertness proof — `params(None)` and the struct default are the SAME
    // code path, so comparing them only pins determinism. The inertness evidence is (a) the
    // untouched parity/golden suites, which all run with `None`, and (b) the assertions below,
    // which pin the exact PRE-feature outcome: the market order fills on the STALE idx1 bar at
    // 101.0 and nothing is ever deferred. If `None` ever started deferring, those go red.
    let sym = "SPARSE";
    let run = |p: EngineParams| {
        let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
        let mut e = StrategyEngine::new(sparse_tape(sym), strat, p);
        let r = e.run();
        (r, e.core.position_of(sym).avg_price, e.core.stale_deferrals)
    };
    let (explicit_none, none_px, none_deferrals) = run(params(None));
    let (defaulted, _, _) = run(EngineParams { cash: 1_000_000.0, ..Default::default() });

    // the pre-feature behavior itself: filled on the STALE idx1 bar, and no gate ever engaged
    assert_eq!(none_px, 101.0, "None must still fill against the fill-forwarded idx1 bar");
    assert_eq!(none_deferrals, 0, "None must never defer");
    assert_eq!(explicit_none.stale_deferrals, 0, "and the result mirrors that");
    assert_eq!(explicit_none.n_trades, 0, "position still open — no closed trade");

    // determinism: explicit `None` and the struct default agree bit-for-bit
    assert_eq!(explicit_none.final_equity.to_bits(), defaulted.final_equity.to_bits());
    assert_eq!(explicit_none.equity_curve.len(), defaulted.equity_curve.len());
    for (a, b) in explicit_none.equity_curve.iter().zip(&defaulted.equity_curve) {
        assert_eq!(a.to_bits(), b.to_bits(), "equity curve must be bit-identical");
    }
}

#[test]
fn stale_market_order_defers_then_fills_on_the_next_fresh_print() {
    let sym = "SPARSE";
    // 90s bound: idx1..idx3 are 60/120/180s past the idx0 print. idx1 (age 60_000) is NOT stale
    // under a 90s bound, so use a tight bound instead to make every forward-filled bar stale.
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(sparse_tape(sym), strat, params(Some(0)));
    let out = eng.run();

    // The order deferred on idx1, idx2 and idx3 (three fill-forwarded bars) and filled at idx4.
    assert_eq!(eng.core.stale_deferrals, 3, "one deferral per fill-forwarded bar");
    assert_eq!(eng.core.position_of("SPARSE").size, 1.0, "filled exactly once");
    // Filled at the FRESH print's price (110.0), never at the stale 101.0.
    assert_eq!(eng.core.position_of("SPARSE").avg_price, 110.0);
    assert!(out.final_equity.is_finite());
}

#[test]
fn fresh_data_fills_are_unchanged_when_the_discipline_is_on() {
    let sym = "LIQUID";
    // every bar prints — the discipline must not alter a single fill
    let tape = || vec![(sym.to_string(), series(sym, &[(100.0, 5.0), (101.0, 5.0), (102.0, 5.0)]))];
    let run = |st: Option<i64>| {
        let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
        let mut e = StrategyEngine::new(tape(), strat, params(st));
        let r = e.run();
        (r, e.core.position_of(sym).avg_price, e.core.stale_deferrals)
    };
    let (off, off_px, _) = run(None);
    let (on, on_px, deferrals) = run(Some(0));
    assert_eq!(deferrals, 0, "a printing tape never defers, even at the tightest bound");
    assert_eq!(off_px.to_bits(), on_px.to_bits(), "same fill price");
    assert_eq!(off.final_equity.to_bits(), on.final_equity.to_bits());
}

#[test]
fn bound_tolerates_staleness_up_to_the_limit() {
    let sym = "SPARSE";
    // idx1 is exactly 60_000ms past the idx0 print. A 60_000 bound is NOT exceeded (strict `>`),
    // so the order fills there at 101.0 exactly as it would with the discipline off.
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(sparse_tape(sym), strat, params(Some(MIN)));
    eng.run();
    assert_eq!(eng.core.stale_deferrals, 0);
    assert_eq!(eng.core.position_of(sym).avg_price, 101.0);

    // One millisecond tighter and the same bar is stale.
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(sparse_tape(sym), strat, params(Some(MIN - 1)));
    eng.run();
    assert!(eng.core.stale_deferrals > 0);
    assert_eq!(eng.core.position_of(sym).avg_price, 110.0);
}

#[test]
fn resting_limit_is_never_gated_by_staleness() {
    let sym = "SPARSE";
    // A buy limit at 101.0 crosses on the STALE idx1 bar. Price-conditional kinds pass through:
    // a repeated price cannot spuriously satisfy a condition it did not already satisfy, and
    // gating them would strand protective exits.
    let strat = LimitOnce { symbol: sym.into(), at: 0, price: 101.0, size: 1.0 };
    let mut eng = StrategyEngine::new(sparse_tape(sym), strat, params(Some(0)));
    eng.run();
    assert_eq!(eng.core.stale_deferrals, 0, "a limit order never counts as a deferral");
    assert_eq!(eng.core.position_of(sym).size, 1.0, "the limit filled on the stale bar");
}

#[test]
fn symbol_that_never_printed_keeps_waiting() {
    let sym = "QUIET";
    // No bar in this tape is a print (all zero-volume, no quotes) — there is no evidence of a
    // tradeable price at all, so the market order waits forever rather than inventing a fill.
    let tape = vec![(sym.to_string(), series(sym, &[(100.0, 0.0), (100.0, 0.0), (100.0, 0.0)]))];
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(tape, strat, params(Some(10 * MIN)));
    eng.run();
    assert_eq!(eng.core.position_of(sym).size, 0.0, "no fill without a print");
    assert!(eng.core.stale_deferrals >= 1, "and the run says so instead of silently doing nothing");
    assert!(!eng.core.price_is_stale("UNKNOWN"), "unknown symbol is total, answers false");
    assert!(eng.core.price_is_stale(sym));
}

// --- the granular sub-bar lane (`fill_pending_granular`) -----------------------------------

const HOUR: i64 = 3_600_000;

/// One flat bar at an explicit ts (open=high=low=close), so a fill price is unambiguous.
fn bar_at(symbol: &str, ts: i64, price: f64, volume: f64) -> Bar {
    Bar {
        ts,
        open: price,
        high: price,
        low: price,
        close: price,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

/// One `(symbol, bars)` series as the engine takes it — named so the two-series return below
/// stays under clippy's `type_complexity` bar.
type Series = Vec<(String, Vec<Bar>)>;

/// Coarse hourly bars (all printing) + sub-bars OFFSET from the coarse ts, so the sub-bar's own
/// print evidence is the only thing that can keep the age at 0.
fn granular_tape(sym: &str, sub_volume: f64) -> (Series, Series) {
    let coarse: Vec<Bar> =
        (0..3i64).map(|i| bar_at(sym, T0 + i * HOUR, 100.0 * (i + 1) as f64, 5.0)).collect();
    let mut subs: Vec<Bar> = Vec::new();
    for step in 0..3i64 {
        for k in 1..=3i64 {
            // deliberately NOT aligned to the coarse ts: the first sub-bar of a step is 15m in
            let price = 100.0 * (step + 1) as f64 + k as f64;
            subs.push(bar_at(sym, T0 + step * HOUR + k * 900_000, price, sub_volume));
        }
    }
    (vec![(sym.to_string(), coarse)], vec![(sym.to_string(), subs)])
}

#[test]
fn granular_sub_bars_are_first_class_prints() {
    // Every sub-bar carries volume, so a fully-printing granular step must NEVER defer — even
    // though each sub-bar is 15m/30m/45m past its COARSE bar's ts and the bound is 60s. This is
    // the regression guard for measuring age at `sub.ts` while only recording the coarse print:
    // that combination defers every sub-bar of every step and the order never fills at all.
    let sym = "GRAN";
    let (coarse, subs) = granular_tape(sym, 1.0);
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(
        coarse,
        strat,
        EngineParams {
            cash: 1_000_000.0,
            max_price_staleness_ms: Some(MIN),
            granular_by_symbol: subs,
            ..Default::default()
        },
    );
    eng.run();
    assert_eq!(eng.core.stale_deferrals, 0, "a printing granular tape never defers");
    assert_eq!(eng.core.position_of(sym).size, 1.0, "the order filled");
    // submitted in on_bar at step 0 (which runs AFTER the fill phase), so it first meets the fill
    // lane at step 1 -> that step's FIRST sub-bar, priced 200 + 1
    assert_eq!(eng.core.position_of(sym).avg_price, 201.0);
}

#[test]
fn quiet_granular_stretch_defers_then_fills_on_the_next_printing_sub_bar() {
    // Same shape, but the sub-bars are zero-volume quote-less (the fill-forward shape) for the
    // step the order first meets. The order must not be lost: it keeps resting and fills on the
    // first sub-bar that IS a print.
    let sym = "GRAN";
    let (coarse, mut subs) = granular_tape(sym, 1.0);
    // blank out step 1's three sub-bars (ts in [T0+HOUR, T0+2*HOUR))
    for b in &mut subs[0].1 {
        if b.ts >= T0 + HOUR && b.ts < T0 + 2 * HOUR {
            b.volume = 0.0;
        }
    }
    let strat = BuyOnce { symbol: sym.into(), at: 0, size: 1.0 };
    let mut eng = StrategyEngine::new(
        coarse,
        strat,
        EngineParams {
            cash: 1_000_000.0,
            max_price_staleness_ms: Some(MIN),
            granular_by_symbol: subs,
            ..Default::default()
        },
    );
    eng.run();
    assert_eq!(eng.core.stale_deferrals, 3, "one deferral per quiet sub-bar of step 1");
    assert_eq!(eng.core.position_of(sym).size, 1.0, "deferred, NOT lost — it filled later");
    // step 2's first sub-bar prints again: 300 + 1
    assert_eq!(eng.core.position_of(sym).avg_price, 301.0);
}

// --- the shared-cash gated lane (`fill_step_gated`) -----------------------------------------

/// Market-buys BOTH symbols once, on bar `at`, at equal weight (so the gate's weight-desc sort
/// falls through to the trigger-order tie-break — the ordering finding 2 flags).
///
/// NB `on_bar` fans out ONCE PER SYMBOL per step (engine.rs `_on_step`), so a bare `index == at`
/// guard would submit each order twice on a two-symbol tape and double every deferral count. The
/// `sent` latch makes it exactly one order per symbol.
struct BuyBothOnce {
    a: String,
    b: String,
    at: usize,
    size: f64,
    sent: bool,
}

impl Strategy<SimBroker> for BuyBothOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == self.at && !self.sent {
            self.sent = true;
            ctx.submit(&self.a, 1, self.size, 0.0, true, None);
            ctx.submit(&self.b, 1, self.size, 0.0, true, None);
        }
    }
}

fn gated_params(staleness: Option<i64>) -> EngineParams {
    EngineParams {
        cash: 1_000_000.0,
        cash_gate: true,
        max_price_staleness_ms: staleness,
        ..Default::default()
    }
}

#[test]
fn cash_gated_lane_is_unchanged_when_every_symbol_prints() {
    // Equal weights, both symbols fresh on every bar: the discipline must not perturb the gate's
    // fill set or its deterministic ordering (the deferral check sits BEFORE the open/free
    // classification and the `seq` counter, so a spurious deferral would reshuffle the tie-break).
    let fresh = || {
        vec![
            ("AAA".to_string(), series("AAA", &[(100.0, 5.0), (101.0, 5.0), (102.0, 5.0)])),
            ("BBB".to_string(), series("BBB", &[(200.0, 5.0), (201.0, 5.0), (202.0, 5.0)])),
        ]
    };
    let run = |st: Option<i64>| {
        let strat = BuyBothOnce { a: "AAA".into(), b: "BBB".into(), at: 0, size: 1.0, sent: false };
        let mut e = StrategyEngine::new(fresh(), strat, gated_params(st));
        let r = e.run();
        let px = (e.core.position_of("AAA").avg_price, e.core.position_of("BBB").avg_price);
        (r, px, e.core.stale_deferrals)
    };
    let (off, off_px, _) = run(None);
    let (on, on_px, deferrals) = run(Some(0));
    assert_eq!(deferrals, 0, "an all-printing tape never defers, even at the tightest bound");
    assert_eq!(off_px, on_px, "same fills at the same prices");
    assert_eq!(off.final_equity.to_bits(), on.final_equity.to_bits());
    for (a, b) in off.equity_curve.iter().zip(&on.equity_curve) {
        assert_eq!(a.to_bits(), b.to_bits());
    }
}

#[test]
fn cash_gated_stale_symbol_defers_without_disturbing_the_fresh_one() {
    // AAA goes fill-forwarded for three bars, BBB prints throughout. Per-symbol independence:
    // BBB must fill on its own idx1 bar exactly as it does with the discipline off, while AAA
    // waits and fills on its own next real print.
    let tape = || {
        vec![
            (
                "AAA".to_string(),
                series(
                    "AAA",
                    &[(100.0, 5.0), (101.0, 0.0), (101.0, 0.0), (101.0, 0.0), (110.0, 7.0)],
                ),
            ),
            (
                "BBB".to_string(),
                series(
                    "BBB",
                    &[(200.0, 5.0), (201.0, 5.0), (202.0, 5.0), (203.0, 5.0), (204.0, 5.0)],
                ),
            ),
        ]
    };
    let run = |st: Option<i64>| {
        let strat = BuyBothOnce { a: "AAA".into(), b: "BBB".into(), at: 0, size: 1.0, sent: false };
        let mut e = StrategyEngine::new(tape(), strat, gated_params(st));
        e.run();
        (
            e.core.position_of("AAA").avg_price,
            e.core.position_of("BBB").avg_price,
            e.core.stale_deferrals,
        )
    };
    let (_, off_bbb, _) = run(None);
    let (on_aaa, on_bbb, deferrals) = run(Some(0));
    assert_eq!(deferrals, 3, "one deferral per fill-forwarded AAA bar; BBB never defers");
    assert_eq!(on_bbb.to_bits(), off_bbb.to_bits(), "the fresh symbol's fill is untouched");
    assert_eq!(on_bbb, 201.0, "BBB filled on its own idx1 bar");
    assert_eq!(on_aaa, 110.0, "AAA waited and filled on its own next real print");
}

/// A deferred EXIT is the highest-consequence case (a position that cannot be closed on a sparse
/// tape), so pin that `MarketClose` defers like `Market` and is not lost.
struct BuyThenCloseOnce {
    symbol: String,
    size: f64,
    closed: bool,
}

impl Strategy<SimBroker> for BuyThenCloseOnce {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(&self.symbol, 1, self.size, 0.0, true, None);
        } else if !self.closed && ctx.position_of(&self.symbol).size > 0.0 {
            self.closed = true;
            ctx.submit_market_close(&self.symbol, -1, self.size, 0.0, true);
        }
    }
}

#[test]
fn market_close_defers_and_is_not_lost() {
    let sym = "SPARSE2";
    // print, print (entry fills here), then three fill-forwarded bars, then a print.
    let tape = vec![(
        sym.to_string(),
        series(
            sym,
            &[(100.0, 5.0), (100.0, 5.0), (101.0, 0.0), (101.0, 0.0), (101.0, 0.0), (110.0, 7.0)],
        ),
    )];
    let strat = BuyThenCloseOnce { symbol: sym.into(), size: 1.0, closed: false };
    let mut eng = StrategyEngine::new(tape, strat, params(Some(0)));
    eng.run();
    // entry filled at idx1 (100.0, a print); the close is submitted from idx1's on_bar and first
    // meets the fill lane at idx2 -> deferred across idx2/idx3/idx4, filled at idx5's print.
    assert_eq!(eng.core.stale_deferrals, 3, "MarketClose defers on every stale bar");
    assert_eq!(eng.core.position_of(sym).size, 0.0, "the exit was NOT lost — it closed");
    assert_eq!(eng.core.trades.len(), 1, "exactly one closed trade");
    assert_eq!(
        eng.core.trades[0].exit_price, 110.0,
        "closed at the fresh print, not the stale one"
    );
}

/// The tick lane: each fill event is the symbol's own just-arrived quote/trade, hence a fresh
/// print with age 0 — the discipline is inert there BY CONSTRUCTION, which is the correct
/// outcome (the tick path already waits for fresh data structurally).
#[test]
fn tick_lane_is_inert_by_construction() {
    let sym = "TICKY";
    let ticks: Vec<Tick> = (0..4i64)
        .map(|i| {
            if i % 2 == 0 {
                Tick::Trade(TradeTick {
                    symbol: sym.to_string(),
                    ts: T0 + i * MIN,
                    local_ts: T0 + i * MIN,
                    price: 100.0 + i as f64,
                    size: 1.0,
                    is_buyer_maker: false,
                })
            } else {
                Tick::Quote(QuoteTick {
                    symbol: sym.to_string(),
                    ts: T0 + i * MIN,
                    local_ts: T0 + i * MIN,
                    bid: 100.0 + i as f64 - 0.5,
                    ask: 100.0 + i as f64 + 0.5,
                    bid_size: 10.0,
                    ask_size: 10.0,
                })
            }
        })
        .collect();
    let run = |st: Option<i64>| {
        let strat = BuyOnFirstTick { symbol: sym.into(), size: 1.0, sent: false };
        let mut e = StrategyEngine::new(
            vec![(sym.to_string(), Vec::new())],
            strat,
            EngineParams {
                cash: 1_000_000.0,
                fill_model: FillModelKind::Tick,
                max_price_staleness_ms: st,
                ..Default::default()
            },
        );
        let r = e.run_ticks(&[(sym.to_string(), ticks.clone())]);
        (r.final_equity, e.core.stale_deferrals)
    };
    let (off, _) = run(None);
    let (on, deferrals) = run(Some(0));
    assert_eq!(deferrals, 0, "quote and trade ticks are both fresh prints");
    assert_eq!(off.to_bits(), on.to_bits(), "tick lane byte-identical with the gate on");
}

#[test]
fn zero_size_trade_ticks_are_still_prints_so_the_tick_lane_stays_inert() {
    // Some venues emit zero-size / index prints; `trade_tick_to_bar` projects those to
    // `volume == 0.0` with no quote, which is the fill-forward SHAPE. Freshness on the tick lane
    // is therefore decided from the tick KIND — a Trade is a transaction regardless of size — so
    // "inert by construction" holds rather than depending on the projection's volume.
    let sym = "ZEROSZ";
    let ticks: Vec<Tick> = (0..4i64)
        .map(|i| {
            Tick::Trade(TradeTick {
                symbol: sym.to_string(),
                ts: T0 + i * MIN,
                local_ts: T0 + i * MIN,
                price: 100.0 + i as f64,
                size: 0.0, // <- the hazard
                is_buyer_maker: false,
            })
        })
        .collect();
    let run = |st: Option<i64>| {
        let strat = BuyOnFirstTick { symbol: sym.into(), size: 1.0, sent: false };
        let mut e = StrategyEngine::new(
            vec![(sym.to_string(), Vec::new())],
            strat,
            EngineParams {
                cash: 1_000_000.0,
                fill_model: FillModelKind::Tick,
                max_price_staleness_ms: st,
                ..Default::default()
            },
        );
        let r = e.run_ticks(&[(sym.to_string(), ticks.clone())]);
        (r.final_equity, e.core.stale_deferrals, e.core.position_of(sym).size)
    };
    let (off, _, off_size) = run(None);
    let (on, deferrals, on_size) = run(Some(0));
    assert_eq!(deferrals, 0, "a zero-size trade tick is still a transaction, hence a print");
    assert_eq!(off_size, on_size, "same fill outcome");
    assert_eq!(off.to_bits(), on.to_bits(), "tick lane byte-identical with the gate on");
}
