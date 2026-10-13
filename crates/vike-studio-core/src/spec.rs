//! What a Studio Run executes — the ONE strategy vocabulary the whole Run pipeline (`run.rs`)
//! branches on, so "which kind of strategy is this" is decided in exactly one place instead of at
//! each of the four runner entry points.
//!
//! Three sources exist today:
//!
//! - [`StrategySpec::Rhai`] — the editor buffer, compiled through `vike_script::RhaiStrategy`
//!   (the original and, until this module, only path).
//! - [`StrategySpec::Native`] — a Rust strategy from the backtest harness registry
//!   (`vike_backtest::harness::registry`), resolved by NAME plus a [`Params`] table.
//! - [`StrategySpec::Plugin`] — a Rust strategy loaded at RUNTIME from an already-built `.so`,
//!   named by its sha256 rather than looked up in any compiled-in registry. It carries no code of
//!   its own (unlike Rhai's `String` and unlike Native's registry lookup, which resolves through a
//!   crate that also compiles Rhai) — see `run.rs`'s `build_strategy` for where the boxed strategy
//!   actually gets built once the loader is wired, and `crates/vike-compute/src/compute_server.rs`
//!   for the scope argument this shape settles.
//!
//! **Why params are a free-form [`Params`] table and not a spec type.** There is no
//! `ParamSpec`/`make_with` mechanism in `vike-backtest`: each registered strategy reads its knobs
//! ad hoc off a `&Params` (`BuyHold::from_params` is the convention — and five of the six
//! registered strategies ignore params entirely). Inventing a parallel description layer here would
//! be a second source of truth that no strategy is obliged to honor, and it would go stale the
//! moment a strategy grows a knob. So the Studio edits params as free-form `(key, value)` text rows
//! and types them with [`params_from_rows`] — future-proof by construction (any strategy's params
//! are expressible the day it lands, with no change here), at the cost of no type-ahead. If a real
//! spec seam ever lands in `vike-backtest`, it can drive the same rows.
//!
//! TOML stays at the TEXT edges only: the wire's `params_toml` string is read back here
//! ([`StrategySpec::native_from_toml_str`]), and a plugin's `create` receives TOML text rendered by
//! `crates/vike-studio-core/src/run/strategy.rs`'s `build_strategy_in`.

use vike_backtest::harness::registry::STRATEGIES;
use vike_config::params_text::parse_scalar;
use vike_model::params::{ParamValue, Params};

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
    Native { name: String, params: Params },
    /// A runtime-loaded Rust strategy: the plugin's declared `name`, the sha256 of the built
    /// artifact it names (NOT a lookup key into any compiled-in registry), and the params table its
    /// `create` reads — the same free-form shape [`StrategySpec::Native`] uses, because there is no
    /// `ParamSpec` seam here either.
    Plugin { name: String, sha: String, params: Params },
}

impl StrategySpec {
    /// A Rhai spec over the given source.
    pub fn rhai(src: impl Into<String>) -> Self {
        StrategySpec::Rhai(src.into())
    }

    /// A native spec: a registry `name` plus its params table.
    pub fn native(name: impl Into<String>, params: Params) -> Self {
        StrategySpec::Native { name: name.into(), params }
    }

    /// A native spec with an EMPTY params table (every registered strategy's params reader
    /// tolerates missing keys — `BuyHold::from_params` defaults `size` to `1.0`).
    pub fn native_default(name: impl Into<String>) -> Self {
        StrategySpec::native(name, empty_params())
    }

    /// A native spec whose params table is given as its **TOML text** — the wire idiom the
    /// `vike-datahub` `RunSlice` verb (PR-3) uses. Params ride the wire as TOML TEXT, exactly like
    /// `RunBacktest` ships the whole profile as text, so the params table never needs serde on the
    /// wire; the server reads the text back into the params table HERE, at the boundary.
    ///
    /// Empty/whitespace text is an empty params table (every registry `from_params` tolerates
    /// missing keys). A malformed table — or a value [`ParamValue`] has no kind for, such as a TOML
    /// datetime — is returned as a `toml::de::Error` so the caller can surface it (the `RunSlice`
    /// server maps it to a `RunError::Strategy`).
    pub fn native_from_toml_str(
        name: impl Into<String>,
        params_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        Ok(StrategySpec::native(name, params_from_toml_str(params_toml)?))
    }

    /// A plugin spec: a declared `name`, the sha256 of the built artifact, and its params table.
    pub fn plugin(name: impl Into<String>, sha: impl Into<String>, params: Params) -> Self {
        StrategySpec::Plugin { name: name.into(), sha: sha.into(), params }
    }

