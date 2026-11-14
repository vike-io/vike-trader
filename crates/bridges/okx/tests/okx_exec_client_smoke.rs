//! LIVE smoke for the okx exec client (`vike_okx::execution::spawn`, the shared actor) (not just the perp REST): drives the ExecutionClientActor →
//! run_loop → VenueRestOkx path AND the audit-A3 `_with_resync` user-data pump. The old
//! `okx_perp_smoke` is flaky because its NON-resync pump loses a fill that lands in the WS
//! auth/subscribe gap; this adapter's `_with_resync` pump replays history to recover it — so a
//! generous fill deadline here should still go green.
//!     cargo test -p vike-okx --test okx_exec_client_smoke -- --ignored --nocapture

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_okx::execution::rest::{PATH_INSTRUMENTS, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;
use vike_secrets::credentials::api_credentials;

const SYMBOL: &str = "BTC-USDT-SWAP";

use vike_model::now_ms;

fn classify(e: &Event) -> (String, &'static str) {
    match e {
        Event::OrderSubmitted(x) => (x.order_client_id.clone(), "Submitted"),
        Event::OrderAccepted(x) => (x.order_client_id.clone(), "Accepted"),
        Event::OrderRejected(x) => (x.order_client_id.clone(), "Rejected"),
        Event::OrderCanceled(x) => (x.order_client_id.clone(), "Canceled"),
        Event::OrderFilled(x) => (x.order_client_id.clone(), "Filled"),
        Event::OrderPartiallyFilled(x) => (x.order_client_id.clone(), "PartiallyFilled"),
        _ => (String::new(), "other"),
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_execution_client_market_round_trip() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let net = demo_net();
    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate());
    let info = transport
        .public(
            &net.rest,
            PATH_INSTRUMENTS,
            &[("instType", "SWAP".into()), ("instId", SYMBOL.into())],
        )
        .expect("public instruments");
    let instrument = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({"data": d})).expect("instruments data"),
    )[SYMBOL]
        .clone();
    let probe = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: net.rest.clone(),
        symbol: SYMBOL.to_string(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let mark = probe.last_price().expect("last_price");
    // The venue's position as the live reconcile pass reads it (`vike_okx::reconcile`, the
    // production factory), in BASE units.
    let recon = vike_okx::recon_client(&creds, SYMBOL, instrument.ct_val, &net)
        .expect("okx recon_client is always Some");
    let venue_qty = |what: &str| -> f64 {
        let rows = recon.fetch_position_status_reports().expect(what);
        rows.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum()
    };
    let base0 = venue_qty("reconcile");
    let leg = instrument.properties.min_qty * instrument.ct_val; // one min lot, in BASE units
    tracing::info!(target: "vike_okx", "adapter smoke: mark={mark} base0={base0} leg={leg} BTC");

    // THE ADAPTER under test.
    let (events, mut rx) = event_channel(512);
    let mut client = vike_okx::execution::spawn(creds, SYMBOL.to_string(), events, net);
    std::thread::sleep(Duration::from_secs(4)); // login+subscribe settle

    let mut wait_for = |order_client_id: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Ingest::Event(e)) => {
                    let (c, k) = classify(&e);
                    if !c.is_empty() {
                        tracing::info!(target: "vike_okx", "adapter event: {c} -> {k}");
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
        "order_client_id": buy, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": leg, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&buy, "Accepted", 15), "{buy} never Accepted");
    // Generous: allow the resync supervisor to recover a gap-window fill.
    assert!(wait_for(&buy, "Filled", 40), "{buy} never Filled (pump/resync)");
    tracing::info!(target: "vike_okx", "BUY filled through the adapter resync pump");

    // SELL to flatten
    let sell = format!("adpSel{}", now_ms() % 100_000_000);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": sell, "venue": "okx", "symbol": SYMBOL,
        "side": -1, "qty": leg, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&sell, "Filled", 40), "{sell} never Filled");

    std::thread::sleep(Duration::from_secs(2));
    let base1 = venue_qty("reconcile after");
    tracing::info!(target: "vike_okx", "adapter round trip: base0={base0} base1={base1}");
    assert!(
        (base1 - base0).abs() < leg * 0.5,
        "adapter round trip must flatten: base0={base0} base1={base1}"
    );

    client.detach();
    tracing::info!(target: "vike_okx", "okx exec client adapter smoke GREEN");
}
