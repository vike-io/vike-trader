//! **Bar or tick: what a backtest replays** — [`ReplayKind`], the ONE type the profile's `[data]
//! kind`, the Studio's data slice and the engine's lane switch name.
//!
//! The profile spells it `kind = "bar"` / `kind = "tick"` (serde lowercase). The Studio wire keeps
//! its own bytes (`Bars` / `Ticks`) in its twin, `vike_backtesthub_client::wire_studio::ReplayKindWire`.
//!
//! ⚠ `vike_backtest::run::profile_surface` publishes `data.kind`'s roster from the `JsonSchema` derive
//! (this crate's `profile-schema` feature): a variant added here is a value `profile.json` gains.

/// Bar or tick replay. Serde-mapped to lowercase TOML strings (`kind = "bar"` / `kind = "tick"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ReplayKind {
    /// `kind=bar` series at an interval, replayed through `vike_simulator::StrategyEngine::run`.
    Bar,
    /// The recorded `kind=quote`/`trade`/`book` series, replayed through
    /// `vike_backtest::run::tick_replay::replay_ticks` (the interval is unused).
    Tick,
}

// The words are the serde tags above (`kind = "bar"`). IMPL-ONLY, so the serde attributes stay on the
// literal `pub enum` block.
vike_utility::word_enum! {
    impl ReplayKind {
        Bar => "bar",
        Tick => "tick",
    }
}
