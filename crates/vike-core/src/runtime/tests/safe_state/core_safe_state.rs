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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);

    // STAGE 1 — inside the confirm-grace (ack 1000 < age 1500 < reject 2000): warn + active
    // confirm, NO reject.
    core.engine.now_ms = 1500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Initialized,
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
        vike_model::OrderStatus::Initialized,
        "in-flight-confirm guard must DEFER the reject past the plain deadline"
    );
    assert!(core.confirm_issued_ms.contains_key("c1"), "still awaiting the in-flight confirm");

    // STAGE 2 — past the EXTENDED deadline (age 3500 > extended 3000) still pre-ack: hard reject.
    core.engine.now_ms = 3500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Rejected,
        "backstop terminalizes only after the EXTENDED confirm-grace elapses"
    );
    assert!(!core.confirm_issued_ms.contains_key("c1"), "cleared once terminalized");
}

/// W19g: the OPTIMISTIC accept (`vike_bridge_core::resolve_ambiguous_submit`'s `Err` arm: the re-query itself
/// failed, so `venue_order_id: None`) that the stage-1 confirm brings back for an order still `Initialized`
/// is no evidence the venue holds it. It must not take the order out of the ladder's scope (`Initialized |
/// Submitted`): the order that never reached the venue is still rejected at the extended deadline.
#[test]
fn watchdog_still_sweeps_an_initialized_order_after_an_optimistic_accept_with_no_venue_id() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    core.engine.now_ms = 1500;
    core.sweep_stuck_orders(); // stage 1: the active confirm
    assert!(core.confirm_issued_ms.contains_key("c1"));
    // the confirm's answer: the re-query failed, so the adapter reports an optimistic accept
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: None,
            ts: 1500,
        }),
        &mut core.engine,
    );
    assert!(
        matches!(
            core.engine.registry["c1"].status,
            vike_model::OrderStatus::Initialized | vike_model::OrderStatus::Submitted
        ),
        "an accept that names no venue order leaves the order in the watchdog's scope, not Accepted"
    );

    core.engine.now_ms = 3500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Rejected,
        "an order that may never have reached the venue is still terminalized by the backstop"
    );
}

/// W13c: an `Initialized` order that HOLDS a venue event (a fill that arrived before its ack, W19g) is
/// an order the venue knows. The backstop must not phantom-reject it at the extended deadline (the fill stays
/// booked on the account while the FSM order is `Rejected`): it asks again, once per grace window, and the
/// answer lands the held fill.
#[test]
fn watchdog_asks_again_instead_of_rejecting_an_initialized_order_that_holds_a_venue_fill() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    // the venue's fill arrives BEFORE any ack: refused from `Initialized`, held on the order
    core.bus.publish(
        Event::OrderFilled(OrderFilled {
            client_order_id: "c1".into(),
            fill: fill_for("c1"),
            ts: 0,
        }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].deferred_events.len(), 1, "the fill is held");

    core.engine.now_ms = 1500;
    core.sweep_stuck_orders(); // stage 1: the active confirm
    assert_eq!(core.confirm_issued_ms.get("c1"), Some(&1500));

    // past the EXTENDED deadline (3000), where an order with nothing held is rejected
    core.engine.now_ms = 3500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Initialized,
        "the venue holds this order: not rejected"
    );
    assert_eq!(core.confirm_issued_ms.get("c1"), Some(&3500), "asked again instead");

    // inside the next grace window it does not ask on every sweep
    core.engine.now_ms = 4000;
    core.sweep_stuck_orders();
    assert_eq!(core.confirm_issued_ms.get("c1"), Some(&3500));
    core.engine.now_ms = 4600;
    core.sweep_stuck_orders();
    assert_eq!(core.confirm_issued_ms.get("c1"), Some(&4600), "and again one grace window later");
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);

    // the answer lands the held fill
    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 4700,
        }),
        &mut core.engine,
    );
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Filled);
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Submitted);

    // past the PLAIN reject deadline (2000) but within the extended window (3000): the guard defers
    // instead of rejecting — the confirm is treated as in flight.
    core.engine.now_ms = 2500;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Submitted,
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Filled);

    // even WELL past the extended deadline, the now-filled order is left untouched (and its guard
    // bookkeeping is pruned) — no phantom reject, no stranded position.
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Filled,
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Submitted);

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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Filled);

    // now WELL past the confirm-grace deadline — the sweep must leave the filled order alone
    core.engine.now_ms = 999_999;
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.registry["c1"].status,
        vike_model::OrderStatus::Filled,
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
}

