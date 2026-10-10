//! "Arbitrary input never panics" harness for every PUBLIC FXCM wire decoder. This crate has no
//! socket: its wire is the C++ shim's JSON — the async order-event envelopes `fc_poll_event`
//! drains, and the Orders / Trades table snapshots the reconcile client reads — and that JSON is
//! the only thing a hostile or corrupted shim can hand the Rust side.
//!
//! - `vike_fxcm::event_mapper::{map_fxcm_event, map_drained_event}` — one envelope
//!   (`fill` / `canceled` / `rejected`), routed through the in-memory order table;
//! - `vike_fxcm::event_mapper::{lots_for, preflight_request, map_placement, from_fxcm_instrument,
//!   is_market, closes_routing, ends_cancelability}` — the sizing / preflight / placement pure layer;
//! - `vike_fxcm::recon_client::{parse_orders, parse_positions, parse_fills, normalize_order_type,
//!   normalize_order_status}` — the reconcile table-snapshot parsers.
//!
//! The module tree is NOT feature-gated (`fxcm = []` only decides whether `build.rs` tries to
//! compile the C++ shim), so this file compiles and runs identically in every build, SDK or not.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed envelope may decode to nothing (or
//! an `Err`), but it must never panic the session thread (a dead thread is a venue that silently
//! stops filling), and it must never fabricate an event flood. Each decoder is fed (a) arbitrary
//! text, (b) lossy-decoded byte noise and (c) structured JSON whose object keys are the shim's REAL
//! envelope / table-column names, so the generator reaches the branches instead of bouncing off the
//! first `.get()`. Outputs are asserted only against the cheap bounds that must always hold.
//!
//! The private decoders (`sys::redact_login_text`, the `RouteTable` the drain loop keeps) are
//! covered by the sibling unit-test files `src/sys_props.rs` and `src/exec_props.rs`.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.

use std::collections::HashMap;

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_fxcm::event_mapper::{
    closes_routing, ends_cancelability, from_fxcm_instrument, is_market, lots_for,
    map_drained_event, map_fxcm_event, map_placement, preflight_request,
};
use vike_fxcm::recon_client::{
    normalize_order_status, normalize_order_type, parse_fills, parse_orders, parse_positions,
};
use vike_model::events::Event;

/// The shim's real envelope keys and Trades / Orders table columns.
const KEYS: &[&str] = &[
    "kind",
    "order_id",
    "trade_id",
    "instrument",
    "side",
    "amount",
    "rate",
    "commission",
    "ts",
    "reason",
    "buysell",
    "type",
    "status",
    "open_rate",
];

/// Real dispatch values: envelope kinds, sides / `buysell` codes, order type / status codes, the
/// instruments and venue order ids the tests route on.
const WORDS: &[&str] = &[
    "fill", "canceled", "rejected", "B", "S", "b", "s", "LE", "SE", "STE", "L", "W", "F", "C", "R",
    "EUR/USD", "EURUSD", "XAU/USD", "O1", "O2", "T1", "",
];

/// The (symbol the parsers are mounted with) values.
const SYMBOLS: &[&str] = &["EURUSD", "XAUUSD", ""];

/// Every f64 class, NaN and the infinities included (`any::<f64>()` alone yields only finite ones).
fn arb_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        8 => any::<f64>(),
        1 => Just(f64::NAN),
        1 => Just(f64::INFINITY),
        1 => Just(f64::NEG_INFINITY),
        1 => Just(f64::MIN_POSITIVE),
        1 => Just(f64::MAX),
    ]
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(0i64), Just(-1), Just(i64::MAX), Just(i64::MIN), any::<i64>()]
            .prop_map(Value::from),
        Just(Value::from(u64::MAX)),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        prop_oneof![Just(1.0e300), Just(-1.0e300), Just(1.0e-300), Just(0.0), Just(-0.0)]
            .prop_map(Value::from),
        prop::sample::select(vec![
            "NaN",
            "inf",
            "-0",
            "1e999",
            "",
            " ",
            ".",
            "9999999999999999999999999999999999999999",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        "-?[0-9]{1,30}(\\.[0-9]{1,6})?".prop_map(Value::String),
        prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
        any::<String>().prop_map(Value::String),
        Just(Value::Array(Vec::new())),
        Just(Value::Object(Map::new())),
    ]
}

