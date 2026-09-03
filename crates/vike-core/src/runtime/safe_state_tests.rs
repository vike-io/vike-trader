//! Safe-state / watchdog / equity-sampler / readiness-gate unit tests — split out of the runtime
//! fold module verbatim (the former inline `#[cfg(test)] mod safe_state_tests` body). `use super::*`
//! re-exports the parent runtime module's items, so nothing about resolution changes.

use super::*;
use std::sync::atomic::AtomicUsize;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, PositionEntry, RiskGate, RiskLimits};
use vike_model::events::{FillEvent, OrderAccepted, OrderFilled, OrderSubmitted, TradeId};
use vike_model::{QuoteTick, TradeTick};

/// Assemble a `CoreThread<RecordingClient>` exactly the way [`spawn_core`] does, minus the
/// OS-thread spawn — so a private method like [`CoreThread::enter_safe_state`] can be driven
/// synchronously ON THE TEST THREAD. That matters for `#[traced_test]`: its subscriber is a
/// thread-local default, so the fault event must fire on this thread to be captured.
fn test_core() -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    )
}

fn stuck_req(coid: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 0,
        ..Default::default()
    }
}

/// A minimal fill for `coid` on the engine's own (venue, symbol), to drive the OMS FSM in tests.
fn fill_for(coid: &str) -> FillEvent {
    FillEvent {
        // minted by this helper — same `t-<coid>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    }
}

/// Audit C3 ladder (genuinely-absent order, with the in-flight-confirm guard): a stuck pre-ack
/// order whose stage-1 confirm never resolves is NOT rejected at the plain reject deadline — the
/// guard DEFERS it one additional grace window (the confirm is presumed in flight), and it is
/// terminalized only after the EXTENDED deadline `submit_ack_timeout + 2·grace` passes still
/// pre-ack. ack=1000, grace=1000 ⇒ plain reject at 2000, extended reject at 3000.
#[test]
fn watchdog_soft_warns_in_grace_then_rejects_after_it() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);

    // STAGE 1 — inside the confirm-grace (ack 1000 < age 1500 < reject 2000): warn + active
    // confirm, NO reject.
    core.engine.now_ms = 1500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Initialized,
        "must NOT reject while still inside the confirm-grace"
    );
    assert!(core.confirm_issued_ms.contains_key("c1"), "order flagged as awaiting confirm");

    // GUARD — past the PLAIN reject deadline (reject 2000 < age 2500 < extended 3000). The confirm
    // issued at stage 1 is still unresolved and the order is still pre-ack, so the guard DEFERS the
    // backstop reject rather than clobbering a possibly-in-flight confirm.
    core.engine.now_ms = 2500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Initialized,
        "in-flight-confirm guard must DEFER the reject past the plain deadline"
    );
    assert!(core.confirm_issued_ms.contains_key("c1"), "still awaiting the in-flight confirm");

    // STAGE 2 — past the EXTENDED deadline (age 3500 > extended 3000) still pre-ack: hard reject.
    core.engine.now_ms = 3500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Rejected,
        "backstop terminalizes only after the EXTENDED confirm-grace elapses"
    );
    assert!(!core.confirm_issued_ms.contains_key("c1"), "cleared once terminalized");
}

/// The in-flight-confirm guard's core purpose (finding #1): a confirm that lands JUST AFTER the
/// plain reject deadline — but within the extended window — must win. The order is NOT
/// phantom-rejected; the real terminal folds and the later sweep leaves it alone. This is the
/// tuning-dependent race (confirm RTT > grace, e.g. Bybit's ~2×5s vs a 5s grace) converted into a
/// guard: ack=1000, grace=1000 ⇒ plain reject 2000, extended 3000; the ack/fill lands at 2500.
#[test]
fn watchdog_guard_defers_so_a_confirm_after_the_plain_deadline_still_wins() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut core.engine,
    );

    // stage 1: enter the grace, active confirm issued
    core.engine.now_ms = 1500;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Submitted);

    // past the PLAIN reject deadline (2000) but within the extended window (3000): the guard defers
    // instead of rejecting — the confirm is treated as in flight.
    core.engine.now_ms = 2500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Submitted,
        "must not reject: the confirm is still in flight past the plain deadline"
    );

    // the confirm's authoritative terminal now lands (slow WS ack / adapter REST re-query)
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 0,
        }),
        &mut core.engine,
    );
    core.bus.publish(
        Event::OrderFilled(OrderFilled {
            client_order_id: "c1".into(),
            fill: fill_for("c1"),
            ts: 0,
        }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Filled);

    // even WELL past the extended deadline, the now-filled order is left untouched (and its guard
    // bookkeeping is pruned) — no phantom reject, no stranded position.
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Filled,
        "a confirm that landed in the extended window must never be clobbered"
    );
    assert!(
        !core.confirm_issued_ms.contains_key("c1"),
        "guard bookkeeping self-cleans on terminalize"
    );
}

/// Audit C3 ladder (the dangerous case): an un-acked order the venue ACTUALLY FILLED must NOT get
/// a synthesized reject. The real terminal (a slow WS ack / the adapter's REST confirm) lands
/// during the confirm-grace, moving the order out of the pre-ack states — so even a much-later
/// sweep leaves the fill untouched instead of clobbering a real position with a phantom reject.
#[test]
fn watchdog_does_not_clobber_a_fill_that_lands_in_the_grace() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut core.engine,
    );

    // un-acked past the timeout but within the grace: soft-warn only, still Submitted
    core.engine.now_ms = 1500;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Submitted);

    // the venue's real ack + fill arrive during the grace (adapter REST confirm / slow WS ack)
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 0,
        }),
        &mut core.engine,
    );
    core.bus.publish(
        Event::OrderFilled(OrderFilled {
            client_order_id: "c1".into(),
            fill: fill_for("c1"),
            ts: 0,
        }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Filled);

    // now WELL past the confirm-grace deadline — the sweep must leave the filled order alone
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Filled,
        "a real fill during the grace must never be clobbered by a synthesized reject"
    );
}

/// Default (timeout None) must be a strict no-op — zero behavior change on merge.
#[test]
fn watchdog_disabled_by_default_is_a_noop() {
    let mut core = test_core(); // submit_ack_timeout defaults to None
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);
}

/// A fresh (within-deadline) pre-ack order must be left alone.
#[test]
fn watchdog_leaves_fresh_order_untouched() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.engine.submit_order(&stuck_req("c1"), 4500, &mut Outbox::default());
    core.engine.now_ms = 5000; // 5000 - 4500 = 500ms < 1000ms deadline
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);
}

