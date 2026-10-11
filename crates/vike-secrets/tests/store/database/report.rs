//! Proof 13 - awkward values round-trip through the store byte for byte.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 13 — AWKWARD values round-trip through the store unchanged
// ---------------------------------------------------------------------------------------------

/// Every value shape a hand-typed credential actually takes: **equals signs inside a value, an
/// empty value, single- and double-quoted values, and leading and trailing spaces.**
///
/// Every one of these is real: an equals sign inside a value is ordinary in a base64 secret, an
/// empty value is a key somebody cleared without deleting, and the quotes and spaces are what a
/// paste carries.
const AWKWARD_VALUES: [(&str, &str); 6] = [
    ("BINANCE_DEMO_API_KEY", "plain"),
    ("BINANCE_DEMO_API_SECRET", "has=equals=signs=="),
    ("BYBIT_DEMO_API_KEY", ""),
    ("BYBIT_DEMO_API_SECRET", "\"double quoted\""),
    ("OKX_DEMO_API_KEY", "'single quoted'"),
    ("OKX_DEMO_API_SECRET", "   spaced out   "),
];

/// **The store holds the bytes it was handed — no trim, no unquoting, no re-encoding.**
///
/// A store whose values were trimmed, unquoted or re-encoded on the way in signs orders with the
/// wrong bytes. The writer and the reader are the production front doors, so this is the round trip
/// a `vike-cli secrets set` and a mount's read make.
#[test]
fn awkward_values_round_trip_through_the_store_unchanged() {
    let fx = Fixture::seeded_with(AWKWARD_VALUES, is_node_key, &classify);

    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the database must answer");
    let got: std::collections::BTreeMap<String, String> =
        resolved.secrets.clone().into_map().into_iter().collect();
    let expected: std::collections::BTreeMap<String, String> =
        AWKWARD_VALUES.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    assert_eq!(got, expected, "a value changed on the way through the database");

    // A rotation replaces exactly the value it names, and every other one stays byte-identical.
    fx.write([("BINANCE_DEMO_API_KEY", "rotated")], is_node_key, &classify);
    let after: std::collections::BTreeMap<String, String> = vike_secrets::resolve_project(fx.arg())
        .expect("read back")
        .secrets
        .into_map()
        .into_iter()
        .collect();
    for (k, v) in AWKWARD_VALUES {
        let want = if k == "BINANCE_DEMO_API_KEY" { "rotated" } else { v };
        assert_eq!(after.get(k).map(String::as_str), Some(want), "{k} after the rotation");
    }
}
