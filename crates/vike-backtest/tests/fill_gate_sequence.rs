//! The per-order gate sequence is ONE sequence, shared by the frozen and the queued tick lanes.
//!
//! `StrategyEngine::gate_pending` is the single definition of session → staleness → emulator
//! stop-release → trigger price. Before it existed, the frozen `fill_pending_tick` and the opt-in
//! queued twin (`engine::queued::fill_pending_tick_queued`) each spelled that sequence inline,
//! ~700 lines apart, and every discipline added to one had to be hand-copied into the other — the
//! session and staleness gates each landed twice for exactly that reason.
//!
//! What this gate pins is the property that makes the sharing checkable rather than assumed:
//!
//! **For TAKER-only order flow the queued lane must be identical to the frozen lane.** The queue
//! model gates resting LIMIT / LIMIT-CLOSE orders only (`queue_kind` — a priced limit); a market
//! or stop order crosses the book and never rests in it, so `queue_model: Some(..)` may not
//! perturb one single fill, deferral, or equity sample of a market/stop tape. Both lanes now reach
//! that outcome by calling the same function, and this test fails the moment a fourth gate (or a
//! reordering of the three) lands on only one of them.
//!
//! The equivalence is asserted TWICE — with every opt-in discipline off, and with all three armed
//! at once — plus a mutation contrast proving the armed run genuinely differs from the bare one,
//! so the equality above cannot pass vacuously.
//!
//! Session coverage of the QUEUED price-conditional branch (the half that deliberately does not go
//! through `gate_pending`, because a resting limit wants the session gate without the staleness
//! one) lives in `session_law.rs`; the staleness discipline's per-lane behaviour lives in
//! `stale_price_wait.rs`. This file covers the seam between them.

use indexmap::IndexMap;
use vike_backtest::engine::Tick;
use vike_backtest::{BacktestResult, EngineParams, QueueModelKind, SimBroker, StrategyEngine};
use vike_model::session::US_EQUITY_REGULAR;
use vike_model::{days_from_civil, Fill, Strategy, TradeTick};

const SYM: &str = "AAPL";

/// Epoch-ms for a UTC instant, via the crate's own civil-calendar math.
fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
}

fn trade(ts: i64, price: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size: 10.0,
        is_buyer_maker: false,
        symbol: SYM.to_string(),
    })
}

/// A US-equity tape straddling the weekend (`US_EQUITY_REGULAR` is 13:30–21:00 UTC, Mon–Fri).
/// Every event is a TRADE print, so the staleness discipline is inert by construction on both
/// lanes — a trade is a print at age 0 — which is itself asserted below.
///
/// | idx | when (UTC)           | session | what happens                                  |
/// |-----|----------------------|---------|-----------------------------------------------|
/// | 0   | Fri 2024-01-05 15:00 | OPEN    | submit a market buy (fills on the next event) |
/// | 1   | Fri 2024-01-05 16:00 | OPEN    | it fills @100; submit a stop sell @ 95        |
/// | 2   | Sat 2024-01-06 15:00 | CLOSED  | a 94.0 print — crosses the resting stop       |
/// | 3   | Mon 2024-01-08 15:00 | OPEN    | the reopen: the deferred stop triggers here   |
/// | 4   | Mon 2024-01-08 16:00 | OPEN    | where an emulator-RELEASED stop child lands   |
fn tape() -> Vec<(String, Vec<Tick>)> {
    vec![(
        SYM.to_string(),
        vec![
            trade(ms(2024, 1, 5, 15), 100.0),
            trade(ms(2024, 1, 5, 16), 100.0),
            trade(ms(2024, 1, 6, 15), 94.0),
            trade(ms(2024, 1, 8, 15), 94.0),
            trade(ms(2024, 1, 8, 16), 93.0),
        ],
    )]
}

/// TAKER-only flow: one market buy and one resting STOP. No priced limit, no tagged quote — so
/// nothing here is `queue_kind` and the queue tracker has, by construction, no order to gate.
/// One order rests at a time, so `resolve_intrabar_fills` never arbitrates and the outcome of
/// each event is unambiguous.
struct TakerFlow {
    seen: usize,
    fills: Vec<Fill>,
}

impl Strategy<SimBroker> for TakerFlow {
    fn on_trade_tick(&mut self, b: &mut SimBroker, _t: &TradeTick) {
        match self.seen {
            0 => b.submit(SYM, 1, 1.0, 0.0, true, None), // market buy → fills on the NEXT event
            1 => b.submit_stop(SYM, -1, 1.0, 95.0, 0.0, true), // resting stop (a TAKER kind)
            _ => {}
        }
        self.seen += 1;
    }

    fn on_fill(&mut self, _b: &mut SimBroker, f: &Fill) {
        self.fills.push(f.clone());
    }
}