/// A fresh (within-deadline) pre-ack order must be left alone.
#[test]
fn watchdog_leaves_fresh_order_untouched() {
    let mut core = test_core();
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.engine.submit_order(&stuck_req("c1"), 4500, &mut Outbox::default());
    core.engine.now_ms = 5000; // 5000 - 4500 = 500ms < 1000ms deadline
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
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
        vike_model::OrderStatus::Initialized,
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Submitted);

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
        vike_model::OrderStatus::Submitted,
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
        vike_model::OrderStatus::Submitted,
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
        vike_model::OrderStatus::Initialized,
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
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Accepted);

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
        vike_model::OrderStatus::Initialized,
        "no reject timeout configured ⇒ the fast rung must never invent a terminal"
    );
    // the reject ladder, being disabled, does nothing either
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
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

// ───────────────────── the trade link (`Ingest::ExecLink`) and the stuck-order ladder ─────────────
use crate::runtime::test_support::{core_of, engine_on};

fn link(route_key: &str, up: bool, at: i64) -> Ingest {
    Ingest::ExecLink(Box::new(vike_exec::ExecLinkUpdate {
        route_key: route_key.into(),
        venue: "sim".into(),
        up,
        at_ts_ms: at,
    }))
}

/// A core whose dispatch clock the test steps: `ExecLink` is filed on the CORE's clock (the one
/// every order's `created_ms` was stamped with), so the resume instant must be steerable.
fn stepped_core() -> (CoreThread<RecordingClient>, std::sync::Arc<std::sync::atomic::AtomicI64>) {
    let t = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = std::sync::Arc::clone(&t);
    let mut core = test_core();
    core.config.clock = Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed));
    core.config.submit_ack_timeout = Some(std::time::Duration::from_millis(1000));
    core.config.submit_ack_confirm_grace = std::time::Duration::from_millis(1000);
    (core, t)
}

/// The CONTROL: with the link never disclosed down, an order that has been un-acked for 5 s is
/// confirmed at the first sweep and rejected at the next (ack 1000, grace 1000 ⇒ extended
/// deadline 3000). Every deferral test below is only meaningful against this.
#[test]
fn without_a_link_loss_a_long_stuck_order_is_confirmed_then_rejected() {
    let (mut core, _clock) = stepped_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.engine.now_ms = 5_000;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()], "prodded first");
    core.engine.now_ms = 5_100;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Rejected);
}

/// **The deferral, over the real dispatch arm, and the resume.** While the account's link is down a
/// pre-ack order ages without limit and the ladder neither confirms nor rejects it; when the link
/// comes back the count restarts at the link-up instant with the SAME values (ack 1000, grace 1000):
/// nothing before `+1000`, a confirm inside the window, a deferral to `+2000`, the reject only past
/// the extended deadline `+3000`.
#[test]
fn a_down_trade_link_defers_the_ladder_and_the_count_resumes_from_link_up() {
    let (mut core, clock) = stepped_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());

    clock.store(500, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", false, 500));

    // far past every deadline of the ladder, link still down: untouched, unprodded
    core.engine.now_ms = 5_000;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
    assert!(core.engine.client.confirms.is_empty(), "no confirm over a link that is down");
    assert!(core.confirm_issued_ms.is_empty());

    clock.store(6_000, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", true, 6_000));

    core.engine.now_ms = 6_900; // 900 after link-up: inside the ack timeout
    core.sweep_stuck_orders();
    assert!(core.engine.client.confirms.is_empty(), "the count restarted at link-up");
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);

    core.engine.now_ms = 7_500; // 1500 after link-up: stage 1
    core.sweep_stuck_orders();
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()]);

    core.engine.now_ms = 8_500; // 2500 after link-up: past the plain deadline, guarded
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);

    core.engine.now_ms = 9_500; // 3500 after link-up: past the extended deadline
    core.sweep_stuck_orders();
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Rejected);
}

