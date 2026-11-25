//! LIVE demo smoke (network + fxPractice creds — run manually):
//!     cargo test -p vike-oanda --test oanda_smokes -- --ignored --nocapture oanda_demo_smoke::
//!
//! OANDA's `fxPractice` environment is a real, safe paper account (virtual balance, same v20
//! API as `fxTrade`) — the OANDA analogue of the crypto venues' testnet/demo accounts, so this
//! mirrors `crates/bridges/binance/tests/binance_demo_smoke.rs`'s ladder rather than falling back
//! to a connect-only smoke: submit a far-from-market resting LIMIT BUY (price = 0.10, nowhere near
//! any real EUR/USD rate, 1 unit — negligible even against virtual margin) through the REAL
//! exec client (`spawn_execution_client`) -> verify it rests in `pendingOrders` -> cancel -> verify it's gone.
//!
//! Credentials come from the credential store's default DEMO oanda account (its `API_KEY` /
//! `ACCOUNT_ID` fields), resolved via `vike_secrets::smoke_directory`. Absent creds -> the test
//! SKIPS (the live gate), never fails CI.

use crate::ingest_wait;
use std::time::Duration;

use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, TimeInForce, now_ms};
use vike_oanda::{OandaTransport, load_oanda_config_from, spawn_execution_client};

/// Drain the ingest channel until `pred` matches an `Event`, or `total` elapses (panics on
/// timeout — a demo-account submit/cancel that never acks is itself a failure worth seeing).
fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    total: Duration,
    pred: impl FnMut(&Event) -> bool,
) -> Event {
    ingest_wait::wait_for(rx, total, pred).unwrap_or_else(|e| panic!("{e}"))
}

/// Fetch pending (resting) orders and report whether `order_client_id` is among them.
fn is_resting(rest: &OandaTransport, base: &str, account_id: &str, order_client_id: &str) -> bool {
    let path = format!("/v3/accounts/{account_id}/pendingOrders");
    let response = rest.get(base, &path, "").expect("pendingOrders");
    response
        .get("orders")
        .and_then(|o| o.as_array())
        .map(|orders| {
            orders.iter().any(|o| {
                o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(order_client_id)
            })
        })
        .unwrap_or(false)
}

#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials — run manually (see module doc)"]
fn oanda_demo_place_and_cancel_far_from_market_limit() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let urls = vike_venues::venue_urls::VenueUrls::seed("oanda");
    let Some(config) =
        load_oanda_config_from(AccountTier::Demo, &creds, &urls).expect("the seed's demo rows")
    else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };
    crate::hold_the_accounts_list(&creds, &urls);

    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(config.clone(), events);

    let order_client_id = format!("vtroandasmoke{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0, // 1 unit — negligible even on a real account, irrelevant on virtual fxPractice
        order_type: vike_model::OrderType::Limit,
        price: Some(0.10), // nowhere near any real EUR/USD rate -> must rest, never fill
        time_in_force: TimeInForce::Gtc,
        timestamp: now_ms(),
        ..Default::default()
    };

    client.submit(&request);
    let accepted_or_rejected = wait_for(&mut rx, Duration::from_secs(20), |event| {
        matches!(event, Event::OrderAccepted(a) if a.order_client_id == order_client_id)
            || matches!(event, Event::OrderRejected(r) if r.order_client_id == order_client_id)
    });
    match accepted_or_rejected {
        Event::OrderAccepted(_) => {
            tracing::info!(target: "vike_oanda::smoke", "order {order_client_id} ACCEPTED")
        }
        Event::OrderRejected(r) => {
            panic!("demo order {order_client_id} was REJECTED: {}", r.reason)
        }
        other => unreachable!("wait_for predicate guarantees Accepted|Rejected, got {other:?}"),
    }

    let rest = OandaTransport::new(config.api_token.expose().clone());
    assert!(
        is_resting(&rest, &config.rest_base, &config.account_id, &order_client_id),
        "order {order_client_id} must be resting in pendingOrders after acceptance"
    );
    tracing::info!(target: "vike_oanda::smoke", "order {order_client_id} confirmed resting");

    client.cancel(&order_client_id);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |event| matches!(event, Event::OrderCanceled(c) if c.order_client_id == order_client_id),
    );

    assert!(
        !is_resting(&rest, &config.rest_base, &config.account_id, &order_client_id),
        "order {order_client_id} must be gone from pendingOrders after cancel"
    );
    tracing::info!(target: "vike_oanda::smoke", "cancel confirmed — demo ladder green");

    drop(client); // deterministic teardown (ExecutionClientActor::drop joins the exec + stream threads)
}

