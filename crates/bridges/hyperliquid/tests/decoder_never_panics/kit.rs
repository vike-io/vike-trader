//! The generator kit: hostile JSON leaves, field-keyed random JSON and the field combinators.

use proptest::prelude::*;
use serde_json::{Map, Value};

use super::{ENVELOPE_KEYS, STR_POOL, WORDS};

// --- the generator kit ---------------------------------------------------------------------

pub(super) fn arb_str() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(STR_POOL).prop_map(String::from),
        4 => prop::sample::select(WORDS).prop_map(String::from),
        1 => any::<i64>().prop_map(|n| n.to_string()),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        2 => "[ -~]{0,12}",
        1 => any::<String>(),
    ]
    .boxed()
}

/// A JSON leaf: null, bool, i64 extremes / zero / negative, u64, f64, hostile strings, and the
/// empty array / object.
pub(super) fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => prop::sample::select(vec![0i64, 1, -1, i64::MAX, i64::MIN]).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        5 => arb_str().prop_map(Value::String),
        1 => Just(Value::Array(Vec::new())),
        1 => Just(Value::Object(Map::new())),
    ]
    .boxed()
}

fn arb_key(keys: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(keys).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Random JSON, depth <= 4, object keys drawn mostly from `keys`.
pub(super) fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(keys), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
pub(super) fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(ENVELOPE_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
pub(super) fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop_oneof![
        8 => prop::sample::select(set).prop_map(|s| Value::String(s.to_string())),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// Decimal text a numeric read would parse (mostly), else hostile text.
fn num_text() -> BoxedStrategy<String> {
    prop_oneof![
        4 => (-1_000_000i64..1_000_000).prop_map(|n| n.to_string()),
        4 => (-1.0e6f64..1.0e6).prop_map(|f| format!("{f:.6}")),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        1 => arb_str(),
    ]
    .boxed()
}

fn json_f64(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A numeric wire field: Hyperliquid sends decimal STRINGS (so usually a string), sometimes a JSON
/// number or any leaf.
pub(super) fn numv() -> BoxedStrategy<Value> {
    prop_oneof![
        7 => num_text().prop_map(Value::String),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(json_f64),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// An integer wire field (epoch-ms stamps, `oid` / `tid` ids): usually a JSON number.
pub(super) fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A small id (`oid`, `tid`): small numbers so a registry / retire set can match, else anything.
pub(super) fn small_id() -> BoxedStrategy<Value> {
    prop_oneof![6 => (0u64..4).prop_map(|n| Value::Number(n.into())), 2 => jint()].boxed()
}

/// An object over exactly the named fields; each is present with p = 0.85 and drawn from its own
/// (mostly well-typed) strategy.
pub(super) fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
    fields
        .into_iter()
        .map(|(k, s)| prop::option::weighted(0.85, s).prop_map(move |v| (k, v)))
        .collect::<Vec<_>>()
        .prop_map(|kvs| {
            let mut m = Map::new();
            for (k, v) in kvs {
                if let Some(v) = v {
                    m.insert(k.to_string(), v);
                }
            }
            Value::Object(m)
        })
        .boxed()
}

pub(super) fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
pub(super) fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}
