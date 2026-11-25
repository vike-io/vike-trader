//! Restore-into-registry LIVE testnet smoke (the proof `vike_venues::venue_restore`'s deribit
//! row waits for): an order a PREVIOUS session left resting at Deribit TESTNET is found by the real
//! `ReconcileClientDeribit`, planned as ADOPT by `vike_execution::recon::plan_restore`, put into a FRESH
//! `Engine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-deribit --test deribit_smokes -- --ignored --nocapture deribit_restore_smoke::
//!
//! ORDER-PLACING (testnet, the most liquid BTC option with a live ask, discovered exactly like
//! `deribit_tif_smoke.rs`): rests one far-below LIMIT BUY of the minimum amount, priced at a quarter
//! of the ask and under the tiered-tick boundary, so it cannot fill. Double-gated like every other
//! `*_smoke.rs` in this crate: network + the default DEMO account's creds
//! via `vike_secrets::smoke_directory`, a `tracing::warn!` and an early return when absent.
//!
//! What it proves, in order:
//!   1. a "previous session" `VenueRestDeribit` (its own authed order-WS) places the order under an order client id
//!      the smoke mints (the `label`, within the 64-character budget);
//!   2. the order client id, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up order client id, lands in
//!      `adopt` with a live status, while the made-up order client id lands in `gone`;
//!   3. a fresh engine over a SECOND `VenueRestDeribit` on a second authed socket (the "after restart"
//!      client, which has never seen the order) takes `plan.adopt` through `reregister_orders`,
//!      holds the order client id live with its venue order id, and `Engine::cancel_order` removes it
//!      at the venue (the next `ReconcileClient` fetch plans it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the user-data stream would deliver (the smoke opens no socket, so it
//!      builds that event itself) folds the restored order to `Canceled`.
//!
//! STEP 3 IS THE ONE THAT ONCE FAILED, AND ITS FAILURE WAS A FINDING, NOT A FLAKE. `VenueRestDeribit`'s
//! `cancel_order` cancels by venue `order_id` from an order client id -> order_id map that lives in ONE
//! `VenueRestDeribit` and is filled only by that client's own submit; the "after restart" client has an
//! empty map. It used to return `Ok(())` and send nothing; it now finds the order by `label` (the
//! first bullet under "Local traps" in the crate's own CLAUDE.md) and cancels it by the id the venue
//! reports. If this smoke stops at "still not gone", that lookup path is broken and the deribit
//! restore row must go back to `awaiting_smoke`.
//!
//! CLEANUP: the PLACING client (the one whose map knows the order id) cancels the order client id from a `Drop`
//! guard, so a failed assertion, a panic or a hung poll still cancels it. A run killed from outside
//! (SIGKILL, power loss) skips destructors and leaves the order resting on testnet; cancel it by hand
//! there.

use crate::demo_credentials::load_demo_credentials;
use vike_bridge_core::rest::{ExecutionClientRest, VenueRest};
use vike_bridge_core::smoke::{CancelOnDrop, adopts, is_gone, plan_pass, restored_limit};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::ReconcileClientDeribit;
use vike_deribit::execution::rest::{VenueRestDeribit, parse_deribit_option_instruments};
use vike_deribit::test_support::{TESTNET_REST, TESTNET_WEBSOCKET, testnet_urls};
use vike_deribit::transport::client::DeribitTransport;
use vike_execution::recon::{ReconcileClient, RestoredOrderRef};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderRequest, OrderStatus, RiskLimits, SymbolProperties, now_ms};
use vike_secrets::credentials::Credentials;

/// The most liquid BTC option with a live ask, as `deribit_tif_smoke.rs` discovers it:
/// `(symbol, ask, properties)`.
fn pick_instrument() -> (String, f64, SymbolProperties) {
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None;
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");
    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let properties = parse_deribit_option_instruments(&info)[&symbol].properties;
    (symbol, ask, properties)
}

/// One `VenueRestDeribit` on its OWN authed testnet order-WS. A fresh call is a fresh client with an
/// EMPTY order client id -> order_id map: it can cancel only what it submits itself.
fn build_rest(creds: &Credentials, symbol: &str, properties: SymbolProperties) -> VenueRestDeribit {
    let mut transport = DeribitTransport::new(
        TESTNET_WEBSOCKET,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    transport.connect().expect("order-WS auth");
    VenueRestDeribit::new(transport, symbol, properties, "BTC")
}

/// Poll until the venue's report no longer keeps `order_client_id` alive (planned `gone`, never `adopt`).
fn wait_until_gone(
    recon: &dyn ReconcileClient,
    symbol: &str,
    restored: &[RestoredOrderRef],
    order_client_id: &str,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (plan, _) = plan_pass(recon, "deribit", symbol, restored);
        if is_gone(&plan, order_client_id) && !adopts(&plan, order_client_id) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{order_client_id} still not gone at the venue 15 s after the engine cancel: {plan:?}. The engine \
             cancel on the restart client (empty order client id -> order_id map) must find the order by \
             label and cancel it; the deribit restore row must not be enabled while this fails."
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + testnet creds — places + cancels a real (non-filling) testnet order — run manually (see module doc)"]
fn deribit_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    let (symbol, ask, properties) = pick_instrument();

    // --- 1. the "previous session": place a far-below resting LIMIT BUY ------------------------
    let previous_session = build_rest(&creds, &symbol, properties);
    let low_price = (ask * 0.25).min(0.004).max(properties.tick_size);
    let order_client_id = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "deribit".to_string(),
        symbol: symbol.clone(),
        side: 1,
        qty: properties.min_qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(low_price),
        timestamp: now_ms(),
        ..Default::default()
    };
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_deribit", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup = CancelOnDrop {
        rest: &previous_session,
        order_client_id: &order_client_id,
        host: "testnet",
    };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "testnet order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client (its own socket), the planner -------------------
    let Some(recon) = ReconcileClientDeribit::connect(&testnet_urls(), &creds, &symbol) else {
        panic!("ReconcileClientDeribit::connect failed — testnet order-WS auth did not succeed");
    };
    let restored = [
        restored_limit(&order_client_id, "deribit", &symbol),
        restored_limit(&ghost, "deribit", &symbol),
    ];
    let (plan, _) = plan_pass(&recon, "deribit", &symbol, &restored);
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
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(build_rest(&creds, &symbol, properties)),
        "deribit",
        &symbol,
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
    wait_until_gone(&recon, &symbol, &restored, &order_client_id);

    // --- 4. the user-data stream's cancel, folded ----------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.clone(),
        reason: "restore smoke: user-data stream cancel".into(),
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
    previous_session.detach();
    tracing::info!(target: "vike_deribit", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
