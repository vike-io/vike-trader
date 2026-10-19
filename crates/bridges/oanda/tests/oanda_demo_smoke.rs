//! LIVE demo smoke (network + fxPractice creds — run manually):
//!     cargo test -p vike-oanda --test oanda_demo_smoke -- --ignored --nocapture
//!
//! OANDA's `fxPractice` environment is a real, safe paper account (virtual balance, same v20
//! API as `fxTrade`) — the OANDA analogue of the crypto venues' testnet/demo accounts, so this
//! mirrors `crates/bridges/binance/tests/binance_demo_smoke.rs`'s ladder rather than falling back
//! to a connect-only smoke: submit a far-from-market resting LIMIT BUY (price = 0.10, nowhere near
//! any real EUR/USD rate, 1 unit — negligible even against virtual margin) through the REAL
//! `OandaExecutionClient` -> verify it rests in `pendingOrders` -> cancel -> verify it's gone.
//!
//! Credentials come from the credential store's default DEMO oanda account (its `API_KEY` /
//! `ACCOUNT_ID` fields), resolved via `vike_secrets::smoke_directory`. Absent creds -> the test
//! SKIPS (the live gate), never fails CI.

use std::time::Duration;

use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, TimeInForce, now_ms};
use vike_oanda::{OandaExecutionClient, OandaRest, load_oanda_config_from};

/// Drain the ingest channel until `pred` matches an `Event`, or `total` elapses (panics on
/// timeout — a demo-account submit/cancel that never acks is itself a failure worth seeing).
fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    total: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timed out waiting for a matching event")
                .expect("ingest channel closed");
            if let Ingest::Event(ev) = ingest
                && pred(&ev)
            {
                return ev;
            }
        }
    })
}

/// Fetch pending (resting) orders and report whether `coid` is among them.
fn is_resting(rest: &OandaRest, base: &str, account_id: &str, coid: &str) -> bool {
    let path = format!("/v3/accounts/{account_id}/pendingOrders");
    let resp = rest.get(base, &path, "").expect("pendingOrders");
    resp.get("orders")
        .and_then(|o| o.as_array())
        .map(|orders| {
            orders
                .iter()
                .any(|o| o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(coid))
        })
        .unwrap_or(false)
}

#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials — run manually (see module doc)"]
fn oanda_demo_place_and_cancel_far_from_market_limit() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = load_oanda_config_from(AccountTier::Demo, &creds) else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };

    let (events, mut rx) = event_channel(64);
    let mut client = OandaExecutionClient::spawn(config.clone(), events);

    let coid = format!("vtroandasmoke{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0, // 1 unit — negligible even on a real account, irrelevant on virtual fxPractice
        order_type: vike_model::OrderType::Limit,
        price: Some(0.10), // nowhere near any real EUR/USD rate -> must rest, never fill
        time_in_force: TimeInForce::Gtc,
        ts: now_ms(),
        ..Default::default()
    };

    client.submit(&request);
    let accepted_or_rejected = wait_for(&mut rx, Duration::from_secs(20), |ev| {
        matches!(ev, Event::OrderAccepted(a) if a.client_order_id == coid)
            || matches!(ev, Event::OrderRejected(r) if r.client_order_id == coid)
    });
    match accepted_or_rejected {
        Event::OrderAccepted(_) => {
            tracing::info!(target: "vike_oanda::smoke", "order {coid} ACCEPTED")
        }
        Event::OrderRejected(r) => panic!("demo order {coid} was REJECTED: {}", r.reason),
        other => unreachable!("wait_for predicate guarantees Accepted|Rejected, got {other:?}"),
    }

    let rest = OandaRest::new(config.api_token.clone());
    assert!(
        is_resting(&rest, &config.rest_base, &config.account_id, &coid),
        "order {coid} must be resting in pendingOrders after acceptance"
    );
    tracing::info!(target: "vike_oanda::smoke", "order {coid} confirmed resting");

    client.cancel(&coid);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |ev| matches!(ev, Event::OrderCanceled(c) if c.client_order_id == coid),
    );

    assert!(
        !is_resting(&rest, &config.rest_base, &config.account_id, &coid),
        "order {coid} must be gone from pendingOrders after cancel"
    );
    tracing::info!(target: "vike_oanda::smoke", "cancel confirmed — demo ladder green");

    drop(client); // deterministic teardown (ExecActor::drop joins the exec + stream threads)
}

/// The venue's view of the resting order named `coid`: `(venue order id, price)`.
fn pending_view(
    rest: &OandaRest,
    base: &str,
    account_id: &str,
    coid: &str,
) -> Option<(String, String)> {
    let path = format!("/v3/accounts/{account_id}/pendingOrders");
    let resp = rest.get(base, &path, "").expect("pendingOrders");
    resp.get("orders").and_then(|o| o.as_array())?.iter().find_map(|o| {
        let named = o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(coid);
        named.then(|| {
            let text = |k: &str| o.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            (text("id"), text("price"))
        })
    })
}

/// Order REPLACE on the practice account: rest a far-from-market limit, MODIFY its price, and prove
/// the three claims no offline test can: the venue accepts the replacement under the SAME client id,
/// the pending order is the NEW one (new venue id, new price), and `@coid` still reaches it (the
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
    let Some(config) = load_oanda_config_from(AccountTier::Demo, &creds) else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };

    let (events, mut rx) = event_channel(64);
    let mut client = OandaExecutionClient::spawn(config.clone(), events);
    let rest = OandaRest::new(config.api_token.clone());

    let coid = format!("vtroandareplace{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.10),
        time_in_force: TimeInForce::Gtc,
        ts: now_ms(),
        ..Default::default()
    };
    client.submit(&request);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |ev| matches!(ev, Event::OrderAccepted(a) if a.client_order_id == coid),
    );
    let (first_id, first_price) =
        pending_view(&rest, &config.rest_base, &config.account_id, &coid).expect("resting");
    assert_eq!(first_price, "0.10000");

    client.modify(&request, None, Some(0.11));
    let modified = wait_for(&mut rx, Duration::from_secs(20), |ev| {
        matches!(ev, Event::OrderModified(m) if m.client_order_id == coid)
            || matches!(ev, Event::OrderModifyRejected(r) if r.client_order_id == coid)
    });
    let Event::OrderModified(modified) = modified else {
        panic!("the replace was REFUSED: {modified:?}");
    };
    let (second_id, second_price) =
        pending_view(&rest, &config.rest_base, &config.account_id, &coid)
            .expect("the replacement must rest under the SAME client id");
    assert_eq!(second_price, "0.11000", "the new price rests");
    assert_ne!(second_id, first_id, "OANDA gave the replacement a NEW venue order id");
    assert_eq!(modified.venue_order_id.as_deref(), Some(second_id.as_str()));

    // The replaced order's cancel line arrives on the stream; it must not end the logical order.
    std::thread::sleep(Duration::from_secs(4));
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(Event::OrderCanceled(c)) = ingest {
            assert_ne!(c.client_order_id, coid, "the replace-cancel surfaced as a cancel");
        }
    }

    client.cancel(&coid); // `@coid` must now name the replacement
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |ev| matches!(ev, Event::OrderCanceled(c) if c.client_order_id == coid),
    );
    assert!(pending_view(&rest, &config.rest_base, &config.account_id, &coid).is_none());
    drop(client);
}
