//! Trigger-law wave 2 gates (law-map A2/A3/A4, backtest side):
//!
//! - **A3** — the protective stop (`SymbolState::stop`) fills through the ONE trigger oracle
//!   (`vike_model::order_fill_price`, Stop arm): a bar that OPENS through the stop fills at the
//!   gapped open (ADVERSE), exactly like a resting stop order; a non-gapping breach still fills
//!   at the stop, byte-identical to the old law.
//! - **A4** — the OCO sibling-cancel runs on EVERY lane: a protective-stop breach cancels the
//!   resting closing-side exits on the coarse and tick lanes too (previously granular-only), so
//!   a wide bar can no longer fire the stop AND the take-profit into a spurious reversed
//!   position.
//! - **A4, reverse direction** — the OTHER end of the same OCO pair: a closing-side limit's own
//!   fill leaves NO armed protective stop behind, so a later stop-less entry cannot inherit one.
//!   Already true via the fill fold rather than the OCO resolver — see the section comment.
//! - **A2** — the stop VERB: by default a resting `OrderKind::Stop` fills same-event at the
//!   trigger oracle's price (pinned here as the documented divergence from the live emulator,
//!   which releases a fired conditional as MARKET → fills the NEXT event); the opt-in
//!   `EngineParams::emulator_release_stops` mirrors the live law — on trigger the stop converts
//!   to a resting market child filled at the next event's price.

use vike_backtest::{EngineParams, StrategyEngine, Tick};
use vike_model::{Bar, Fill, Strategy, TradeTick};

const SYM: &str = "TEST";

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 100.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn trade(ts: i64, px: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price: px,
        size: 1.0,
        is_buyer_maker: false,
        symbol: SYM.to_string(),
    })
}

/// Costless params (slippage/fees 0) so fill prices assert exactly.
fn params() -> EngineParams {
    EngineParams { cash: 100_000.0, ..EngineParams::default() }
}

/// Enters long 1.0 with a protective stop at `stop` on bar 0 (fills bar 1 open); optionally
/// rests a TP sell limit at `tp` in the same breath. Collects fills.
struct BracketOnce {
    stop: f64,
    tp: Option<f64>,
    fills: Vec<Fill>,
}

impl Strategy<vike_backtest::SimBroker> for BracketOnce {
    fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, 1.0, 0.0, false, Some(self.stop));
            if let Some(tp) = self.tp {
                ctx.submit_limit(SYM, -1, 1.0, tp, 0.0, false, None);
            }
        }
    }
    fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

// ---- A3: protective-stop fill price through the oracle ----

#[test]
fn protective_stop_gap_open_fills_at_the_gapped_open() {
    // entry @100 (bar1 open), stop 95; bar2 GAPS open=92 below the stop.
    let bars = vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 102.0, 99.0, 101.0),
        bar(3_000, 92.0, 93.0, 90.0, 91.0),
    ];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        BracketOnce { stop: 95.0, tp: None, fills: Vec::new() },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + stop close: {fills:?}");
    assert_eq!(fills[0].price, 100.0, "entry at bar1 open");
    // the oracle's Stop arm: min(stop, open) = min(95, 92) — the whole gap is slippage.
    // (The retired law booked 95 here, under-reporting the gap.)
    assert_eq!(fills[1].price, 92.0, "protective stop fills at the gapped open");
    assert_eq!(fills[1].ts, 3_000);
    assert_eq!(eng.core.position_of(SYM).size, 0.0);
}

#[test]
fn protective_stop_non_gap_breach_still_fills_at_the_stop() {
    // bar2 opens ABOVE the stop and trades down through it: fill exactly at 95 (old law kept).
    let bars = vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 102.0, 99.0, 101.0),
        bar(3_000, 98.0, 99.0, 94.0, 96.0),
    ];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        BracketOnce { stop: 95.0, tp: None, fills: Vec::new() },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!(fills[1].price, 95.0, "no gap ⇒ byte-identical to the old at-stop fill");
}

// ---- A4: OCO sibling-cancel on the coarse lane ----