/// Audit ex1 residual: when an un-acked order enters the confirm-grace (stage 1), the watchdog
/// ACTIVELY issues a confirm to the venue client ONCE — so a truly-wedged adapter is PRODDED into
/// re-querying, not only waited on. It must NOT re-issue on a later sweep while the order is still
/// in the grace window, and it must NOT reject during the grace (the active confirm's terminal is
/// what wins; stage 2 only backstops if even that yields nothing).
#[test]
fn watchdog_issues_confirm_once_during_the_grace() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    // enter the grace (ack 1000 < age 1500 < reject 2000): active confirm issued once, NO reject
    core.engine.now_ms = 1500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "watchdog must actively confirm as the order enters the grace"
    );
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Initialized,
        "must not reject during the grace — the active confirm's terminal is awaited"
    );

    // a second sweep STILL inside the window must NOT re-issue the confirm (once per episode)
    core.engine.now_ms = 1800;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "confirm is issued at most once per stuck episode"
    );
}

/// The watchdog stays a strict no-op when disabled (timeout None): no confirm is ever issued, so
/// opting out of the watchdog opts out of the active confirm too (zero behavior change on merge).
#[test]
fn watchdog_disabled_issues_no_confirm() {
    let mut core = test_core(); // submit_ack_timeout defaults to None
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert!(core.engine.client.confirms.is_empty(), "disabled watchdog issues no confirm");
}

// -- FAST IN-FLIGHT CONFIRM (recon path-to-superset, F1-A) --------------------------------------

/// OFF by default (`inflight_confirm = None`): the fast rung is a strict no-op — a stuck pre-ack
/// order driven across many ticks never triggers a `confirm`, and no dedup state is ever written.
/// This is the inert-default / p99<10µs guarantee at the unit level.
#[test]
fn inflight_confirm_off_never_confirms() {
    let mut core = test_core(); // inflight_confirm defaults to None
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(30_000));
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    for now in [2_000, 4_000, 6_000, 10_000] {
        core.engine.now_ms = now;
        core.sweep_inflight_confirms(now);
    }
    assert!(
        core.engine.client.confirms.is_empty(),
        "disabled fast in-flight confirm must never issue a confirm"
    );
    assert!(core.inflight_confirm_last_ms.is_empty(), "no dedup state is written when disabled");
}

/// In-band + REJECT-FREE (the feature's core behavior): an order stuck SUBMITTED-but-unacked whose
/// age is inside `[inflight_confirm, submit_ack_timeout)` gets exactly one reject-free `confirm`; a
/// second sweep before the re-confirm gap does NOT re-confirm; after the gap it re-confirms — and no
/// terminal (no `OrderRejected`) is EVER synthesized by this path. inflight=2s, ack=30s.
#[test]
fn inflight_confirm_fires_in_band_reject_free() {
    let mut core = test_core();
    core.config.inflight_confirm = Some(std::time::Duration::from_millis(2_000));
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(30_000));
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    // fold the synchronous submit so the order is genuinely SUBMITTED-but-unacked
    core.bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Submitted);

    // age 3s: inside [2s, 30s) — exactly one reject-free confirm, order status UNCHANGED
    core.engine.now_ms = 3_000;
    core.sweep_inflight_confirms(3_000);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "one confirm the first time the order is seen in-band"
    );
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Submitted,
        "the fast rung must NEVER move the order toward a terminal"
    );
    assert!(core.inflight_confirm_last_ms.contains_key("c1"), "own dedup map records the confirm");

    // age 4s: only 1s since the last confirm (< the 2s re-confirm gap) — no re-confirm
    core.engine.now_ms = 4_000;
    core.sweep_inflight_confirms(4_000);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "must not re-confirm before the inflight_confirm re-confirm gap elapses"
    );

    // age 5s: 2s since the last confirm (>= the gap) — re-confirm once more, still reject-free
    core.engine.now_ms = 5_000;
    core.sweep_inflight_confirms(5_000);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string(), "c1".to_string()],
        "re-confirms once the re-confirm gap has elapsed"
    );
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Submitted,
        "no OrderRejected is ever synthesized by the in-flight-confirm path"
    );
}

/// Below the lower band bound the order is not yet "stuck" and must be left completely alone (no
/// confirm, no dedup entry). inflight=2s, order aged only 1s.
#[test]
fn inflight_confirm_leaves_a_too_young_order_untouched() {
    let mut core = test_core();
    core.config.inflight_confirm = Some(std::time::Duration::from_millis(2_000));
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(30_000));
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.engine.now_ms = 1_000; // age 1s < inflight_confirm 2s
    core.sweep_inflight_confirms(1_000);
    assert!(
        core.engine.client.confirms.is_empty(),
        "an order younger than the band is not confirmed"
    );
    assert!(core.inflight_confirm_last_ms.is_empty(), "and no dedup entry is written for it");
}

/// HAND-OFF at `submit_ack_timeout`: once an order crosses the timeout the early rung STOPS touching
/// it (band guard) and the existing reject ladder (`sweep_stuck_orders` STAGE 1) owns it — proving
/// no double-handling and that the early rung never fights the ladder. inflight=2s, ack=30s,
/// grace=15s (plain reject deadline 45s, so age 31s is a clean STAGE-1 window).
#[test]
fn inflight_confirm_hands_off_at_submit_ack_timeout() {
    let mut core = test_core();
    core.config.inflight_confirm = Some(std::time::Duration::from_millis(2_000));
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(30_000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(15_000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    // in-band (age 3s): the early rung confirms it
    core.engine.now_ms = 3_000;
    core.sweep_inflight_confirms(3_000);
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()]);

    // AT submit_ack_timeout (age 30s): the early rung hands off — it issues no further confirm
    core.engine.now_ms = 30_000;
    core.sweep_inflight_confirms(30_000);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "the early rung stops confirming at submit_ack_timeout"
    );

    // PAST it (age 31s): the early rung STILL won't touch it (band guard) — and the LATE reject
    // ladder now owns it. Prove NO double-handling: at this age the early rung issues 0 confirms and
    // only sweep_stuck_orders STAGE 1 acts, tracked in its OWN separate `confirm_issued_ms`.
    core.engine.now_ms = 31_000;
    core.sweep_inflight_confirms(31_000);
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "the early rung never confirms past the band, even after the ladder has taken over"
    );
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string(), "c1".to_string()],
        "the late reject ladder (STAGE 1) now owns the order and issues its own confirm"
    );
    assert!(
        core.confirm_issued_ms.contains_key("c1"),
        "the late ladder tracks it in confirm_issued_ms, SEPARATE from the early rung's dedup map"
    );
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Initialized,
        "still merely un-acked — neither rung has terminalized it inside the grace"
    );
}

