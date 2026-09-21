//! LIVE smoke for the `OkxExecutionClient` ADAPTER (not just the perp REST): drives the ExecActor →
//! run_loop → OkxPerpRest path AND the audit-A3 `_with_resync` user-data pump. The old
//! `okx_perp_smoke` is flaky because its NON-resync pump loses a fill that lands in the WS
//! auth/subscribe gap; this adapter's `_with_resync` pump replays history to recover it — so a
//! generous fill deadline here should still go green.
//!     cargo test -p vike-okx --test okx_exec_client_smoke -- --ignored --nocapture

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::lanes::Ingest;
use vike_exec::{ExecutionClient, event_channel};
use vike_model::events::Event;
use vike_okx::OkxExecutionClient;
use vike_okx::perp::{OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};

const SYMBOL: &str = "BTC-USDT-SWAP";

use vike_model::clock::now_ms;

fn classify(e: &Event) -> (String, &'static str) {
    match e {
        Event::OrderSubmitted(x) => (x.client_order_id.clone(), "Submitted"),
        Event::OrderAccepted(x) => (x.client_order_id.clone(), "Accepted"),
        Event::OrderRejected(x) => (x.client_order_id.clone(), "Rejected"),
        Event::OrderCanceled(x) => (x.client_order_id.clone(), "Canceled"),
        Event::OrderFilled(x) => (x.client_order_id.clone(), "Filled"),
        Event::OrderPartiallyFilled(x) => (x.client_order_id.clone(), "PartiallyFilled"),
        _ => (String::new(), "other"),
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_exec_client_market_round_trip() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("okx", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({"data": d})).expect("instruments data"),
    )[SYMBOL]
        .clone();
    let probe = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let mark = probe.last_price().expect("last_price");
    let base0 = probe
        .reconcile_positions()
        .expect("reconcile")
        .positions
        .iter()
        .find(|(s, _)| s == SYMBOL)
        .map_or(0.0, |(_, q)| *q);
    let leg = inst.properties.min_qty * inst.ct_val; // one min lot, in BASE units
    tracing::info!(target: "vike_okx", "adapter smoke: mark={mark} base0={base0} leg={leg} BTC");

    // THE ADAPTER under test.
    let (events, mut rx) = event_channel(512);
    let mut client = OkxExecutionClient::spawn(
        creds,
        SYMBOL.to_string(),
        inst.properties,
        inst.ct_val,
        events,
        false,
    );
    std::thread::sleep(Duration::from_secs(4)); // login+subscribe settle

    let mut wait_for = |coid: &str, want: &str, secs: u64| -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Ingest::Event(e)) => {
                    let (c, k) = classify(&e);
                    if !c.is_empty() {
                        tracing::info!(target: "vike_okx", "adapter event: {c} -> {k}");
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
        "client_order_id": buy, "venue": "okx", "symbol": SYMBOL,
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
        "client_order_id": sell, "venue": "okx", "symbol": SYMBOL,
        "side": -1, "qty": leg, "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    client.submit(&req);
    assert!(wait_for(&sell, "Filled", 40), "{sell} never Filled");

    std::thread::sleep(Duration::from_secs(2));
    let base1 = probe
        .reconcile_positions()
        .expect("reconcile after")
        .positions
        .iter()
        .find(|(s, _)| s == SYMBOL)
        .map_or(0.0, |(_, q)| *q);
    tracing::info!(target: "vike_okx", "adapter round trip: base0={base0} base1={base1}");
    assert!(
        (base1 - base0).abs() < leg * 0.5,
        "adapter round trip must flatten: base0={base0} base1={base1}"
    );

    client.detach();
    tracing::info!(target: "vike_okx", "OkxExecutionClient adapter smoke GREEN");
}