/// A confirm issued BEFORE the loss does not stand in for one after it: the loss forgets it, so the
/// restarted ladder prods the venue again before it can reject.
#[test]
fn a_confirm_issued_before_the_loss_is_forgotten_at_the_loss() {
    let (mut core, clock) = stepped_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    core.engine.now_ms = 1_500;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.client.confirms.len(), 1);
    assert!(core.confirm_issued_ms.contains_key("c1"));

    clock.store(1_600, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", false, 1_600));
    assert!(core.confirm_issued_ms.is_empty(), "the pre-loss confirm is forgotten");
    clock.store(1_700, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", true, 1_700));

    core.engine.now_ms = 3_200; // 1500 after link-up
    core.sweep_stuck_orders();
    assert_eq!(core.engine.client.confirms.len(), 2, "prodded again after the recovery");
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);
}

/// ⚠ **PINS A KNOWN LIMIT (review F2), not a wanted behaviour: the deferral has no cap.** Every `up`
/// restarts the count of every pre-ack order of the account, so a link that flaps with a period
/// shorter than `submit_ack_timeout` keeps a genuinely wedged order below the confirm threshold
/// forever: 20 s of flapping against a ladder that rejects at 3 s (ack 1000 + 2 x grace 1000)
/// neither prods nor rejects it. When a bounded deferral lands (reported to the coordinator, not
/// decided here) this test is the one that must change, and its control half is the proof the order
/// IS stuck.
#[test]
fn a_flapping_trade_link_defers_a_wedged_order_without_bound() {
    let (mut core, clock) = stepped_core();
    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    for k in 0..20_i64 {
        let t = k * 1_000;
        clock.store(t + 100, std::sync::atomic::Ordering::Relaxed);
        core.dispatch(link("sim", false, t + 100));
        clock.store(t + 200, std::sync::atomic::Ordering::Relaxed);
        core.dispatch(link("sim", true, t + 200));
        core.engine.now_ms = t + 900; // 700 after this cycle's link-up: inside the ack timeout
        core.sweep_stuck_orders();
    }
    assert!(core.engine.client.confirms.is_empty(), "20 s of flapping: never prodded");
    assert_eq!(core.engine.registry["c1"].status, vike_model::OrderStatus::Initialized);

    // CONTROL: the same order, the link now steady, is prodded at the first sweep past the timeout.
    core.engine.now_ms = 19_200 + 1_500;
    core.sweep_stuck_orders();
    assert_eq!(core.engine.client.confirms, vec!["c1".to_string()], "the order really was stuck");
}

