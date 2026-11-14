//! One-off DEMO cleanup: reconcile the OKX BTC-USDT-SWAP position and, if it is not flat,
//! submit ONE reduce-only market order (direct REST — confirmed by a re-reconcile, no WS-fill
//! dependency) to return it to flat. Read-only when already flat.
//!     cargo test -p vike-okx --test okx_flatten_check -- --ignored --nocapture

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_okx::execution::rest::{PATH_INSTRUMENTS, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;
use vike_secrets::credentials::api_credentials;

const SYMBOL: &str = "BTC-USDT-SWAP";

use vike_model::now_ms;

#[test]
#[ignore = "network + demo creds — run manually"]
fn okx_flatten_if_needed() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("okx", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "okx") else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };

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

    let rest = VenueRestOkx {
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

    // The position and balance as the live reconcile pass reads them (`vike_okx::reconcile`).
    let recon = vike_okx::recon_client(&creds, SYMBOL, instrument.ct_val, &net)
        .expect("okx recon_client is always Some");
    let position = |what: &str| -> f64 {
        let rows = recon.fetch_position_status_reports().expect(what);
        rows.iter().filter(|p| p.symbol == SYMBOL).map(|p| p.qty).sum()
    };
    let base = position("reconcile");
    let balance = recon.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_okx", "OKX position (base units): {base}  balance={balance:?}");

    // A dust threshold of half a contract's base value — below this, treat as flat.
    if base.abs() < instrument.ct_val * 0.5 {
        tracing::info!(target: "vike_okx", "already flat — nothing to clean up");
        return;
    }

    // Flatten: reduce-only market, opposite side, qty = |base| (base units; VenueRestOkx converts to
    // contracts internally).
    let side = vike_model::closing_side(base);
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": format!("okxflat{}", now_ms() % 100_000_000),
        "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base.abs(), "order_type": "market", "reduce_only": true, "ts": now_ms()
    }))
    .unwrap();
    tracing::info!(target: "vike_okx", "flattening: side={side} qty={} (reduce-only market)", base.abs());
    for event in rest.submit_order(&req) {
        tracing::info!(target: "vike_okx", "flatten event: {event:?}");
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let base_after = position("reconcile after");
    tracing::info!(target: "vike_okx", "OKX position AFTER flatten (base units): {base_after}");
    assert!(
        base_after.abs() < instrument.ct_val * 0.5,
        "flatten must return to ~flat: {base_after}"
    );
}
