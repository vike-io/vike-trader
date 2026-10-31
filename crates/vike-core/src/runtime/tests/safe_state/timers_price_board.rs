//! The wheel-backed watchdog sweep (audit co6), the runtime's PriceBoard write-sites, and (at the
//! bottom) the RISK CLOCK: the margin call and the drawdown latch of every engine off the timer wheel.

use super::*;
use crate::runtime::test_support::{core_of, engine_on, mount_of, sim_engine_with};

// ---- audit co6: the watchdog sweep backed by the DeadlineTimerWheel at the drain boundary ----

/// Backing the watchdog with the [`DeadlineTimerWheel`] at the drain-loop boundary
/// ([`CoreThread::drive_due_timers`]) drives the SAME sweep at the SAME cadence as the retired
/// per-message `Ingest::Watchdog` dispatch: the confirm-grace ladder transitions are identical to
/// `watchdog_soft_warns_in_grace_then_rejects_after_it`, but now fired via the boundary path
/// instead of a direct `sweep_stuck_orders()`. ack=1000, grace=1000 ⇒ reject 2000, extended 3000.
#[test]
fn watchdog_wheel_matches_direct_sweep_transitions() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.arm_boundary_timers(0); // tick = 500ms; StuckSweep armed at 500ms
    assert!(!core.timers.is_empty(), "the watchdog arms the boundary sweep timer");
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    // boundary at 1500 — inside the grace: stage-1 active confirm, NO reject
    core.drive_due_timers(1500);
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
    assert!(core.confirm_issued_ms.contains_key("c1"), "flagged awaiting confirm");
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()], "active confirm issued once");

    // an intermediate boundary (1600) must NOT fire a second sweep — the timer re-armed at 2000
    core.drive_due_timers(1600);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "no spurious mid-cadence sweep — the wheel fast-skips until the next deadline"
    );

    // boundary at 2500 — past the plain reject deadline but the in-flight-confirm guard DEFERS
    core.drive_due_timers(2500);
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Initialized,
        "guard defers past the plain deadline, exactly like the direct sweep"
    );

    // boundary at 3500 — past the extended deadline, still pre-ack: backstop reject
    core.drive_due_timers(3500);
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Rejected,
        "the wheel-driven sweep terminalizes the stuck order identically"
    );
    assert!(!core.confirm_issued_ms.contains_key("c1"), "guard bookkeeping self-cleans");
}

/// Default config (watchdog off): arming the boundary timers adds NOTHING, the wheel stays empty,
/// and driving it is a strict no-op — the runtime gates the boundary advance behind `is_empty`, so
/// the wheel-free fold path is byte-identical to today.
#[test]
fn boundary_timers_noop_when_watchdog_disabled() {
    let mut core = test_core(); // submit_ack_timeout defaults to None
    core.arm_boundary_timers(0);
    assert!(core.timers.is_empty(), "no timer armed when the watchdog is disabled");
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.drive_due_timers(999_999); // even a huge now fires nothing
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
    assert!(core.engine.client.confirms.is_empty(), "disabled watchdog issues no confirm");
}

/// The [`TimerKind::StuckSweep`] timer is self-rescheduling — it fires on EVERY cadence boundary
/// it is due, not once. After each firing boundary the wheel is still armed for the next, so the
/// sweep keeps running across the whole session (the old OS-thread-per-tick cadence).
#[test]
fn watchdog_wheel_rearms_across_cadences() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.arm_boundary_timers(0);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    for now in [1500, 2000, 2500, 3000] {
        core.drive_due_timers(now);
        assert!(!core.timers.is_empty(), "the cadence timer re-arms after firing (now={now})");
    }
    // the sweep genuinely ran (stage-1 confirm was issued), proving the fires were real
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()]);
}

// ---- portfolio-observer PR-1 T4: the runtime's PriceBoard write-sites ----

