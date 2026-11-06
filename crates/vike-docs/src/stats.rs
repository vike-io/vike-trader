//! `stats.json` — roster sizes, the latency-gate budget and the generator identity.
//!
//! `latency_p99_budget_ns` is `vike_core::latency_budget::P99_BUDGET_NS` itself — the very
//! constant `crates/vike-core/tests/runtime_latency.rs` asserts the baseline variant against — read
//! from the library, not copied. What is exported is the BUDGET (the gate's ceiling), never a
//! measurement. `event_count` is the length of `vike_model::events::Event::TABLE`.

use serde_json::{Value, json};
use vike_core::latency_budget::P99_BUDGET_NS;
use vike_data::store::store_kind::STORE_KINDS;
use vike_exec::recon::DivergenceKind;
use vike_indicators::{pair_registry, registry as indicator_registry};
use vike_model::events::Event;
use vike_model::{AssetClass, VENUES};
use vike_strategy::{PORTABLE_STRATEGIES, SCRIPT_ONLY, SIMULATOR_ONLY};
use vike_venues::venue_caps::ORDER_KINDS;

/// Version stamp for the OUTPUT SHAPE of every file this module renders, carried in `stats.json`.
/// Bump it when a field is renamed/removed or its meaning changes; adding a field — or a whole
/// file, as `indicators.json` and `templates.json` were — is compatible and does not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// The whole `stats.json` document. `generated_from` is the caller's — the commit this render
/// describes, resolved by [`generated_from`](crate::generated_from) from the bin's argv; see
/// [`DEFAULT_GENERATED_FROM`](crate::DEFAULT_GENERATED_FROM) for why it is a parameter and not
/// baked in.
#[must_use]
pub fn stats_value(generated_from: &str) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "venue_count": VENUES.len(),
        "indicator_count": indicator_registry().len(),
        "pair_indicator_count": pair_registry().len(),
        "portable_strategy_count": PORTABLE_STRATEGIES.len(),
        "simulator_only_strategy_count": SIMULATOR_ONLY.len(),
        "script_only_strategy_count": SCRIPT_ONLY.len(),
        "event_count": Event::TABLE.len(),
        "store_kind_count": STORE_KINDS.len(),
        "order_kind_count": ORDER_KINDS.len(),
        "asset_class_count": AssetClass::ALL.len(),
        "divergence_kind_count": DivergenceKind::ALL.len(),
        "latency_p99_budget_ns": P99_BUDGET_NS,
        "generator_version": env!("CARGO_PKG_VERSION"),
        "generated_from": generated_from,
    })
}
