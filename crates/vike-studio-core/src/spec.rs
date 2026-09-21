//! What a Studio Run executes — the ONE strategy vocabulary the whole Run pipeline (`run.rs`)
//! branches on, so "which kind of strategy is this" is decided in exactly one place instead of at
//! each of the four runner entry points.
//!
//! Two sources exist today:
//!
//! - [`StrategySpec::Rhai`] — the editor buffer, compiled through `vike_script::RhaiStrategy`
//!   (the original and, until this module, only path).
//! - [`StrategySpec::Native`] — a Rust strategy from the backtest harness registry
//!   (`vike_backtest::harness::registry`), resolved by NAME plus a `toml::Value` params table.
//!
//! **Why params are a bare `toml::Value` and not a spec type.** There is no `ParamSpec`/`make_with`
//! mechanism in `vike-backtest`: each registered strategy reads its knobs ad hoc off a
//! `&toml::Value` (`BuyHold::from_params` is the convention — and five of the six registered
//! strategies ignore params entirely). Inventing a parallel description layer here would be a
//! second source of truth that no strategy is obliged to honor, and it would go stale the moment a
//! strategy grows a knob. So the Studio edits params as free-form `(key, value)` text rows and
//! serializes them with [`params_from_rows`] — future-proof by construction (any strategy's
//! params are expressible the day it lands, with no change here), at the cost of no type-ahead.
//! If a real spec seam ever lands in `vike-backtest`, it can drive the same rows.

use toml::Value;

use vike_backtest::harness::registry::STRATEGIES;

/// Every native strategy name the Studio can run — the backtest harness registry's own roster
/// (`vike_backtest::harness::registry::STRATEGIES`), re-exported so `vike-studio` never has to
/// name the harness module and a strategy added to the registry appears in the Studio for free.
pub fn native_strategies() -> &'static [&'static str] {
    STRATEGIES
}

/// The strategy a Run/Sweep/Walk-Forward/Compare executes.
#[derive(Debug, Clone, PartialEq)]
pub enum StrategySpec {
    /// Rhai source text, compiled per run (per sweep point, with that point's overrides).
    Rhai(String),
    /// A [`native_strategies`] name plus the `strategy.params` table it reads its knobs from.
    Native { name: String, params: Value },
}

impl StrategySpec {
    /// A Rhai spec over the given source.
    pub fn rhai(src: impl Into<String>) -> Self {
        StrategySpec::Rhai(src.into())
    }

    /// A native spec: a registry `name` plus its params table.
    pub fn native(name: impl Into<String>, params: Value) -> Self {
        StrategySpec::Native { name: name.into(), params }
    }

    /// A native spec with an EMPTY params table (every registered strategy's params reader
    /// tolerates missing keys — `BuyHold::from_params` defaults `size` to `1.0`).
    pub fn native_default(name: impl Into<String>) -> Self {
        StrategySpec::native(name, empty_params())
    }

    /// A native spec whose params table is given as its **TOML text** — the wire idiom the
    /// `vike-datahub` `RunSlice` verb (PR-3) uses. Params ride the wire as TOML TEXT, exactly like
    /// `RunBacktest` ships the whole profile as text, so a `toml::Value` never needs serde on the
    /// wire; the server parses the text back into the params table HERE, at the boundary.
    ///
    /// Empty/whitespace text is an empty params table (every registry `from_params` tolerates
    /// missing keys). A malformed table is returned as a `toml::de::Error` so the caller can surface
    /// it (the `RunSlice` server maps it to a `RunError::Strategy`).
    pub fn native_from_toml_str(
        name: impl Into<String>,
        params_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        let params = if params_toml.trim().is_empty() {
            empty_params()
        } else {
            toml::from_str::<Value>(params_toml)?
        };
        Ok(StrategySpec::native(name, params))
    }

    /// A short human label for this spec — what the Studio's toolbar/compare table shows.
    pub fn label(&self) -> String {
        match self {
            StrategySpec::Rhai(_) => "rhai".to_string(),
            StrategySpec::Native { name, .. } => name.clone(),
        }
    }
}

impl Default for StrategySpec {
    /// An empty Rhai script — the pre-native default, so a `StrategySpec`-shaped field added to an
    /// existing struct keeps that struct's `Default` meaning "the Rhai path".
    fn default() -> Self {
        StrategySpec::Rhai(String::new())
    }
}

/// An empty `strategy.params` table (a TOML table, NOT `Value::String("")`) — the shape every
/// registry `from_params` reader expects when nothing is configured.
pub fn empty_params() -> Value {
    Value::Table(toml::map::Map::new())
}

/// Build a `strategy.params` table from the Studio's free-form `(key, value-text)` editor rows.
///
/// Each row's value text is parsed as a TOML scalar so `3`, `2.5`, `true` and `"BTCUSDT"` all reach
/// the strategy as the TOML type the author meant (`BuyHold::from_params` accepts an integer OR a
/// float for `size`, and reads `symbol` as a string). Text that is NOT valid TOML on its own — the
/// common case of an unquoted symbol, `BTCUSDT` — falls back to a plain string, so the editor never
/// forces the user to remember TOML quoting for the overwhelmingly common case.
///
/// Rows with a blank key are skipped (an empty trailing row in the editor is not a param). A
/// duplicate key keeps the LAST row, matching how a TOML document would resolve it.
pub fn params_from_rows(rows: &[(String, String)]) -> Value {
    let mut table = toml::map::Map::new();
    for (key, text) in rows {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        table.insert(key.to_string(), parse_scalar(text));
    }
    Value::Table(table)
}