/// The Quote/Trade tick lanes additionally feed `ExecutionEngine.price_board` (Tasks 1-3) at
/// dispatch time, alongside the existing `account.set_mark` — a plain field store, no behavior
/// change to the pre-existing account-mark/strategy-dispatch path.
#[test]
fn tick_lanes_feed_the_price_board() {
    let mut core = test_core();
    let venue = core.engine.venue.clone();
    let symbol = core.engine.symbol.clone();
    core.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
        venue: venue.clone(),
        symbol: symbol.clone(),
        quote: QuoteTick {
            ts: 1_000,
            local_ts: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    })));
    core.dispatch(Ingest::Trade(Box::new(TradeUpdate {
        venue: venue.clone(),
        symbol: symbol.clone(),
        trade: TradeTick {
            ts: 2_000,
            local_ts: 0,
            price: 100.5,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
    })));
    let c = core.engine.price_board.cell(&venue, &symbol).expect("board fed");
    assert_eq!(c.bid, Some((99.0, 1_000)));
    assert_eq!(c.ask, Some((101.0, 1_000)));
    assert_eq!(c.last_trade, Some((100.5, 2_000)));
    // existing behavior unchanged: account mark = trade price (last set_mark win)
    assert_eq!(core.engine.account.mark_of(&venue, &symbol), Some(100.5));
}

// ---- the CLOCK sweeps EVERY engine, the mounted ones too (owner ruling 2026-10-25) --------------
// «OANDA will be checked for margin once per closed candle, not every 30 seconds: no!! what if the
// candles are 4h, you will check once per 4h?? no, check once per 30 sec.» The margin call and the
// drawdown latch used to run from `drive_strategy`'s closed bar alone for an engine a mount drives, so
// a mount on a 4-hour series was looked at once per 4 hours. The boundary timer now sweeps every live
// engine. Most tests below are the mounted-only node the live root builds when every admitting venue is
// mounted: `PriceOnlyConfig::venues` is EMPTY, so the config is only the clock (cadence and age bound).

/// A mount that trades nothing: the 4-hour series it listens to never closes in these tests.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

pub(super) fn margin() -> vike_model::MarginCallConfig {
    vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 }
}

/// A clock config: seed 100, the price-only set `venues` (empty = a mounted-only node), and, when `mounted`,
/// a mount of `sim` on a 4-HOUR series.
pub(super) fn clock_config(
    margin_call: Option<vike_model::MarginCallConfig>,
    drawdown: Option<f64>,
    venues: &[&str],
    mounted: bool,
) -> CoreConfig {
    CoreConfig {
        seed_cash: 100.0,
        margin_call,
        max_drawdown: drawdown,
        strategy: mounted.then(|| mount_of("sim", "BTCUSDT", "4h", Box::new(Idle))),
        price_only: Some(PriceOnlyConfig {
            venues: venues.iter().map(|v| v.to_string()).collect(),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        ..CoreConfig::default()
    }
}

/// `(primary sim, extra bybit)`, seeds 100 each.
pub(super) fn clock_core(config: CoreConfig) -> CoreThread<RecordingClient> {
    core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(100.0, engine_on("bybit", "ETHUSDT", RecordingClient::default()))],
        config,
    )
}

pub(super) fn mounted_skip_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|e| e.contains("mounted-engine clock sweeps SKIP")).count()
}

/// A closed bar of `sim`'s 4-hour series closing at `close`.
fn bar_4h(ts: i64, close: f64) -> (SeriesKey, Bar) {
    (("sim".to_string(), "BTCUSDT".to_string(), "4h".to_string()), flat_bar_unit_volume(ts, close))
}

/// ⚠ **THE RULING: a mounted engine on 4-hour bars is margin-called by the CLOCK, between bars.** Long 10
/// at 100 on a seed of 100 with the market at 40 (a breach), quoted 5 s before the sweep, and NO bar
/// anywhere: the sweep at the first interval releases the reduce-only close. Before the ruling a venue a
/// mount drives was swept only by its own closed bar, and a node with no price-only venue armed no timer.
#[test]
fn a_mounted_engine_on_4h_bars_is_margin_called_by_the_clock_between_bars() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.arm_boundary_timers(0);
    assert!(!core.timers.is_empty(), "a mounted-only node arms the clock");

    core.drive_due_timers(29_000);
    assert!(core.engine.client.submissions.is_empty(), "not due before the interval");
    core.drive_due_timers(30_100);
    let sent = &core.engine.client.submissions;
    assert_eq!(sent.len(), 1, "the breached MOUNTED engine is liquidated at the first sweep");
    assert!(sent[0].reduce_only, "…as a reduce-only close");
    assert!(core.recent.iter().any(|e| e.starts_with("MARGIN CALL")), "{:?}", core.recent);
}

