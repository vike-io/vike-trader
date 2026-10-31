//! post-only LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-okx --test okx_post_only_smoke -- --ignored --nocapture
//!
//! Proves the okx post-only wire against the real venue (simulated-trading), with the venue's OWN
//! records as the witness:
//!
//! 1. a far-below post-only LIMIT BUY rests as `ordType=post_only` (orders-pending), then is cancelled;
//! 2. a post-only LIMIT BUY priced ABOVE the ask would take liquidity: the venue accepts and then
//!    cancels it (`state=canceled`, `cancelSource=31`), it executes nothing and does not rest, and the
//!    bridge's order-stream mapper (`event_mapper`) turns the venue's row into the typed terminal
//!    (`OrderCanceled` with the post-only reason);
//! 3. a post-only MARKET order is loud-denied client-side (wire untouched).
//!
//! Double-gated like every other `*_smoke.rs` in this crate (network + the default DEMO account's
//! creds + passphrase via `vike_secrets::smoke_directory`; self-skips when absent).

use std::assert_matches;
use std::time::{Duration, Instant};

use serde_json::json;

use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::AccountTier;
use vike_model::TimeInForce;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::now_ms;
use vike_okx::perp::{
    OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDERS_PENDING, REST, parse_okx_perp_instruments,
};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};
use vike_secrets::credentials::{Credentials, api_credentials};

const SYMBOL: &str = "BTC-USDT-SWAP";

fn load_demo_creds() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "okx");
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

fn po_req(coid: &str, qty: f64, price: Option<f64>, order_type: &str) -> vike_model::OrderRequest {
    let mut req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": order_type, "price": price, "ts": now_ms()
    }))
    .unwrap();
    req.time_in_force = TimeInForce::PostOnly;
    req
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_post_only_ladder() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
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
    assert!(mark > 0.0, "live mark required");
    let leg_base = inst.properties.min_qty * inst.ct_val;

    // 1. RESTS: far below the market a post-only buy is a plain maker order
    let coid = format!("vtrpo1{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, leg_base, Some(mark * 0.9), "limit"));
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "resting post-only: {events:?}");
    let pending = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    let row = pending
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|o| o.get("clOrdId").and_then(|c| c.as_str()) == Some(coid.as_str()))
        })
        .cloned()
        .unwrap_or_else(|| panic!("{coid} must rest: {pending}"));
    tracing::info!(target: "vike_okx", "resting row: {row}");
    assert_eq!(row.get("ordType").and_then(|v| v.as_str()), Some("post_only"), "{row}");
    rest.cancel_order(&coid).expect("cancel the resting post-only order");

    // 2. CROSSES: a buy just above the market lifts the ask — the venue must not execute it. (0.2%
    // above: the venue's own price band is +-0.5% around the market and refuses a limit beyond it with
    // 51006 before any post-only rule applies — measured, the first run of this smoke priced 1% above.)
    let coid = format!("vtrpo2{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, leg_base, Some(mark * 1.002), "limit"));
    tracing::info!(target: "vike_okx", "crossing submit events: {events:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    let row = loop {
        let hist = rest.get_orders_history(100).expect("orders-history");
        if let Some(row) = hist.as_array().and_then(|rows| {
            rows.iter()
                .find(|o| o.get("clOrdId").and_then(|c| c.as_str()) == Some(coid.as_str()))
                .cloned()
        }) {
            break row;
        }
        assert!(Instant::now() < deadline, "{coid} never appeared in orders-history");
        std::thread::sleep(Duration::from_millis(500));
    };
    tracing::info!(target: "vike_okx", "crossing row: {row}");
    assert_eq!(row.get("ordType").and_then(|v| v.as_str()), Some("post_only"), "{row}");
    assert_eq!(row.get("state").and_then(|v| v.as_str()), Some("canceled"), "{row}");
    let filled = row.get("accFillSz").and_then(|v| v.as_str()).unwrap_or("0");
    assert_eq!(filled.parse::<f64>().unwrap_or(1.0), 0.0, "must not execute: {row}");
    // the bridge's order-stream mapper turns that row into the typed terminal event
    let mapped = vike_okx::event_mapper::map_okx_order(&row, "okx", SYMBOL);
    tracing::info!(target: "vike_okx", "mapped: {mapped:?}");
    assert_matches!(
        mapped.as_slice(),
        [Event::OrderCanceled(c)] if c.is_post_only_cross(),
        "{mapped:?}"
    );

    // 3. a post-only MARKET order is refused locally
    let coid = format!("vtrpo3{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, leg_base, None, "market"));
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("PostOnly needs a limit order"),
        "{events:?}"
    );

    // nothing rests
    let pending = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    assert!(
        pending.as_array().unwrap_or(&vec![]).iter().all(|o| {
            o.get("clOrdId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrpo"))
        }),
        "no post-only smoke order may rest: {pending}"
    );
    tracing::info!(target: "vike_okx", "okx post-only ladder green: rests far, crossing cancelled by the venue and typed, market denied, book flat");
}
