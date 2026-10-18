//! `eval_snapshot_rule`: the price edge, the drawdown latch and the reconcile-alert latch.

use crate::delivery::FiredAlert;
use crate::rule::{AlertRule, RuleTrigger};

use super::gate::{maybe_fire, scalar_crossed};
use super::{RuleState, SnapshotFacts};

/// Evaluate a snapshot-driven rule ([`RuleTrigger::Price`] / [`Drawdown`](RuleTrigger::Drawdown) /
/// [`ReconAlert`](RuleTrigger::ReconAlert)) against `facts`. Returns `None` for any other trigger.
pub fn eval_snapshot_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    facts: &dyn SnapshotFacts,
    now_ms: i64,
) -> Option<FiredAlert> {
    match &rule.trigger {
        RuleTrigger::Price { venue, symbol, op, level } => {
            // No published mark yet ⇒ nothing to compare (and DON'T seed last_value from a phantom
            // — the first real mark then can't spuriously "cross").
            let cur = facts.mark(venue, symbol)?;
            if scalar_crossed(*op, st, cur, *level) {
                maybe_fire(rule, st, now_ms, format!("{venue} {symbol} {op} {level} (mark {cur})"))
            } else {
                None
            }
        }
        RuleTrigger::Drawdown { pct } => {
            // ⚠ The caller must fill `drawdown_curve` from `Portfolio::drawdown_curve`, NOT from
            // `CoreSnapshot::equity()`. This rule FOLLOWS the core's own latch onto the daemon's own
            // equity curve (configured capital + realized + unrealized P&L) instead of the
            // cross-venue equity TOTAL, which on an `Authoritative` block includes the venue's
            // wallet for the whole account the credentials open. Reading the total meant a third
            // party's withdrawal from a SHARED account fired this alert with no trading behind it,
            // while a real 25% loss on the daemon's own ~9000 of book was 3.6% of a 62647 total and
            // never fired at all (the CI box, 2026-08-17). The latch and the alert now measure the same
            // quantity by construction — `Portfolio::pnl_total`'s fold is pinned bit-identical to
            // the engine-side scalar the latch uses, so an operator can never be told "no drawdown"
            // about a core that just latched itself liquidate-only.
            //
            // ⚠ That pairing is now the CALLER's to keep: this crate no longer names `Portfolio`,
            // so nothing here can check which quantity arrived. The one production caller states it
            // at the conversion (`crates/vike-tradehub/src/alerts.rs`).
            let cur = facts.drawdown_curve();
            let peak = st.peak_equity.map_or(cur, |p| p.max(cur));
            st.peak_equity = Some(peak);
            // `peak > 0.0` guard — note what it excludes: a portfolio
            // with no configured capital base (`capital_base == 0.0` — a wire-built observe
            // portfolio, or a core assembled with `seed_cash: 0.0`) and no profit yet. The core
            // announces that condition on its own side (`sweep_drawdown_latch`'s DISARMED note);
            // firing an alert whose percentage has no denominator would be worse than silence.
            let dd = if peak > 0.0 { (peak - cur) / peak } else { 0.0 };
            let breached = dd >= *pct;
            let rising = breached && !st.latched;
            st.latched = breached; // re-arm once the curve recovers back under the threshold
            if rising {
                maybe_fire(
                    rule,
                    st,
                    now_ms,
                    format!(
                        "own-PnL drawdown {:.2}% >= {:.2}% (peak {peak}, curve {cur}, capital_base {}, own_pnl {})",
                        dd * 100.0,
                        *pct * 100.0,
                        facts.capital_base(),
                        facts.pnl_total()
                    ),
                )
            } else {
                None
            }
        }
        RuleTrigger::ReconAlert { divergence_kind: kind } => {
            let alerts = facts.recon_alerts();
            let matching = alerts.iter().find(|a| match kind {
                None => true,
                Some(k) => a.kind == k.as_str(),
            });
            let present = matching.is_some();
            let rising = present && !st.latched;
            // Capture detail BEFORE flipping the latch (borrow of `matching` ends here).
            let detail = matching
                .map(|a| format!("{} — {} ({} pending)", a.kind, a.detail, a.proposed_event_count));
            st.latched = present;
            match (rising, detail) {
                (true, Some(d)) => maybe_fire(rule, st, now_ms, format!("reconcile alert: {d}")),
                _ => None,
            }
        }
        _ => None,
    }
}
