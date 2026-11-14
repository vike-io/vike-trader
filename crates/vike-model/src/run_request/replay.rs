//! **Bar or tick: what a backtest replays** — [`ReplayKind`], the ONE type the profile's `[data]
//! kind`, the Studio's data slice and the engine's lane switch name.
//!
//! The profile spells it `kind = "bar"` / `kind = "tick"` (serde lowercase). The Studio wire keeps
//! its own bytes (`Bars` / `Ticks`) in its twin, `vike_backtesthub_client::wire_studio::ReplayKindWire`.
//!
//! ⚠ `vike_backtest::profile_surface` publishes `data.kind`'s roster by PARSING this file
//! ([`REPLAY_SOURCE`], the `crate::risk::surface` shape): keep the enum a flat list of unit variants.

/// Bar or tick replay. Serde-mapped to lowercase TOML strings (`kind = "bar"` / `kind = "tick"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReplayKind {
    /// `kind=bar` series at an interval, replayed through `vike_simulator::StrategyEngine::run`.
    Bar,
    /// The recorded `kind=quote`/`trade`/`book` series, replayed through
    /// `vike_backtest::tick_replay::replay_ticks` (the interval is unused).
    Tick,
}

/// This file's source, verbatim: the exporter several layers up reads [`ReplayKind`]'s roster off it.
pub const REPLAY_SOURCE: &str = include_str!("replay.rs");

/// Repo-relative path of [`REPLAY_SOURCE`], the evidence the exporter cites.
pub const REPLAY_SOURCE_PATH: &str = "crates/vike-model/src/run_request/replay.rs";