/// The warning band is announced by the clock too: long 20 at 100 on a seed of 100, the market at 100,
/// equity 100 against a maintenance requirement of 100 - inside the 5% band, short of the liquidation line.
#[test]
fn a_mounted_engine_in_the_warning_band_is_warned_by_the_clock_between_bars() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 20.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 100.0, 100.5, 25_000);
    core.arm_boundary_timers(0);
    core.drive_due_timers(30_100);
    assert!(
        core.recent.iter().any(|e| e.starts_with("margin-call WARNING")),
        "warned within one interval of the breach: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty(), "a warning releases nothing");
}

/// ⚠ **THE SAME STALENESS RULES.** A mounted engine whose open position's only price is older than the bound
/// is SKIPPED by the margin call (no liquidation), the skip is said ONCE however many sweeps pass, a
/// fresh quote brings the engine back in the same breath, and a later outage is a new episode. (The
/// drawdown latch is deliberately NOT part of this: it prices a stale MOUNTED engine as the closed bars
/// do, see `a_stale_mounted_engine_does_not_blind_the_clocks_latch…` below.)
#[test]
fn a_mounted_engine_with_a_stale_mark_is_skipped_once_by_the_clock() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1); // stamped at t=1
    core.arm_boundary_timers(0);

    for now in [70_000, 101_000, 132_000] {
        core.drive_due_timers(now);
    }
    assert!(core.engine.client.submissions.is_empty(), "no liquidation off a stale quote");
    assert_eq!(mounted_skip_notes(&core), 1, "said once, not at every sweep: {:?}", core.recent);

    // The feed comes back: a fresh quote, the same breach - the next sweep (due at 162 000) acts.
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 162_500);
    core.drive_due_timers(163_000);
    assert_eq!(core.engine.client.submissions.len(), 1, "liquidated on the fresh mark");

    // A LATER outage (the position is still open) is a new episode and is said again.
    core.drive_due_timers(260_000);
    assert_eq!(mounted_skip_notes(&core), 2, "{:?}", core.recent);
}

/// The drawdown latch rides the clock for a node with no price-only venue: a 50% loss of the capital on
/// the MOUNTED engine flips every engine to liquidate-only at the sweep, with no bar anywhere.
#[test]
fn the_clock_runs_the_drawdown_latch_for_a_mounted_only_node() {
    let mut core = clock_core(clock_config(None, Some(0.2), &[], true));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 90.0, 90.5, 25_000);
    core.arm_boundary_timers(0);

    core.drive_due_timers(29_000);
    assert_eq!(core.engine.trading_state, TradingState::Active, "not due yet");
    core.drive_due_timers(30_100);
    assert_eq!(core.engine.trading_state, TradingState::Reducing);
    assert_eq!(core.extra_engines[0].1.trading_state, TradingState::Reducing, "account-wide");
    assert!(core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH:")), "{:?}", core.recent);
}

/// A mounted engine and a price-only one are swept alike, each by its own number: the breached engines of
/// both kinds are liquidated once at the first sweep, in their own books.
#[test]
fn the_clock_sweeps_a_mounted_and_a_price_only_engine_alike() {
    let mut core = clock_core(clock_config(Some(margin()), None, &["bybit"], true));
    for (eng, venue, symbol) in
        [(&mut core.extra_engines[0].1, "bybit", "ETHUSDT"), (&mut core.engine, "sim", "BTCUSDT")]
    {
        set_position(eng, venue, symbol, 10.0, 100.0);
        eng.price_board.set_quote(venue, symbol, 40.0, 40.5, 25_000);
    }
    core.arm_boundary_timers(0);
    core.drive_due_timers(30_100);
    assert_eq!(core.engine.client.submissions.len(), 1, "the mounted engine");
    assert_eq!(core.extra_engines[0].1.client.submissions.len(), 1, "the price-only engine");
}