/// An order that ACKs leaves the pre-ack set, so the fast rung prunes it from its dedup map and
/// never confirms it again (self-cleaning bound, mirroring the reject ladder's `confirm_issued_ms`).
#[test]
fn inflight_confirm_prunes_once_the_order_acks() {
    let mut core = test_core();
    core.config.inflight_confirm = Some(std::time::Duration::from_millis(2_000));
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(30_000));
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut core.engine,
    );

    // in-band confirm → dedup entry recorded
    core.engine.now_ms = 3_000;
    core.sweep_inflight_confirms(3_000);
    assert!(core.inflight_confirm_last_ms.contains_key("c1"));

    // the venue acks — the order leaves {Initialized, Submitted}
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 0,
        }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Accepted);

    // next sweep prunes the acked order and never re-confirms it
    core.engine.now_ms = 6_000;
    core.sweep_inflight_confirms(6_000);
    assert!(
        !core.inflight_confirm_last_ms.contains_key("c1"),
        "an acked order is pruned from the dedup map"
    );
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "an acked order is never re-confirmed by the fast rung"
    );
}

/// With the reject ladder OFF (`submit_ack_timeout = None`) the band is `[inflight_confirm, +inf)`:
/// the fast rung becomes the ONLY stuck-order signal and stays reject-free forever — without a
/// configured reject timeout we must never invent a terminal.
#[test]
fn inflight_confirm_with_no_ack_timeout_is_the_only_signal_still_reject_free() {
    let mut core = test_core();
    core.config.inflight_confirm = Some(std::time::Duration::from_millis(2_000));
    // submit_ack_timeout stays None (reject ladder disabled)
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    // far past any conceivable timeout — still confirmed, NEVER rejected
    core.engine.now_ms = 500_000;
    core.sweep_inflight_confirms(500_000);
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()]);
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_exec::OrderStatus::Initialized,
        "no reject timeout configured ⇒ the fast rung must never invent a terminal"
    );
    // the reject ladder, being disabled, does nothing either
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_exec::OrderStatus::Initialized);
}

/// The additive `Command::ConfirmOrder` verb routes to the owning engine's `client.confirm`
/// (exactly like `Command::Cancel`) — the on-demand GUI/session path to the same active confirm
/// the watchdog issues.
#[test]
fn confirm_order_command_routes_to_the_owning_client() {
    let mut core = test_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.dispatch(Ingest::Command(Command::Order(OrderIntent::Confirm("c1".into()))));
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()]);
}

/// A terminal order is never confirmed — `confirm_order`'s is-live gate (a filled/rejected order
/// needs no status re-query and must not spend a venue round-trip).
#[test]
fn confirm_order_command_skips_a_terminal_order() {
    let mut core = test_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.bus.publish(
        Event::OrderRejected(OrderRejected {
            client_order_id: "c1".into(),
            reason: "x".to_string().into(),
            ts: 0,
        }),
        &mut core.engine,
    );
    assert!(core.engine.registry["c1"].status.is_terminal());
    core.dispatch(Ingest::Command(Command::Order(OrderIntent::Confirm("c1".into()))));
    assert!(core.engine.client.confirms.is_empty(), "a terminal order must not be confirmed");
}

#[traced_test]
#[test]
fn enter_safe_state_emits_error_event() {
    // build a runtime core in its normal test way, then trip the safe state.
    let mut core = test_core();
    core.enter_safe_state("boom".to_string());
    assert!(logs_contain("boom")); // from tracing_test: the error! fired with the reason
}

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

// ---- portfolio-observer PR-3 T4: the timer-armed equity sampler ----

/// Open (or resize/close) a BOTH-side position directly on an account — the same shortcut
/// `snapshot.rs`'s `engine_with_position` helper uses, bypassing the full OMS FSM (the
/// sampler only reads folded `account.positions` state, so driving a real fill adds nothing
/// but noise here). Setting `size` to `0.0` mirrors what a real close leaves behind: the
/// entry is NOT removed, only zeroed — see [`CoreThread::any_position_open`]'s doc.
fn set_position(
    engine: &mut ExecutionEngine<RecordingClient>,
    venue: &str,
    symbol: &str,
    size: f64,
    avg_px: f64,
) {
    engine.account.positions.insert(
        (venue.into(), symbol.into(), "BOTH".into()),
        PositionEntry { size, avg_px, ..Default::default() },
    );
}

#[test]
fn any_position_open_ignores_zero_size_entries() {
    let mut core = test_core();
    assert!(!core.any_position_open(), "a fresh engine has no positions");
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    assert!(core.any_position_open(), "a nonzero-size entry is open");
    // closing leaves a ZERO-SIZE entry behind — the map is never shrunk back to empty
    set_position(&mut core.engine, "sim", "BTCUSDT", 0.0, 100.0);
    assert!(!core.engine.account.positions.is_empty(), "the zero-size entry stays in the map");
    assert!(!core.any_position_open(), "a zero-size entry must read as flat, never as open");
}

#[test]
fn any_position_open_scans_extra_engines() {
    let primary = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let extra = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "ETHUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    let mut core = assemble_core(
        primary,
        vec![(0.0, extra)],
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    assert!(!core.any_position_open(), "neither engine has a position yet");
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 3.0, 50.0);
    assert!(core.any_position_open(), "an open position on an EXTRA engine still counts");
}

/// Drive a fill so a position opens, advance the mock clock past the interval (via direct
/// boundary-method calls — the SAME manual-clock harness `watchdog_wheel_matches_direct_sweep_
/// transitions` above uses for the stuck-order timer), and assert the injected closure
/// received a batch with both a `"TOTAL"` row and at least one per-venue row.
#[test]
fn sampler_arms_on_open_and_fires_samples() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = test_core();
    core.config.equity_sample = Some(Duration::from_millis(1_000));
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));

    // flat book: the boundary must not arm anything
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_none(), "flat book must not arm the sampler");

    // open a position -> the boundary arms the timer for now + interval
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_some(), "an open position must arm the sampler");

    // advance the mock clock PAST the interval and drive the boundary — the timer fires
    core.drive_due_timers(1_000);
    let rows = captured.lock().unwrap();
    assert!(rows.iter().any(|r| r.venue == "TOTAL"), "batch must include a TOTAL row");
    assert!(rows.iter().any(|r| r.venue != "TOTAL"), "batch must include a per-venue row");
}

/// Open then fully CLOSE a position (leaving the zero-size entry `any_position_open_ignores_
/// zero_size_entries` proved is never removed) and assert the timer is cancelled: no further
/// samples fire even long past the interval that would otherwise have re-armed it.
#[test]
fn sampler_disarms_when_flat() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = test_core();
    core.config.equity_sample = Some(Duration::from_millis(1_000));
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));

    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_some(), "armed while open");

    // close the position: a CLOSED position leaves a ZERO-SIZE entry, never removed
    set_position(&mut core.engine, "sim", "BTCUSDT", 0.0, 100.0);
    assert!(!core.engine.account.positions.is_empty(), "zero-size entry stays in the map");
    core.maintain_equity_timer(500); // well before the original deadline (1000)
    assert!(core.equity_timer.is_none(), "flat must cancel the armed timer");

    // advance well past the original deadline and drive the boundary: nothing fires
    core.drive_due_timers(5_000);
    assert!(captured.lock().unwrap().is_empty(), "a cancelled timer must not fire");
}

