//! Unified cross-venue attribution (task 6): [`binance_broker_coid`]/[`strip_broker_coid_prefix`]
//! are the encode/decode pair for Binance's Broker/Link `newClientOrderId` prefix. No link id ⇒
//! byte-identical (bare coid); a configured link id prefixes `newClientOrderId` on BOTH builders,
//! bounded to the venue's 36-char ceiling.
use super::{
    binance_broker_coid, build_perp_order_params, build_spot_order_params, strip_broker_coid_prefix,
};
use vike_model::{OrderRequest, SymbolProperties};

#[test]
fn broker_coid_prefixes_and_bounds_36() {
    // no link id → unchanged
    assert_eq!(binance_broker_coid(None, "abc123"), "abc123");
    // with link id → x-<id>-<coid>, and the whole thing stays <= 36
    let c = binance_broker_coid(Some("ABC123"), "deadbeef01");
    assert!(c.starts_with("x-ABC123-"));
    assert!(c.len() <= 36, "newClientOrderId must fit 36, got {}", c.len());
    // an over-long coid is truncated to keep the prefix and the 36 bound
    let long = binance_broker_coid(Some("ABC123"), &"z".repeat(40));
    assert!(long.starts_with("x-ABC123-"));
    assert_eq!(long.len(), 36);
}

/// The inverse recovers the bare coid exactly, round-tripping through the encode side.
#[test]
fn strip_is_the_inverse_of_prefix_for_untruncated_ids() {
    let prefixed = binance_broker_coid(Some("ABC123"), "deadbeef01");
    assert_eq!(strip_broker_coid_prefix(&prefixed), "deadbeef01");
    // no link id: encode is a no-op, decode is a no-op too
    let bare = binance_broker_coid(None, "deadbeef01");
    assert_eq!(strip_broker_coid_prefix(&bare), "deadbeef01");
}

/// A hyphenated link id (legal: `validate_code`'s `CoidPrefix` arm checks length only, not
/// charset) must still round-trip — the separator is the LAST `-`, not the first, since the
/// coid half is always hyphen-free.
#[test]
fn strip_uses_last_hyphen_so_a_hyphenated_link_id_round_trips() {
    let prefixed = binance_broker_coid(Some("AB-12"), "deadbeef01");
    assert_eq!(prefixed, "x-AB-12-deadbeef01");
    assert_eq!(strip_broker_coid_prefix(&prefixed), "deadbeef01");
}

/// A string that never carried the prefix (no link id configured, or Aster's always-bare coids)
/// passes through the decode side unchanged — no false-positive strip.
#[test]
fn strip_is_a_no_op_on_bare_ids() {
    assert_eq!(strip_broker_coid_prefix("deadbeef01"), "deadbeef01");
    assert_eq!(strip_broker_coid_prefix(""), "");
    // starts with "x-" but no second '-': not our shape, left alone
    assert_eq!(strip_broker_coid_prefix("x-onlyone"), "x-onlyone");
}

/// Both builders stamp the prefixed id into `newClientOrderId` when a link id is configured;
/// absent, the wire byte is exactly the bare coid (byte-identical to before this field existed).
#[test]
fn build_order_params_apply_link_id_to_new_client_order_id() {
    let props = SymbolProperties::default();
    let req = OrderRequest {
        client_order_id: "deadbeef01".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.01,
        order_type: "market".to_string(),
        ..Default::default()
    };
    let ncoid = |params: &[(&'static str, String)]| {
        params
            .iter()
            .find(|(k, _)| *k == "newClientOrderId")
            .map(|(_, v)| v.clone())
            .expect("newClientOrderId present")
    };

    let spot_none = build_spot_order_params(&req, "BTCUSDT", &props, "binance", None);
    assert_eq!(ncoid(&spot_none), "deadbeef01", "no link id: bare coid, byte-identical");
    let spot_some = build_spot_order_params(&req, "BTCUSDT", &props, "binance", Some("ABC123"));
    assert_eq!(ncoid(&spot_some), "x-ABC123-deadbeef01");

    let perp_none = build_perp_order_params(&req, "BTCUSDT", &props, "binance-perp", None);
    assert_eq!(ncoid(&perp_none), "deadbeef01", "no link id: bare coid, byte-identical");
    let perp_some =
        build_perp_order_params(&req, "BTCUSDT", &props, "binance-perp", Some("ABC123"));
    assert_eq!(ncoid(&perp_some), "x-ABC123-deadbeef01");
}
