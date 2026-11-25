//! What a Studio Run executes — the ONE strategy vocabulary the whole Run pipeline
//! (`vike_backtesthub::studio_run`, on the backtest server)
//! branches on, so "which kind of strategy is this" is decided in exactly one place instead of at
//! each of the four runner entry points.
//!
//! Three sources exist today:
//!
//! - [`StrategySpec::Rhai`] — the editor buffer, compiled through `vike_rhai::RhaiStrategy`
//!   (the original path; its text stays internal to the run, a buffer is not a file), or
//!   [`StrategySpec::RhaiWithParameters`], the same buffer at a `strategy.params` table.
//! - [`StrategySpec::File`] — a strategy FILE of the backtest server's own `strategies/` folder,
//!   named by its path under that folder plus a [`Params`] table: a `.rhai` file runs in the server's
//!   process, a `.rs` file in a sandboxed child. No strategy is compiled into the server, so nothing
//!   is resolved by a name.
//! - [`StrategySpec::StrategyFile`] — a Rust strategy loaded at RUNTIME from an already-built `.so`,
//!   named by its sha256 rather than looked up in any compiled-in registry. It carries no code of
//!   its own (unlike Rhai's `String`) — see `crates/vike-backtesthub/src/studio_run/strategy.rs`'s
//!   `build_strategy` for where the boxed strategy
//!   actually gets built once the loader is wired, and `crates/vike-backtesthub/src/backtesthub_server.rs`
//!   for the scope argument this shape settles.
//!
//! **Why params are a free-form [`Params`] table and not a spec type.** A strategy file declares
//! its keys in its own header (`vike_strategy::file`), and that header is the one description of
//! them; a parallel description layer here would be a second source of truth. So the Studio edits
//! params as free-form `(key, value)` text rows and types them with [`params_from_rows`], at the
//! cost of no type-ahead. A header-driven editor can drive the same rows.
//!
//! TOML stays at the TEXT edges only: the wire's `params_toml` and `parameters_toml` strings are read
//! back here ([`StrategySpec::file_from_toml_str`], [`StrategySpec::rhai_from_toml_str`]), and a strategy file's `create` receives TOML text rendered by
//! `crates/vike-backtesthub/src/studio_run/strategy.rs`'s `loaded_file_builder`.

use vike_config::params_text::parse_scalar;
#[cfg(any(doc, test))]
use vike_model::params::ParamValue;
use vike_model::params::Params;

/// The strategy a Run/Sweep/Walk-Forward/Compare executes.
#[derive(Debug, Clone, PartialEq)]
pub enum StrategySpec {
    /// Rhai source text, compiled per run (per sweep point, with that point's overrides).
    Rhai(String),
    /// [`StrategySpec::Rhai`] run at a `strategy.params` table (2026-11-20): the script's `param()` values and the
    /// run's `commission*`/`slippage*` costs, applied as a [`StrategySpec::File`]'s table is (a sweep point's overrides
    /// land on the same table). Its own variant because the wire keeps `Rhai`'s bytes a bare string.
    RhaiWithParameters { source: String, parameters: Params },
    /// A strategy file of the server's `strategies/` folder, by its path under that folder
    /// (`trend/momentum.rs`), plus the `strategy.params` table it reads its knobs from, and the SDK a
    /// `.rs` file is built against over its own pin (`None`: as the file says; decision 0296).
    File { file: String, params: Params, sdk: Option<u32> },
    /// A runtime-loaded Rust strategy: the strategy file's declared `name`, the sha256 of the built
    /// artifact it names (NOT a lookup key into any compiled-in registry), and the params table its
    /// `create` reads — the same free-form shape [`StrategySpec::File`] uses.
    StrategyFile { name: String, sha: String, params: Params },
}

impl StrategySpec {
    /// A Rhai spec over the given source.
    pub fn rhai(src: impl Into<String>) -> Self {
        StrategySpec::Rhai(src.into())
    }