/// Only transitions move anything: a repeated `up` does not move the resume instant, an `up` for a link never
/// filed down resumes nothing, and an account this core runs no engine for files nothing (a typo in
/// a producer must not grow the map).
#[test]
fn exec_link_files_only_real_transitions_for_carried_accounts() {
    let (mut core, clock) = stepped_core();
    core.dispatch(link("sim", true, 0)); // up with no down before it
    assert!(core.exec_link.is_empty(), "nothing to resume");
    core.dispatch(link("nobody", false, 0));
    assert!(core.exec_link.is_empty(), "no engine carries this route key");

    core.engine.submit_order(&stuck_req("c1"), 0, &mut Outbox::default());
    clock.store(100, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", false, 100));
    clock.store(900, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", false, 900)); // a repeat
    clock.store(2_000, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", true, 2_000));
    clock.store(2_500, std::sync::atomic::Ordering::Relaxed);
    core.dispatch(link("sim", true, 2_500)); // a repeat: the resume instant stays 2000
    core.engine.now_ms = 3_100; // 1100 after the FIRST link-up
    core.sweep_stuck_orders();
    assert_eq!(
        core.engine.client.confirms,
        vec!["c1".to_string()],
        "counted from 2000, not from the repeat at 2500 (that would still be inside the timeout)"
    );
}

/// The fast in-flight rung defers on the SAME rule and keeps the SAME age, per ENGINE: account
/// `other`'s link is down, `sim`'s is up. Both accounts hold an order stuck for 5 s; only `sim`'s is
/// re-queried. The down account's count restarts at its link-up, so the hand-off to the reject ladder
/// (which reads the same age) stays exact.
#[test]
fn the_inflight_rung_defers_per_account_and_resumes_with_the_same_age() {
    let other_req = vike_model::OrderRequest { venue: "other".into(), ..stuck_req("o1") };
    let mut core = core_of(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        vec![(1.0, engine_on("other", "BTCUSDT", RecordingClient::default()))],
        CoreConfig {
            inflight_confirm: Some(std::time::Duration::from_millis(2_000)),
            submit_ack_timeout: Some(std::time::Duration::from_millis(30_000)),
            ..CoreConfig::default()
        },
    );
    core.engine.submit_order(&stuck_req("s1"), 0, &mut Outbox::default());
    core.extra_engines[0].1.submit_order(&other_req, 0, &mut Outbox::default());

    core.note_exec_link(
        vike_exec::ExecLinkUpdate {
            route_key: "other".into(),
            venue: "other".into(),
            up: false,
            at_ts_ms: 1_000,
        },
        1_000,
    );
    core.sweep_inflight_confirms(5_000);
    assert_eq!(core.engine.client.confirms, vec!["s1".to_string()], "the up account is prodded");
    assert!(core.extra_engines[0].1.client.confirms.is_empty(), "the down account is deferred");
    assert!(!core.inflight_confirm_last_ms.contains_key("o1"));

    core.note_exec_link(
        vike_exec::ExecLinkUpdate {
            route_key: "other".into(),
            venue: "other".into(),
            up: true,
            at_ts_ms: 6_000,
        },
        6_000,
    );
    core.sweep_inflight_confirms(7_000); // 1000 after link-up: below the 2000 band floor
    assert!(core.extra_engines[0].1.client.confirms.is_empty(), "the count restarted at link-up");
    core.sweep_inflight_confirms(8_500); // 2500 after link-up: in band
    assert_eq!(core.extra_engines[0].1.client.confirms, vec!["o1".to_string()]);
}

// ───────────────── the stuck-order ladder over a PRICE-ONLY account's engine ─────────────────
//
// An account on a venue no mount drives (`CoreConfig::price_only`) is an EXTRA engine, and the ladder
// used to walk the primary's registry alone. Every test drives the ladder through the boundary timer
// (`drive_due_timers(now)`, the fake clock), ack 1000 / grace 1000 ⇒ confirm at 1000, plain reject
// 2000, extended reject 3000, exactly the numbers of the primary-engine tests above.

/// `(primary sim, extra bybit)`; the venues in `price_only` (none ⇒ the field stays `None`) are the
/// price-only set. The watchdog timers are armed.
fn ladder_core(price_only: &[&str]) -> CoreThread<RecordingClient> {
    let mut core = core_of(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        vec![(1.0, engine_on("bybit", "BTCUSDT", RecordingClient::default()))],
        CoreConfig {
            submit_ack_timeout: Some(std::time::Duration::from_millis(1000)),
            submit_ack_confirm_grace: std::time::Duration::from_millis(1000),
            price_only: (!price_only.is_empty()).then(|| PriceOnlyConfig {
                venues: price_only.iter().map(|v| v.to_string()).collect(),
                sweep_interval: std::time::Duration::from_secs(30),
                max_price_age: std::time::Duration::from_secs(60),
                price_source: std::collections::BTreeMap::new(),
            }),
            ..CoreConfig::default()
        },
    );
    core.arm_boundary_timers(0);
    core
}

/// A stuck order of the price-only account (`bybit`), created at 0.
fn submit_bybit(core: &mut CoreThread<RecordingClient>, coid: &str) {
    let req = vike_model::OrderRequest { venue: "bybit".into(), ..stuck_req(coid) };
    core.extra_engines[0].1.submit_order(&req, 0, &mut Outbox::default());
}

fn status_in_extra(core: &CoreThread<RecordingClient>, coid: &str) -> vike_model::OrderStatus {
    core.extra_engines[0].1.registry[coid].status
}

/// ⚠ THE POINT: a price-only account's order that never gets a venue answer climbs the SAME ladder as
/// the primary's, rung for rung and at the same instants, and is rejected LOCALLY in its own engine.
/// The primary's order beside it is the control: both move together.
#[test]
fn a_price_only_accounts_stuck_order_climbs_the_same_ladder_as_the_primarys() {
    let mut core = ladder_core(&["bybit"]);
    core.engine.submit_order(&stuck_req("s1"), 0, &mut Outbox::default());
    submit_bybit(&mut core, "p1");

    core.drive_due_timers(900); // inside the ack timeout: nobody is prodded
    assert!(core.engine.client.confirms.is_empty());
    assert!(core.extra_engines[0].1.client.confirms.is_empty());

    core.drive_due_timers(1500); // STAGE 1: the active confirm, once, to each order's OWN client
    assert_eq!(core.engine.client.confirms, vec!["s1".to_string()]);
    assert_eq!(core.extra_engines[0].1.client.confirms, vec!["p1".to_string()]);
    assert!(core.confirm_issued_ms.contains_key("s1") && core.confirm_issued_ms.contains_key("p1"));
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized);

    core.drive_due_timers(2500); // past the plain deadline: the in-flight-confirm guard DEFERS both
    assert_eq!(core.engine.registry["s1"].status, vike_model::OrderStatus::Initialized);
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized);
    assert_eq!(core.extra_engines[0].1.client.confirms.len(), 1, "asked once, not again");

    core.drive_due_timers(3500); // past the extended deadline, still pre-ack: the backstop reject
    assert_eq!(core.engine.registry["s1"].status, vike_model::OrderStatus::Rejected);
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Rejected);
    assert!(core.confirm_issued_ms.is_empty(), "the bookkeeping self-cleans for both");
}

