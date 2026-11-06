//! Restore-into-registry LIVE demo smoke (the proof `vike_venues::venue_restore`'s okx row
//! waits for): an order a PREVIOUS session left resting at the OKX demo (simulated-trading) venue is
//! found by the real `OkxReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put into
//! a FRESH `ExecutionEngine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_restore_smoke::
//!
//! ORDER-PLACING (SWAP, `BTC-USDT-SWAP`): rests one far-from-market LIMIT BUY (half the last price,
//! ONE contract's worth of base) on the OKX DEMO account, double-gated like every other `*_smoke.rs`
//! in this crate: network + the DEMO account's creds + passphrase via
//! `vike_secrets::smoke_directory`, a `tracing::warn!` and an early return when absent. Every
//! transport is `UreqOkxTransport::new(true)`
//! (`x-simulated-trading: 1`; demo and mainnet share the host, the HEADER is the switch, see
//! `crates/bridges/okx/CLAUDE.md`). Nothing reduces or touches a position.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under a coid the smoke mints (`clOrdId`,
//!      alphanumeric, within the 32-character budget);
//!   2. the coid, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid, lands in
//!      `adopt` with a live status, while the made-up coid lands in `gone`;
//!   3. a fresh engine over a SECOND REST client (the "after restart" client) takes `plan.adopt`
//!      through `reregister_orders`, holds the coid live with its venue order id, and
//!      `ExecutionEngine::cancel_order` removes it at the venue (the next `ReconClient` fetch plans
//!      it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the private `orders` stream would deliver (the smoke opens no socket, so
//!      it builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the coid from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: the venue's already-gone codes are swallowed). A run
//! killed from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a
//! half-price BUY that cannot fill, so cancel it by hand on the demo account.

use crate::demo_creds::load_demo_creds;
use crate::demo_swap::build_rest;
use serde_json::json;
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::smoke::{
    CancelOnDrop, adopts, is_gone, plan_pass, restored_limit, wait_until_gone,
};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Outbox, RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatus, RiskLimits, now_ms};
use vike_okx::OkxReconClient;
use vike_okx::perp::REST;
use vike_okx::transport::UreqOkxTransport;

const SYMBOL: &str = "BTC-USDT-SWAP";

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn okx_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let previous_session = build_rest(&creds, SYMBOL);
    let mark = previous_session.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");
    let ct_val = previous_session.ct_val;
    // one contract's worth of BASE, half the market: the BUY rests and cannot cross
    let leg_base = previous_session.properties.min_qty * ct_val;
    let limit_px = mark * 0.5;
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": leg_base, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_okx", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { rest: &previous_session, coid: &coid, host: "demo" };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client, the planner ------------------------------------
    let recon = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        REST,
        SYMBOL,
        ct_val,
    );
    let restored = [restored_limit(&coid, "okx", SYMBOL), restored_limit(&ghost, "okx", SYMBOL)];
    let (plan, _) = plan_pass(&recon, "okx", SYMBOL, &restored);
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED: {plan:?}");
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert!(report.status.is_live(), "an adopted report is live: {report:?}");
    assert!(!is_gone(&plan, &coid), "a resting coid is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "a coid the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -------------------------
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(build_rest(&creds, SYMBOL)),
        "okx",
        SYMBOL,
    );
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order = engine.registry.get(&coid).expect("the coid is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");

    engine.cancel_order(&coid);
    assert!(
        engine.client.last_cancel_error.is_none(),
        "the engine cancel was refused: {:?}",
        engine.client.last_cancel_error
    );
    while let Some(ev) = engine.client.poll_events() {
        assert!(!matches!(ev, Event::OrderCancelRejected(_)), "cancel rejected: {ev:?}");
    }
    wait_until_gone(&recon, "okx", SYMBOL, &restored, &coid);

    // --- 4. the private stream's cancel, folded ------------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.clone(),
        reason: "restore smoke: private stream cancel".into(),
        ts: now_ms(),
    });
    engine.on_event(&canceled, &mut Outbox::default());
    assert_eq!(
        engine.registry[&coid].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_okx", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