/// The `"TOTAL"` row `sample_equity` produces must equal `py_sum` of its own per-venue rows
/// AND `CoreSnapshot`'s `equity_total` for the identical engine state — the same cross-venue
/// aggregate law, bit-for-bit (`to_bits()`), computed two ways.
#[test]
fn sample_total_equals_snapshot_law() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 2.0, 100.0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 105.0, 0);
    core.engine.now_ms = 1_234;

    core.sample_equity(1_234);
    let total =
        core.equity_rows.iter().find(|r| r.venue == "TOTAL").expect("TOTAL row present").clone();
    assert!(total.equity != 0.0, "sanity: the priced position must move equity off the seed");

    let snap = CoreSnapshot::build(
        1,
        &core.engine,
        &core.extra_engines,
        core.config.seed_cash,
        core.config.price_cfg,
        vike_exec::MarginCallConfig::default().mm_requirement,
        &core.recent,
        &core.bars,
        &[],
        &core.fault,
        0,
        0,
        crate::snapshot::ReconBlock::default(),
        &[],
    );
    assert_eq!(
        total.equity.to_bits(),
        snap.portfolio.equity_total.to_bits(),
        "TOTAL must match CoreSnapshot's equity_total law"
    );

    let per_venue_sum = vike_model::py_sum(
        core.equity_rows.iter().filter(|r| r.venue != "TOTAL").map(|r| r.equity),
    );
    assert_eq!(total.equity.to_bits(), per_venue_sum.to_bits(), "TOTAL == py_sum(per-venue)");
}

/// The warn-once missing-price wiring PR-2's `resolve_equity` doc comment deferred to this
/// sampler: an open position with NO priceable source on the board must be `note()`d as
/// `Missing` on that engine's OWN `PriceBoard` — the same bookkeeping `PriceBoard::note`'s
/// own unit tests (`note_tracks_missing_and_rearms_on_recovery`) exercise directly, wired
/// here through `sample_equity` instead of a bare `note()` call.
#[test]
fn sample_equity_notes_missing_prices_on_the_board() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    // no mark/quote/trade/bar_close ever set on the board -> resolves Missing
    assert!(
        core.engine.price_board.missing_price_instruments("sim").is_none(),
        "nothing noted before the sampler ever runs"
    );

    core.sample_equity(0);

    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_some_and(|s| s.contains("BTCUSDT")),
        "an unpriced open position must be noted as missing on that engine's board"
    );
    let row = core.equity_rows.iter().find(|r| r.venue == "sim").expect("per-venue row present");
    assert_eq!(row.missing_prices, 1);
}

/// The recovery half of the same wiring: `PriceBoard::note`'s `Priced` arm (proven directly
/// by `PriceBoard`'s own `note_tracks_missing_and_rearms_on_recovery`) must ALSO be reached
/// through the sampler, not just `Missing`. Without it a symbol that resolves Missing once
/// and later recovers stays stuck in the per-venue missing set forever (the warn-once never
/// re-arms). Sample once with no price on the board (misses), feed the board a price, sample
/// again, and assert the symbol is gone from `missing_price_instruments`.
#[test]
fn sampler_note_clears_missing_on_recovery() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);

    core.sample_equity(0);
    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_some_and(|s| s.contains("BTCUSDT")),
        "unpriced open position must be noted missing on the first sample"
    );

    // the board now has a price for the position -> the NEXT sample must resolve it Priced
    // and CLEAR the stale missing entry, not leave it stuck forever.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 1);
    core.sample_equity(1);

    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_none_or(|s| !s.contains("BTCUSDT")),
        "a position that resolves Priced must be note()'d too, clearing it from the missing set"
    );
}

// ---- portfolio-observer PR-4 T5: per-mount readiness gate -------------------------------

/// A minimal maker-shaped strategy for the readiness-gate tests: submits a market buy on
/// EVERY quote tick, unconditionally, and counts its own calls. "Did an order reach the
/// engine" is then a direct proxy for "did `drain_broker` actually drain this dispatch's
/// buffered submit" (Pending must discard it; Ready must not) — and the call counter
/// independently proves the hook itself still ran while Pending (warmup/observation must be
/// unaffected by the gate; only the ORDER OUTPUT is gated).
struct AlwaysSubmitStrategy {
    calls: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

fn quote_tick() -> QuoteTick {
    QuoteTick {
        ts: 0,
        local_ts: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "BTCUSDT".into(),
    }
}

fn maker_mount(calls: Arc<AtomicUsize>) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(AlwaysSubmitStrategy { calls }),
    }
}

/// Like [`test_core`] but with a caller-supplied [`CoreConfig`] — the readiness-gate tests
/// need a mounted strategy + `readiness_gate` set, neither of which `test_core`'s hardcoded
/// `CoreConfig::default()` allows.
fn core_with(config: CoreConfig) -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(engine, Vec::new(), config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

/// gate ON, a maker mount, NO board price for its symbol -> a quote-tick dispatch that would
/// normally submit is DISCARDED (the engine's order registry stays empty) while Pending, even
/// though the strategy hook itself still ran. Feed the board a price and re-probe: the mount
/// flips Ready, and the NEXT submit DOES land in the registry.
#[test]
fn pending_mount_discards_submits_until_symbol_prices() {
    let calls = Arc::new(AtomicUsize::new(0));
    let config = CoreConfig {
        readiness_gate: true,
        strategy: Some(maker_mount(Arc::clone(&calls))),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    assert_eq!(
        core.mount_states,
        vec![MountState::Pending],
        "readiness_gate: true must seed every mount Pending"
    );

    // no board price yet -> a dispatch that would normally submit must be discarded.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the hook must still fire while Pending");
    assert!(
        core.engine.registry.is_empty(),
        "a Pending mount's buffered submit must never reach the engine"
    );

    // the boundary probe with STILL no board price must not flip it.
    core.maintain_mount_readiness(0);
    assert_eq!(core.mount_states[0], MountState::Pending, "no price yet -> still Pending");

    // the symbol prices -> the boundary probe flips Pending -> Ready.
    core.engine.price_board.set_quote("sim", "BTCUSDT", 99.0, 101.0, 0);
    core.maintain_mount_readiness(0);
    assert_eq!(
        core.mount_states[0],
        MountState::Ready,
        "a priced symbol must flip Pending -> Ready"
    );

    // a submit now DOES land.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 2, "the hook ran again on the Ready dispatch");
    assert_eq!(core.engine.registry.len(), 1, "a Ready mount's submit must reach the engine");
}

/// gate OFF (the default): a maker submits immediately, exactly as today — no Pending
/// suppression, and the boundary probe is never even reachable (every mount starts Ready).
#[test]
fn readiness_gate_off_is_byte_identical() {
    assert!(!CoreConfig::default().readiness_gate, "sanity: the gate defaults to off");
    let calls = Arc::new(AtomicUsize::new(0));
    let config =
        CoreConfig { strategy: Some(maker_mount(Arc::clone(&calls))), ..CoreConfig::default() };
    let mut core = core_with(config);
    assert_eq!(
        core.mount_states,
        vec![MountState::Ready],
        "readiness_gate: false must seed every mount Ready"
    );

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        core.engine.registry.len(),
        1,
        "gate off: a submit lands immediately, exactly like before this feature existed"
    );
}

// ---- one-price law: the decision lanes read the SAME resolver-priced equity the display does --

/// Capture-only strategy for the one-price-law test: records `ctx.equity` on every quote tick.
struct EquityCapture {
    seen: Arc<Mutex<Vec<f64>>>,
}
impl Strategy<LiveBroker> for EquityCapture {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.seen.lock().unwrap().push(broker.equity);
    }
}