/// MARKET_IF_TOUCHED on the practice account (wave 2 of the order kinds; NOT RUN in that wave: the owner
/// wants one demo pass after every order-kind wave, and no FX broker is contacted before 2026-11-09): a
/// priceless `take_profit` BUY with its trigger at 0.10 (far under any EUR/USD rate, so it waits for the
/// market to FALL to it, the take-profit side) is accepted, rests in `pendingOrders` as type
/// `MARKET_IF_TOUCHED` at `0.10000`, and the client's cancel ends it. What it proves is the wire shape
/// and the rest; what OANDA does with one placed on the STOP side (it fires on a cross from either
/// side, `crates/bridges/oanda/src/execution/mod.rs`'s `build_order_body_on`) it does not touch. Its pass is
/// what lets the capability row list `take_profit` for oanda. Run it as the module doc says, with
/// `oanda_demo_smoke::oanda_demo_place_and_cancel_far_from_market_mit`.
#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials — run manually (see module doc)"]
fn oanda_demo_place_and_cancel_far_from_market_mit() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let urls = vike_venues::venue_urls::VenueUrls::seed("oanda");
    let Some(config) =
        load_oanda_config_from(AccountTier::Demo, &creds, &urls).expect("the seed's demo rows")
    else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };
    crate::hold_the_accounts_list(&creds, &urls);

    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(config.clone(), events);
    let rest = OandaTransport::new(config.api_token.expose().clone());

    let order_client_id = format!("vtroandamit{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::TakeProfit,
        trigger_price: Some(0.10), // nowhere near any real EUR/USD rate -> must rest, never fire
        time_in_force: TimeInForce::Gtc,
        timestamp: now_ms(),
        ..Default::default()
    };
    client.submit(&request);
    match wait_for(&mut rx, Duration::from_secs(20), |event| {
        matches!(event, Event::OrderAccepted(a) if a.order_client_id == order_client_id)
            || matches!(event, Event::OrderRejected(r) if r.order_client_id == order_client_id)
    }) {
        Event::OrderAccepted(_) => {}
        Event::OrderRejected(r) => panic!("demo MIT {order_client_id} was REJECTED: {}", r.reason),
        other => unreachable!("wait_for predicate guarantees Accepted|Rejected, got {other:?}"),
    }
    let path = format!("/v3/accounts/{}/pendingOrders", config.account_id);
    let pending = rest.get(&config.rest_base, &path, "").expect("pendingOrders");
    let named = |o: &&serde_json::Value| {
        o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(order_client_id.as_str())
    };
    let row = pending["orders"]
        .as_array()
        .and_then(|orders| orders.iter().find(named))
        .unwrap_or_else(|| panic!("MIT {order_client_id} must rest in pendingOrders: {pending}"));
    tracing::info!(target: "vike_oanda::smoke", "CAPTURED pending MIT row: {row}");
    assert_eq!(row["type"], "MARKET_IF_TOUCHED");
    assert_eq!(row["price"], "0.10000");
    assert_eq!(row["units"], "1");

    client.cancel(&order_client_id);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |event| matches!(event, Event::OrderCanceled(c) if c.order_client_id == order_client_id),
    );
    assert!(pending_view(&rest, &config.rest_base, &config.account_id, &order_client_id).is_none());
    drop(client);
}

/// The venue's view of the resting order named `order_client_id`: `(venue order id, price)`.
fn pending_view(
    rest: &OandaTransport,
    base: &str,
    account_id: &str,
    order_client_id: &str,
) -> Option<(String, String)> {
    let path = format!("/v3/accounts/{account_id}/pendingOrders");
    let response = rest.get(base, &path, "").expect("pendingOrders");
    response.get("orders").and_then(|o| o.as_array())?.iter().find_map(|o| {
        let named =
            o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(order_client_id);
        named.then(|| {
            let text = |k: &str| o.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            (text("id"), text("price"))
        })
    })
}

/// Order REPLACE on the practice account: rest a far-from-market limit, MODIFY its price, and prove
/// the three claims no offline test can: the venue accepts the replacement under the SAME client id,
/// the pending order is the NEW one (new venue id, new price), and `@order_client_id` still reaches it (the
/// cancel). Also proves the replaced order's `ORDER_CANCEL` reached the stream and did NOT surface
/// as an `OrderCanceled` for the logical order.
///
/// Not wired into `.github/workflows/live-smokes.yml`: run it by hand first (see the module doc).
#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials — run manually (see module doc)"]
fn oanda_demo_replace_far_from_market_limit() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let urls = vike_venues::venue_urls::VenueUrls::seed("oanda");
    let Some(config) =
        load_oanda_config_from(AccountTier::Demo, &creds, &urls).expect("the seed's demo rows")
    else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };
    crate::hold_the_accounts_list(&creds, &urls);

    let (events, mut rx) = event_channel(64);
    let mut client = spawn_execution_client(config.clone(), events);
    let rest = OandaTransport::new(config.api_token.expose().clone());

    let order_client_id = format!("vtroandareplace{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.10),
        time_in_force: TimeInForce::Gtc,
        timestamp: now_ms(),
        ..Default::default()
    };
    client.submit(&request);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |event| matches!(event, Event::OrderAccepted(a) if a.order_client_id == order_client_id),
    );
    let (first_id, first_price) =
        pending_view(&rest, &config.rest_base, &config.account_id, &order_client_id)
            .expect("resting");
    assert_eq!(first_price, "0.10000");

    client.modify(&request, None, Some(0.11));
    let modified = wait_for(&mut rx, Duration::from_secs(20), |event| {
        matches!(event, Event::OrderModified(m) if m.order_client_id == order_client_id)
            || matches!(event, Event::OrderModifyRejected(r) if r.order_client_id == order_client_id)
    });
    let Event::OrderModified(modified) = modified else {
        panic!("the replace was REFUSED: {modified:?}");
    };
    let (second_id, second_price) =
        pending_view(&rest, &config.rest_base, &config.account_id, &order_client_id)
            .expect("the replacement must rest under the SAME client id");
    assert_eq!(second_price, "0.11000", "the new price rests");
    assert_ne!(second_id, first_id, "OANDA gave the replacement a NEW venue order id");
    assert_eq!(modified.venue_order_id.as_deref(), Some(second_id.as_str()));

    // The replaced order's cancel line arrives on the stream; it must not end the logical order.
    std::thread::sleep(Duration::from_secs(4));
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(Event::OrderCanceled(c)) = ingest {
            assert_ne!(
                c.order_client_id, order_client_id,
                "the replace-cancel surfaced as a cancel"
            );
        }
    }

    client.cancel(&order_client_id); // `@order_client_id` must now name the replacement
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |event| matches!(event, Event::OrderCanceled(c) if c.order_client_id == order_client_id),
    );
    assert!(pending_view(&rest, &config.rest_base, &config.account_id, &order_client_id).is_none());
    drop(client);
}
