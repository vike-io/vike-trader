//! vike-backtest — the backtest HARNESS over the `vike-simulator` simulator.
//!
//! A [`run::profile::ProfileBacktest`] (TOML) names a data slice, engine costs and a strategy FILE; the
//! harness loads the slice from a `HistoryStore` ([`run::tick_replay`] for ticks), runs it
//! through `vike_simulator::StrategyEngine`, and composes the result into a report. Around that: parameter
//! sweeps and searches ([`search::paramscan`], [`search`] with its methods, [`search::trial_ledger`]),
//! walk-forward ([`walkforward`], its combinatorial purged form included) and Monte Carlo
//! permutation tests ([`permutation`]).
//!
//! ⚠ **This is a plain LIBRARY, with no feature and no DataFusion.** The backtesthub, the
//! `backtest` CLI and the Polymarket research bins that used to sit here (`backtesthub_server`,
//! `backtest_cli`, `research`, `starter`, `binutil`, `bin/`) are `vike-backtesthub`'s now (layer 65,
//! beside the other daemons), together with the `datafusion-store` and `venue-fetch` features that
//! existed only to keep their weight out of this crate's consumers. The concrete store engine
//! (`vike_store::HistoryStoreDataFusion`) reaches this crate as a DEV dependency alone.
//!
//! ⚠ **The SIMULATOR is not in this crate any more.** `engine` (`StrategyEngine`/`OrderBrokerSimulator`),
//! `vector_engine`, `impact`, `latency`, `queue_model`, `schedule`, `timeframe`, `bar_buffer` and
//! `ref_strategies` left for `vike-simulator` (layer 25) —
//! docs/decisions/0087-the-simulator-leaves-the-harness.md. Spell them `vike_simulator::…`, from inside
//! this crate as well as outside it: nothing here re-exports them.
//!
//! ⚠ **Nothing from another crate is re-exported here either**, and that is the other half of the
//! same change. This file used to carry `pub use` blocks for eight `vike_analytics` modules, four
//! `vike_fills` modules and their flat names, `TradingRecord`, and a "backward compatibility"
//! `vike_model::{Strategy, consolidate_quotes, consolidate_trades}` — each one a second name for
//! something that lives one crate down, which the root `CLAUDE.md`'s one-name rule forbids. Every
//! caller names the owner now: `vike_analytics::{metrics, report, overfit, …}`,
//! `vike_analytics::TradingRecord`, `vike_fills::{broker_sim, fill_model, …}`. (The same rule
//! retired `binutil`'s argv re-export, `harness::BuyHold` and the harness's second `report` alias.)
//! The one exception the rule allows — "the consumer cannot name the canonical crate" — applies to
//! no consumer: every one ranks above `vike-analytics` (15) and `vike-fills` (20).

#![warn(unreachable_pub)]

// Monte Carlo permutation tests over a profile's bars (`[permutation]`).
pub mod permutation;
// One profile to one result: the profile, the data plan, the dispatcher, the tick replay, the report.
pub mod run;
pub mod search; // the search family: Euler core, the seam, the grid, the methods and the trial ledger
pub mod walkforward;
// ⚠ `pub mod wire_record;` stood here (`TradingRecord` <-> the `TradingRecordWire` DTO) and MOVED to
// `vike_backtesthub_client::wire_record` on 2026-11-10: the desktop decodes every Studio answer with it
// and links no harness (docs/decisions/0078's amendment of that day).

use std::fmt;

/// Crate-wide harness error. Reused by the dispatcher/bin tasks that build on
/// [`run::profile`] — kept at the crate root rather than in `profile.rs` so later modules can depend
/// on it without depending on the profile parser specifically.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessError {
    /// Filesystem I/O failure (e.g. reading a profile file).
    Io(String),
    /// TOML parse/deserialize failure, or an unparsable timestamp.
    Parse(String),
    /// A parsed profile failed a semantic validation rule: one typed refusal, with its id
    /// (`run::refusals`), so nothing refuses a profile without a variant. Boxed, so an `Err` stays
    /// the size of a `String` however many fields a refusal carries; built by `HarnessError::from`.
    Validation(Box<run::refusals::ProfileRefusal>),
    /// A downstream data-store failure: loading the bar/tick slice, or a slice the store holds that
    /// cannot be run as asked (empty, misaligned, short of coverage).
    Data(String),
}

impl HarnessError {
    /// The refusal SENTENCE alone, without the [`fmt::Display`] prefix.
    ///
    /// The `Display` rendering is right for one error coming back from a door
    /// (`harness validation error [profile.engine.cash.not_positive]: engine.cash must be > 0, got 0`) and wrong for a LIST, where
    /// the prefix repeats on every row while the row already carries its own severity. That list
    /// is `profile::ProfileBacktest::validate_all`, which builds each `vike_model::Diagnostic`
    /// from this. A refusal reads `error [<id>]: <text>` (`refusal_enum!`'s `rendered`): the id a person looks up
    /// in the published `refusals.json`, a carrier's forwarded one when it has one.
    pub fn message(&self) -> String {
        match self {
            Self::Io(e) | Self::Parse(e) | Self::Data(e) => e.clone(),
            Self::Validation(refusal) => refusal.rendered(),
        }
    }
}

impl From<run::refusals::ProfileRefusal> for HarnessError {
    fn from(refusal: run::refusals::ProfileRefusal) -> Self {
        Self::Validation(Box::new(refusal))
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HarnessError::Io(e) => write!(f, "harness io error: {e}"),
            HarnessError::Parse(e) => write!(f, "harness parse error: {e}"),
            HarnessError::Validation(e) => write!(f, "harness validation {}", e.rendered()),
            HarnessError::Data(e) => write!(f, "harness data error: {e}"),
        }
    }
}

impl std::error::Error for HarnessError {}