/// A position with NO mark recorded but a live quote on the board: the strategy's `ctx.equity`
/// must now equal the snapshot's resolver-priced equity bit-for-bit (previously it was
/// `equity_all`'s silent-zero, differing by the FULL unrealized amount) — and must NOT equal the
/// mark-only law, proving the decision lane genuinely switched sources.
#[test]
fn strategy_ctx_equity_matches_snapshot_for_quote_only_position() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let config = CoreConfig {
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(EquityCapture { seen: Arc::clone(&seen) }),
        }),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    // bid/ask on the board, NO mark slot — the resolver values the long at the BID (104)
    core.engine.price_board.set_quote("sim", "BTCUSDT", 104.0, 106.0, 1);

    core.drive_strategy_tick("sim", "BTCUSDT", 105.0, 2, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let ctx_equity = *seen.lock().unwrap().first().expect("hook captured equity");

    let snap = CoreSnapshot::build(
        1,
        &core.engine,
        &core.extra_engines,
        core.config.seed_cash,
        core.config.price_cfg,
        vike_exec::MarginCallConfig::default().mm_requirement,
        &core.recent,
        &core.bars,
        &[],
        &core.fault,
        0,
        0,
        crate::snapshot::ReconBlock::default(),
        &[],
    );
    assert_eq!(
        ctx_equity.to_bits(),
        snap.portfolio.venues[0].equity.to_bits(),
        "decision-lane equity must equal the displayed snapshot equity bit-for-bit"
    );
    // the tick path marked the account at the tick price (105), so the mark-only law differs:
    // the two lanes genuinely read different stores, and the decision lane now reads the board's.
    assert!(
        ctx_equity != core.engine.account.equity_all(core.config.seed_cash),
        "quote-priced ctx.equity must diverge from the mark-only legacy law here"
    );
}

/// Drawdown latch on a loss visible ONLY through the quote lane (no mark ever recorded): the
/// resolver-priced sweep must latch Reducing. Under the old `equity_all` source both sweeps read
/// the bare seed (unmarked position = silent zero) and the crash was invisible by construction.
#[test]
fn drawdown_latch_acts_on_resolver_priced_equity() {
    let mut core = test_core();
    core.config.seed_cash = 1_000.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    // flat quote first: equity 1000 seeds the high-water-mark
    core.engine.price_board.set_quote("sim", "BTCUSDT", 100.0, 100.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "no drawdown at the HWM seed");
    // the bid crashes to 40 (long valued at bid): equity 1000 + 10·(40-100) = 400 → 60% > 20%
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "a quote-lane crash must trip the latch (mark-only equity never saw it)"
    );
    assert!(
        core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH")),
        "the latch warning lands in the ring: {:?}",
        core.recent
    );
}

// ---- the drawdown latch measures the daemon's OWN PnL, never a wallet -----------------------
//
// The the CI box shape, measured 2026-08-17: nine paper `Delta` blocks at 1000 seed each plus one
// `Authoritative` bybit block carrying 53647.10600813 — the whole UNIFIED wallet of a SHARED demo
// account — with the HWM latched at ~62647. `sweep_drawdown_latch` used to fold
// `Σ ExecutionEngine::resolved_equity`, which includes that wallet, so (1) a third party
// withdrawing from it trips this daemon into liquidate-only and (2) a 25% loss on the daemon's own
// ~9000 of book is 3.6% of 62647 and never trips it. The tests below pin BOTH directions.

/// A `(primary sim Delta, extra bybit)` two-engine core with `seed_cash` on both, so the latch's
/// capital base is `2 · seed`. The bybit engine's account is flipped to `Authoritative` with
/// `wallet` USDT through the REAL adoption path (`Account::apply_account_state`, the same fold a
/// live venue `AccountState` frame and `CoreThread::reconcile_reports` both take) rather than by
/// assigning the field — so the test exercises the mode the defect lives in.
fn dd_core_with_venue_wallet(seed: f64, wallet: f64) -> CoreThread<RecordingClient> {
    let primary = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let mut extra = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "ETHUSDT",
    );
    dd_set_wallet(&mut extra, wallet);
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    let mut core = assemble_core(
        primary,
        vec![(seed, extra)],
        CoreConfig { seed_cash: seed, ..CoreConfig::default() },
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    core.config.seed_cash = seed;
    core
}

/// Adopt `wallet` USDT authoritatively onto `eng` — the venue's attested balance for the WHOLE
/// account the credentials open. Absolute, not additive (see `Account::apply_account_state`), so
/// calling it twice is exactly what a third-party deposit/withdrawal looks like from here.
fn dd_set_wallet(eng: &mut ExecutionEngine<RecordingClient>, wallet: f64) {
    eng.account.apply_account_state(
        &vike_model::events::AccountState {
            venue: "bybit".into(),
            balances: vec![("USDT".to_string(), wallet)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );
    assert_eq!(
        eng.account.balance_mode,
        BalanceMode::Authoritative,
        "the frame must flip the mode"
    );
}

/// ⚠ **A THIRD PARTY moving money in a shared venue wallet must not trip this daemon.** The
/// daemon's own book is FLAT throughout: no position, no fill, no fee, no funding. Only the
/// venue-attested balance changes, by −23647.
///
/// FAILS on the pre-fix latch: `Σ resolved_equity` seeds an HWM of `1000 + 53647.10600813` and
/// then reads `1000 + 30000`, a 43.3% drawdown, so the whole core latches liquidate-only because
/// somebody else took their money out.
#[test]
fn a_third_party_wallet_movement_does_not_move_the_drawdown_latch() {
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.pnl_curve_peak,
        Some(2_000.0),
        "the HWM is the CONFIGURED capital base (2 × 1000), not the 54647 the account holds"
    );
    // ...and now somebody else withdraws 23647 from the shared account.
    dd_set_wallet(&mut core.extra_engines[0].1, 30_000.0);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a wallet this daemon does not own moved; its own PnL did not, so nothing may latch"
    );
    assert_eq!(
        core.extra_engines[0].1.trading_state,
        TradingState::Active,
        "and the venue engine stays Active too — the latch flips every engine or none"
    );
    assert!(
        !core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH")),
        "no latch warning, and no DISARMED warning either: {:?}",
        core.recent
    );
    assert_eq!(core.pnl_curve_peak, Some(2_000.0), "the HWM did not move either");
}