/// A price-only order that the venue DOES answer leaves the ladder: nothing is rejected, and the
/// confirm bookkeeping is pruned from the extra engine's registry too.
#[test]
fn a_price_only_order_the_venue_acks_is_never_rejected_and_its_entry_is_pruned() {
    let mut core = ladder_core(&["bybit"]);
    submit_bybit(&mut core, "p1");
    core.drive_due_timers(1500);
    assert!(core.confirm_issued_ms.contains_key("p1"));

    core.bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "p1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1500,
        }),
        &mut core.extra_engines[0].1,
    );
    core.drive_due_timers(3500);
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Accepted);
    assert!(core.confirm_issued_ms.is_empty(), "pruned: the order is no longer pre-ack");
}

/// What the ladder did NOT cover stays uncovered: with no `price_only` set at all, and with a set that
/// names another venue (the extra engine then stands for a further account of a MOUNTED venue), an
/// extra engine's stuck order is neither prodded nor rejected, while the primary's is, as ever.
#[test]
fn an_extra_engine_outside_the_price_only_set_is_not_swept() {
    for set in [&[][..], &["binance"][..]] {
        let mut core = ladder_core(set);
        core.engine.submit_order(&stuck_req("s1"), 0, &mut Outbox::default());
        submit_bybit(&mut core, "p1");
        core.drive_due_timers(1500);
        core.drive_due_timers(2500);
        core.drive_due_timers(3500);
        assert_eq!(core.engine.registry["s1"].status, vike_model::OrderStatus::Rejected, "{set:?}");
        assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized, "{set:?}");
        assert!(core.extra_engines[0].1.client.confirms.is_empty(), "{set:?}");
        assert!(!core.confirm_issued_ms.contains_key("p1"), "{set:?}");
    }
}

/// The trade-link deferral applies per ACCOUNT to the price-only engine: while its link is down the
/// ladder neither prods nor rejects, a confirm issued before the loss is forgotten at the loss, and the
/// count restarts at the link-up instant with the same values.
#[test]
fn a_down_trade_link_defers_a_price_only_accounts_ladder_and_the_count_resumes() {
    let mut core = ladder_core(&["bybit"]);
    submit_bybit(&mut core, "p1");
    core.drive_due_timers(1500);
    assert_eq!(core.extra_engines[0].1.client.confirms.len(), 1);

    let update = |up: bool, at: i64| vike_exec::ExecLinkUpdate {
        route_key: "bybit".into(),
        venue: "bybit".into(),
        up,
        at_ts_ms: at,
    };
    core.note_exec_link(update(false, 1_600), 1_600);
    assert!(core.confirm_issued_ms.is_empty(), "the pre-loss confirm is forgotten");

    core.drive_due_timers(5_000); // far past every deadline, link still down
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized);
    assert_eq!(core.extra_engines[0].1.client.confirms.len(), 1, "no confirm over a dead link");

    core.note_exec_link(update(true, 6_000), 6_000);
    core.drive_due_timers(7_500); // 1500 after link-up: stage 1 again
    assert_eq!(
        core.extra_engines[0].1.client.confirms.len(),
        2,
        "prodded again after the recovery"
    );
    core.drive_due_timers(8_500); // 2500: guarded
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized);
    core.drive_due_timers(9_500); // 3500: past the extended deadline
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Rejected);
}

