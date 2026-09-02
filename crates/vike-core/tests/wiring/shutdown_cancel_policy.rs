//! SHUTDOWN POLICY wiring gate: `CoreConfig::cancel_orders_on_shutdown` really is consulted by the
//! core's own teardown, and OFF really is the behaviour that shipped before the field existed.
//!
//! # Why this file exists separately from the unit tests
//!
//! `crates/vike-core/src/runtime/apply.rs` proves what the sweep DOES (it cancels every resting
//! order, it is a no-op on an empty book, it never closes a position) by calling
//! `cancel_resting_on_shutdown` directly. That says nothing about whether anything CALLS it. This
//! file drives a real [`spawn_core`] and stops it through the real `shutdown_and_join` path, so the
//! `if self.config.cancel_orders_on_shutdown` arm in the teardown block is what is under test.
//!
//! Deleting that arm leaves every unit test in `apply.rs` green and turns
//! [`shutdown_cancels_resting_orders_when_the_policy_is_on`] red. That is the whole point.
//!
//! # ⚠ What a green run here does NOT prove
//!
//! That `systemctl stop` cancels anything — but no longer for the reason this paragraph used to
//! give. It said SIGTERM ended the process without running any Rust code, because no signal handler
//! existed anywhere in the workspace. `vike_ops::stop` changed that: a service stop now reaches
//! teardown, and this policy with it.
//!
//! What a green run here still does not prove is the DAEMON's wiring. This test drives
//! `spawn_core`/`shutdown_and_join` directly; that the binary reaches the same teardown from a real
//! signal is a different claim, gated separately by `crates/vike-tradehub/tests/sigterm_stop.rs`
//! (which raises an actual SIGTERM at the shipped binary) and by
//! `crates/vike-ops/tests/graceful_stop_pin.rs`. Nor does it prove the cancel COMPLETES: the
//! teardown is hard-capped, and whatever the deadline abandons is not cancelled.
//! `docs/ops/graceful-stop.md` and `docs/ops/kill-switches.md` §D carry the operator-facing version.

use vike_core::{spawn_core, CoreConfig};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, RiskLimits, TradingState,
};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::OrderRequest;

/// `RecordingClient` emits NOTHING of its own, which is what makes it the right client here: an
/// order it accepted stays non-terminal until a test says otherwise, i.e. it RESTS — the exact
/// state a book is in when a daemon is stopped. (`TestExecutionClient` fills every submit
/// immediately, so nothing would ever be resting to cancel.)
fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn limit_order(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    })
}

/// Submit one order and walk it to `Accepted` exactly as a real adapter does — the emitter split's
/// synchronous `[OrderSubmitted, OrderAccepted]` pair. Returns the handle's snapshot cell, captured
/// BEFORE `shutdown_and_join` consumes the handle, so the FINAL published snapshot is readable
/// after the core thread is gone.
fn run_to_shutdown(cancel_on_shutdown: bool, with_resting_order: bool) -> Vec<String> {
    let cfg = CoreConfig { cancel_orders_on_shutdown: cancel_on_shutdown, ..CoreConfig::default() };
    let handle = spawn_core(engine(), cfg);

    if with_resting_order {
        handle.send_command(Command::Order(OrderIntent::Submit(limit_order("resting-1"))));
        let sender = handle.event_sender();
        sender
            .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: "resting-1".into(),
                ts: 0,
            }))
            .unwrap();
        sender
            .blocking_send(Event::OrderAccepted(OrderAccepted {
                client_order_id: "resting-1".into(),
                venue_order_id: Some("v-1".into()),
                ts: 0,
            }))
            .unwrap();
    }

    let cell = handle.snapshot_cell();
    // Lossless shutdown: every queued message folds, THEN the teardown block runs, THEN the final
    // snapshot publishes — so the sweep's `recent` note is in the snapshot read below.
    handle.shutdown_and_join();
    cell.load().recent_events.iter().map(|s| s.to_string()).collect()
}

/// THE DEFAULT, and the decision the user made explicitly: a stop LEAVES the book resting. This is
/// byte-identical to every version of this teardown before the flag existed, and the reason the
/// safer-sounding value is not the default — flipping it would silently change what every existing
/// deployment's stop does to live venue state.
#[test]
fn shutdown_leaves_resting_orders_alone_by_default() {
    let recent = run_to_shutdown(false, true);
    assert!(
        !recent.iter().any(|m| m.contains("shutdown: cancelled")),
        "the default teardown must not cancel anything: {recent:?}"
    );
}

