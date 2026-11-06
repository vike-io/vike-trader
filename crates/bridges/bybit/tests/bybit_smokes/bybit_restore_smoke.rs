//! Restore-into-registry LIVE demo smoke (the proof `vike_venues::venue_restore`'s bybit
//! row waits for): an order a PREVIOUS session left resting at the Bybit demo venue is found by the
//! real `BybitReconClient`, planned as ADOPT by `vike_exec::recon::plan_restore`, put into a FRESH
//! `ExecutionEngine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture bybit_restore_smoke::
//!
//! ORDER-PLACING (linear perp, `BTCUSDT`): rests one far-from-market LIMIT BUY (90% of the last
//! price, sized just over `min_notional`, so it cannot fill) on the Bybit DEMO account, double-gated
//! like every other `*_smoke.rs` in this crate: network + the DEMO account's creds (the unlabelled
//! one, else the one labelled `demo`-tier row: `demo_label`) via `vike_secrets::smoke_directory`, a
//! `tracing::warn!` and an early return when absent. Nothing reduces or touches a position.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under a coid the smoke mints;
//!   2. the coid, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up coid, lands in
//!      `adopt` with a live status, while the made-up coid lands in `gone`;
//!   3. a fresh engine over a SECOND REST client (the "after restart" client) takes `plan.adopt`
//!      through `reregister_orders`, holds the coid live with its venue order id, and
//!      `ExecutionEngine::cancel_order` removes it at the venue;
//!   4. BYBIT'S RULE: `/v5/order/realtime` reports recently CLOSED orders too
//!      (`recon_client.rs`'s module doc), so after the cancel the venue may still LIST the order,
//!      terminal. The smoke waits until the planner says `gone`, never `adopt`, and asserts that a
//!      row the venue still lists is TERMINAL: a closed order must never be adopted;
//!   5. the `OrderCanceled` the private stream would deliver (the smoke opens no socket, so it
//!      builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the coid from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: the venue's already-gone codes are swallowed). A run
//! killed from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a
//! 10%-below-market BUY, so cancel it by hand on the demo account.

use crate::demo_label::demo_label;
use serde_json::json;
use vike_bridge_core::rest::{LiveRestClient, VenueRest};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::smoke::{
    CancelOnDrop, adopts, is_gone, plan_pass, restored_limit, wait_until_gone,
};
use vike_bybit::BybitReconClient;
use vike_bybit::perp::{BybitPerpRest, DEMO_REST, PATH_INSTRUMENTS, parse_bybit_perp_instruments};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Outbox, RiskGate,
};
use vike_model::AccountTier;
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatus, RiskLimits, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTCUSDT";

type PerpRest = BybitPerpRest<UreqBybitTransport>;

/// The double-gate every `*_smoke.rs` in this crate uses (see `bybit_reconcile_smoke.rs`).
fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), AccountTier::Demo);
    let creds = api_credentials(&account, "bybit");
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

/// One signed linear-perp REST client on the demo host, built the way `bybit_tif_smoke.rs` builds
/// its own. A fresh call is a fresh client: no state about any order.
fn build_rest(creds: &Credentials) -> PerpRest {
    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(SYMBOL))],
            &BybitV5Signer::new(creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)[SYMBOL].clone();
    BybitPerpRest {
        category: vike_bybit::data::Category::Linear,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        leverage: 2.0,
    }
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn bybit_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let previous_session = build_rest(&creds);
    let mark = previous_session.last_price().expect("tickers");
    assert!(mark > 0.0, "live mark required");
    let props = previous_session.properties;
    // ~10% below market stays inside Bybit's price band and, on the tick grid, never crosses.
    let limit_px = (mark * 0.9 / props.tick_size).round() * props.tick_size;
    let qty = f64::max(props.min_qty, (props.min_notional * 1.4) / limit_px);
    let qty = (qty / props.step_size).ceil() * props.step_size;
    let coid = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_bybit", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop { rest: &previous_session, coid: &coid, host: "demo" };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client, the planner ------------------------------------
    let recon = BybitReconClient::new(
        BybitV5Signer::new(&creds, now_ms),
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        DEMO_REST,
        SYMBOL,
    );
    let restored =
        [restored_limit(&coid, "bybit", SYMBOL), restored_limit(&ghost, "bybit", SYMBOL)];
    let (plan, _) = plan_pass(&recon, "bybit", SYMBOL, &restored);
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED: {plan:?}");
    assert_eq!(plan.adopt.len(), 1, "only the real coid is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert!(report.status.is_live(), "an adopted report is live: {report:?}");
    assert!(!is_gone(&plan, &coid), "a resting coid is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "a coid the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -------------------------
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(build_rest(&creds)),
        "bybit",
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

    // --- 4. bybit reports closed orders: a CLOSED order must never be adopted ------------------
    let orders = wait_until_gone(&recon, "bybit", SYMBOL, &restored, &coid);
    match orders.iter().find(|o| o.client_order_id.as_deref() == Some(coid.as_str())) {
        Some(row) => {
            assert!(
                row.status.is_terminal(),
                "a row the venue still lists after the cancel must be terminal: {row:?}"
            );
            tracing::info!(target: "vike_bybit", "closed order still listed by the venue as {} -> planned gone", row.status.as_str());
        }
        None => {
            tracing::info!(target: "vike_bybit", "closed order no longer listed -> planned gone by absence")
        }
    }

    // --- 5. the private stream's cancel, folded ------------------------------------------------
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
    tracing::info!(target: "vike_bybit", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> closed row not adopted -> folded Canceled");
}
