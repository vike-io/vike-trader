//! Unified cross-venue attribution (task 6): [`binance_broker_order_client_id`]/[`strip_broker_order_client_id_prefix`]
//! are the encode/decode pair for Binance's Broker/Link `newClientOrderId` prefix. No link id ⇒
//! byte-identical (bare order client id); a configured link id prefixes `newClientOrderId` on BOTH builders
//! when the whole id fits 35, and an id it would push past 35 goes BARE, never cut.
use super::{
    binance_broker_order_client_id, build_perp_order_params, build_spot_order_params,
    strip_broker_order_client_id_prefix,
};
use vike_model::{OrderRequest, SymbolProperties};

/// The longest admitted broker code (12) with a 20-character v2 id: 35, the budget exactly.
const CODE_12: &str = "ABCDEFGHIJKL";
const ID_20: &str = "S00A3k3x9q2m7d1CSTP9";
/// A real 8-character Link ID with the longest full v2 id (27): 38 > 35.
const CODE_8: &str = "ABCD1234";
const ID_27: &str = "S00A307k3x9q2m7d1p4CSTP9R99";

#[test]
fn broker_order_client_id_prefixes_what_fits_35_and_sends_the_rest_bare() {
    // no link id → unchanged
    assert_eq!(binance_broker_order_client_id(None, "abc123"), "abc123");
    // an id that fits gets today's bytes
    assert_eq!(binance_broker_order_client_id(Some("ABC123"), "deadbeef01"), "x-ABC123-deadbeef01");
    let at_budget = binance_broker_order_client_id(Some(CODE_12), ID_20);
    assert_eq!(at_budget, "x-ABCDEFGHIJKL-S00A3k3x9q2m7d1CSTP9");
    assert_eq!(at_budget.len(), 35);
    // over the budget: the id itself, whole
    assert_eq!(binance_broker_order_client_id(Some(CODE_8), ID_27), ID_27, "38 > 35 goes bare");
    let one_over = format!("{ID_20}X");
    assert_eq!(
        binance_broker_order_client_id(Some(CODE_12), &one_over),
        one_over,
        "36 > 35 goes bare"
    );
    let long = "z".repeat(40);
    assert_eq!(binance_broker_order_client_id(Some("ABC123"), &long), long, "never cut");
}

/// Two ids the old cut-to-36 made one (`x-ABCDEFGHIJKL-` + their shared first 21 characters) stay
/// two: each goes out whole.
#[test]
fn ids_the_old_cut_merged_stay_distinct() {
    let a = "S00A307k3x9q2m7d1p4OLEN";
    let b = "S00A307k3x9q2m7d1p4OLSL";
    assert_eq!(&a[..21], &b[..21], "the old 36-character cut kept only these 21");
    let (wa, wb) = (
        binance_broker_order_client_id(Some(CODE_12), a),
        binance_broker_order_client_id(Some(CODE_12), b),
    );
    assert_ne!(wa, wb);
    assert_eq!((wa.as_str(), wb.as_str()), (a, b));
}

/// The inverse recovers the bare order client id exactly, round-tripping through both answers of the encode
/// side, and the encode side answers the same every time (a cancel re-derives the stored id).
#[test]
fn strip_is_the_inverse_of_both_answers() {
    let prefixed = binance_broker_order_client_id(Some("ABC123"), "deadbeef01");
    assert_eq!(strip_broker_order_client_id_prefix(&prefixed, Some("ABC123")), "deadbeef01");
    // no link id: encode is a no-op, decode is a no-op too
    let bare = binance_broker_order_client_id(None, "deadbeef01");
    assert_eq!(strip_broker_order_client_id_prefix(&bare, None), "deadbeef01");
    for (code, id) in [(CODE_12, ID_20), (CODE_8, ID_27)] {
        let wire = binance_broker_order_client_id(Some(code), id);
        assert_eq!(strip_broker_order_client_id_prefix(&wire, Some(code)), id, "{code} {id}");
        assert_eq!(binance_broker_order_client_id(Some(code), id), wire, "re-derived: {code} {id}");
    }
}

/// A hyphenated link id (legal: `validate_code`'s `OrderClientIdPrefix` arm checks length only, not
/// charset) must still round-trip: the prefix is matched whole, so its own `-` is no separator.
#[test]
fn a_hyphenated_link_id_round_trips() {
    let prefixed = binance_broker_order_client_id(Some("AB-12"), "deadbeef01");
    assert_eq!(prefixed, "x-AB-12-deadbeef01");
    assert_eq!(strip_broker_order_client_id_prefix(&prefixed, Some("AB-12")), "deadbeef01");
}