/// The other half, and the one that must NOT be lost in the process: a real loss on the daemon's
/// OWN book latches at the configured fraction of its own capital, even while a large adopted
/// wallet sits beside it. 25% of 2000 of capital, with 53647 of somebody else's money in the same
/// account.
///
/// FAILS on the pre-fix latch: `Σ resolved_equity` moves from 54647.1 to 54147.1, a 0.91%
/// drawdown, so a 20% rule never fires — the exact invisibility the the CI box measurement showed.
#[test]
fn a_real_loss_on_the_daemons_own_book_latches_at_the_threshold() {
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    // The daemon's own position, on the VENUE engine (the live mount — where the loss really is).
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 100.0, 100.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "flat at the HWM seed");
    assert_eq!(core.pnl_curve_peak, Some(2_000.0), "HWM = the capital base, own PnL still 0");

    // The long is valued at the BID, which drops to 50: own PnL = 10·(50−100) = −500, i.e. 25% of
    // the 2000 capital base. Nothing about the venue WALLET changed.
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "a 25% loss of the daemon's own capital must latch a 20% rule"
    );
    assert_eq!(
        core.extra_engines[0].1.trading_state,
        TradingState::Reducing,
        "every engine latches, so the venue mount is liquidate-only too"
    );
    let warn = core
        .recent
        .iter()
        .find(|e| e.starts_with("DRAWDOWN LATCH:"))
        .expect("the latch warning lands in the ring");
    assert!(warn.contains("capital_base=2000.00"), "warning names the base: {warn}");
    assert!(warn.contains("own_pnl=-500.00"), "…and the daemon's own PnL: {warn}");
    assert!(warn.contains("drawdown=25.00%"), "…and the fraction it is 25% of: {warn}");
}

/// STARTUP + RESTART. Before any PnL exists the curve IS the capital base, so the latch is armed
/// against configured capital from the FIRST sweep (the old code seeded from the first OBSERVED
/// equity, i.e. from whatever the wallet held at that instant). Across a restart the four
/// `Account` PnL terms come back through `AccountSnapshot`, so the curve RESUMES underwater — and
/// because the peak seeds at `max(capital_base, curve)` rather than at the curve, a restart
/// forgives no loss booked below configured capital.
#[test]
fn the_drawdown_peak_seeds_at_configured_capital_not_at_an_underwater_restart_curve() {
    // A core whose book is ALREADY down 500 at the first sweep — the post-restart shape.
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.pnl_curve_peak,
        Some(2_000.0),
        "the peak seeds at the capital base, NOT at the 1500 curve it restarted underwater on"
    );
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "so the pre-restart 25% loss is still a 25% drawdown and still latches"
    );
}

/// ⚠ The one failure direction worse than measuring against the wrong number: measuring against
/// NOTHING. A non-positive capital base leaves no denominator for the fraction, so the latch cannot
/// arm — and it says so ONCE rather than sitting silent. (`RunProfile::validate` refuses this
/// combination at load; this is the belt for a `CoreConfig` assembled directly.)
#[test]
fn a_non_positive_capital_base_disarms_loudly_and_only_once() {
    let mut core = dd_core_with_venue_wallet(0.0, 53_647.10600813);
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 1);
    core.sweep_drawdown_latch(0.2);
    core.sweep_drawdown_latch(0.2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "no fraction exists, so nothing can be judged breached"
    );
    let notes: Vec<_> =
        core.recent.iter().filter(|e| e.starts_with("DRAWDOWN LATCH DISARMED")).collect();
    assert_eq!(notes.len(), 1, "said once, not once per closed bar: {:?}", core.recent);
    assert!(notes[0].contains("seed_cash"), "and it names the knob to set: {}", notes[0]);
}

/// Margin-call sweep prices BOTH sides of the breach test through the resolver (risk-lane
/// completion): equity AND maintenance margin read the same board, so a STALE `Account.marks`
/// scalar can no longer sit on one side of the comparison.
///
/// Part 1 — the #518 asymmetry, healed: stale mark 100, fresh quote 40. The OLD split basis
/// judged margin off the stale mark (10·100·0.05 = 50) against a fresh-quote equity (40) and
/// LIQUIDATED an account that is genuinely healthy at the real price (maintenance
/// 10·40·0.05 = 20 ≤ equity 40 with room). One basis ⇒ Healthy.
/// Part 2 — a genuine quote-lane breach still liquidates, with the plan itself priced off the
/// quote (candidates and per-unit margin at 40, not the stale 100).
#[test]
fn margin_call_sweep_prices_margin_and_equity_on_one_basis() {
    let cfg =
        vike_exec::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };

    // Part 1: stale-mark overstatement no longer manufactures a liquidation.
    let mut core = test_core();
    core.config.seed_cash = 640.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale mark — no longer consulted
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1); // fresh crash
                                                                        // one basis: equity = 640 + 10·(40−100) = 40; margin_used = 10·40·0.05 = 20 → healthy
    core.sweep_margin_call(&cfg, 3);
    assert!(
        !core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "a stale mark must not overstate margin against fresh-quote equity: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty(), "no liquidation order for a healthy book");

    // Part 2: a genuine breach on the one basis liquidates, plan priced off the quote.
    let mut core = test_core();
    core.config.seed_cash = 100.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale mark — no longer consulted
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1);
    // equity = 100 − 600 = −500; margin_used = 20 → breach (remaining ≤ 0, LEAN buffer holds);
    // excess 520 at per-unit 40·0.05 = 2 → capped at the full 10 units.
    core.sweep_margin_call(&cfg, 3);
    assert!(
        core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "a genuine quote-lane breach must still liquidate: {:?}",
        core.recent
    );
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "one reduce-only liquidation order reaches the client"
    );
    let liq = &core.engine.client.submissions[0];
    assert!(liq.reduce_only, "liquidation is reduce-only");
    assert_eq!(liq.qty, 10.0, "the plan prices the close off the quote basis (full close)");
}

