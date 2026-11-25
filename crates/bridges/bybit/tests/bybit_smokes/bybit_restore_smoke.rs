//! Restore-into-registry LIVE demo smoke (the proof `vike_venues::venue_restore`'s bybit
//! row waits for): an order a PREVIOUS session left resting at the Bybit demo venue is found by the
//! real `ReconcileClientBybit`, planned as ADOPT by `vike_execution::recon::plan_restore`, put into a FRESH
//! `Engine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture bybit_restore_smoke::
//!
//! ORDER-PLACING (linear perp, `BTCUSDT.P`, the venue's `BTCUSDT`): rests one far-from-market LIMIT BUY (90%
//! of the last
//! price, sized just over `min_notional`, so it cannot fill) on the Bybit DEMO account, double-gated
//! like every other `*_smoke.rs` in this crate: network + the DEMO account's creds (the unlabelled
//! one, else the one labelled `demo`-tier row: `demo_label`) via `vike_secrets::smoke_directory`, a
//! `tracing::warn!` and an early return when absent. Nothing reduces or touches a position.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under an order client id the smoke mints;
//!   2. the order client id, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up order client id, lands in
//!      `adopt` with a live status, while the made-up order client id lands in `gone`;
//!   3. a fresh engine over a SECOND REST client (the "after restart" client) takes `plan.adopt`
//!      through `reregister_orders`, holds the order client id live with its venue order id, and
//!      `Engine::cancel_order` removes it at the venue;
//!   4. BYBIT'S RULE: `/v5/order/realtime` reports recently CLOSED orders too
//!      (`reconcile/mod.rs`'s module doc), so after the cancel the venue may still LIST the order,
//!      terminal. The smoke waits until the planner says `gone`, never `adopt`, and asserts that a
//!      row the venue still lists is TERMINAL: a closed order must never be adopted;
//!   5. the `OrderCanceled` the private stream would deliver (the smoke opens no socket, so it
//!      builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the order client id from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: the venue's already-gone codes are swallowed). A run
//! killed from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a
//! 10%-below-market BUY, so cancel it by hand on the demo account.

use crate::demo_label::demo_label;
use serde_json::json;
use vike_bridge_core::rest::{ExecutionClientRest, VenueRest};
use vike_bridge_core::smoke::{
    CancelOnDrop, adopts, is_gone, plan_pass, restored_limit, wait_until_gone,
};
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bybit::ReconcileClientBybit;
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, VenueRestBybit, parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, RiskGate,
};
use vike_model::AccountTier;
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatus, RiskLimits, now_ms};
use vike_secrets::credentials::{Credentials, api_credentials};

/// The linear perpetual's USER name (decision 0290): what the order, the reconcile client, the planner and the fresh
/// engine name, read as its kept row's [`WIRE`] in `linear` (`vike_bybit::history::kept_list_for_tests`).
const SYMBOL: &str = "BTCUSDT.P";
/// The venue's own name of [`SYMBOL`]: what the grid read and the REST clients sign.
const WIRE: &str = "BTCUSDT";

type PerpRest = VenueRestBybit<RestTransportJsonUreq>;

/// The double-gate every `*_smoke.rs` in this crate uses (see `bybit_reconcile_smoke.rs`).
fn load_demo_credentials() -> Option<Credentials> {
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
    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            &demo,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!(WIRE))],
            &BybitV5Signer::new(creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)[WIRE].clone();
    VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo,
        mainnet: false,
        symbol: WIRE.to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    }
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn bybit_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let Some(creds) = load_demo_credentials() else { return };

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let previous_session = build_rest(&creds);
    let mark = previous_session.last_price().expect("tickers");
    assert!(mark > 0.0, "live mark required");
    let properties = previous_session.properties;
    // ~10% below market stays inside Bybit's price band and, on the tick grid, never crosses.
    let limit_px = (mark * 0.9 / properties.tick_size).round() * properties.tick_size;
    let qty = f64::max(properties.min_qty, (properties.min_notional * 1.4) / limit_px);
    let qty = (qty / properties.step_size).ceil() * properties.step_size;
    let order_client_id = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_bybit", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup =
        CancelOnDrop { rest: &previous_session, order_client_id: &order_client_id, host: "demo" };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client, the planner ------------------------------------
    let recon = ReconcileClientBybit::new(
        BybitV5Signer::new(&creds, now_ms),
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        previous_session.base_url.as_str(), // the demo host `build_rest` signs against
        SYMBOL,
    );
    let restored = [
        restored_limit(&order_client_id, "bybit", SYMBOL),
        restored_limit(&ghost, "bybit", SYMBOL),
    ];
    let (plan, _) = plan_pass(&recon, "bybit", SYMBOL, &restored);
    assert!(
        adopts(&plan, &order_client_id),
        "the resting order client id must be ADOPTED: {plan:?}"
    );
    assert_eq!(plan.adopt.len(), 1, "only the real order client id is adopted: {plan:?}");
    let report = &plan.adopt[0];
    assert!(report.status.is_live(), "an adopted report is live: {report:?}");
    assert!(!is_gone(&plan, &order_client_id), "a resting order client id is never gone: {plan:?}");
    assert!(is_gone(&plan, &ghost), "an order client id the venue never had is GONE: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a client that has never seen the order -------------------------
    let mut engine = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(build_rest(&creds)),
        "bybit",
        SYMBOL,
    );
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let restored_order =
        engine.registry.get(&order_client_id).expect("the order client id is in the registry");
    assert!(restored_order.status.is_live(), "restored live: {:?}", restored_order.status);
    assert!(restored_order.venue_order_id.is_some(), "the venue order id came across");

    engine.cancel_order(&order_client_id);
    assert!(
        engine.client.last_cancel_error.is_none(),
        "the engine cancel was refused: {:?}",
        engine.client.last_cancel_error
    );
    while let Some(event) = engine.client.poll_events() {
        assert!(!matches!(event, Event::OrderCancelRejected(_)), "cancel rejected: {event:?}");
    }

    // --- 4. bybit reports closed orders: a CLOSED order must never be adopted ------------------
    let orders = wait_until_gone(&recon, "bybit", SYMBOL, &restored, &order_client_id);
    match orders.iter().find(|o| o.order_client_id.as_deref() == Some(order_client_id.as_str())) {
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
        order_client_id: order_client_id.clone(),
        reason: "restore smoke: private stream cancel".into(),
        timestamp: now_ms(),
        post_only_would_cross: false,
        refusal_id: None,
    });
    engine.on_event(&canceled, &mut EventBusOutbox::default());
    assert_eq!(
        engine.registry[&order_client_id].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_bybit", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> closed row not adopted -> folded Canceled");
}