#[test]
fn coarse_lane_wide_bar_cannot_fill_both_stop_and_tp() {
    // The law-map A4 reproducer: stop 95 + TP 110, bar2 spans BOTH (high 115, low 90).
    // The stop (checked before the fill phase) flattens; the TP must CANCEL, not fill a
    // spurious reversed short on the flat book.
    let bars = vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 102.0, 99.0, 101.0),
        bar(3_000, 100.0, 115.0, 90.0, 100.0),
        bar(4_000, 100.0, 101.0, 99.0, 100.0),
    ];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        BracketOnce { stop: 95.0, tp: Some(110.0), fills: Vec::new() },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + stop only — the TP was OCO-canceled: {fills:?}");
    assert_eq!(fills[1].price, 95.0, "the stop's fill (bar opens above it)");
    assert_eq!(eng.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(eng.core.pending_of(SYM).is_empty(), "the TP left the book");
}

// ---- A3 + A4 on the tick lane ----

/// Tick twin of [`BracketOnce`]: enters on the first trade tick.
struct BracketOnceTicks {
    stop: f64,
    tp: Option<f64>,
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<vike_backtest::SimBroker> for BracketOnceTicks {
    fn on_trade_tick(&mut self, ctx: &mut vike_backtest::SimBroker, _t: &TradeTick) {
        if !self.armed {
            self.armed = true;
            ctx.submit(SYM, 1, 1.0, 0.0, false, Some(self.stop));
            if let Some(tp) = self.tp {
                ctx.submit_limit(SYM, -1, 1.0, tp, 0.0, false, None);
            }
        }
    }
    fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn tick_lane_protective_stop_fills_at_the_tick_and_cancels_the_tp() {
    // t1 arms; t2 fills the entry @100; t3 prints 92 — a one-price bar through the stop, so the
    // oracle fills at the PRINT (92), not the stop level, and the TP cancels; t4 prints 111,
    // which previously filled the orphaned TP into a spurious short.
    let ticks = vec![(
        SYM.to_string(),
        vec![trade(1_000, 100.0), trade(2_000, 100.0), trade(3_000, 92.0), trade(4_000, 111.0)],
    )];
    let bars = vec![bar(1_000, 100.0, 100.0, 100.0, 100.0)]; // registration series (unused by ticks)
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        BracketOnceTicks { stop: 95.0, tp: Some(110.0), armed: false, fills: Vec::new() },
        params(),
    );
    eng.run_ticks(&ticks);
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + stop only: {fills:?}");
    assert_eq!(fills[0].price, 100.0, "entry at t2's print");
    assert_eq!(fills[1].price, 92.0, "stop fills at the through-print, not the stop level");
    assert_eq!(fills[1].ts, 3_000);
    assert_eq!(eng.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(eng.core.pending_of(SYM).is_empty(), "the TP left the book");
}

// ---- A4, the REVERSE direction: a TP fill disarms the protective stop ----
//
// UNPINNED, NOT UNIMPLEMENTED. `SimBroker::apply_fill` has always cleared `SymbolState::stop` on
// a `FillKind::Close`/`Flip` step (`sim_broker.rs`, predating the TradeFold extraction in #377) —
// so the exit->stop OCO direction already matched the paper book, and matched it MORE broadly
// (any closing fill, not only a bracket leg). What was missing was a test saying so: the wave-2
// review read `check_stop`'s one-directional `cancel_protective_exits` as the whole law and
// reported a dormant-stop revival bug that the fold layer had already closed. These rows pin it
// from the outside, so the next reader does not have to re-derive it from the fold.
//
// The residual the kind-linked law deliberately does NOT cover: a resting `OrderKind::Stop`
// ORDER (as opposed to the implicit `SymbolState::stop` level) is not canceled when a TP fills,
// because `is_protective_exit_sibling` excludes stop kinds — a closing-side stop order is
// equally likely a breakout ENTRY, and an id-less book cannot tell the two apart. Closing that
// needs coid linkage (`ContingencyBook`), which is the paper book's half of the resolver.

/// Enters long `qty` with a protective stop on the first bar, rests a TP sell limit of `tp_qty`,
/// then re-enters long 1.0 with NO stop on bar `reentry_idx` — the revival probe.
struct TpThenReentry {
    stop: f64,
    tp: f64,
    tp_qty: f64,
    qty: f64,
    reentry_idx: usize,
    fills: Vec<Fill>,
}

impl Strategy<vike_backtest::SimBroker> for TpThenReentry {
    fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit(SYM, 1, self.qty, 0.0, false, Some(self.stop));
            ctx.submit_limit(SYM, -1, self.tp_qty, self.tp, 0.0, false, None);
        }
        if ctx.index == self.reentry_idx {
            ctx.submit(SYM, 1, 1.0, 0.0, false, None);
        }
    }
    fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn tp_fill_disarms_the_protective_stop_and_no_entry_inherits_it() {
    // entry @100 (bar1 open), stop 95, TP 110. bar2 high 112 fills the TP flat; bar3 re-enters
    // long WITHOUT a stop; bar4 trades down to 90 — through the ORPHANED 95 level.
    //
    // The failure this forbids: a TP fill leaving `SymbolState::stop = Some(95)` armed on the flat
    // book, silently inherited by the stop-less re-entry and fired on bar 6 — a protective level
    // the strategy never asked for on that position.
    let bars = vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 102.0, 99.0, 101.0),
        bar(3_000, 100.0, 112.0, 99.0, 111.0), // TP 110 fills -> flat
        bar(4_000, 100.0, 101.0, 99.0, 100.0), // re-entry submitted here, fills bar 5
        bar(5_000, 100.0, 101.0, 99.0, 100.0),
        bar(6_000, 98.0, 99.0, 90.0, 91.0), // crosses the orphaned 95
    ];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        TpThenReentry {
            stop: 95.0,
            tp: 110.0,
            tp_qty: 1.0,
            qty: 1.0,
            reentry_idx: 3,
            fills: Vec::new(),
        },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 3, "entry + TP + re-entry only — no revived stop fill: {fills:?}");
    assert_eq!(fills[1].price, 110.0, "the TP filled");
    assert_eq!(fills[2].ts, 5_000, "the stop-less re-entry");
    // `protective_stop_of(SYM) == None` is NOT the load-bearing assert below: if `fold_fill`'s
    // `FillKind::Close => st.stop = None` (sim_broker.rs) were removed, the orphaned 95 level
    // would still fire on bar 6, and `check_protective_stops`'s own `self.core.sym[si].stop =
    // None` (engine.rs) would clear it right there — so this line would pass either way. What
    // actually catches that regression is `fills.len() == 3` above and `position_of(SYM).size`
    // below: a revived stop adds a 4th fill and flattens the re-entry to 0.0. Do not trim those
    // two as redundant with this one.
    assert_eq!(eng.core.protective_stop_of(SYM), None, "the TP fill disarmed the stop");
    assert_eq!(eng.core.position_of(SYM).size, 1.0, "the re-entry survived bar 6 unprotected");
}