/// Parse one free-form value cell into a TOML scalar, falling back to a bare string.
///
/// Parsed as a one-key DOCUMENT (`v = <text>`) rather than `text.parse::<Value>()`: the latter is a
/// document parse in `toml` too, so `2.5` alone is NOT a valid document and would never round-trip.
fn parse_scalar(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::String(String::new());
    }
    match toml::from_str::<Value>(&format!("v = {trimmed}")) {
        Ok(doc) => doc.get("v").cloned().unwrap_or_else(|| Value::String(trimmed.to_string())),
        Err(_) => Value::String(trimmed.to_string()),
    }
}

/// `base` with each `(name, value)` override applied on top — how a sweep point specializes a
/// native strategy's params. A non-table `base` (never produced by [`params_from_rows`], but a
/// caller could hand one in) is replaced by a fresh table rather than silently dropping the
/// overrides.
pub fn params_with_overrides(base: &Value, overrides: &[(String, f64)]) -> Value {
    let mut table = base.as_table().cloned().unwrap_or_default();
    for (name, value) in overrides {
        table.insert(name.clone(), Value::Float(*value));
    }
    Value::Table(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_strategies_is_the_harness_registry_roster() {
        assert_eq!(native_strategies(), STRATEGIES);
        assert!(native_strategies().contains(&"buy_hold"));
    }

    #[test]
    fn params_from_rows_types_each_scalar() {
        let rows = vec![
            ("size".to_string(), "3".to_string()),
            ("rate".to_string(), "2.5".to_string()),
            ("flag".to_string(), "true".to_string()),
            ("quoted".to_string(), "\"ETHUSDT\"".to_string()),
            ("bare".to_string(), "BTCUSDT".to_string()),
        ];
        let v = params_from_rows(&rows);
        assert_eq!(v.get("size").and_then(toml::Value::as_integer), Some(3));
        assert_eq!(v.get("rate").and_then(toml::Value::as_float), Some(2.5));
        assert_eq!(v.get("flag").and_then(toml::Value::as_bool), Some(true));
        assert_eq!(v.get("quoted").and_then(toml::Value::as_str), Some("ETHUSDT"));
        assert_eq!(
            v.get("bare").and_then(toml::Value::as_str),
            Some("BTCUSDT"),
            "an unquoted symbol must not be a parse error — it is a string"
        );
    }

    #[test]
    fn params_from_rows_skips_blank_keys_and_keeps_the_last_duplicate() {
        let rows = vec![
            ("   ".to_string(), "9".to_string()),
            ("size".to_string(), "1".to_string()),
            ("size".to_string(), "2".to_string()),
        ];
        let v = params_from_rows(&rows);
        assert_eq!(v.as_table().unwrap().len(), 1);
        assert_eq!(v.get("size").and_then(toml::Value::as_integer), Some(2));
    }

    #[test]
    fn empty_params_is_a_table_not_a_string() {
        assert!(empty_params().as_table().is_some());
        assert!(params_from_rows(&[]).as_table().unwrap().is_empty());
    }

    #[test]
    fn params_with_overrides_layers_on_top_of_the_base() {
        let base = params_from_rows(&[
            ("size".to_string(), "1".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
        ]);
        let v = params_with_overrides(&base, &[("size".to_string(), 4.0)]);
        assert_eq!(v.get("size").and_then(toml::Value::as_float), Some(4.0));
        assert_eq!(
            v.get("symbol").and_then(toml::Value::as_str),
            Some("BTCUSDT"),
            "an un-swept key must survive the override"
        );
    }

    #[test]
    fn spec_labels_and_default() {
        assert_eq!(StrategySpec::rhai("fn on_bar() {}").label(), "rhai");
        assert_eq!(StrategySpec::native_default("buy_hold").label(), "buy_hold");
        assert_eq!(StrategySpec::default(), StrategySpec::Rhai(String::new()));
    }

    /// The `RunSlice` wire idiom (PR-3): params arrive as TOML TEXT and are parsed back into the
    /// same table `params_from_rows`/`native` would build. Empty text is an empty table; a bad
    /// table is a parse error (not a panic).
    #[test]
    fn native_from_toml_str_parses_params_text() {
        let spec =
            StrategySpec::native_from_toml_str("buy_hold", "size = 2\nsymbol = \"BTCUSDT\"\n")
                .expect("valid params text");
        match spec {
            StrategySpec::Native { name, params } => {
                assert_eq!(name, "buy_hold");
                assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(2));
                assert_eq!(params.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
            }
            other => panic!("expected Native, got {other:?}"),
        }

        // empty/whitespace text -> an empty params table (not a parse error)
        let empty = StrategySpec::native_from_toml_str("buy_hold", "   ").expect("empty is ok");
        assert_eq!(empty, StrategySpec::native_default("buy_hold"));

        // a malformed table is a parse error, surfaced (not a panic)
        assert!(StrategySpec::native_from_toml_str("buy_hold", "size = = 2").is_err());
    }
}