/// W13c on a price-only engine: an order that HOLDS a venue fill (the venue knows it) is asked about
/// again, once per grace window, and never rejected, exactly like the primary's.
#[test]
fn the_ladder_asks_again_instead_of_rejecting_a_price_only_order_that_holds_a_venue_fill() {
    let mut core = ladder_core(&["bybit"]);
    submit_bybit(&mut core, "p1");
    core.bus.publish(
        Event::OrderFilled(OrderFilled {
            client_order_id: "p1".into(),
            fill: FillEvent { venue: "bybit".into(), ..fill_for("p1") },
            ts: 0,
        }),
        &mut core.extra_engines[0].1,
    );
    assert_eq!(core.extra_engines[0].1.registry["p1"].deferred_events.len(), 1, "the fill is held");

    core.drive_due_timers(1500);
    assert_eq!(core.confirm_issued_ms.get("p1"), Some(&1500));
    core.drive_due_timers(3500); // past the extended deadline, where an order holding nothing is rejected
    assert_eq!(status_in_extra(&core, "p1"), vike_model::OrderStatus::Initialized);
    assert_eq!(core.confirm_issued_ms.get("p1"), Some(&3500), "asked again instead");
    assert_eq!(core.extra_engines[0].1.client.confirms.len(), 2);
}

/// A STOPPED price-only account (decision 0169) leaves the ladder: its client is detaching, so a
/// confirm to it or a synthesized `OrderRejected` into its dead engine would be wrongly terminal for
/// an order that is live at the venue. A RUNNING price-only account beside it climbs as ever.
#[test]
fn a_retired_price_only_engine_is_not_swept_while_a_live_one_is() {
    let mut core = core_of(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        vec![
            (1.0, engine_on("bybit", "BTCUSDT", RecordingClient::default())),
            (1.0, engine_on("okx", "BTCUSDT", RecordingClient::default())),
        ],
        CoreConfig {
            submit_ack_timeout: Some(std::time::Duration::from_millis(1000)),
            submit_ack_confirm_grace: std::time::Duration::from_millis(1000),
            price_only: Some(PriceOnlyConfig {
                venues: ["bybit", "okx"].iter().map(|v| v.to_string()).collect(),
                sweep_interval: std::time::Duration::from_secs(30),
                max_price_age: std::time::Duration::from_secs(60),
                price_source: std::collections::BTreeMap::new(),
            }),
            ..CoreConfig::default()
        },
    );
    core.arm_boundary_timers(0);
    for (i, venue, coid) in [(0, "bybit", "dead"), (1, "okx", "live")] {
        let req = vike_model::OrderRequest { venue: venue.into(), ..stuck_req(coid) };
        core.extra_engines[i].1.submit_order(&req, 0, &mut Outbox::default());
    }
    core.retired_engines.push(1); // the bybit account was stopped

    assert!(!core.stuck_ladder_covers(1) && core.stuck_ladder_covers(2));
    for now in [1500, 2500, 3500] {
        core.drive_due_timers(now);
    }
    assert!(
        core.extra_engines[0].1.client.confirms.is_empty(),
        "nothing sent to the detaching client"
    );
    assert_eq!(
        core.extra_engines[0].1.registry["dead"].status,
        vike_model::OrderStatus::Initialized
    );
    assert!(!core.confirm_issued_ms.contains_key("dead"));
    assert_eq!(core.extra_engines[1].1.client.confirms, vec!["live".to_string()]);
    assert_eq!(core.extra_engines[1].1.registry["live"].status, vike_model::OrderStatus::Rejected);
}