/// TWO ACCOUNTS OF ONE VENUE, both mounted-venue engines: the labelled account's breach is closed in ITS
/// book (`EngineRoute::Engine`), and the default account, flat, is not touched.
#[test]
fn the_clock_closes_a_labelled_accounts_breach_in_its_own_book() {
    let mut alt = engine_on("sim", "BTCUSDT", RecordingClient::default());
    alt.route_key = "sim#ALT".to_string();
    let mut core = core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(100.0, alt)],
        clock_config(Some(margin()), None, &[], false),
    );
    set_position(&mut core.extra_engines[0].1, "sim", "BTCUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.arm_boundary_timers(0);
    core.drive_due_timers(30_100);
    assert_eq!(core.extra_engines[0].1.client.submissions.len(), 1, "ALT's own book is closed");
    assert!(core.engine.client.submissions.is_empty(), "…and the default account's is not touched");
}

/// A STOPPED (retired) engine is skipped: its client is detaching, a liquidation could only be refused.
#[test]
fn the_clock_skips_a_stopped_engine() {
    let mut alt = engine_on("sim", "BTCUSDT", RecordingClient::default());
    alt.route_key = "sim#ALT".to_string();
    let mut core = core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(100.0, alt)],
        clock_config(Some(margin()), None, &[], false),
    );
    set_position(&mut core.extra_engines[0].1, "sim", "BTCUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.retired_engines.push(1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(30_100);
    assert!(core.extra_engines[0].1.client.submissions.is_empty(), "nothing sent to a dead engine");
    assert_eq!(mounted_skip_notes(&core), 0, "a stop is no skip episode: {:?}", core.recent);
}

/// A stopped engine's FROZEN mark does not blind the clock's latch: the tombstone keeps its position (the
/// operator's to close) but its board never moves again, so priced by the age bound it would read stale for
/// ever and every clock sweep would skip the whole node. It is priced as the bar path prices it, and a real
/// loss on a live engine still trips the latch.
#[test]
fn a_stopped_engines_frozen_mark_does_not_blind_the_clock_latch() {
    let mut alt = engine_on("sim", "BTCUSDT", RecordingClient::default());
    alt.route_key = "sim#ALT".to_string();
    let mut core = core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(100.0, alt), (100.0, engine_on("bybit", "ETHUSDT", RecordingClient::default()))],
        clock_config(None, Some(0.2), &["bybit"], false),
    );
    set_position(&mut core.extra_engines[0].1, "sim", "BTCUSDT", 1.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("sim", "BTCUSDT", 100.0, 100.5, 1); // frozen at t=1
    core.retired_engines.push(1);
    set_position(&mut core.extra_engines[1].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[1].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 25_000);
    core.arm_boundary_timers(0);
    core.drive_due_timers(30_100);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
}

/// ⚠ **NO DOUBLE LIQUIDATION: a bar and the clock in the same moment.** The bar path releases the close; the
/// clock sweep that follows within one interval finds the engine already liquidating and adds nothing
/// (the close has not folded back yet). The quiet window is ONE interval, but the close still IN FLIGHT is
/// netted from the next one for as long as it lives (second review, 1): a sweep after the window does not send
/// the same close again while the first is unfilled; once that order is gone the engine is judged afresh.
#[test]
fn a_bar_then_the_clock_do_not_both_liquidate_an_engine() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 10_000);
    core.arm_boundary_timers(0);

    let (key, bar) = bar_4h(10_000, 40.0);
    core.engine.now_ms = 10_000;
    core.drive_strategy(&key, &bar);
    assert_eq!(core.engine.client.submissions.len(), 1, "the closed bar liquidates");

    core.drive_due_timers(30_100); // 20 s later, the order not yet folded back
    assert_eq!(core.engine.client.submissions.len(), 1, "the clock adds no second order");
    core.drive_due_timers(61_000); // a whole interval after the release: the window is over, the close is not
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "the close still in flight covers the breach: {:?}",
        core.recent
    );
    // the close is gone (a reject, a cancel): the engine is judged afresh
    for mo in core.engine.registry.values_mut() {
        mo.status = vike_model::OrderStatus::Canceled;
    }
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 91_000);
    core.drive_due_timers(92_000);
    assert_eq!(core.engine.client.submissions.len(), 2, "…and sends the next close");
}