#[test]
fn a_partial_tp_fill_leaves_the_stop_armed() {
    // The conservative guard: a TP that closes only HALF the position must not strip the
    // protection from the half still open. Entry long 2, TP sells 1 at 110, stop 95 stays armed
    // and fires on the later break.
    let bars = vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 102.0, 99.0, 101.0),
        bar(3_000, 100.0, 112.0, 99.0, 111.0), // TP fills 1 of 2
        bar(4_000, 98.0, 99.0, 94.0, 96.0),    // breaches 95 with 1.0 still open
    ];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        TpThenReentry {
            stop: 95.0,
            tp: 110.0,
            tp_qty: 1.0,
            qty: 2.0,
            reentry_idx: usize::MAX,
            fills: Vec::new(),
        },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 3, "entry + partial TP + protective stop: {fills:?}");
    assert_eq!(fills[2].price, 95.0, "the remaining half was still protected");
    assert_eq!(eng.core.position_of(SYM).size, 0.0);
    assert_eq!(eng.core.protective_stop_of(SYM), None, "and disarmed once it fired");
}

/// Tick twin of the disarm: the closing-side limit fills on the tick lane and the stop goes.
#[test]
fn tick_lane_tp_fill_disarms_the_protective_stop() {
    let ticks = vec![(
        SYM.to_string(),
        vec![trade(1_000, 100.0), trade(2_000, 100.0), trade(3_000, 111.0), trade(4_000, 90.0)],
    )];
    let bars = vec![bar(1_000, 100.0, 100.0, 100.0, 100.0)];
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        BracketOnceTicks { stop: 95.0, tp: Some(110.0), armed: false, fills: Vec::new() },
        params(),
    );
    eng.run_ticks(&ticks);
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + TP: {fills:?}");
    assert_eq!(fills[1].price, 111.0, "the TP filled at the crossing print (oracle Limit arm)");
    assert_eq!(eng.core.protective_stop_of(SYM), None, "the stop disarmed on the tick lane too");
    assert_eq!(eng.core.position_of(SYM).size, 0.0);
}

