//! `indicators.json` — every built-in indicator and pair indicator, by category.
//! - `indicators.json` — [`fn@vike_indicators::registry`] and [`vike_indicators::pair_registry`], the
//!   built-in rosters (`IndicatorMeta::user` rows never sit in `registry()`, so nothing user-loaded
//!   is exported). Every [`vike_indicators::Category`] variant is rendered, walked off
//!   `Category::ALL` in its order, EMPTY ones included — `User` is the class user-loaded
//!   indicators land in and no built-in row occupies it — because a category that vanished for
//!   having no rows is the silent omission the gate's category-set assertion exists to refuse.

use serde_json::{Value, json};
use vike_indicators::{
    Category, IndicatorMeta, PairMeta, ParamSpec, pair_registry, registry as indicator_registry,
};

/// Kebab-case name of a [`Category`] — the `name` of a category record.
const fn category_name(category: &Category) -> &'static str {
    match category {
        Category::Overlap => "overlap",
        Category::Momentum => "momentum",
        Category::Volatility => "volatility",
        Category::Volume => "volume",
        Category::Statistics => "statistics",
        Category::Pattern => "pattern",
        Category::Price => "price",
        Category::Structure => "structure",
        Category::User => "user",
    }
}

/// One [`ParamSpec`]: the parameter's name, its built-in default, and the range the Studio's
/// sweep grid offers for it. All four numbers are the registry's own `f64`s, rendered as JSON
/// numbers.
fn param_spec_value(spec: &ParamSpec) -> Value {
    json!({
        "name": spec.name,
        "default": spec.default,
        "min": spec.min,
        "max": spec.max,
        "step": spec.step,
    })
}

/// One built-in indicator: registry `name` as `id`, `pretty` as `display`, its parameter surface,
/// and `batch_only` — the flag marking the few indicators whose streaming path cannot equal the
/// batch kernel because the batch reads future bars ([`IndicatorMeta::batch_only`]'s doc).
fn indicator_value(meta: &IndicatorMeta) -> Value {
    json!({
        "id": meta.name,
        "display": meta.pretty,
        "params": meta.params.iter().map(param_spec_value).collect::<Vec<_>>(),
        "batch_only": meta.batch_only,
    })
}

/// One pair indicator ([`PairMeta`]) — the same shape minus `batch_only`, which the pair registry
/// does not carry.
fn pair_value(meta: &PairMeta) -> Value {
    json!({
        "id": meta.name,
        "display": meta.pretty,
        "params": meta.params.iter().map(param_spec_value).collect::<Vec<_>>(),
    })
}

/// The whole `indicators.json` document: `categories` (every `Category::ALL` member, in that
/// order, each with its built-in rows in registry order) and `pairs` (every pair-registry row, in
/// registry order).
#[must_use]
pub fn indicators_value() -> Value {
    let categories: Vec<Value> = Category::ALL
        .iter()
        .map(|category| {
            let name = category_name(category);
            let indicators: Vec<Value> = indicator_registry()
                .iter()
                .filter(|meta| category_name(&meta.category) == name)
                .map(indicator_value)
                .collect();
            json!({ "name": name, "indicators": indicators })
        })
        .collect();
    let pairs: Vec<Value> = pair_registry().iter().map(pair_value).collect();
    json!({ "categories": categories, "pairs": pairs })
}