    /// A Rhai buffer at the table given as its **TOML text**, the wire idiom ([`Self::file_from_toml_str`]'s
    /// rules: a malformed table is an error): a [`StrategySpec::RhaiWithParameters`], or the bare
    /// [`StrategySpec::Rhai`] for an empty table, which is that buffer's run.
    pub fn rhai_from_toml_str(
        source: impl Into<String>,
        parameters_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        let (source, parameters) = (source.into(), params_from_toml_str(parameters_toml)?);
        Ok(if parameters.is_empty() {
            StrategySpec::Rhai(source)
        } else {
            StrategySpec::RhaiWithParameters { source, parameters }
        })
    }

    /// A file spec: the file's path under the server's `strategies/` folder plus its params table.
    pub fn file(file: impl Into<String>, params: Params) -> Self {
        StrategySpec::File { file: file.into(), params, sdk: None }
    }

    /// This spec with its file built against `sdk` (`None`: as the file says). Only a
    /// [`StrategySpec::File`] has an SDK to name: any other spec comes back unchanged.
    #[must_use]
    pub fn with_sdk(self, sdk: Option<u32>) -> Self {
        match self {
            StrategySpec::File { file, params, .. } => StrategySpec::File { file, params, sdk },
            other => other,
        }
    }

    /// A file spec whose params table is given as its **TOML text** — the wire idiom the
    /// `RunSlice` verb uses. Params ride the wire as TOML TEXT, exactly like `RunBacktest` ships the
    /// whole profile as text, so the params table never needs serde on the wire; the server reads
    /// the text back into the params table HERE, at the boundary.
    ///
    /// Empty/whitespace text is an empty params table. A malformed table — or a value
    /// [`ParamValue`] has no kind for, such as a TOML datetime — is returned as a `toml::de::Error`
    /// so the caller can surface it (the `RunSlice` server maps it to a `RunError::Strategy`).
    pub fn file_from_toml_str(
        file: impl Into<String>,
        params_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        Ok(StrategySpec::file(file, params_from_toml_str(params_toml)?))
    }

    /// A strategy file spec: a declared `name`, the sha256 of the built artifact, and its params table.
    pub fn strategy_file(name: impl Into<String>, sha: impl Into<String>, params: Params) -> Self {
        StrategySpec::StrategyFile { name: name.into(), sha: sha.into(), params }
    }

    /// A strategy file spec whose params table is given as its **TOML text** — the wire idiom, mirroring
    /// [`StrategySpec::file_from_toml_str`]. Empty/whitespace text is an empty params table; a
    /// malformed table is a `toml::de::Error` the caller surfaces rather than a panic.
    pub fn strategy_file_from_toml_str(
        name: impl Into<String>,
        sha: impl Into<String>,
        params_toml: &str,
    ) -> Result<Self, toml::de::Error> {
        Ok(StrategySpec::strategy_file(name, sha, params_from_toml_str(params_toml)?))
    }

    /// A short human label for this spec — what the Studio's toolbar/compare table shows.
    pub fn label(&self) -> String {
        match self {
            StrategySpec::Rhai(_) | StrategySpec::RhaiWithParameters { .. } => "rhai".to_string(),
            StrategySpec::File { file, .. } => file.clone(),
            StrategySpec::StrategyFile { name, .. } => name.clone(),
        }
    }

    /// The params table a sweep point's overrides land on: a file's, a built strategy file's and a Rhai buffer's
    /// with parameters own, `None` for a bare Rhai buffer, whose overrides ride as script `param()` bindings alone.
    /// The Studio reads it where it makes a sweep axis, to type a knob this table holds as an `Int` as one.
    pub fn base_params(&self) -> Option<&Params> {
        match self {
            StrategySpec::Rhai(_) => None,
            StrategySpec::File { params, .. }
            | StrategySpec::StrategyFile { params, .. }
            | StrategySpec::RhaiWithParameters { parameters: params, .. } => Some(params),
        }
    }
}

