//! Margin-mode step-2 LIVE demo smoke — proves the OKX un-hardcode against the real venue
//! (simulated-trading): rests a tiny far-from-market limit BUY with a NON-default requested
//! mode (`margin_mode: Some(Isolated)` → `tdMode:"isolated"`), asserts the venue ACCEPTS it,
//! reads the resting order back off `/api/v5/trade/orders-pending` to pin that the venue
//! itself reports `tdMode == "isolated"` (not just that our request said so), then cancels.
//! Never fills (price is ~50% below market).
//!
//!     cargo test -p vike-okx --test okx_margin_mode_smoke -- --ignored --nocapture
//!
//! Double-gated exactly like every other `*_smoke.rs` in this crate (network + `OKX_DEMO_*`
//! creds + passphrase in the workspace `.env` via `load_workspace_dotenv_from`; self-skips with a
//! `tracing::warn!` when absent — see `okx_perp_smoke.rs` / `okx_reconcile_smoke.rs`).
//! The offline byte-identity twins (unset ≡ pre-flip golden bytes, Cash denied without a wire
//! call) live in `perp.rs::margin_mode_tests` and `tests/offline/r6_okx_parity.rs`.

use serde_json::json;

use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Credentials, Environment,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::clock::now_ms;
use vike_model::events::Event;
use vike_model::MarginMode;
use vike_okx::perp::{
    parse_okx_perp_instruments, OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDERS_PENDING, REST,
};
use vike_okx::transport::{unwrap_okx, OkxTransport, UreqOkxTransport};

const SYMBOL: &str = "BTC-USDT-SWAP";

/// The crate's standard smoke double-gate: workspace `.env` → `OKX_DEMO_*`; `None` = self-skip.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("okx", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_isolated_margin_order_rests_and_cancels() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();

    let rest = OkxPerpRest {
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
    let mark = rest.last_price().expect("ticker");

    // one contract's worth of BASE, ~50% below market → rests, never fills
    let leg_base = inst.properties.min_qty * inst.ct_val;
    let px = mark * 0.5;
    let coid = format!("vtMgnIso{}", now_ms() % 100_000_000);
    let mut req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": leg_base, "order_type": "limit", "price": px, "ts": now_ms()
    }))
    .unwrap();
    req.margin_mode = Some(MarginMode::Isolated); // THE non-default mode under test

    let events = rest.submit_order(&req);
    tracing::info!(target: "vike_okx", "submit events: {events:?}");
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(events[0], Event::OrderSubmitted(_)));
    match &events[1] {
        Event::OrderAccepted(a) => {
            tracing::info!(target: "vike_okx", "ACCEPTED isolated order: venue_order_id={:?}", a.venue_order_id);
        }
        Event::OrderRejected(r) => {
            panic!("venue rejected the isolated-mode order (report, don't force): {}", r.reason)
        }
        other => panic!("unexpected event: {other:?}"),
    }

    // Venue-truth read-back: the resting row must carry tdMode == "isolated".
    let pending = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let row = pending
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|o| o.get("clOrdId").and_then(|c| c.as_str()) == Some(coid.as_str()))
        })
        .expect("our resting order is in orders-pending")
        .clone();
    let td_mode = row.get("tdMode").and_then(|t| t.as_str()).unwrap_or("");
    tracing::info!(target: "vike_okx", "venue reports resting tdMode={td_mode}");
    assert_eq!(td_mode, "isolated", "venue must report the ISOLATED trade mode: {row}");

    rest.cancel_order(&coid).expect("cancel");
    tracing::info!(
        target: "vike_okx",
        "okx margin-mode smoke green: isolated limit rested (venue-confirmed tdMode) -> canceled"
    );
}