/// The other order of the same moment: the clock liquidates first, the closed bar that lands inside the
/// window adds nothing.
#[test]
fn the_clock_then_a_bar_do_not_both_liquidate_an_engine() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100);
    assert_eq!(core.engine.client.submissions.len(), 1, "the clock liquidates");

    let (key, bar) = bar_4h(35_000, 40.0);
    core.engine.now_ms = 35_000;
    core.drive_strategy(&key, &bar);
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "the bar inside the window adds no second order"
    );
    let (key, bar) = bar_4h(61_000, 40.0);
    core.engine.now_ms = 61_000;
    core.drive_strategy(&key, &bar);
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "…after the window the close still in flight covers it"
    );
    for mo in core.engine.registry.values_mut() {
        mo.status = vike_model::OrderStatus::Canceled;
    }
    core.engine.now_ms = 62_000;
    core.drive_strategy(&key, &bar);
    assert_eq!(core.engine.client.submissions.len(), 2, "…and once it is gone it judges again");
}

// ---- third review: a margin call nets the closes already IN FLIGHT ---------------------------------------
//
// The margin call's only guard was the quiet window, armed after ITS OWN release. A close it did not send
// (a manual Flatten or MarketExit, a stuck pre-ack close) armed none, and a close of its own still unfilled
// after one interval was sent AGAIN. The gate nets in-flight reduce-only orders only against a flat book,
// and oanda, ig, fxcm, ibkr, ctrader and alpaca put no reduce-only flag on the wire, so a second full-size
// sell flips the position. The margin call now takes the remaining quantity of the engine's live reduce-only
// MARKET closes on that symbol and side from the close it is about to send.

/// A manual `Flatten`/`MarketExit` as the engine holds it: a reduce-only market sell of `qty` on sim/BTCUSDT.
fn manual_close(qty: f64) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: "manual".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ts: 0,
        ..Default::default()
    }
}

/// ⚠ **A close in flight that this sweep did not send arms no quiet window, and must still stop a second
/// one.** Long 10 at 100 on a seed of 100, the market at 40 (a full breach), and an operator's flatten of the
/// whole 10 already out and unfilled: the clock sends NOTHING, now or at the sweeps after.
#[test]
fn a_manual_flatten_in_flight_stops_the_clock_sending_a_second_close() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.engine.submit_order(&manual_close(10.0), 0, &mut Outbox::default());
    assert_eq!(core.engine.client.submissions.len(), 1, "PREMISE: the flatten is out");
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100);
    assert_eq!(core.engine.client.submissions.len(), 1, "covered: {:?}", core.recent);
    assert!(!core.recent.iter().any(|e| e.starts_with("MARGIN CALL")), "{:?}", core.recent);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 60_000);
    core.drive_due_timers(61_000); // past any quiet window: the flatten is still the cover
    assert_eq!(core.engine.client.submissions.len(), 1, "{:?}", core.recent);
}

/// The PARTIAL remainder: an operator's close of 4 is out; the breach asks for all 10, so the clock sends the
/// other 6 and the two together never exceed the position.
#[test]
fn a_partial_close_in_flight_is_netted_from_the_margin_calls_close() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    core.engine.submit_order(&manual_close(4.0), 0, &mut Outbox::default());
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100);
    let sent: Vec<f64> = core.engine.client.submissions.iter().map(|r| r.qty).collect();
    assert_eq!(sent.len(), 2, "the manual close and the remainder: {sent:?}");
    assert!((sent[1] - 6.0).abs() < 1e-9, "only the 6 still missing, sent {}", sent[1]);
}

/// A reduce-only take-profit that RESTS far from the market cannot fill now, so it is not cover: the market
/// close still goes out in full. (Green before the change too - the margin call counted nothing then; this
/// pins that the netting added for the manual flatten does not over-count what only rests.)
#[test]
fn a_resting_take_profit_does_not_stop_the_margin_calls_close() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 25_000);
    let tp = vike_model::OrderRequest {
        client_order_id: "tp".into(),
        order_type: vike_model::OrderType::Limit,
        price: Some(500.0),
        ..manual_close(10.0)
    };
    core.engine.submit_order(&tp, 0, &mut Outbox::default());
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100);
    let sent: Vec<f64> = core.engine.client.submissions.iter().map(|r| r.qty).collect();
    assert_eq!(sent.len(), 2, "the take-profit and the margin call's close: {sent:?}");
    assert!((sent[1] - 10.0).abs() < 1e-9, "the close in full, sent {}", sent[1]);
}

// ---- the adversarial review's findings on the clock (finding 1, 7 and 8) ----------------------------