/// The flag, wired: with it on, the real teardown runs the sweep.
#[test]
fn shutdown_cancels_resting_orders_when_the_policy_is_on() {
    let recent = run_to_shutdown(true, true);
    assert!(
        recent.iter().any(|m| m.contains("shutdown: cancelled 1 resting order")),
        "the teardown must run the sweep when the policy is on: {recent:?}"
    );
    assert!(
        recent.iter().any(|m| m.contains("positions untouched")),
        "and must say it cancelled orders rather than closed positions: {recent:?}"
    );
}

/// MUTATION SENTINEL for the pair above: the difference between them must come from the FLAG and
/// not from the order setup. With the policy on and an empty book, no cancel note appears — so a
/// sweep that fired unconditionally, or one wired to something other than the flag, is caught.
#[test]
fn the_policy_being_on_does_not_invent_a_cancel_on_an_empty_book() {
    let recent = run_to_shutdown(true, false);
    assert!(
        !recent.iter().any(|m| m.contains("shutdown: cancelled")),
        "nothing rested, so nothing is cancelled and nothing claims to be: {recent:?}"
    );
}

/// The sweep runs from a HALTED core — the state `enter_safe_state` and the dead-man's switch both
/// leave behind, and therefore the state a daemon is most likely to be stopped in. `MassCancel` is
/// ungated by `TradingState`, so this works; a sweep routed through anything the kill switch gates
/// would silently do nothing in exactly the case that matters most.
#[test]
fn the_shutdown_sweep_still_runs_from_a_halted_core() {
    let cfg = CoreConfig { cancel_orders_on_shutdown: true, ..CoreConfig::default() };
    let handle = spawn_core(engine(), cfg);
    handle.send_command(Command::Order(OrderIntent::Submit(limit_order("resting-1"))));
    let sender = handle.event_sender();
    sender
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: "resting-1".into(),
            ts: 0,
        }))
        .unwrap();
    sender
        .blocking_send(Event::OrderAccepted(OrderAccepted {
            client_order_id: "resting-1".into(),
            venue_order_id: Some("v-1".into()),
            ts: 0,
        }))
        .unwrap();
    handle.send_command(Command::SetTradingState(TradingState::Halted));

    let cell = handle.snapshot_cell();
    handle.shutdown_and_join();
    let snap = cell.load();
    assert_eq!(snap.trading_state, TradingState::Halted, "precondition: halted");
    let recent: Vec<String> = snap.recent_events.iter().map(|s| s.to_string()).collect();
    assert!(
        recent.iter().any(|m| m.contains("shutdown: cancelled 1 resting order")),
        "a halted core must still cancel its book on the way out: {recent:?}"
    );
}

/// ⚠ THE ANTI-VACUITY GATE for this whole file. Every test above distinguishes its two cases by the
/// PRESENCE of a `recent` note, and an "expected absent" assertion passes just as happily when the
/// setup silently built nothing at all — which is precisely how a batch of adversarial tests in this
/// repo turned out to be inert (a forged coid that existed in neither the registry nor the
/// contingency book). So prove the fixture: the order the sweep is supposed to find really is
/// registered and really is non-terminal by the time a stop would see it.
#[test]
fn the_harness_really_leaves_a_resting_order_for_the_sweep_to_find() {
    let handle = spawn_core(engine(), CoreConfig::default());
    handle.send_command(Command::Order(OrderIntent::Submit(limit_order("resting-1"))));
    let sender = handle.event_sender();
    sender
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: "resting-1".into(),
            ts: 0,
        }))
        .unwrap();
    sender
        .blocking_send(Event::OrderAccepted(OrderAccepted {
            client_order_id: "resting-1".into(),
            venue_order_id: Some("v-1".into()),
            ts: 0,
        }))
        .unwrap();

    let cell = handle.snapshot_cell();
    handle.shutdown_and_join();
    let snap = cell.load();
    let order =
        snap.orders.iter().find(|o| o.client_order_id == "resting-1").expect(
            "the order must reach the core's registry, or every test in this file is vacuous",
        );
    assert_eq!(
        order.status,
        vike_exec::OrderStatus::Accepted,
        "and it must be RESTING (a terminal order is not something a stop can cancel)"
    );
}
