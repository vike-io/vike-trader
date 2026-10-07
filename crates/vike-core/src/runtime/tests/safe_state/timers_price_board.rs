//! The wheel-backed watchdog sweep (audit co6) and the runtime's PriceBoard write-sites.

use super::*;

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
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);
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
        vike_exec::OrderStatus::Initialized,
        "guard defers past the plain deadline, exactly like the direct sweep"
    );

    // boundary at 3500 — past the extended deadline, still pre-ack: backstop reject
    core.drive_due_timers(3500);
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Rejected,
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
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);
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
