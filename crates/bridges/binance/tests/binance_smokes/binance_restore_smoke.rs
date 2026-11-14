//! Restore-into-registry LIVE demo smoke (the proof `vike_venues::venue_restore`'s binance
//! row waits for): an order a PREVIOUS session left resting at the Binance demo venue is found by
//! the real `ReconcileClientBinance`, planned as ADOPT by `vike_execution::recon::plan_restore`, put into a
//! FRESH `Engine` with `reregister_orders`, and then cancelled THROUGH that engine.
//!
//!     cargo test -p vike-binance --test binance_smokes -- --ignored --nocapture binance_restore_smoke::
//!
//! ORDER-PLACING (spot, `BTCUSDT`): rests one far-from-market LIMIT BUY (10% below the mark, sized just
//! over `min_notional`, so it can never fill) on the Binance DEMO account, double-gated like every
//! other `*_smoke.rs` in this crate: network + the default DEMO account's key
//! pair via `vike_secrets::smoke_directory`, a `tracing::warn!` and an early return when absent.
//!
//! What it proves, in order:
//!   1. a "previous session" REST client places the order under an order client id the smoke mints;
//!   2. the order's order client id, handed to `plan_restore` as a `RestoredOrderRef` beside a made-up order client id,
//!      lands in `adopt` with a live status, while the made-up order client id lands in `gone`;
//!   3. a fresh engine over a SECOND REST client (the "after restart" client, which has never seen
//!      the order) takes `plan.adopt` through `reregister_orders`, holds the order client id live with its
//!      venue order id, and `Engine::cancel_order` removes it at the venue (the next
//!      `ReconcileClient` fetch plans it `gone`, never `adopt`);
//!   4. the `OrderCanceled` the venue's user-data stream would deliver (the smoke opens no socket,
//!      so it builds that event itself) folds the restored order to `Canceled`.
//!
//! CLEANUP: the placing client cancels the order client id from a `Drop` guard, so a failed assertion, a panic
//! or a hung poll still cancels it (idempotent: `-2011` "unknown order" is swallowed). A run killed
//! from outside (SIGKILL, power loss) skips destructors and leaves the order resting; it is a
//! BUY 10% under the mark that cannot fill, so cancel it by hand on the demo account.
//!
//! NOT covered here: the perp lane (`BTCUSDT.P`, `ReconcileClientBinance::perp`) and a broker-stamped
//! order client id (`link_id: Some(..)`, whose `x-<code>-` prefix the report parsers strip); both are covered
//! by `binance_perp_restore_smoke.rs`'s
//! `binance_perp_restore_adopts_a_resting_order_and_the_engine_cancels_it`.

use crate::demo_credentials::{demo_hosts, load_demo_credentials};
use vike_binance::ReconcileClientBinance;
use vike_binance::execution::rest::spot::{
    PATH_EXCHANGE_INFO, VenueRestBinanceSpot, parse_symbol_properties,
};
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::rest::{ExecutionClientRest, VenueRest};
use vike_bridge_core::smoke::{
    CancelOnDrop, adopts, is_gone, plan_pass, restored_limit, wait_until_gone,
};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, RiskGate,
};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{OrderStatus, RiskLimits, now_ms};
use vike_secrets::credentials::Credentials;

const SYMBOL: &str = "BTCUSDT";

/// The demo spot host, the seed's demo row.
fn demo_rest() -> &'static str {
    demo_hosts().spot_rest
}

type SpotRest = VenueRestBinanceSpot<BinanceHmacSigner, UreqTransport>;

/// One signed spot REST client on the demo host, clock-synced, the way `binance_demo_smoke.rs`
/// builds its submit client. A fresh call is a fresh client: no state about any order.
fn build_rest(creds: &Credentials) -> SpotRest {
    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let info = transport
        .public(demo_rest(), PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)[SYMBOL];
    let rest = VenueRestBinanceSpot {
        link_id: None,
        signer: BinanceHmacSigner::new(creds, now_ms),
        transport,
        base_url: demo_rest().to_string(),
        symbol: SYMBOL.to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = rest.server_time_offset(now_ms()).expect("server time");
    rest.signer.set_offset_ms(offset);
    rest
}

#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn binance_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };

    // --- 1. the "previous session": place a far-from-market resting LIMIT BUY -------------------
    let previous_session = build_rest(&creds);
    let mark = vike_binance::family::execution::spot_fee::ticker_price(
        &previous_session.transport,
        demo_rest(),
        SYMBOL,
    )
    .expect("ticker mark must be live");
    // 10% below the mark, on the tick grid (the `binance_capture_smoke.rs` price): half the mark
    // trips the demo spot gateway's PERCENT_PRICE_BY_SIDE bid floor (-1013), yet a BTC bid 10%
    // down never fills inside the test window.
    let tick = previous_session.properties.tick_size;
    let limit_px = (mark * 0.9 / tick).round() * tick;
    let qty = (previous_session.properties.min_notional.max(5.0) * 1.6) / limit_px;
    let order_client_id = format!("vtrestore{}", now_ms() % 100_000_000);
    let ghost = format!("vtrestoreghost{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = VenueRest::submit_order(&previous_session, &request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    // From here on the order may be resting: the guard cancels it on every exit path.
    let cleanup =
        CancelOnDrop { rest: &previous_session, order_client_id: &order_client_id, host: "demo" };
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // --- 2. the restart: the real recon client, the planner ------------------------------------
    let recon = ReconcileClientBinance::spot(
        BinanceHmacSigner::new(&creds, now_ms),
        UreqTransport::new("binance")
            .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate()),
        demo_rest(),
        SYMBOL,
    );
    let restored = [
        restored_limit(&order_client_id, "binance", SYMBOL),
        restored_limit(&ghost, "binance", SYMBOL),
    ];
    let (plan, _) = plan_pass(&recon, "binance", SYMBOL, &restored);
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
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientRest::new(build_rest(&creds)),
        "binance",
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
    wait_until_gone(&recon, "binance", SYMBOL, &restored, &order_client_id);

    // --- 4. the venue stream's cancel, folded --------------------------------------------------
    let canceled = Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.clone(),
        reason: "restore smoke: user-data stream cancel".into(),
        timestamp: now_ms(),
    });
    engine.on_event(&canceled, &mut EventBusOutbox::default());
    assert_eq!(
        engine.registry[&order_client_id].status,
        OrderStatus::Canceled,
        "the fold must turn the restored order terminal"
    );

    drop(cleanup);
    tracing::info!(target: "vike_binance", "restore smoke green: placed -> adopted -> re-registered -> engine cancel -> gone at venue -> folded Canceled");
}