/// **V8 (owner ruling 2026-11-12): an id is OURS only when its prefix carries OUR link id.** With
/// `LINK` configured `x-LINK-abc123` is our `abc123`; another broker's `x-OTHER-abc123`, and a link id
/// that only begins like ours, stay exactly as received; with NO link id configured every `x-...` id
/// stays as received; our own unprefixed ids are unchanged either way.
#[test]
fn only_an_id_under_our_own_link_id_is_ours() {
    assert_eq!(strip_broker_order_client_id_prefix("x-LINK-abc123", Some("LINK")), "abc123");
    assert_eq!(
        strip_broker_order_client_id_prefix("x-OTHER-abc123", Some("LINK")),
        "x-OTHER-abc123"
    );
    assert_eq!(
        strip_broker_order_client_id_prefix("x-LINKS-abc123", Some("LINK")),
        "x-LINKS-abc123"
    );
    assert_eq!(strip_broker_order_client_id_prefix("x-LINK-stop1", None), "x-LINK-stop1");
    assert_eq!(strip_broker_order_client_id_prefix("x-OTHER-abc123", None), "x-OTHER-abc123");
    assert_eq!(strip_broker_order_client_id_prefix("abc123", Some("LINK")), "abc123");
    assert_eq!(strip_broker_order_client_id_prefix("abc123", None), "abc123");
}

/// A string that never carried the prefix (no link id configured, or Aster's always-bare order client ids)
/// passes through the decode side unchanged — no false-positive strip.
#[test]
fn strip_is_a_no_op_on_bare_ids() {
    assert_eq!(strip_broker_order_client_id_prefix("deadbeef01", None), "deadbeef01");
    assert_eq!(strip_broker_order_client_id_prefix("", None), "");
    // starts with "x-" but no second '-': not our shape, left alone
    assert_eq!(strip_broker_order_client_id_prefix("x-onlyone", None), "x-onlyone");
}

/// Both builders stamp the prefixed id into `newClientOrderId` when a link id is configured;
/// absent, the wire byte is exactly the bare order client id (byte-identical to before this field existed).
#[test]
fn build_order_params_apply_link_id_to_new_order_client_id() {
    let properties = SymbolProperties::default();
    let req = OrderRequest {
        order_client_id: "deadbeef01".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.01,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    let ncoid = |params: &[(&'static str, String)]| {
        params
            .iter()
            .find(|(k, _)| *k == "newClientOrderId")
            .map(|(_, v)| v.clone())
            .expect("newClientOrderId present")
    };

    let spot_none = build_spot_order_params(&req, "BTCUSDT", &properties, "binance", None);
    assert_eq!(ncoid(&spot_none), "deadbeef01", "no link id: bare order client id, byte-identical");
    let spot_some =
        build_spot_order_params(&req, "BTCUSDT", &properties, "binance", Some("ABC123"));
    assert_eq!(ncoid(&spot_some), "x-ABC123-deadbeef01");

    let perp_none = build_perp_order_params(&req, "BTCUSDT", &properties, "binance-perp", None);
    assert_eq!(ncoid(&perp_none), "deadbeef01", "no link id: bare order client id, byte-identical");
    let perp_some =
        build_perp_order_params(&req, "BTCUSDT", &properties, "binance-perp", Some("ABC123"));
    assert_eq!(ncoid(&perp_some), "x-ABC123-deadbeef01");

    // an id the prefix would push past 35: bare on both lanes
    let long = OrderRequest { order_client_id: ID_27.to_string(), ..req };
    let spot_long = build_spot_order_params(&long, "BTCUSDT", &properties, "binance", Some(CODE_8));
    assert_eq!(ncoid(&spot_long), ID_27);
    let perp_long =
        build_perp_order_params(&long, "BTCUSDT", &properties, "binance-perp", Some(CODE_8));
    assert_eq!(ncoid(&perp_long), ID_27);
}

/// [`super::json_id`], the one reader of an id the venue sends as a JSON number or a string (an
/// `orderId`, and the perp mapper's non-string `ps`): a string as itself, a number as its digits,
/// anything else empty.
#[test]
fn json_id_reads_a_string_or_a_number_and_nothing_else() {
    use serde_json::json;
    assert_eq!(super::json_id(&json!("BOTH")), "BOTH");
    assert_eq!(super::json_id(&json!(8_675_309)), "8675309");
    assert_eq!(super::json_id(&json!(true)), "");
    assert_eq!(super::json_id(&json!(null)), "");
}