/// ⚠ **A STALE MOUNTED ENGINE DOES NOT BLIND THE CLOCK'S LATCH** (finding 1). The mount's tick feed is
/// silent (a datahub restart, a deploy; a bar-close-only mount reads stale 60 s after each close and an
/// event-driven book always) while a fresh price-only leg is down: the latch prices the mounted engine as
/// the closed bars do (permissively) and trips on the real loss, where "stale makes the whole evaluation
/// unmeasurable" returned nothing for the node. The MARGIN CALL still skips the stale mounted engine (it
/// acts on orders, so it acts on fresh numbers only), and says so once. Mounted `sim` holds 10 at 100 with
/// its last quote (40) stamped at t=1, a loss of 600 priced permissively; price-only `bybit` holds 10 at 100
/// with a fresh quote at 99, a loss of 10 that is under the threshold ALONE (5% of the 200 base against 20%),
/// so only the mounted engine's loss can trip the latch: a latch that left the stale engine out of the curve
/// would stay Active here, as would one that called the evaluation unmeasurable.
#[test]
fn a_stale_mounted_engine_does_not_blind_the_clocks_latch_and_its_margin_call_skip_is_said_once() {
    let build = |mounted_position: f64| {
        let mut core = clock_core(clock_config(Some(margin()), Some(0.2), &["bybit"], true));
        set_position(&mut core.engine, "sim", "BTCUSDT", mounted_position, 100.0);
        core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1);
        set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
        core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 99.0, 99.5, 69_000);
        core.arm_boundary_timers(0);
        core
    };
    // CONTROL: with the mounted engine flat, the fresh leg alone trips nothing.
    let mut control = build(0.0);
    control.drive_due_timers(70_000);
    assert_eq!(
        control.engine.trading_state,
        TradingState::Active,
        "PREMISE: 99 alone is not a trip"
    );

    let mut core = build(10.0);
    core.drive_due_timers(70_000);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "the stale mounted engine's loss tripped the latch: {:?}",
        core.recent
    );
    assert_eq!(core.extra_engines[0].1.trading_state, TradingState::Reducing, "…account-wide");
    assert!(
        core.engine.client.submissions.is_empty(),
        "the stale mounted engine is not liquidated"
    );
    assert!(core.extra_engines[0].1.client.submissions.is_empty(), "the healthy leg neither");
    core.drive_due_timers(101_000);
    assert_eq!(mounted_skip_notes(&core), 1, "the skip is said once: {:?}", core.recent);
}

/// **A holding margin-call WARNING is said once per EPISODE by the clock** (finding 8): the 64-line ring
/// is shared with fills and rejects, and a band that holds for an hour would otherwise put 120 lines of
/// the same warning into it. The closed-bar pass repeats per bar, as it always did. Long 20 at 100 on a
/// seed of 100, the market at 100 (inside the band, short of liquidation), ten clock intervals; then the
/// band is left (a healthy market) and re-entered: a new episode, a second line.
#[test]
fn a_holding_margin_warning_is_said_once_per_episode_by_the_clock_and_per_bar_as_ever() {
    let mut core = clock_core(clock_config(Some(margin()), None, &[], false));
    set_position(&mut core.engine, "sim", "BTCUSDT", 20.0, 100.0);
    core.arm_boundary_timers(0);
    let warnings = |c: &CoreThread<RecordingClient>| {
        c.recent.iter().filter(|e| e.starts_with("margin-call WARNING")).count()
    };
    let mut now = 30_100;
    let mut sweep = |core: &mut CoreThread<RecordingClient>, px: f64| {
        core.engine.price_board.set_quote("sim", "BTCUSDT", px, px + 0.5, now - 5_000);
        core.drive_due_timers(now);
        now += 30_100;
    };
    for _ in 0..10 {
        sweep(&mut core, 100.0);
    }
    assert_eq!(warnings(&core), 1, "ten sweeps of one holding band, one line: {:?}", core.recent);
    sweep(&mut core, 150.0); // healthy: the episode ends
    assert_eq!(warnings(&core), 1);
    sweep(&mut core, 100.0); // the band again: a new episode
    assert_eq!(warnings(&core), 2, "{:?}", core.recent);
    // the closed-bar pass is untouched: a line per bar
    core.sweep_margin_call(&margin(), 9_000_000);
    core.sweep_margin_call(&margin(), 9_060_000);
    assert_eq!(warnings(&core), 4, "per bar, as ever: {:?}", core.recent);
}

