//! `eval_signal_rule`: feed health, breaker trips, resolutions, the recorder-liveness rules.

use crate::delivery::FiredAlert;
use crate::rule::{AlertRule, FeedState, RuleTrigger};

use super::gate::{filter_matches, maybe_fire, prefix_matches};
use super::{AlertSignal, RuleState};

/// Evaluate a status-signal rule ([`RuleTrigger::Feed`] / [`FillRateBreaker`](RuleTrigger::FillRateBreaker)
/// / [`PolymarketResolution`](RuleTrigger::PolymarketResolution) /
/// [`SeriesStale`](RuleTrigger::SeriesStale) / [`SeriesSlow`](RuleTrigger::SeriesSlow) /
/// [`FamilyCollapse`](RuleTrigger::FamilyCollapse)) against one [`AlertSignal`].
/// Returns `None` for any other trigger or a non-matching signal.
pub fn eval_signal_rule(
    rule: &AlertRule,
    st: &mut RuleState,
    sig: &AlertSignal,
    now_ms: i64,
) -> Option<FiredAlert> {
    match (&rule.trigger, sig) {
        (RuleTrigger::Feed { venue, state }, AlertSignal::Feed { venue: sv, degraded }) => {
            let want_degraded = matches!(state, FeedState::Degraded);
            if filter_matches(venue, sv.as_str()) && want_degraded == *degraded {
                let what = if *degraded { "degraded" } else { "recovered" };
                maybe_fire(rule, st, now_ms, format!("feed {sv} {what}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::FillRateBreaker { venue, symbol },
            AlertSignal::FillRateBreaker { venue: sv, symbol: ss },
        ) => {
            if filter_matches(venue, sv.as_str()) && filter_matches(symbol, ss.as_str()) {
                maybe_fire(rule, st, now_ms, format!("fill-rate breaker tripped {sv} {ss}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::PolymarketResolution { token_id },
            AlertSignal::PolymarketResolution { token_id: st_id },
        ) => {
            if filter_matches(token_id, st_id.as_str()) {
                maybe_fire(rule, st, now_ms, format!("polymarket market resolved: {st_id}"))
            } else {
                None
            }
        }
        (
            RuleTrigger::SeriesStale { series_prefix },
            AlertSignal::SeriesStale { series, silent_for_ms, rows },
        ) => {
            if !prefix_matches(series_prefix, series.as_str()) {
                return None;
            }
            let body = match silent_for_ms {
                Some(ms) => format!(
                    "series {series} has STOPPED receiving rows — silent for {}s after {rows} rows",
                    ms / 1_000
                ),
                None => format!(
                    "series {series} has NEVER received a row — check the venue's stream name"
                ),
            };
            maybe_fire(rule, st, now_ms, body)
        }
        (
            RuleTrigger::SeriesSlow { series_prefix },
            AlertSignal::SeriesSlow {
                series,
                observed_per_s,
                expected_per_s,
                window_secs,
                governor,
                governor_per_s,
            },
        ) => {
            if !prefix_matches(series_prefix, series.as_str()) {
                return None;
            }
            // Both rates to two decimals: the fault is an ORDER-OF-MAGNITUDE shortfall (0.42
            // against 10), so precision past that is noise, and rounding to integers would render
            // the measured broken lane as "0".
            let body = format!(
                "series {series} is running at {observed_per_s:.2}/s against an expected \
                 {expected_per_s:.2}/s over {window_secs}s — while {governor} ran at \
                 {governor_per_s:.2}/s, so the instrument was busy and this lane was not"
            );
            maybe_fire(rule, st, now_ms, body)
        }
        (
            RuleTrigger::FamilyCollapse { series_prefix },
            AlertSignal::FamilyCollapse {
                family,
                observed_items,
                baseline_items,
                window_secs,
                members,
                ring_windows,
                licence,
                licence_items,
            },
        ) => {
            if !prefix_matches(series_prefix, family.as_str()) {
                return None;
            }
            // COUNTS, never a rate, and the word "baseline" never "expected": the second number is
            // this family's own recent median and the body must not read as an authority it is not.
            // The member count is in the line because it is what tells an operator this is the
            // WHOLE family and not one rotated-out token, and the licence is what tells them the
            // process was still receiving data.
            //
            // ⚠ The empty-licence arm is not decoration. A recorder that records exactly ONE
            // family has nothing outside it to ask, and its producer waives the licence rather than
            // being permanently unable to fire; rendering that case through the sentence below
            // would assert corroboration that does not exist, which is precisely the false
            // diagnosis this whole rule is careful about.
            let witness = if licence.is_empty() {
                " — and this process records no other series, so nothing corroborates it"
                    .to_string()
            } else {
                format!(
                    ", while {licence} produced {licence_items} in the SAME window, so this \
                     recorder was still receiving data and this family was not"
                )
            };
            let body = format!(
                "family {family} produced {observed_items} items in {window_secs}s across \
                 {members} member series — against a rolling baseline of {baseline_items} \
                 (median of its own last {ring_windows} windows){witness}"
            );
            maybe_fire(rule, st, now_ms, body)
        }
        _ => None,
    }
}
