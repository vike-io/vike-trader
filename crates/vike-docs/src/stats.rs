//! `stats.json` — roster sizes, the latency-gate budget and the generator identity.
//!
//! `latency_p99_budget_ns` is `vike_execution::latency_budget::P99_BUDGET_NS` itself — the very
//! constant `crates/vike-core/tests/runtime_latency.rs` asserts the baseline variant against — read
//! from the library, not copied. What is exported is the BUDGET (the gate's ceiling), never a
//! measurement. `event_count` is the length of `vike_model::events::Event::TABLE`. The three
//! strategy counts are the lengths of `templates.json`'s rosters over the same shipped files.

use serde_json::{Value, json};
use vike_data::store::store_kind::STORE_KINDS;
use vike_execution::latency_budget::P99_BUDGET_NS;
use vike_indicators::{pair_registry, registry as indicator_registry};
use vike_model::events::Event;
use vike_model::{AssetClass, OrderType, ReconcileDivergenceKind, VENUES};
use vike_strategy::file::StrategyFileHeader;

use crate::templates::is_simulator_only;

/// Version stamp for the OUTPUT SHAPE of every file this module renders, carried in `stats.json`.
/// Bump it when a field is renamed/removed or its meaning changes; adding a field — or a whole
/// file, as `indicators.json` and `templates.json` were — is compatible and does not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// The whole `stats.json` document. `generated_from` is the caller's — the commit this render
/// describes, resolved by [`generated_from`](crate::generated_from) from the bin's argv; see
/// [`DEFAULT_GENERATED_FROM`](crate::DEFAULT_GENERATED_FROM) for why it is a parameter and not
/// baked in. `strategies` is [`shipped_strategies`](crate::shipped_strategies)'s answer.
#[must_use]
pub fn stats_value(generated_from: &str, strategies: &[(String, StrategyFileHeader)]) -> Value {
    let simulator_only = strategies.iter().filter(|(_, header)| is_simulator_only(header)).count();
    json!({
        "schema_version": SCHEMA_VERSION,
        "venue_count": VENUES.len(),
        "indicator_count": indicator_registry().len(),
        "pair_indicator_count": pair_registry().len(),
        "portable_strategy_count": strategies.len() - simulator_only,
        "simulator_only_strategy_count": simulator_only,
        // No file is the by-name inline-script arm this counted (see `templates`' module doc).
        "script_only_strategy_count": 0,
        "event_count": Event::TABLE.len(),
        "store_kind_count": STORE_KINDS.len(),
        "order_kind_count": OrderType::KNOWN.len(),
        "asset_class_count": AssetClass::ALL.len(),
        "divergence_kind_count": ReconcileDivergenceKind::ALL.len(),
        "latency_p99_budget_ns": P99_BUDGET_NS,
        "generator_version": env!("CARGO_PKG_VERSION"),
        "generated_from": generated_from,
    })
}
