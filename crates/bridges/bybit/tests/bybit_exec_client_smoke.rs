//! LIVE smoke for the bybit exec client (`vike_bybit::execution::spawn`, the shared actor) ITSELF (not just the perp REST): drives the
//! ExecutionClientActor → run_loop → VenueRestBybit path AND the audit-A3 `_with_resync` user-data pump, and
//! asserts on the exact `Event`s the adapter pushes into its `EventSender`.
//!     cargo test -p vike-bybit --test bybit_exec_client_smoke -- --ignored --nocapture
//!
//! Flow: spawn adapter → MARKET BUY (min qty) → observe [OrderSubmitted, OrderAccepted, OrderFilled]
//! (fill delivered by the pump, or recovered by resync if it lands in the auth gap) → MARKET SELL to
//! flatten → observe its fill → reconcile-confirm flat via the production reconcile client
//! (`vike_bybit::reconcile`) → detach.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, VenueRestBybit, parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_secrets::credentials::api_credentials;

use vike_model::now_ms;

fn classify(e: &Event) -> (String, &'static str) {
    match e {
        Event::OrderSubmitted(x) => (x.order_client_id.clone(), "Submitted"),
        Event::OrderAccepted(x) => (x.order_client_id.clone(), "Accepted"),
        Event::OrderRejected(x) => (x.order_client_id.clone(), "Rejected"),
        Event::OrderCanceled(x) => (x.order_client_id.clone(), "Canceled"),
        Event::OrderModified(x) => (x.order_client_id.clone(), "Modified"),
        Event::OrderFilled(x) => (x.order_client_id.clone(), "Filled"),
        Event::OrderPartiallyFilled(x) => (x.order_client_id.clone(), "PartiallyFilled"),
        _ => (String::new(), "other"),
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_execution_client_market_round_trip() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    // The demo hosts, from the release seed's rows.
    let demo = seed_endpoints(AccountTier::Demo);
    // properties + mark (public instruments + last price) — a throwaway REST client for the read side.
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            &demo.rest,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    let probe = VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo.rest.clone(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    };
    let mark = probe.last_price().expect("last_price");
    // The venue's position as the live reconcile pass reads it (`vike_bybit::reconcile`, the
    // production factory): the sum of the BTCUSDT rows, none read as flat.
    let recon = vike_bybit::recon_client(&creds, "BTCUSDT", &demo.rest)
        .expect("bybit recon_client is always Some");
    let venue_qty = |what: &str| -> f64 {
        recon.fetch_position_status_reports().expect(what).iter().map(|p| p.qty).sum()
    };
    let base0 = venue_qty("reconcile");
    let qty =
        f64::max(instrument.properties.min_qty, (instrument.properties.min_notional * 1.4) / mark);
    let qty = (qty / instrument.properties.step_size).ceil() * instrument.properties.step_size;
    tracing::info!(target: "vike_bybit", "adapter smoke: mark={mark} base0={base0} qty={qty}");

    // THE ADAPTER under test.
    let (events, mut rx) = event_channel(512);
    let mut client = vike_bybit::execution::spawn(creds, "BTCUSDT".to_string(), events, demo);
    std::thread::sleep(Duration::from_secs(4)); // pump auth+subscribe settle

    // collect events for `secs`, returning true as soon as `order_client_id` reaches `want`; logs each event.
    let mut wait_for = |order_client_id: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Ingest::Event(e)) => {
                    let (c, k) = classify(&e);
                    if !c.is_empty() {
                        tracing::info!(target: "vike_bybit", "adapter event: {c} -> {k}");
                        assert_ne!(
                            (c.as_str(), k),
                            (order_client_id, "Rejected"),
                            "adapter rejected {order_client_id}"
                        );
                        if c == order_client_id && k == want {
                            return true;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        false
    };

    // BUY
    let buy = format!("adpBuy{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": buy, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&buy, "Accepted", 15), "{buy} never Accepted");
    assert!(wait_for(&buy, "Filled", 30), "{buy} never Filled (pump/resync)");
    tracing::info!(target: "vike_bybit", "BUY filled through the adapter pump");

    // SELL to flatten
    let sell = format!("adpSel{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": sell, "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": qty, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&sell, "Filled", 30), "{sell} never Filled");

    // reconcile-confirm flat (independent REST client)
    std::thread::sleep(Duration::from_secs(2));
    let base1 = venue_qty("reconcile after");
    tracing::info!(target: "vike_bybit", "adapter round trip: base0={base0} base1={base1}");
    assert!(
        (base1 - base0).abs() < instrument.properties.step_size * 1.5,
        "adapter round trip must flatten: base0={base0} base1={base1}"
    );

    client.detach();
    tracing::info!(target: "vike_bybit", "bybit exec client adapter smoke GREEN");
}

/// LIVE proof of the DOM drag-to-modify path THROUGH the adapter (`ExecutionClientActorCommand::Modify` → native V5
/// amend): place a resting BUY LIMIT far below market (never fills), amend its price via
/// `client.modify`, observe `OrderModified`, then cancel. No fill, no position taken.
#[test]
#[ignore = "network + demo creds — run manually"]
fn bybit_exec_client_resting_modify_cancel() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };
    let demo = seed_endpoints(AccountTier::Demo);
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            &demo.rest,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    let probe = VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo.rest.clone(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    };
    let mark = probe.last_price().expect("last_price");
    // resting bid ~10% below market (won't fill in the test window; stays inside Bybit's price band).
    let round =
        |p: f64| (p / instrument.properties.tick_size).round() * instrument.properties.tick_size;
    let px0 = round(mark * 0.90);
    let px1 = round(mark * 0.92); // the amended price — still safely below market
    let qty =
        f64::max(instrument.properties.min_qty, (instrument.properties.min_notional * 1.4) / px0);
    let qty = (qty / instrument.properties.step_size).ceil() * instrument.properties.step_size;

    let (events, mut rx) = event_channel(512);
    let mut client = vike_bybit::execution::spawn(creds, "BTCUSDT".to_string(), events, demo);
    std::thread::sleep(Duration::from_secs(4)); // pump auth+subscribe settle

    let mut wait_for = |order_client_id: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(Ingest::Event(e)) = rx.try_recv() {
                let (c, k) = classify(&e);
                if !c.is_empty() {
                    tracing::info!(target: "vike_bybit", "adapter event: {c} -> {k}");
                    assert_ne!(
                        (c.as_str(), k),
                        (order_client_id, "Rejected"),
                        "adapter rejected {order_client_id}"
                    );
                    if c == order_client_id && k == want {
                        return true;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        false
    };

    let order_client_id = format!("adpMod{}", now_ms() % 100_000_000);
    let order: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "limit", "price": px0, "ts": now_ms()
    }))
    .unwrap();
    tracing::info!(target: "vike_bybit", "resting BUY LIMIT {qty} @ {px0} (mark {mark}); will amend → {px1}");
    client.submit(&order);
    assert!(wait_for(&order_client_id, "Accepted", 15), "{order_client_id} never Accepted");

    // THE path under test: DOM drag → ExecutionClientActorCommand::Modify → VenueRestBybit::modify_order.
    client.modify(&order, None, Some(px1));
    assert!(
        wait_for(&order_client_id, "Modified", 15),
        "{order_client_id} never Modified (live amend through the adapter)"
    );
    tracing::info!(target: "vike_bybit", "resting order AMENDED through the adapter");

    client.cancel(&order_client_id);
    assert!(wait_for(&order_client_id, "Canceled", 15), "{order_client_id} never Canceled");
    client.detach();
    tracing::info!(target: "vike_bybit", "bybit exec client live modify smoke GREEN");
}
