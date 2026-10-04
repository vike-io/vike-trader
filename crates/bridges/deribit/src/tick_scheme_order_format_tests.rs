//! The tiered-grid order-format path (deribit-tick-scheme). With a `tick_scheme` present a limit
//! above a tier boundary snaps onto the COARSE tier grid the venue requires; with NO scheme the
//! price formats BYTE-IDENTICALLY to today's scalar `format_to_step_f(px, tick_size)` — the OFF
//! pin. `DeribitOrderTransport` is built but never `connect()`-ed: `build_order_params` is pure
//! and never touches the socket.
use serde_json::Value;
use vike_bridge_core::format::format_to_step_f;
use vike_model::{OrderRequest, SymbolProperties, TickScheme, TickTier};

use super::DeribitRest;
use crate::transport::DeribitOrderTransport;

const SYMBOL: &str = "BTC-8JUL26-62000-C";

/// The real Deribit BTC-option grid: base 0.0001, one step to 0.0005 above 0.005.
fn option_scheme() -> TickScheme {
    TickScheme::new(0.0001, &[TickTier { above_price: 0.005, tick_size: 0.0005 }])
        .expect("valid deribit grid")
}

fn rest(properties: SymbolProperties) -> DeribitRest {
    let tx = DeribitOrderTransport::new("wss://test.invalid", "id", "secret", None);
    DeribitRest::new(tx, SYMBOL, properties, "BTC")
}

fn limit_req(px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "c-tier".to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 0.1,
        order_type: "limit".to_string(),
        price: Some(px),
        ..Default::default()
    }
}

fn priced(params: &Value) -> f64 {
    params["price"].as_f64().expect("a numeric price param")
}

/// A tiered-grid option's properties: the same base tick 0.0001 the scheme is built from (so the
/// `with_tick_scheme` base-tick invariant holds), plus the tiered grid attached.
fn tiered_properties() -> SymbolProperties {
    SymbolProperties { tick_size: 0.0001, step_size: 0.1, min_qty: 0.1, ..Default::default() }
        .with_tick_scheme(option_scheme())
}

/// ABOVE the boundary: the limit snaps onto the 0.0005 tier grid — NOT the 0.0001 base grid the
/// scalar path would produce (0.0333, which is OFF the tier grid and the venue rejects). This is
/// the order-entry landmine the lane closes.
#[test]
fn tiered_limit_above_boundary_snaps_to_the_coarse_tier() {
    let params = rest(tiered_properties()).build_order_params(&limit_req(0.03330000001));
    // round-half-even on the 0.0005 grid: 0.0333.. → 0.0335
    assert_eq!(priced(&params), 0.0335, "snapped onto the coarse tier grid");
    // contrast: today's scalar/base-grid formatting is 0.0333, which is NOT a multiple of 0.0005
    assert_eq!(format_to_step_f(0.03330000001, 0.0001).parse::<f64>().unwrap(), 0.0333);
    // the tiered price really is on the coarse grid (a multiple of 0.0005 is idempotent under it)
    let px = priced(&params);
    assert_eq!(format_to_step_f(px, 0.0005).parse::<f64>().unwrap(), px);
}

/// AT the exact boundary (0.005): resolves to the LOWER tier (0.0001), and 0.005 is on that grid.
#[test]
fn tiered_limit_at_the_boundary_uses_the_lower_tier() {
    let params = rest(tiered_properties()).build_order_params(&limit_req(0.005));
    assert_eq!(priced(&params), 0.005);
}

/// BELOW the boundary: the base 0.0001 grid applies — and equals today's scalar formatting.
#[test]
fn tiered_limit_below_boundary_uses_the_base_grid() {
    let below = 0.0043;
    let params = rest(tiered_properties()).build_order_params(&limit_req(below));
    assert_eq!(priced(&params), 0.0043);
    assert_eq!(priced(&params), format_to_step_f(below, 0.0001).parse::<f64>().unwrap());
}

/// THE OFF PIN: a SCHEME-LESS instrument formats the limit price EXACTLY as the pre-change
/// scalar path `format_to_step_f(px, tick_size)`. Includes a .5-tie on a 0.5 grid, where
/// truncation (today) and round-half-even DISAGREE — the scheme-less path must TRUNCATE like
/// today (12345.5), catching any accidental unconditional `round_price`.
#[test]
fn schemeless_order_formats_byte_identically_to_today() {
    for (tick, px) in [
        (0.5f64, 12345.3f64),
        (0.5, 12345.75), // .5 tie: truncate → 12345.5, round-half-even → 12346.0
        (0.0001, 0.03330000001), // an option-shaped price but NO scheme → the base/scalar path
        (0.0005, 0.0337),
    ] {
        let p = SymbolProperties {
            tick_size: tick,
            step_size: 0.1,
            min_qty: 0.1,
            ..Default::default()
        };
        assert!(p.tick_scheme.is_none());
        let params = rest(p).build_order_params(&limit_req(px));
        let want = format_to_step_f(px, tick).parse::<f64>().unwrap_or(0.0);
        assert_eq!(priced(&params), want, "tick={tick} px={px}: today's scalar expression");
    }
    // the sharp explicit pin for the tie: TRUNCATION (12345.5), never round-half-even (12346.0)
    let p = SymbolProperties { tick_size: 0.5, ..Default::default() };
    let params = rest(p).build_order_params(&limit_req(12345.75));
    assert_eq!(priced(&params), 12345.5, "scheme-less path truncates like today");
}
