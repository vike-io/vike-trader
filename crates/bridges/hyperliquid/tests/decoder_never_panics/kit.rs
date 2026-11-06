//! The generator kit: Hyperliquid's vocabulary over the shared generator shapes
//! (`vike_bridge_core::json_arb`), plus the field combinators of its own wire.

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::json_arb::PoolKit;

use super::{ENVELOPE_KEYS, STR_POOL, WORDS};

// --- the generator kit ---------------------------------------------------------------------

/// Hyperliquid's vocabulary over the shared generator shapes.
pub(super) const KIT: PoolKit = PoolKit { strs: STR_POOL, words: WORDS };

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
pub(super) fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => KIT.arb_json(ENVELOPE_KEYS)].boxed()
}

/// An integer wire field (epoch-ms stamps, `oid` / `tid` ids): usually a JSON number.
pub(super) fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => KIT.num_text().prop_map(Value::String),
        1 => KIT.arb_leaf(),
    ]
    .boxed()
}

/// A small id (`oid`, `tid`): small numbers so a registry / retire set can match, else anything.
pub(super) fn small_id() -> BoxedStrategy<Value> {
    prop_oneof![6 => (0u64..4).prop_map(|n| Value::Number(n.into())), 2 => jint()].boxed()
}
