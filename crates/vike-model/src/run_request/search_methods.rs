//! `search_methods` — the parameter-search METHOD names a backtest request can carry, and the
//! default. Two consts; they were `vike_datahub_client::proto`'s until the flag vocabulary
//! ([`super::flag_vocab`]) needed the roster for its `--optimizer` row and a vocabulary at layer 10
//! cannot name a layer-25 protocol crate.
//!
//! ⚠ **The wire carries NO const**: `vike_datahub_client::proto::WireSearch.optimizer` is a
//! `String` the operator typed, so moving these changed no byte on the wire. What reads them is the
//! refusal text (`expected grid|euler|tpe|genetic`) and the MCP schema's `enum`.

/// The parameter-search METHODS this protocol can carry, and the ONE roster four surfaces read.
///
/// ⚠ **It lives in the VOCABULARY crate and the ENGINE reads it, which looks inverted and is not.**
/// `vike-cli` (which spelling-checks `--optimizer` before a dial or a spawn) and `vike-backtest`
/// (which implements the methods) both take `vike-model` as a normal dependency, and
/// `vike-datahub-client` (the wire) does too — the shared crate BELOW the sides that must not
/// disagree; `crates/vike-cli/Cargo.toml` carries "no concrete backend, no vike-backtest, no engine
/// crates". `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §5.4 records the
/// three rosters that disagreed before it, and §15.1 requires the gate that now holds it closed.
pub const SEARCH_METHODS: [&str; 4] = ["grid", "euler", "tpe", "genetic"];

/// The method a search runs when none is named: the exhaustive product. The first row of
/// [`SEARCH_METHODS`], and `vike_backtest::harness::sweep::GridSearch`'s own `Optimizer::name`.
pub const DEFAULT_SEARCH_METHOD: &str = "grid";
