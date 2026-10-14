//! The PURE alert evaluator: `(rule + per-rule state + one input) -> Option<FiredAlert>`. No I/O,
//! no clocks (the caller passes `now_ms`), no delivery (that is [`crate::delivery`]) — so every
//! trigger's fire/no-fire is unit-tested with plain values. [`crate::AlertEngine`] holds the
//! [`RuleState`] map + the sinks and folds these functions over the enabled rules OFF the hot fold.
//!
//! **Nothing here names a vike type.** [`eval_snapshot_rule`] reads its facts through
//! [`SnapshotFacts`] and [`eval_event_rule`] takes an [`AlertEvent`], both owned by this module,
//! so every entry point compiles in the one vike-free build the standalone recorder watchdog
//! links. Its output type, [`FiredAlert`], lives in [`crate::delivery`] alongside the sinks that
//! consume it.
//!
//! Four inputs, one per evaluation entry point (mirrors [`RuleTrigger`]'s grouping):
//! - [`eval_snapshot_rule`] over the caller's [`SnapshotFacts`] — price crossings, drawdown, recon;
//! - [`eval_indicator_rule`] over an [`IndicatorSample`] the consumer computed;
//! - [`eval_event_rule`] over an [`AlertEvent`] — fills, rejects;
//! - [`eval_signal_rule`] over an [`AlertSignal`] — feed health, breaker trip, resolution, and a
//!   RECORDED SERIES that stopped receiving rows.
//!
//! Each function no-ops (returns `None`) for a rule whose trigger belongs to a different input, so
//! the engine can hand every enabled rule to the matching entry point without pre-partitioning.
//!
//! Edge/latch semantics live HERE (in [`RuleState`]): scalar crossings ([`Compare`]) edge-detect
//! via `last_value`; drawdown/recon LATCH via `latched`; discrete events/signals fire on each
//! occurrence, rate-limited only by the rule's cooldown/once (both applied by [`maybe_fire`]).

#[cfg(doc)]
use super::delivery::FiredAlert;
#[cfg(doc)]
use super::rule::{Compare, RuleTrigger};
#[cfg(doc)]
use gate::maybe_fire;

mod event;
mod gate;
mod indicator;
mod input;
mod signal;
mod snapshot;

pub use event::eval_event_rule;
pub use gate::RuleState;
pub use indicator::eval_indicator_rule;
pub use input::{AlertEvent, AlertSignal, IndicatorSample, ReconAlertFact, SnapshotFacts};
pub use signal::eval_signal_rule;
pub use snapshot::eval_snapshot_rule;

#[cfg(test)]
mod tests;