/// **The closed-bar lane meets the margin-call sweep** — the exact ten-line window this round of
/// the fix is about. `drive_strategy` writes the bar close into the account mark slot and then
/// calls `sweep_margin_call`, so before the law moved inside `Account::set_mark_from` a crashing
/// candle could displace a fresh venue mark and be the basis of a liquidation decision in the
/// same call.
///
/// Two bases are asserted separately, because they are NOT the same code path and only one of
/// them was ever exposed:
/// - the RUNTIME sweep is board-priced (#518/#524): it resolves through `price_board`, whose
///   mark and bar_close are separate source-tagged slots, so it was already immune. Pinned here
///   so a future change that re-points it at `Account.marks` fails loudly.
/// - the ACCOUNT slot itself — read by the pre-trade gate, `margin_in_use_by`, `equity_all`, the
///   trailing-stop seed, `LiveBroker.price` and the legacy account-priced `check_margin_call`
///   entry — WAS exposed. The non-vacuity half below shows that basis genuinely flipping from
///   Healthy to Liquidate once the mark ages out, which is exactly what a crashing close would
///   have done at any time before this fix.
#[test]
fn a_closed_bar_cannot_hand_the_margin_call_sweep_a_candle_close_basis() {
    let cfg =
        vike_exec::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let key: SeriesKey = ("sim".to_string(), "BTCUSDT".to_string(), "1m".to_string());
    // long 10 @ 100, seeded 640: healthy at the mark (used 50 vs equity 640), and deeply
    // underwater on the account basis if a crash close were to take the slot (equity −260).
    let crash = Bar {
        ts: 60_000,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };

    let mut core = test_core();
    core.config.seed_cash = 640.0;
    core.config.margin_call = Some(cfg);
    core.engine.now_ms = 1_000;
    core.engine.account.set_mark_staleness_ms(core.config.mark_staleness_ms);
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 1_000);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 1_000);

    core.drive_strategy(&key, &crash);

    assert_eq!(
        core.engine.account.mark_of("sim", "BTCUSDT"),
        Some(100.0),
        "the closed-bar lane must not displace a fresh venue mark"
    );
    assert!(
        !core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "the sweep that runs ten lines later must not liquidate on a candle-close basis: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty());
    // the account basis, at the mark: used 10·100·0.05 = 50 vs equity 640 → healthy
    let equity = core.engine.account.equity_all(640.0);
    assert_eq!(
        vike_exec::check_margin_call(&core.engine.account, equity, &cfg),
        vike_exec::MarginCall::Healthy
    );

    // NON-VACUITY: age the mark past the ownership window and drive the SAME bar. The close now
    // legitimately reclaims the slot, and the account basis flips to a liquidation — i.e. the
    // assertion above is pinning a real difference, not an inert one.
    core.engine.now_ms = 1_000 + core.config.mark_staleness_ms + 1;
    core.drive_strategy(&key, &crash);
    assert_eq!(core.engine.account.mark_of("sim", "BTCUSDT"), Some(10.0), "a silent mark releases");
    let equity = core.engine.account.equity_all(640.0);
    assert!(
        matches!(
            vike_exec::check_margin_call(&core.engine.account, equity, &cfg),
            vike_exec::MarginCall::Liquidate(_)
        ),
        "the account basis really does swing on which concept holds the slot"
    );
}

// ---- steal/core-per-mount-budget: per-mount fill ATTRIBUTION + optional loss/notional BUDGET ----

/// A mount that submits ONE market order per hook call. Under [`RecordingClient`] the order is
/// RECORDED but never fills, so it RESTS — a mount whose resting order the budget latch can cancel
/// and whose post-latch intents `drain_broker` discards. The `symbol` arg is ignored (the mount
/// pins its own symbol), so this one strategy serves every mount.
struct BudgetSubmit;
impl Strategy<LiveBroker> for BudgetSubmit {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        broker.submit_market("x", 1, 1.0);
    }
}

fn budget_mount(symbol: &str) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(BudgetSubmit),
    }
}

/// One `mount_id -> budget` entry — the [`CoreConfig::mount_budgets`] shape. These mounts carry no
/// `controller_id`, so their id is the legacy `{venue}__{symbol}__{interval}` derivation.
fn one_budget(symbol: &str, b: MountBudget) -> std::collections::HashMap<String, MountBudget> {
    let mut m = std::collections::HashMap::new();
    m.insert(crate::strategy_state::mount_id_of("sim", symbol, "1m"), b);
    m
}

/// A fill for `coid` on (sim, symbol). Distinct `trade_id` per (coid, side, px) so the account's
/// per-trade dedup never collapses two injected fills. `mark_price: None` on purpose — a fill with a
/// mark writes the `PriceBoard` mark slot (engine `on_event`), which would then dominate the crash
/// price the sweep tests set via `set_mark`; leaving it `None` keeps the board reflecting only what
/// each test explicitly sets.
fn coid_fill(coid: &str, symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // No static prefix to hoist, so this is `new` + `expect` rather than `prefixed`: keeping the
        // id byte-identical matters more than the shape, and `coid` is a caller literal here.
        trade_id: TradeId::new(format!("{coid}-{side}-{px}")).expect("coid is never empty"),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn budget_bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A scratch journal directory for the margin-budget lane, removed when the guard drops. The
/// journal's own `open` calls `create_dir_all`, so this reserves the path without creating it.
fn budget_journal_dir() -> crate::scratch::Scratch {
    crate::scratch::Scratch::reserved("mb")
}

/// The coid `mount_idx` currently owns (its first, in insertion order) — the submit path tags it.
fn coid_of(core: &CoreThread<RecordingClient>, mount_idx: usize) -> String {
    core.coid_mount
        .iter()
        .find(|(_, &m)| m == mount_idx)
        .map(|(c, _)| c.clone())
        .expect("mount submitted at least one order")
}

/// OFF/default: with an EMPTY `mount_budgets`, no mount is watched — the sweep is never armed, a
/// direct sweep call no-ops, and NOTHING latches or cancels on even a catastrophic loss. The
/// attribution ledger STILL folds (it is additive + read-only and never gates a fold decision), so
/// trading behavior is byte-identical to a budget-free runtime.
#[test]
fn mount_budget_off_is_byte_identical() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        ..CoreConfig::default() // mount_budgets: empty (the default)
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    assert!(!core.any_mount_budget, "no active budget must leave the sweep un-armed");

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid, "BTCUSDT", 1, 10.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    // a catastrophic crash on the board, then a DIRECT sweep call: still no latch (no budget).
    core.engine.price_board.set_mark("sim", "BTCUSDT", 1.0, 2);
    core.sweep_mount_budgets(3);

    assert!(!core.mount_latched[0], "no budget must never latch, even on a huge loss");
    assert!(core.engine.client.cancels.is_empty(), "no budget must issue no scoped cancel");
    assert_eq!(core.engine.trading_state, TradingState::Active, "account state untouched");
    assert_eq!(
        core.mount_attr[0].size.to_bits(),
        10.0_f64.to_bits(),
        "attribution still folds (additive, read-only)"
    );
}

