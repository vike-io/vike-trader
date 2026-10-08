//! Safe-state entry, the stuck-order watchdog ladder, the in-flight confirm, the confirm command.

use super::*;

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
