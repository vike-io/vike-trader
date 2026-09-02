//! LIVE smoke for the `BybitExecutionClient` ADAPTER itself (not just the perp REST): drives the
//! ExecActor → run_loop → BybitPerpRest path AND the audit-A3 `_with_resync` user-data pump, and
//! asserts on the exact `Event`s the adapter pushes into its `EventSender`.
//!     cargo test -p vike-bybit --test bybit_exec_client_smoke -- --ignored --nocapture
//!
//! Flow: spawn adapter → MARKET BUY (min qty) → observe [OrderSubmitted, OrderAccepted, OrderFilled]
//! (fill delivered by the pump, or recovered by resync if it lands in the auth gap) → MARKET SELL to
//! flatten → observe its fill → reconcile-confirm flat via a separate REST client → detach.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::perp::{parse_bybit_perp_instruments, BybitPerpRest, DEMO_REST, PATH_INSTRUMENTS};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_bybit::BybitExecutionClient;
use vike_exec::lanes::Ingest;
use vike_exec::{event_channel, ExecutionClient};
use vike_model::events::Event;

use vike_model::clock::now_ms;

fn classify(e: &Event) -> (String, &'static str) {
    match e {
        Event::OrderSubmitted(x) => (x.client_order_id.clone(), "Submitted"),
        Event::OrderAccepted(x) => (x.client_order_id.clone(), "Accepted"),
        Event::OrderRejected(x) => (x.client_order_id.clone(), "Rejected"),
        Event::OrderCanceled(x) => (x.client_order_id.clone(), "Canceled"),
        Event::OrderModified(x) => (x.client_order_id.clone(), "Modified"),
        Event::OrderFilled(x) => (x.client_order_id.clone(), "Filled"),
        Event::OrderPartiallyFilled(x) => (x.client_order_id.clone(), "PartiallyFilled"),
        _ => (String::new(), "other"),
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_exec_client_market_round_trip() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    // properties + mark (public instruments + last price) — a throwaway REST client for the read side.
    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    let probe = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    let mark = probe.last_price().expect("last_price");
    let base0 =
        probe.reconcile_positions().expect("reconcile").positions.first().map_or(0.0, |(_, q)| *q);
    let qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / mark);
    let qty = (qty / inst.properties.step_size).ceil() * inst.properties.step_size;
    tracing::info!(target: "vike_bybit", "adapter smoke: mark={mark} base0={base0} qty={qty}");

    // THE ADAPTER under test.
    let (events, mut rx) = event_channel(512);
    let mut client =
        BybitExecutionClient::spawn(creds, "BTCUSDT".to_string(), inst.properties, events, false);
    std::thread::sleep(Duration::from_secs(4)); // pump auth+subscribe settle

    // collect events for `secs`, returning true as soon as `coid` reaches `want`; logs each event.
    let mut wait_for = |coid: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Ingest::Event(e)) => {
                    let (c, k) = classify(&e);
                    if !c.is_empty() {
                        tracing::info!(target: "vike_bybit", "adapter event: {c} -> {k}");
                        assert_ne!((c.as_str(), k), (coid, "Rejected"), "adapter rejected {coid}");
                        if c == coid && k == want {
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
        "client_order_id": buy, "venue": "bybit", "symbol": "BTCUSDT",
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
        "client_order_id": sell, "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": qty, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&sell, "Filled", 30), "{sell} never Filled");

    // reconcile-confirm flat (independent REST client)
    std::thread::sleep(Duration::from_secs(2));
    let base1 = probe
        .reconcile_positions()
        .expect("reconcile after")
        .positions
        .first()
        .map_or(0.0, |(_, q)| *q);
    tracing::info!(target: "vike_bybit", "adapter round trip: base0={base0} base1={base1}");
    assert!(
        (base1 - base0).abs() < inst.properties.step_size * 1.5,
        "adapter round trip must flatten: base0={base0} base1={base1}"
    );

    client.detach();
    tracing::info!(target: "vike_bybit", "BybitExecutionClient adapter smoke GREEN");
}

/// LIVE proof of the DOM drag-to-modify path THROUGH the adapter (`ExecCommand::Modify` → native V5
/// amend): place a resting BUY LIMIT far below market (never fills), amend its price via
/// `client.modify`, observe `OrderModified`, then cancel. No fill, no position taken.
#[test]
#[ignore = "network + demo creds — run manually"]
fn bybit_exec_client_resting_modify_cancel() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };
    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    let probe = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    let mark = probe.last_price().expect("last_price");
    // resting bid ~10% below market (won't fill in the test window; stays inside Bybit's price band).
    let round = |p: f64| (p / inst.properties.tick_size).round() * inst.properties.tick_size;
    let px0 = round(mark * 0.90);
    let px1 = round(mark * 0.92); // the amended price — still safely below market
    let qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / px0);
    let qty = (qty / inst.properties.step_size).ceil() * inst.properties.step_size;

    let (events, mut rx) = event_channel(512);
    let mut client =
        BybitExecutionClient::spawn(creds, "BTCUSDT".to_string(), inst.properties, events, false);
    std::thread::sleep(Duration::from_secs(4)); // pump auth+subscribe settle

    let mut wait_for = |coid: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if let Ok(Ingest::Event(e)) = rx.try_recv() {
                let (c, k) = classify(&e);
                if !c.is_empty() {
                    tracing::info!(target: "vike_bybit", "adapter event: {c} -> {k}");
                    assert_ne!((c.as_str(), k), (coid, "Rejected"), "adapter rejected {coid}");
                    if c == coid && k == want {
                        return true;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        false
    };

    let coid = format!("adpMod{}", now_ms() % 100_000_000);
    let order: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "limit", "price": px0, "ts": now_ms()
    }))
    .unwrap();
    tracing::info!(target: "vike_bybit", "resting BUY LIMIT {qty} @ {px0} (mark {mark}); will amend → {px1}");
    client.submit(&order);
    assert!(wait_for(&coid, "Accepted", 15), "{coid} never Accepted");

    // THE path under test: DOM drag → ExecCommand::Modify → BybitPerpRest::modify_order.
    client.modify(&order, None, Some(px1));
    assert!(
        wait_for(&coid, "Modified", 15),
        "{coid} never Modified (live amend through the adapter)"
    );
    tracing::info!(target: "vike_bybit", "resting order AMENDED through the adapter");

    client.cancel(&coid);
    assert!(wait_for(&coid, "Canceled", 15), "{coid} never Canceled");
    client.detach();
    tracing::info!(target: "vike_bybit", "BybitExecutionClient live modify smoke GREEN");
}