/// Costless params; `queue` picks the lane and `armed` turns on all three opt-in disciplines the
/// shared gate sequence runs (the session gate with a real US-equity calendar, the staleness wait,
/// and the emulator stop release).
fn params(queue: bool, armed: bool) -> EngineParams {
    let mut session_calendars = IndexMap::new();
    session_calendars.insert(SYM.to_string(), US_EQUITY_REGULAR); // ignored when the gate is off
    EngineParams {
        cash: 1_000_000.0,
        session_gate: armed,
        session_calendars,
        max_price_staleness_ms: if armed { Some(60_000) } else { None },
        emulator_release_stops: armed,
        queue_model: if queue { Some(QueueModelKind::RiskAdverse) } else { None },
        ..EngineParams::default()
    }
}

fn run(queue: bool, armed: bool) -> (BacktestResult, Vec<Fill>) {
    let strat = TakerFlow { seen: 0, fills: Vec::new() };
    let mut eng =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], strat, params(queue, armed));
    let res = eng.run_ticks(&tape());
    (res, eng.strategy.fills)
}

/// Every observable the two lanes must agree on, floats compared BITWISE — "byte-identical" is the
/// contract the frozen lane carries, so a tolerance here would defeat the test.
fn assert_lanes_agree(armed: bool) {
    let (frozen, frozen_fills) = run(false, armed);
    let (queued, queued_fills) = run(true, armed);

    assert_eq!(frozen_fills, queued_fills, "fills diverge (armed = {armed})");
    assert_eq!(
        frozen.final_equity.to_bits(),
        queued.final_equity.to_bits(),
        "final equity diverges (armed = {armed})"
    );
    let frozen_curve: Vec<u64> = frozen.equity_curve.iter().map(|e| e.to_bits()).collect();
    let queued_curve: Vec<u64> = queued.equity_curve.iter().map(|e| e.to_bits()).collect();
    assert_eq!(frozen_curve, queued_curve, "equity curve diverges (armed = {armed})");
    assert_eq!(frozen.equity_ts, queued.equity_ts, "equity stamps diverge (armed = {armed})");
    assert_eq!(frozen.trades, queued.trades, "closed trades diverge (armed = {armed})");
    assert_eq!(
        (frozen.session_deferrals, frozen.stale_deferrals, frozen.intrabar_both_hit),
        (queued.session_deferrals, queued.stale_deferrals, queued.intrabar_both_hit),
        "deferral / intrabar counters diverge (armed = {armed})"
    );
    assert_eq!(frozen.dropped, queued.dropped, "dropped orders diverge (armed = {armed})");
}

/// The default path: no discipline armed, so the shared sequence is three no-ops and a trigger
/// check. Turning the queue model on must not move a single number.
#[test]
fn taker_flow_is_identical_on_both_lanes_with_every_gate_off() {
    assert_lanes_agree(false);
}

/// The same equivalence with the session gate, the staleness wait and the emulator stop release
/// ALL armed — the three gates the shared sequence runs, exercised together. This is the case the
/// hand-synced twins used to have to keep in step by hand.
#[test]
fn taker_flow_is_identical_on_both_lanes_with_every_gate_armed() {
    assert_lanes_agree(true);
}

/// The mutation contrast that stops the two tests above from passing vacuously: arming the gates
/// genuinely changes the run, on BOTH lanes, in the same way.
///
/// Gates off, the Saturday 94.0 print — an hour no US venue was open to print — crosses the
/// resting stop and closes the position there. Gates on, that pass is refused and counted; the
/// stop fires at Monday's reopen instead, and the emulator release makes it a market child that
/// lands on the tick AFTER its trigger.
#[test]
fn arming_the_gates_moves_the_weekend_fill_to_the_reopen_on_both_lanes() {
    let saturday = ms(2024, 1, 6, 15);
    let reopen = ms(2024, 1, 8, 15);

    for queue in [false, true] {
        let (bare, bare_fills) = run(queue, false);
        let (armed, armed_fills) = run(queue, true);

        assert_eq!(bare.session_deferrals, 0, "gates off ⇒ never counted (queue = {queue})");
        assert!(
            bare_fills.iter().any(|f| f.ts == saturday),
            "gates off: the closed-Saturday print fills the stop (queue = {queue}): {bare_fills:?}"
        );
        assert!(
            armed_fills.iter().all(|f| f.ts != saturday),
            "gates on: nothing fills on the closed Saturday (queue = {queue}): {armed_fills:?}"
        );
        assert!(
            armed.session_deferrals > 0,
            "gates on: the refused Saturday pass is counted (queue = {queue})"
        );
        assert!(
            armed_fills.iter().any(|f| f.ts >= reopen),
            "gates on: the deferred stop fills at/after the reopen (queue = {queue}): {armed_fills:?}"
        );
        // Every event on this tape is a TRADE, hence a print at age 0, so the staleness half of the
        // shared sequence is inert on the tick lane — on the QUEUED lane no less than the frozen
        // one, which `stale_price_wait.rs` only ever proved for the frozen twin.
        assert_eq!(
            armed.stale_deferrals, 0,
            "the tick lane is stale-inert by construction (queue = {queue})"
        );
        // Both configurations end flat: the stop closes the buy, only the timing moves.
        assert_eq!(bare_fills.len(), 2, "buy + stop (queue = {queue}): {bare_fills:?}");
        assert_eq!(armed_fills.len(), 2, "buy + stop (queue = {queue}): {armed_fills:?}");
    }
}
