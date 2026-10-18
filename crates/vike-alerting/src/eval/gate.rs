//! What every entry point shares: `RuleState`, scope/crossing helpers, the cooldown/once gate.

// A plain `use`, deliberately: [`FiredAlert`] is defined next to the sinks that consume it (see
// `eval.rs`'s `//!`) and is this module's output type, so it has to be in scope here — but it is
// NOT re-exported. Every consumer, in and out of this crate, reaches the type through the crate's
// flat vocabulary (`vike_alerting::FiredAlert`, re-exported from `delivery` by `lib.rs`).
use crate::delivery::FiredAlert;

use crate::rule::{AlertRule, Compare};

#[cfg(doc)]
use super::eval_snapshot_rule;
#[cfg(doc)]
use crate::rule::RuleTrigger;

/// Per-rule mutable evaluation state, keyed by `AlertRule::id` in [`crate::AlertEngine`].
/// Rebuilt empty whenever the engine is (re)built — a fresh session re-arms every latch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleState {
    /// previous scalar sample (price / indicator value) — for edge (crossing) detection. `None`
    /// until the first sample, so a rule can never fire on its very first observation (no prior to
    /// cross from).
    pub last_value: Option<f64>,
    /// Running session-peak of the DRAWDOWN CURVE — for [`RuleTrigger::Drawdown`]. Named
    /// `peak_equity` historically; the quantity is `vike_exec::Portfolio::drawdown_curve`
    /// (configured capital + the daemon's own P&L), never the cross-venue equity total. See the
    /// `Drawdown` arm of [`eval_snapshot_rule`] for why the wallet had to leave this number.
    pub peak_equity: Option<f64>,
    /// the latch for [`RuleTrigger::Drawdown`] / [`RuleTrigger::ReconAlert`]: `true` while the
    /// condition currently holds, so a fire happens only on the rising edge and re-arms on release.
    pub latched: bool,
    /// wall-clock ms of the last fire — the cooldown gate.
    pub last_fired_ms: Option<i64>,
    /// a `once` rule has already fired this session.
    pub fired_once: bool,
}

/// The rule's display label — its `name`, or its `id` when unnamed.
fn label(rule: &AlertRule) -> String {
    if rule.name.is_empty() { rule.id.clone() } else { rule.name.clone() }
}

/// `filter` (an optional scope) matches `val` when it is `None` (unscoped) or equal.
pub(crate) fn filter_matches(filter: &Option<String>, val: &str) -> bool {
    match filter {
        None => true,
        Some(f) => f.as_str() == val,
    }
}

/// [`filter_matches`]' PREFIX twin, shared by the three recorder triggers
/// ([`RuleTrigger::SeriesStale`], [`SeriesSlow`](RuleTrigger::SeriesSlow),
/// [`FamilyCollapse`](RuleTrigger::FamilyCollapse)) — the first one's doc carries the reason an
/// exact series name cannot be the scope on a venue whose symbols rotate.
///
/// A prefix on a FAMILY key behaves identically because the first two segments of
/// `{kind}/{venue}/{family}` are the first two of every member's `{kind}/{venue}/{symbol}`, which
/// is why one function serves all three.
pub(crate) fn prefix_matches(filter: &Option<String>, val: &str) -> bool {
    match filter {
        None => true,
        Some(p) => val.starts_with(p.as_str()),
    }
}

/// Did `cur` cross `bound` in `op`'s direction, given the previous sample `prev`? Strict edge:
/// `Above` needs `prev <= bound < cur`; `Below` needs `prev >= bound > cur`.
fn crossed(op: Compare, prev: f64, cur: f64, bound: f64) -> bool {
    match op {
        Compare::Above => prev <= bound && cur > bound,
        Compare::Below => prev >= bound && cur < bound,
    }
}

/// Fold a scalar sample into `st`, returning whether it crossed `bound` in `op`'s direction.
/// ALWAYS records `cur` as the new `last_value` (so the next call has a prior), even when it does
/// not fire — that is what makes the crossing edge-accurate.
pub(crate) fn scalar_crossed(op: Compare, st: &mut RuleState, cur: f64, bound: f64) -> bool {
    // No prior ⇒ cannot detect a crossing yet (`is_some_and` is false on `None`).
    let fired = st.last_value.is_some_and(|prev| crossed(op, prev, cur, bound));
    st.last_value = Some(cur);
    fired
}

/// Apply the rule's cooldown/once gate and, if it passes, build the [`FiredAlert`] and record the
/// fire. `title` is the rule label; `body` is the caller's specifics. Returns `None` when the gate
/// suppresses the fire (leaving `st`'s fire bookkeeping untouched).
pub(crate) fn maybe_fire(
    rule: &AlertRule,
    st: &mut RuleState,
    now_ms: i64,
    body: String,
) -> Option<FiredAlert> {
    if rule.once && st.fired_once {
        return None;
    }
    if rule.cooldown_ms > 0
        && let Some(last) = st.last_fired_ms
        && now_ms.saturating_sub(last) < rule.cooldown_ms
    {
        return None;
    }
    st.last_fired_ms = Some(now_ms);
    if rule.once {
        st.fired_once = true;
    }
    Some(FiredAlert {
        rule_id: rule.id.clone(),
        title: label(rule),
        body,
        ts_ms: now_ms,
        targets: rule.targets.clone(),
    })
}