// ---- A2: the stop VERB — default divergence pinned; opt-in emulator mirror ----

/// Rests a buy stop (breakout) at `trigger` on bar 0; collects fills.
struct StopVerbOnce {
    trigger: f64,
    fills: Vec<Fill>,
}

impl Strategy<vike_backtest::SimBroker> for StopVerbOnce {
    fn on_bar(&mut self, ctx: &mut vike_backtest::SimBroker, _bar: &Bar) {
        if ctx.index == 0 {
            ctx.submit_stop(SYM, 1, 1.0, self.trigger, 0.0, false);
        }
    }
    fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn stop_verb_bars() -> Vec<Bar> {
    vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 104.0, 106.0, 103.0, 105.0), // triggers the 105 buy stop (high 106)
        bar(3_000, 107.0, 108.0, 106.0, 107.0),
    ]
}

#[test]
fn stop_verb_default_fills_same_bar_at_trigger_the_pinned_divergence() {
    // DEFAULT (emulator_release_stops = false): the resting stop fills ON the trigger bar at the
    // oracle's price — max(trigger, open) = 105. This is the DOCUMENTED divergence from the live
    // law (law-map A2): live always emulator-arms the stop verb and releases a fired conditional
    // as a MARKET, which fills the NEXT bar's open (107 here). Do not "fix" this default —
    // backtests keep the historical same-bar semantics unless the knob below opts in.
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), stop_verb_bars())],
        StopVerbOnce { trigger: 105.0, fills: Vec::new() },
        params(),
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].ts, 2_000, "same-bar fill on the trigger bar");
    assert_eq!(fills[0].price, 105.0, "at the trigger (oracle Stop-arm price)");
}

#[test]
fn stop_verb_released_as_market_fills_next_bar_open_with_the_knob() {
    // OPT-IN (emulator_release_stops = true): the trigger converts the stop to a resting market
    // child — the live emulator's release law — so the fill lands on the NEXT bar at its open.
    let p = EngineParams { emulator_release_stops: true, ..params() };
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), stop_verb_bars())],
        StopVerbOnce { trigger: 105.0, fills: Vec::new() },
        p,
    );
    eng.run();
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].ts, 3_000, "released market fills the NEXT bar");
    assert_eq!(fills[0].price, 107.0, "at that bar's open");
}

#[test]
fn stop_verb_release_on_the_tick_lane_fills_the_next_tick() {
    // Tick twin: trigger print converts the stop; the NEXT print fills it at its price.
    struct StopVerbTicks {
        armed: bool,
        fills: Vec<Fill>,
    }
    impl Strategy<vike_backtest::SimBroker> for StopVerbTicks {
        fn on_trade_tick(&mut self, ctx: &mut vike_backtest::SimBroker, _t: &TradeTick) {
            if !self.armed {
                self.armed = true;
                ctx.submit_stop(SYM, 1, 1.0, 105.0, 0.0, false);
            }
        }
        fn on_fill(&mut self, _ctx: &mut vike_backtest::SimBroker, fill: &Fill) {
            self.fills.push(fill.clone());
        }
    }
    let ticks = vec![(
        SYM.to_string(),
        vec![trade(1_000, 100.0), trade(2_000, 106.0), trade(3_000, 104.0)],
    )];
    let bars = vec![bar(1_000, 100.0, 100.0, 100.0, 100.0)];
    let p = EngineParams { emulator_release_stops: true, ..params() };
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        StopVerbTicks { armed: false, fills: Vec::new() },
        p,
    );
    eng.run_ticks(&ticks);
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].ts, 3_000, "released market fills the tick AFTER the trigger print");
    assert_eq!(fills[0].price, 104.0, "at that tick's price, not the trigger's");
}