/// Arbitrary JSON, depth <= 4, object keys drawn mostly from the real field names.
fn arb_json() -> impl Strategy<Value = Value> {
    let key = prop_oneof![
        8 => prop::sample::select(KEYS).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ];
    arb_leaf().prop_recursive(4, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key.clone(), inner), 0..8)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// Raw bytes -> text the way a lossy buffer read would produce it.
fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn set(mut v: Value, key: &str, child: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert(key.to_string(), child);
    }
    v
}

/// An object over exactly `keys`, each present with high probability and holding a hostile leaf.
fn shaped(keys: &'static [&'static str]) -> impl Strategy<Value = Value> {
    prop::collection::vec(prop::option::weighted(0.85, arb_leaf()), keys.len()..=keys.len())
        .prop_map(move |vals| {
            Value::Object(
                keys.iter()
                    .zip(vals)
                    .filter_map(|(k, v)| v.map(|v| ((*k).to_string(), v)))
                    .collect(),
            )
        })
}

/// A value drawn from the shim's real dispatch words five times in six, a hostile leaf otherwise.
fn mostly(words: &'static [&'static str]) -> impl Strategy<Value = Value> {
    prop_oneof![
        5 => prop::sample::select(words).prop_map(|s| Value::String(s.to_string())),
        1 => arb_leaf(),
    ]
}

/// An async order-event envelope: `kind` and `order_id` mostly real (the ids the tests route on),
/// the rest hostile — `amount` positive most of the time so the fill arm survives its guard.
fn arb_envelope() -> impl Strategy<Value = Value> {
    let amount = prop_oneof![
        5 => (1u32..10_000_000).prop_map(Value::from),
        1 => arb_leaf(),
    ];
    (
        shaped(&["trade_id", "rate", "commission", "ts", "reason"]),
        mostly(&["fill", "canceled", "rejected"]),
        mostly(&["O1", "O2"]),
        mostly(&["EUR/USD", "XAU/USD", "EURUSD"]),
        mostly(&["B", "S"]),
        amount,
    )
        .prop_map(|(base, kind, order_id, instrument, side, amount)| {
            let v = set(base, "kind", kind);
            let v = set(v, "order_id", order_id);
            let v = set(v, "instrument", instrument);
            let v = set(v, "side", side);
            set(v, "amount", amount)
        })
}

/// One Orders / Trades table row.
fn arb_row() -> impl Strategy<Value = Value> {
    (
        shaped(&["order_id", "trade_id", "amount", "open_rate", "commission"]),
        mostly(&["EUR/USD", "XAU/USD", "EURUSD"]),
        mostly(&["B", "S"]),
        mostly(&["LE", "SE", "STE", "L", "S"]),
        mostly(&["W", "F", "C", "R"]),
    )
        .prop_map(|(base, instrument, buysell, ty, status)| {
            let v = set(base, "instrument", instrument);
            let v = set(v, "buysell", buysell);
            let v = set(v, "type", ty);
            set(v, "status", status)
        })
}

