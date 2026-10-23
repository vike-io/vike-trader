//! Writing a `vike_config::RunProfile` into the live core's configuration: the glue between the
//! DOCUMENT (the profile, owned by `vike-config`) and the RUNTIME (`vike_core::CoreConfig`,
//! `vike_core::JournalConfig`, `vike_model::MarginCallConfig`).
//!
//! It lived in `vike-core`'s `run_profile` beside the schema until decision 0157. The schema moved
//! down to `vike-config` (a settings document, off the latency-gated crate), and what has to name a
//! core or exec type stayed up here, where the mapping is still compile-checked against the real
//! types:
//!
//! - [`apply_guards_and_sinks`] overwrites only the `CoreConfig` fields a profile NAMES and returns a
//!   [`GuardsReport`] for the caller to disclose (a [`ConfirmGraceHazard`] when the stuck-order
//!   watchdog's pairing breaks its hard lower bound).
//! - [`send_initial_trading_state`] is the one `[guards]` key that is not a `CoreConfig` field:
//!   `guards.initial_trading_state` is sent to the spawned core as a `SetTradingState` command,
//!   never over a stricter state the journal restored ([`initial_trading_state_to_send`] decides).
//! - [`journal_config_from`] builds the write-ahead journal's config from the `config.journal_dir`
//!   row, the cadence row and the profile's `[sinks.journal]` tuning ([`tune_journal`]).
//!
//! `[guards.margin_call]` needs no converter: the profile parses it as `vike_model::MarginCallConfig`,
//! the engine's own type, and [`apply_guards_and_sinks`] copies it into `CoreConfig::margin_call`.
//!
//! `consumption_gate` is the gate that keeps `[guards]` and `[sinks]` from drifting back into
//! declared-but-inert: it destructures both structs exhaustively, so a new field is a compile error
//! until it is classified. Each file's own `//!` and function docs carry the arguments.

mod confirm_grace;
mod initial_state;
mod journal_env;

pub use confirm_grace::{ConfirmGraceHazard, GuardsReport, apply_guards_and_sinks};
pub use initial_state::{
    initial_trading_state_to_send, restored_trading_states, send_initial_trading_state,
};
pub use journal_env::{journal_config_from, tune_journal};

#[cfg(test)]
use confirm_grace::{confirm_budget, confirm_grace_clears_bound};
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use vike_config::run_profile::{Guards, Sinks, samples};
#[cfg(test)]
use vike_config::{ProfileTradingState, RunProfile};

#[cfg(test)]
mod consumption_gate;

#[cfg(test)]
mod glue_tests;
