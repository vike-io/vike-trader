//! LIVE smoke tests for the Aster bridge. Both are `#[ignore]`d and self-skip when no `ASTER_*`
//! creds are in the workspace `.env`, so they never run in CI and never fail a credential-less box.
//!
//! Creds resolve LIVE-then-TESTNET (`ASTER_LIVE_*` preferred), matching `make_engine`'s aster arm.
//! The `trade` agent (Read/Perp/Spot, no withdraw) was live-mainnet-verified: `GET /fapi/v3/balance`
//! → 200 OK. The read-only `live_balance_auth` reproduces that; it's safe (no orders, no funds).
//!
//! `live_order_roundtrip` submits a REAL far-from-market order on whatever network the creds target
//! (MAINNET if `ASTER_LIVE_*`), so it is DOUBLE-gated: `#[ignore]` AND requires `ASTER_SMOKE_ORDER=1`.
//! On an unfunded account the venue rejects it (no funds move); a funded/testnet account is needed to
//! observe an accept/fill. Run:
//!   cargo test -p vike-aster --test aster_smoke -- --ignored --nocapture
//!   ASTER_SMOKE_ORDER=1 cargo test -p vike-aster --test aster_smoke live_order -- --ignored --nocapture

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vike_bridge_core::credentials::{Environment, load_workspace_dotenv_from};
use vike_bridge_core::{Credentials, Signer, VenueRest};

fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
}

/// LIVE-then-TESTNET creds from the workspace `.env`; `None` ⇒ the test self-skips.
fn resolve() -> Option<(Environment, Credentials)> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    vike_aster::signing::load_aster_credentials(Environment::Live, &vars)
        .map(|c| (Environment::Live, c))
        .or_else(|| {
            vike_aster::signing::load_aster_credentials(Environment::Demo, &vars)
                .map(|c| (Environment::Demo, c))
        })
}

/// Read-only auth proof: a signed `GET /fapi/v3/balance` must return 200 with a JSON balance array.
/// A wrong EIP-712 signature (field, nonce, domain, or param order) fails this with a non-200.
#[test]
#[ignore = "live: signed GET /fapi/v3/balance with real creds (read-only, no orders)"]
fn live_balance_auth() {
    let Some((env, creds)) = resolve() else {
        eprintln!("SKIP live_balance_auth: no ASTER_{{LIVE,TESTNET}}_* creds in workspace .env");
        return;
    };
    let signer = vike_aster::signing::AsterSigner::new(&creds, now_us);
    let prepared = signer.prepare(&[], "GET", "/fapi/v3/balance");
    let host = vike_aster::urls::urls_for(env).fapi_rest;
    let url = format!("{host}/fapi/v3/balance?{}", prepared.query);
    let agent = vike_bridge_core::http::blocking_agent_with_timeout(Duration::from_secs(15));
    let mut resp = agent.get(&url).call().expect("network reaching Aster");
    let status = resp.status();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    eprintln!("balance status={status} body={}", body.chars().take(300).collect::<String>());
    assert_eq!(status.as_u16(), 200, "auth failed ({env:?}): {body}");
    let v: serde_json::Value = serde_json::from_str(&body).expect("balance JSON");
    assert!(v.is_array(), "expected a balance array, got: {body}");
}

/// Full signed-order round-trip: submit a tiny far-from-market LIMIT and cancel it. The venue MUST
/// return a terminal event (Accepted on a funded account, Rejected on an unfunded one) — either way
/// proving the signed order path reaches Aster. DOUBLE-gated (`#[ignore]` + `ASTER_SMOKE_ORDER=1`)
/// because it places a REAL order on the creds' network (mainnet for `ASTER_LIVE_*`).
#[test]
#[ignore = "live: submits a REAL far-from-market order (mainnet if ASTER_LIVE_*); needs ASTER_SMOKE_ORDER=1"]
fn live_order_roundtrip() {
    if std::env::var("ASTER_SMOKE_ORDER").as_deref() != Ok("1") {
        eprintln!("SKIP live_order_roundtrip: set ASTER_SMOKE_ORDER=1 to place a REAL order");
        return;
    }
    let Some((env, creds)) = resolve() else {
        eprintln!("SKIP live_order_roundtrip: no ASTER creds in workspace .env");
        return;
    };
    let sym = "BTCUSDT";
    let (properties, _base) =
        vike_aster::fetch_aster_properties(env, &creds, &format!("{sym}.P")).unwrap_or_default();
    let rest = vike_aster::perp::AsterPerpRest {
        signer: vike_aster::signing::AsterSigner::new(&creds, now_us),
        transport: vike_bridge_core::UreqTransport::new("aster"),
        base_url: vike_aster::urls::urls_for(env).fapi_rest.to_string(),
        symbol: sym.to_string(),
        properties,
        leverage: 1.0,
        builder: None,
    };
    let coid = format!("smoke-{}", now_us());
    let req = vike_model::OrderRequest {
        client_order_id: coid.clone(),
        venue: "aster".to_string(),
        symbol: sym.to_string(),
        side: 1,
        qty: 0.001,
        order_type: "limit".to_string(),
        price: Some(1000.0), // far below BTC mark → never fills; canceled anyway
        ..Default::default()
    };
    let events = rest.submit_order(&req);
    eprintln!("submit -> {events:?}");
    assert!(!events.is_empty(), "expected a terminal/accept event from the venue");
    let _ = rest.cancel_order(&coid);
}