impl Default for StrategySpec {
    /// An empty Rhai script — the first default, so a `StrategySpec`-shaped field added to an
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn spec_labels_and_default() {
        assert_eq!(StrategySpec::rhai("fn on_bar() {}").label(), "rhai");
        assert_eq!(
            StrategySpec::file("trend/buy_hold.rs", empty_params()).label(),
            "trend/buy_hold.rs"
        );
        assert_eq!(
            StrategySpec::strategy_file("my_strat", "a".repeat(64), empty_params()).label(),
            "my_strat"
        );
        assert_eq!(StrategySpec::default(), StrategySpec::Rhai(String::new()));
    }

    /// The strategy file wire idiom (mirrors `file_from_toml_str_parses_params_text`): params arrive as
    /// TOML text and the sha rides verbatim — a truncated sha would name a different artifact.
    #[test]
    fn strategy_file_from_toml_str_parses_params_text_and_keeps_the_sha_whole() {
        let sha = "b".repeat(64);
        let spec = StrategySpec::strategy_file_from_toml_str("my_strat", sha.clone(), "fast = 5\n")
            .expect("valid params text");
        match spec {
            StrategySpec::StrategyFile { name, sha: got_sha, params } => {
                assert_eq!(name, "my_strat");
                assert_eq!(got_sha, sha);
                assert_eq!(params.get("fast").and_then(ParamValue::as_integer), Some(5));
            }
            other => panic!("expected Strategy file, got {other:?}"),
        }

        let empty = StrategySpec::strategy_file_from_toml_str("my_strat", sha.clone(), "   ")
            .expect("empty is ok");
        assert_eq!(empty, StrategySpec::strategy_file("my_strat", sha, empty_params()));

        assert!(
            StrategySpec::strategy_file_from_toml_str("my_strat", "a".repeat(64), "x = = 1")
                .is_err()
        );
    }

    /// The `RunSlice` wire idiom (PR-3): params arrive as TOML TEXT and are parsed back into the
    /// same table `params_from_rows`/`file` would build. Empty text is an empty table; a bad
    /// table is a parse error (not a panic).
    #[test]
    fn file_from_toml_str_parses_params_text() {
        let file = "trend/buy_hold.rs";
        let spec = StrategySpec::file_from_toml_str(file, "size = 2\nsymbol = \"BTCUSDT\"\n")
            .expect("valid params text");
        match spec {
            StrategySpec::File { file: got, params, sdk } => {
                assert_eq!(got, file);
                assert_eq!(sdk, None, "a file builds as it says until a run names an SDK");
                assert_eq!(params.get("size").and_then(ParamValue::as_integer), Some(2));
                assert_eq!(params.get("symbol").and_then(ParamValue::as_str), Some("BTCUSDT"));
            }
            other => panic!("expected File, got {other:?}"),
        }

        // empty/whitespace text -> an empty params table (not a parse error)
        let empty = StrategySpec::file_from_toml_str(file, "   ").expect("empty is ok");
        assert_eq!(empty, StrategySpec::file(file, empty_params()));

        // a malformed table is a parse error, surfaced (not a panic)
        assert!(StrategySpec::file_from_toml_str(file, "size = = 2").is_err());
    }

    /// The text edge round-trips: the TOML text the client renders (`toml::to_string(&Params)`,
    /// `vike_studio::backend::backtesthub`'s `to_wire_spec`) reads back into the identical table here.
    #[test]
    fn params_toml_text_round_trips_through_the_wire_reader() {
        let params = params_from_rows(&[
            ("rungs".to_string(), "4".to_string()),
            ("size".to_string(), "2.0".to_string()),
            ("symbol".to_string(), "BTCUSDT".to_string()),
            ("venues".to_string(), "{ BTCUSDT = \"binance\" }".to_string()),
        ]);
        let text = toml::to_string(&params).expect("a params table renders");
        let back =
            StrategySpec::file_from_toml_str("grid_dca/grid.rs", &text).expect("and reads back");
        assert_eq!(back, StrategySpec::file("grid_dca/grid.rs", params));
    }
}