/// A client whose `submit` panics: the stand-in for the `expect("journal append")` a margin call's
/// write-ahead record carries, which unwinds through the same sweep the same way.
#[derive(Default)]
struct PanicIf {
    panics: bool,
    submissions: Vec<vike_model::OrderRequest>,
}

impl vike_exec::ExecutionClient for PanicIf {
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        None
    }
    fn submit(&mut self, request: &vike_model::OrderRequest) {
        assert!(!self.panics, "the venue client is down");
        self.submissions.push(request.clone());
    }
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// An engine on `venue`/`symbol` that is long 10 at 100 with its last quote (40) fresh at 25 000: a full breach
/// on a seed of 100. Its client panics on submit when `panics`.
fn breached_engine(venue: &str, symbol: &str, panics: bool) -> ExecutionEngine<PanicIf> {
    let mut e = engine_on(venue, symbol, PanicIf { panics, submissions: Vec::new() });
    e.account.positions.insert(
        (venue.into(), symbol.into(), vike_model::events::PositionSide::Both),
        PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() },
    );
    e.price_board.set_quote(venue, symbol, 40.0, 40.5, 25_000);
    e
}

/// ⚠ **A PANIC IN THE CLOCK SWEEP LANDS IN THE SAFE STATE, and the clock keeps ticking** (finding 7). The
/// bar path runs under `handle`'s `catch_unwind`; the timer wheel did not, so a margin call that unwound
/// (a journal append that fails, a client that panics) killed the `vt-core` thread with no `Halted` state
/// and no cancel sweep. Now the sweep call is guarded like a message is, and the timer re-arms.
#[test]
fn a_panic_in_the_clock_sweep_lands_in_the_safe_state_and_the_clock_keeps_ticking() {
    let engine = breached_engine("sim", "BTCUSDT", true);
    let mut core = core_of(engine, Vec::new(), clock_config(Some(margin()), None, &[], false));
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100); // the breach releases a liquidation; the client panics
    assert_eq!(
        core.fault.as_deref(),
        Some("the venue client is down"),
        "the fault is the injected panic's own reason"
    );
    assert_eq!(
        core.engine.trading_state,
        TradingState::Halted,
        "…and the core is in the safe state"
    );
    assert!(!core.timers.is_empty(), "the clock re-armed itself");
    // The order registered before the client panicked is still in flight, so the next tick sends nothing:
    // the thread lives, the first fault stays and the clock goes on ticking.
    core.drive_due_timers(61_000);
    assert_eq!(core.fault.as_deref(), Some("the venue client is down"));
    assert!(!core.timers.is_empty(), "…and it re-armed again");
}

/// ⚠ **AN ENGINE WHOSE CLIENT PANICS DOES NOT STOP THE MARGIN CALL OF THE ENGINES BELOW IT** (third review, 2).
/// The sweep walks the engines from the highest index down to 0, and one `catch_unwind` around the whole
/// sweep meant a panic at engine k ended it there for ever: engine 0 (the primary) never got a margin call
/// while engine k kept panicking. Each engine's margin call is now its own stage. Both engines are in a full
/// breach; engine 1's client panics, engine 0's does not: engine 0's close still goes out (a halted core lets a
/// covered reduce through), the fault is engine 1's, said once.
#[test]
fn a_panicking_engine_does_not_stop_the_margin_call_of_the_engines_below_it() {
    let mut core = core_of(
        breached_engine("sim", "BTCUSDT", false),
        vec![(100.0, breached_engine("bybit", "ETHUSDT", true))],
        clock_config(Some(margin()), None, &[], false),
    );
    core.arm_boundary_timers(0);

    core.drive_due_timers(30_100);
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "engine 0 was still swept: {:?}",
        core.recent
    );
    assert_eq!(core.fault.as_deref(), Some("the venue client is down"), "engine 1's fault");
    assert_eq!(core.engine.trading_state, TradingState::Halted, "the safe state");
    core.drive_due_timers(61_000);
    assert_eq!(core.risk_clock_faulted.len(), 1, "one fault per stage, not one per sweep");
    assert!(!core.timers.is_empty());
}
