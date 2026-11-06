//! The prompt-grounding block the authoring loop prepends: [`prior_work_summary`].

use super::learning::{LearningScope, LearningStore};
use super::trial::TrialLedger;

#[cfg(doc)]
use super::trial::Trial;

/// How many prior trials [`prior_work_summary`] names in the prompt. Small on purpose: the point
/// is grounding, not a data dump — a long tail of near-identical rejected attempts would crowd out
/// the request itself.
pub const PROMPT_TOP_K: usize = 6;

/// The prompt-grounding block: what has already been tried on THIS slice and what the model chose
/// to remember about it. Prepended to the user prompt by the authoring loop (payoff (a); the
/// cross-session deflation is payoff (b) and lives in `agent::deflate::deflate_with_prior`).
///
/// Ranking is ONE comparator, documented rather than clever: accepted before rejected, then by
/// PER-OBSERVATION Sharpe ([`Trial::sr_per_obs`]) descending, then newest first. Rejected rows
/// carry `0.0` in both Sharpe fields, so within that group the ordering degenerates to recency —
/// which is what you want from them ("here is what just failed"), while the accepted group leads
/// with the current best.
///
/// ⚠ It ranks on a DIFFERENT field from the one it prints, and the comparator's own comment carries
/// why: the printed `oos_sharpe` is annualized off the slice's interval, so a ledger file spanning
/// the change that made it so holds rows on two scales — and ranking a pre-change row against a
/// post-change one is a factor of ~37.9 on 1m. `sr_per_obs` was never annualized and cannot drift
/// that way.
///
/// Returns an EMPTY string when there is nothing to say (no matching trial, no matching learning),
/// so the caller can leave the prompt byte-identical to the pre-ledger one.
pub fn prior_work_summary(
    ledger: &TrialLedger,
    learnings: &LearningStore,
    venue: &str,
    symbol: &str,
    interval: &str,
    top_k: usize,
) -> String {
    let trials = ledger.for_slice(venue, symbol, interval);
    let notes = learnings.matching(venue, symbol, interval);
    if trials.is_empty() && notes.is_empty() {
        return String::new();
    }
    let accepted = trials.iter().filter(|t| t.accepted).count();
    let mut out = format!(
        "# Prior work on {symbol} @ {venue} {interval}\n\
         {} previous trial(s) on this exact slice, {accepted} accepted. Do not resubmit a \
         previously rejected approach unchanged.\n",
        trials.len()
    );
    if !notes.is_empty() {
        out.push_str("\n## Notes recorded on earlier runs\n");
        for n in &notes {
            let tag = match n.scope {
                LearningScope::Global => "global",
                LearningScope::Slice { .. } => "this slice",
            };
            out.push_str(&format!("- ({tag}) {}\n", n.text));
        }
    }
    if !trials.is_empty() {
        let mut ranked = trials;
        // ⚠ Ranked on `sr_per_obs`, NOT on the `oos_sharpe` it PRINTS, and the difference is a
        // migration rather than a preference.
        //
        // Within one slice the two order identically — annualization is a uniform positive factor
        // under a monotone `sqrt`, so it cannot reorder anything. That was true while every row on
        // disk shared one factor. It stopped being true the moment the displayed Sharpe started
        // deriving from the slice's interval: a ledger file written BEFORE that change carries
        // `oos_sharpe` on the daily 252 scale, one written after carries it on the interval's, and
        // the two land in the SAME slice's list. Sorting on the printed number would then rank a
        // pre-change row against a post-change one across a factor of ~37.9 on 1m.
        //
        // `sr_per_obs` is per-observation and has never been annualized, so it is immune to that by
        // construction — and it is the field the deflation already trusts
        // (`TrialLedger::prior_sharpes_for`). The printed value stays `oos_sharpe`: an operator
        // reads an annualized Sharpe, and a stale row printing its old scale is a cosmetic wrong
        // number, while a mis-RANKED list is what a model then builds on.
        ranked.sort_by(|a, b| {
            b.accepted
                .cmp(&a.accepted)
                .then(b.sr_per_obs.total_cmp(&a.sr_per_obs))
                .then(b.ts_ms.cmp(&a.ts_ms))
        });
        out.push_str("\n## Previous attempts (best first)\n");
        for t in ranked.iter().take(top_k) {
            if t.accepted {
                out.push_str(&format!(
                    "- ACCEPTED: OOS Sharpe {:.2} over {} trades — {}\n",
                    t.oos_sharpe, t.n_trades, t.explanation
                ));
            } else {
                let why = if t.problems.is_empty() {
                    "no reason recorded".to_string()
                } else {
                    t.problems.join("; ")
                };
                out.push_str(&format!(
                    "- REJECTED after {} attempt(s): {why} — {}\n",
                    t.attempts, t.explanation
                ));
            }
        }
    }
    out
}