/// A table snapshot: a JSON array of rows.
fn arb_table() -> impl Strategy<Value = Value> {
    prop::collection::vec(arb_row(), 0..6).prop_map(Value::Array)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // ---- async order-event envelope ---------------------------------------------------------

    /// `map_fxcm_event` is total over arbitrary JSON; one envelope emits at most the dual-publish
    /// pair, and a bare `Fill` is always followed by its `OrderFilled` wrap.
    #[test]
    fn envelope_decoder_survives_structured_json(v in arb_json(), coid in any::<String>()) {
        let out = map_fxcm_event(&v, &coid);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        prop_assert_eq!(
            out.iter().filter(|e| matches!(e, Event::Fill(_))).count(),
            out.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count()
        );
    }

    #[test]
    fn envelope_decoder_survives_shaped_envelopes(v in arb_envelope(), coid in any::<String>()) {
        let out = map_fxcm_event(&v, &coid);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
    }

    /// ...and over whatever text parses as JSON, the way `drain_events` feeds it.
    #[test]
    fn envelope_decoder_survives_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let out = map_fxcm_event(&v, "c");
            prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        }
    }

    /// (c) The drain loop's shape: a run of envelopes through ONE routing table, a route dropped
    /// when an envelope closes it (`closes_routing`), exactly as `drain_events` does.
    #[test]
    fn drained_envelopes_survive_a_sequence_through_one_routing_table(
        envelopes in prop::collection::vec(prop_oneof![arb_envelope(), arb_json()], 1..10),
        routed in prop::collection::vec(prop::sample::select(vec!["O1", "O2", "O3", ""]), 0..4),
    ) {
        let mut routes: HashMap<String, String> =
            routed.iter().map(|id| ((*id).to_string(), format!("coid-{id}"))).collect();
        for v in &envelopes {
            let evs = map_drained_event(v, &routes);
            prop_assert!(evs.len() <= 2, "event flood: {} events", evs.len());
            let _ = ends_cancelability(&evs);
            if closes_routing(&evs) {
                let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
                routes.remove(oid);
            }
        }
    }

    // ---- pure layer -------------------------------------------------------------------------

    /// `lots_for` is total over every f64 pair (NaN, infinities, subnormals included); an accepted
    /// size is at least one lot.
    #[test]
    fn lots_for_survives_any_floats(qty in arb_f64(), base in arb_f64()) {
        if let Ok(lots) = lots_for(qty, base) {
            prop_assert!(lots >= 1, "{lots} lots from qty {qty} / base {base}");
        }
    }

    /// ...and over the realistic grid: whole multiples, near-multiples, huge lot counts.
    #[test]
    fn lots_for_survives_grid_shaped_floats(
        base in prop_oneof![Just(1.0), Just(1000.0), Just(0.1), 1.0e-9f64..1.0e9],
        lots in prop_oneof![0u64..100, Just(u64::from(u32::MAX)), Just(1u64 << 40), any::<u64>()],
        jitter in prop_oneof![Just(0.0), Just(0.5), -1.0e-9f64..1.0e-9],
    ) {
        let qty = lots as f64 * base + jitter;
        let _ = lots_for(qty, base);
    }

    /// A priced non-market request is ALWAYS refused; everything else defers to `lots_for`.
    #[test]
    fn preflight_refuses_every_priced_resting_request(
        order_type in prop_oneof![prop::sample::select(vec!["market", "MARKET", "limit", "stop", ""]).prop_map(String::from), any::<String>()],
        qty in arb_f64(),
        price in prop::option::of(arb_f64()),
        base in arb_f64(),
    ) {
        let out = preflight_request(&order_type, qty, price, base);
        if !is_market(&order_type) && price.is_some() {
            prop_assert!(out.is_err());
        }
    }

    /// A placement outcome is always exactly one event, and a failure always names a reason.
    #[test]
    fn placement_mapper_is_one_event_with_a_reason(
        coid in any::<String>(),
        ts in any::<i64>(),
        ok in any::<bool>(),
        text in any::<String>(),
    ) {
        let outcome = if ok { Ok(text.as_str()) } else { Err(text.as_str()) };
        let out = map_placement(&coid, ts, outcome);
        prop_assert_eq!(out.len(), 1);
        if !ok {
            prop_assert!(matches!(&out[0], Event::OrderRejected(r) if !r.reason.is_empty()));
        }
    }

    #[test]
    fn symbol_and_code_helpers_survive_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        let _ = from_fxcm_instrument(&text);
        let _ = is_market(&text);
        let _ = normalize_order_type(&text);
        let _ = normalize_order_status(&text);
    }

    // ---- reconcile table snapshots ----------------------------------------------------------

    /// The three snapshot parsers are total over arbitrary text and byte noise.
    #[test]
    fn recon_parsers_survive_arbitrary_text(
        text in prop_oneof![any::<String>(), arb_noise()],
        symbol in prop::sample::select(SYMBOLS),
    ) {
        let _ = parse_orders(&text, symbol);
        let _ = parse_positions(&text, symbol);
        let _ = parse_fills(&text, symbol);
    }

    /// ...and over arbitrary JSON.
    #[test]
    fn recon_parsers_survive_structured_json(v in arb_json(), symbol in prop::sample::select(SYMBOLS)) {
        let body = v.to_string();
        let _ = parse_orders(&body, symbol);
        let _ = parse_positions(&body, symbol);
        let _ = parse_fills(&body, symbol);
    }

    /// ...and over table snapshots with hostile cells: a position query always answers one row,
    /// the others never invent rows.
    #[test]
    fn recon_parsers_survive_shaped_tables(table in arb_table(), symbol in prop::sample::select(SYMBOLS)) {
        let n = table.as_array().map_or(0, Vec::len);
        let body = table.to_string();
        let positions = parse_positions(&body, symbol).expect("a JSON array");
        prop_assert_eq!(positions.len(), 1);
        prop_assert!(parse_orders(&body, symbol).expect("a JSON array").len() <= n);
        prop_assert!(parse_fills(&body, symbol).expect("a JSON array").len() <= n);
    }
}