/// ATTRIBUTION: fills route to the ORIGINATING mount's ledger by coid (not by (venue, symbol)), and
/// a realized-PnL close is booked to that mount ONLY — a sibling mount's ledger is untouched.
#[test]
fn mount_attribution_folds_fills_to_the_originating_mount() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        extra_mounts: vec![budget_mount("ETHUSDT")],
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    let coid_b = coid_of(&core, 1);
    assert_ne!(coid_a, coid_b);

    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 3.0, 100.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill(&coid_b, "ETHUSDT", 1, 7.0, 50.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size.to_bits(), 3.0_f64.to_bits());
    assert_eq!(core.mount_attr[0].avg_px.to_bits(), 100.0_f64.to_bits());
    assert_eq!(core.mount_attr[1].size.to_bits(), 7.0_f64.to_bits());
    assert_eq!(core.mount_attr[1].avg_px.to_bits(), 50.0_f64.to_bits());

    // a close of A's BTC (sell 3 @ 110) attributes +30 realized to mount 0 only.
    core.coid_mount.insert("cA-close".to_string(), 0);
    core.bus
        .publish(Event::Fill(coid_fill("cA-close", "BTCUSDT", -1, 3.0, 110.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "A's position closed");
    assert!(
        (core.mount_attr[0].realized_pnl - 30.0).abs() < 1e-9,
        "A realized {}",
        core.mount_attr[0].realized_pnl
    );
    assert_eq!(
        core.mount_attr[1].realized_pnl.to_bits(),
        0.0_f64.to_bits(),
        "B's ledger untouched by A's close"
    );
}

/// BUDGET: a mount breaching `max_loss` latches liquidate-only — its OWN resting order is canceled
/// (scoped by coid), its post-latch intents are discarded, and the ACCOUNT trading_state is NOT
/// touched — while a SIBLING mount with no budget is untouched and keeps trading.
#[test]
fn mount_budget_latches_liquidate_only_and_leaves_the_sibling() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        extra_mounts: vec![budget_mount("ETHUSDT")],
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: false },
        ),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;
    assert!(core.any_mount_budget, "an active budget arms the sweep");

    // A rests an order + takes a long 1 @ 100; B rests its own order.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 1.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_b = coid_of(&core, 1);

    // BTCUSDT crashes to 40: A's long 1 @ 100 marks -60 loss > 50.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 2);
    core.sweep_mount_budgets(3);

    assert!(core.mount_latched[0], "A breached max_loss -> latched");
    assert!(!core.mount_latched[1], "B has no budget -> never latched");
    assert!(
        core.engine.client.cancels.contains(&coid_a),
        "A's resting order canceled: {:?}",
        core.engine.client.cancels
    );
    assert!(
        !core.engine.client.cancels.contains(&coid_b),
        "the sibling's resting order must NOT be canceled"
    );
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a per-mount latch must NOT flip the account-wide trading_state"
    );

    // A's post-latch intents are discarded; the sibling keeps trading.
    let n = core.engine.registry.len();
    core.drive_strategy_tick("sim", "BTCUSDT", 40.0, 4, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(core.engine.registry.len(), n, "a latched mount's new submit is discarded");
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 5, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(core.engine.registry.len(), n + 1, "the sibling mount keeps trading");

    // the widened per-mount view (published on CoreSnapshot via arc-swap) surfaces the latch, each
    // mount's budget, and the resolver-priced attributed position — read-only.
    let views = core.mount_views();
    assert!(views[0].latched, "the published view surfaces mount 0's latch");
    assert!(!views[1].latched, "the sibling view is not latched");
    assert!(views[0].budget.is_some(), "the view carries mount 0's budget");
    assert!(views[1].budget.is_none(), "the sibling carries no budget");
    assert_eq!(
        views[0].position.to_bits(),
        1.0_f64.to_bits(),
        "the view surfaces mount 0's attributed net position"
    );
}

/// BUDGET (max_notional arm): a mount whose gross notional exceeds `max_notional` latches, even with
/// no loss — proving the notional arm is independent of the loss arm.
#[test]
fn mount_budget_latches_on_notional() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: None, max_notional: Some(150.0), flatten_on_breach: false },
        ),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    // long 2 @ 100; mark 100 -> gross notional 200 > 150 (no loss at all).
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 2.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 2);
    core.sweep_mount_budgets(3);
    assert!(core.mount_latched[0], "notional 200 > 150 must latch");
    assert!(core.engine.client.cancels.contains(&coid_a), "and cancel the mount's resting order");
}

/// BOUNDARY + JOURNAL: the latch fires at the per-CLOSED-bar boundary (via `drive_strategy`, not a
/// direct sweep call), and with `flatten_on_breach` the reduce-only MARKET flatten is journaled
/// write-ahead as a `MarginCallLiquidate` record (same shape + replay contract as a margin call) —
/// carrying, since journal v14, the OWNING mount id, which is what lets a restore book its fill into
/// that mount's ledger rather than the residual row.
#[test]
fn mount_budget_flatten_fires_at_bar_boundary_and_is_journaled() {
    let dir = budget_journal_dir();
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: true },
        ),
        journal: Some(JournalConfig::at(dir.to_path_buf())),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;

    // A rests an order + takes a long 1 @ 100.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 1.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    // crash the board mark to 40 (top of the resolve chain, robust), then let a CLOSED BAR drive
    // drive_strategy so the budget sweep runs AT THE BOUNDARY (no direct sweep call) and latches
    // (A's long 1 @ 100 marks -60 loss > 50) + flattens.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 2);
    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: budget_bar(60_000, 40.0),
    })));
    assert!(core.mount_latched[0], "the latch must fire at the per-closed-bar boundary");
    assert!(core.engine.client.cancels.contains(&coid_a), "and cancel the resting order");

    // a reduce-only MARKET flatten (sell 1 to close the long) was submitted...
    let flat = core
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.reduce_only && r.order_type == "market")
        .expect("a reduce-only flatten market was submitted");
    assert_eq!(flat.symbol, "BTCUSDT");
    assert!((flat.qty - 1.0).abs() < 1e-12);
    assert_eq!(flat.side, vike_model::closing_side(1.0), "sell to close a long");

    // ...and it was journaled WRITE-AHEAD as a MarginCallLiquidate record. Flush + DROP the core
    // (releasing the journal's file handle + single-writer lock) BEFORE reading, so `read_all` never
    // races the live writer's open handle (Windows exclusive-open safety).
    core.journal.as_mut().unwrap().flush().unwrap();
    drop(core);
    let records = crate::journal::CommandJournal::read_all(&dir).unwrap();
    let want_mount = crate::strategy_state::mount_id_of("sim", "BTCUSDT", "1m");
    assert!(
        records.iter().any(|r| matches!(
            r,
            crate::journal::JournalRecord::MarginCallLiquidate { req, mount_id, .. }
                if req.reduce_only && req.symbol == "BTCUSDT" && req.order_type == "market"
                    // v14: the OWNING mount rides the record, so the restore-side
                    // `fold_coid_mounts` credits this order to the mount that released it instead
                    // of leaving it — and its realized loss — in the residual row.
                    && mount_id.as_deref() == Some(want_mount.as_str())
        )),
        "the flatten must be journaled write-ahead, naming mount `{want_mount}`: {records:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
