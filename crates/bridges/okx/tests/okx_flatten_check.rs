//! One-off DEMO cleanup: reconcile the OKX BTC-USDT-SWAP position and, if it is not flat,
//! submit ONE reduce-only market order (direct REST — confirmed by a re-reconcile, no WS-fill
//! dependency) to return it to flat. Read-only when already flat.
//!     cargo test -p vike-okx --test okx_flatten_check -- --ignored --nocapture

use serde_json::json;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_okx::perp::{OkxPerpRest, PATH_INSTRUMENTS, REST, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};

const SYMBOL: &str = "BTC-USDT-SWAP";

use vike_model::clock::now_ms;

#[test]
#[ignore = "network + demo creds — run manually"]
fn okx_flatten_if_needed() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("okx", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({"data": d})).expect("instruments data"),
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

    let snap = rest.reconcile_positions().expect("reconcile");
    let base = snap.positions.iter().find(|(s, _)| s == SYMBOL).map_or(0.0, |(_, q)| *q);
    tracing::info!(target: "vike_okx", "OKX position (base units): {base}  balance={}", snap.balance);

    // A dust threshold of half a contract's base value — below this, treat as flat.
    if base.abs() < inst.ct_val * 0.5 {
        tracing::info!(target: "vike_okx", "already flat — nothing to clean up");
        return;
    }

    // Flatten: reduce-only market, opposite side, qty = |base| (base units; OkxPerpRest converts to
    // contracts internally).
    let side = vike_model::closing_side(base);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": format!("okxflat{}", now_ms() % 100_000_000),
        "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base.abs(), "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    tracing::info!(target: "vike_okx", "flattening: side={side} qty={} (reduce-only market)", base.abs());
    for ev in rest.submit_order(&req) {
        tracing::info!(target: "vike_okx", "flatten event: {ev:?}");
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = rest.reconcile_positions().expect("reconcile after");
    let base_after = after.positions.iter().find(|(s, _)| s == SYMBOL).map_or(0.0, |(_, q)| *q);
    tracing::info!(target: "vike_okx", "OKX position AFTER flatten (base units): {base_after}");
    assert!(base_after.abs() < inst.ct_val * 0.5, "flatten must return to ~flat: {base_after}");
}