    /// A plugin spec whose params table is given as its **TOML text** — the wire idiom, mirroring
    /// [`StrategySpec::native_from_toml_str`]. Empty/whitespace text is an empty params table; a
    /// malformed table is a `toml::de::Error` the caller surfaces rather than a panic.
    pub fn plugin_from_toml_str(
        name: impl Into<String>,
        sha: impl Into<String>,
        params_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        Ok(StrategySpec::plugin(name, sha, params_from_toml_str(params_toml)?))
    }

    /// A short human label for this spec — what the Studio's toolbar/compare table shows.
    pub fn label(&self) -> String {
        match self {
            StrategySpec::Rhai(_) => "rhai".to_string(),
            StrategySpec::Native { name, .. } => name.clone(),
            StrategySpec::Plugin { name, .. } => name.clone(),
        }
    }

    /// The params table a sweep point's overrides land on: Native's and Plugin's own, `None` for
    /// Rhai, whose overrides ride as script `param()` bindings and never enter a table. The Studio
    /// reads it where it makes a sweep axis, to type a knob this table holds as an `Int` as one.
    pub fn base_params(&self) -> Option<&Params> {
        match self {
            StrategySpec::Rhai(_) => None,
            StrategySpec::Native { params, .. } | StrategySpec::Plugin { params, .. } => {
                Some(params)
            }
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

/// The wire's params TEXT → the params table: empty/whitespace text is the empty table, anything
/// else a TOML document read straight into [`Params`] by serde.
fn params_from_toml_str(params_toml: &str) -> Result<Params, toml::de::Error> {
    if params_toml.trim().is_empty() {
        Ok(empty_params())
    } else {
        toml::from_str::<Params>(params_toml)
    }
}

/// An empty `strategy.params` table — the shape every registry `from_params` reader expects when
/// nothing is configured.
pub fn empty_params() -> Params {
    Params::new()
}

/// Build a `strategy.params` table from the Studio's free-form `(key, value-text)` editor rows.
///
/// Each row's value text is read by `vike_config::params_text`'s `parse_scalar`, the ONE scalar
/// parser, so `3`, `2.5`, `true` and `"BTCUSDT"` all reach the strategy as the kind the author
/// meant (`BuyHold::from_params` accepts an integer OR a float for `size`, and reads `symbol` as a
/// string). Text that is NOT valid TOML on its own — the common case of an unquoted symbol,
/// `BTCUSDT` — falls back to a plain string, so the editor never forces the user to remember TOML
/// quoting for the overwhelmingly common case.
///
/// Rows with a blank key are skipped (an empty trailing row in the editor is not a param). A
/// duplicate key keeps the LAST row, matching how a TOML document would resolve it.
pub fn params_from_rows(rows: &[(String, String)]) -> Params {
    let mut table = Params::new();
    for (key, text) in rows {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        table.insert(key, parse_scalar(text));
    }
    table
}

/// `base` with each `(name, value)` override applied on top — how a sweep point specializes a
/// native or plugin strategy's params. Each override is written AS GIVEN: its kind was decided
/// where the sweep axis was made (`vike_studio`'s `start_sweep`, which types a knob the base table
/// holds as an `Int` as an `Int`), and it crossed the wire as that kind
/// (`vike_datahub_client::wire_studio::WireParamscan`'s `axes`). So a swept INTEGER knob reaches an
/// `as_integer` reader (`rungs`, `exit_delay_ms`, `cooldown_ms`) as an integer — the defect
/// `docs/decisions/0083-the-runtime-plugin-join-lands.md` recorded.
pub fn params_with_overrides(base: &Params, overrides: &[(String, ParamValue)]) -> Params {
    let mut table = base.clone();
    for (name, value) in overrides {
        table.insert(name.clone(), value.clone());
    }
    table
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
        assert_eq!(v.get("size").and_then(ParamValue::as_integer), Some(3));
        assert_eq!(v.get("rate").and_then(ParamValue::as_float), Some(2.5));
        assert_eq!(v.get("flag").and_then(ParamValue::as_bool), Some(true));
        assert_eq!(v.get("quoted").and_then(ParamValue::as_str), Some("ETHUSDT"));
        assert_eq!(
            v.get("bare").and_then(ParamValue::as_str),
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
        assert_eq!(v.len(), 1);
        assert_eq!(v.get("size").and_then(ParamValue::as_integer), Some(2));
    }

    #[test]
    fn empty_params_is_an_empty_table() {
        assert!(empty_params().is_empty());
        assert!(params_from_rows(&[]).is_empty());
    }

    #[test]
    fn params_with_overrides_layers_on_top_of_the_base() {
        let base = params_from_rows(&[
            ("size".to_string(), "1.0".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
        ]);
        let v = params_with_overrides(&base, &[("size".to_string(), ParamValue::Float(4.0))]);
        assert_eq!(v.get("size").and_then(ParamValue::as_float), Some(4.0));
        assert_eq!(
            v.get("symbol").and_then(ParamValue::as_str),
            Some("BTCUSDT"),
            "an un-swept key must survive the override"
        );
    }

    /// Decision 0083's recorded defect: a swept knob the axis carries as an INTEGER is written as an
    /// integer, so an `as_integer` reader sees it, and a `Float` stays a `Float` even at an integral
    /// value. The kind is decided where the axis is made (`vike_studio`'s `start_sweep`, whose
    /// `a_sweep_axis_is_an_int_only_on_an_integer_knob` pins the rule); this table writes it as
    /// given.
    #[test]
    fn a_swept_integer_knob_stays_an_integer() {
        let base = params_from_rows(&[
            ("rungs".to_string(), "4".to_string()),
            ("size".to_string(), "1.5".to_string()),
        ]);
        let v = params_with_overrides(
            &base,
            &[
                ("rungs".to_string(), ParamValue::Int(6)),
                ("size".to_string(), ParamValue::Float(2.0)),
            ],
        );
        assert_eq!(v.get("rungs"), Some(&ParamValue::Int(6)), "an Int knob keeps its kind");
        assert_eq!(v.get("rungs").and_then(ParamValue::as_integer), Some(6));
        assert_eq!(
            v.get("size"),
            Some(&ParamValue::Float(2.0)),
            "a Float knob stays a Float, even at an integral value"
        );
    }

    #[test]
    fn spec_labels_and_default() {
        assert_eq!(StrategySpec::rhai("fn on_bar() {}").label(), "rhai");
        assert_eq!(StrategySpec::native_default("buy_hold").label(), "buy_hold");
        assert_eq!(
            StrategySpec::plugin("my_strat", "a".repeat(64), empty_params()).label(),
            "my_strat"
        );
        assert_eq!(StrategySpec::default(), StrategySpec::Rhai(String::new()));
    }

    /// The plugin wire idiom (mirrors `native_from_toml_str_parses_params_text`): params arrive as
    /// TOML text and the sha rides verbatim — a truncated sha would name a different artifact.
    #[test]
    fn plugin_from_toml_str_parses_params_text_and_keeps_the_sha_whole() {
        let sha = "b".repeat(64);
        let spec = StrategySpec::plugin_from_toml_str("my_strat", sha.clone(), "fast = 5\n")
            .expect("valid params text");
        match spec {
            StrategySpec::Plugin { name, sha: got_sha, params } => {
                assert_eq!(name, "my_strat");
                assert_eq!(got_sha, sha);
                assert_eq!(params.get("fast").and_then(ParamValue::as_integer), Some(5));
            }
            other => panic!("expected Plugin, got {other:?}"),
        }

        let empty = StrategySpec::plugin_from_toml_str("my_strat", sha.clone(), "   ")
            .expect("empty is ok");
        assert_eq!(empty, StrategySpec::plugin("my_strat", sha, empty_params()));

        assert!(StrategySpec::plugin_from_toml_str("my_strat", "a".repeat(64), "x = = 1").is_err());
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
                assert_eq!(params.get("size").and_then(ParamValue::as_integer), Some(2));
                assert_eq!(params.get("symbol").and_then(ParamValue::as_str), Some("BTCUSDT"));
            }
            other => panic!("expected Native, got {other:?}"),
        }

        // empty/whitespace text -> an empty params table (not a parse error)
        let empty = StrategySpec::native_from_toml_str("buy_hold", "   ").expect("empty is ok");
        assert_eq!(empty, StrategySpec::native_default("buy_hold"));

        // a malformed table is a parse error, surfaced (not a panic)
        assert!(StrategySpec::native_from_toml_str("buy_hold", "size = = 2").is_err());
    }

    /// The text edge round-trips: the TOML text the client renders (`toml::to_string(&Params)`,
    /// `vike_studio::backend::remote`'s `to_wire_spec`) reads back into the identical table here.
    #[test]
    fn params_toml_text_round_trips_through_the_wire_reader() {
        let params = params_from_rows(&[
            ("rungs".to_string(), "4".to_string()),
            ("size".to_string(), "2.0".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
            ("venues".to_string(), "{ BTCUSDT = \"binance\" }".to_string()),
        ]);
        let text = toml::to_string(&params).expect("a params table renders");
        let back = StrategySpec::native_from_toml_str("grid", &text).expect("and reads back");
        assert_eq!(back, StrategySpec::native("grid", params));
    }
}
