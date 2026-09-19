//! `gapfill` — turn a coverage report into a fill PLAN: exactly which days to fetch, for which
//! kinds, from a source that can actually serve them.
//!
//! ## Why a plan, and not just a date range
//!
//! Handing a backfill `--from 2026-07-01 --to 2026-08-01` re-fetches every day in it, including the
//! ones already recorded. The store accepts those rows — a different writer means a different commit
//! key, and it never dedups by row VALUE — so a `scan_trades` afterwards returns both copies and a
//! backtest quietly sees double the volume. `compact_series_superseding` + the store'"'"s persisted
//! source policy exist to resolve that, but the far better outcome is **not creating it**: fetch only
//! the days that are missing, and there is no overlap to resolve.
//!
//! That is the whole idea here. [`plan_fill`] takes what
//! [`vike_data::coverage::InstrumentCoverage`] already knows — which days each kind has — and emits
//! the complement, per kind, narrowed to what [`crate::caps`] says this source can serve.
//!
//! ## The honesty requirement
//!
//! A plan must not silently omit what it cannot do. On Polymarket the venue serves NO history at all,
//! and no venue-direct source anywhere on the roster serves `trade` or `book`
//! ([`crate::caps::backfill_caps`]) — so "fill this book gap from the venue" is a request no code in
//! this workspace can satisfy. [`FillPlan::unavailable`] names those kinds explicitly rather than
//! leaving them out of `days`, because a caller that sees an empty plan must be able to tell "nothing
//! to do" from "this source cannot help you".
//!
//! ## What it deliberately does NOT know
//!
//! How to fetch. Each source has its own bin, its own paging, its own credentials. This is the pure
//! decision — which days, which kinds, is it even possible — and it is a fold over two things that
//! already exist. Wiring a plan to a fetcher is per-source work.
//!
//! ⚠ **Day granularity.** The store's file index is `date=`-partitioned, so a day the recorder
//! covered for six hours reads as PRESENT and is not planned. Within-day trust is a different
//! question, answered by `vike_data::quality::DayQuality` over scanned rows.

use std::collections::BTreeMap;

use vike_data::coverage::InstrumentCoverage;

use crate::caps::{PLANNABLE_KINDS, Source, backfill_caps};

/// Epoch-ms per UTC day.
const DAY_MS: i64 = 86_400_000;

/// What one fill would do, before doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FillPlan {
    pub venue: String,
    /// The instrument label — the symbol, or the group for a grouped series.
    pub label: String,
    pub source: Source,
    /// `day index -> the kinds missing that day AND servable by this source`, ascending.
    /// Only days with at least one fetchable kind appear.
    pub days: BTreeMap<i64, Vec<&'static str>>,
    /// Kinds that are missing somewhere but which this source can NEVER serve for this venue.
    /// Named rather than omitted — see the module doc's honesty requirement.
    pub unavailable: Vec<&'static str>,
}

impl FillPlan {
    /// `true` when there is nothing to fetch. Check [`unavailable`](Self::unavailable) before
    /// reporting that as "already complete": an empty plan with a non-empty `unavailable` means the
    /// gaps are real and this source simply cannot close them.
    pub fn is_empty(&self) -> bool {
        self.days.is_empty()
    }

    /// Total `(day, kind)` fetches this plan implies — the unit of work, and what a progress bar
    /// counts.
    pub fn fetch_count(&self) -> usize {
        self.days.values().map(Vec::len).sum()
    }

    /// The contiguous day RANGES this plan covers, inclusive — most fetchers page a range rather
    /// than a day at a time, so handing them `[(d, d+3)]` beats four separate calls.
    pub fn ranges(&self) -> Vec<(i64, i64)> {
        let mut out: Vec<(i64, i64)> = Vec::new();
        for &day in self.days.keys() {
            match out.last_mut() {
                Some((_, end)) if *end + 1 == day => *end = day,
                _ => out.push((day, day)),
            }
        }
        out
    }

    /// [`ranges`](Self::ranges) as inclusive epoch-ms windows — `[00:00:00.000 of start, 23:59:59.999
    /// of end]`, the same inclusive-bound convention the store's coverage and gap reports use.
    pub fn ms_ranges(&self) -> Vec<(i64, i64)> {
        self.ranges().into_iter().map(|(a, b)| (a * DAY_MS, (b + 1) * DAY_MS - 1)).collect()
    }
}

/// Plan a fill of `cov`'s gaps from `source`.
///
/// A day is planned for a kind when that kind LACKS the day and the source can serve it. Days no
/// kind has are included too — a hole in everything is still a hole; it simply is not a
/// [`PartialDay`](vike_data::coverage::PartialDay), which is why this walks the per-kind gap ranges
/// rather than only the partial days.
pub fn plan_fill(cov: &InstrumentCoverage, source: Source) -> FillPlan {
    let caps = backfill_caps(source, &cov.key.venue);
    let mut days: BTreeMap<i64, Vec<&'static str>> = BTreeMap::new();
    let mut unavailable: Vec<&'static str> = Vec::new();

    for kind in PLANNABLE_KINDS {
        let Some(k) = cov.kinds.get(kind) else { continue };
        // Every day inside this kind's own recorded span that it lacks, plus — for a kind never
        // recorded at all — every day any OTHER kind has, since that is the window the instrument
        // was being watched over.
        let missing: Vec<i64> = if k.present.is_empty() {
            cov.spanned_days()
        } else {
            k.gaps.iter().flat_map(|&(a, b)| a..=b).collect()
        };
        if missing.is_empty() {
            continue;
        }
        if !caps.serves(kind) {
            unavailable.push(kind);
            continue;
        }
        for day in missing {
            days.entry(day).or_default().push(kind);
        }
    }

    // Deterministic within a day: PLANNABLE_KINDS order, which is also the order the report renders.
    for v in days.values_mut() {
        v.sort_by_key(|k| PLANNABLE_KINDS.iter().position(|t| t == k).unwrap_or(usize::MAX));
    }

    FillPlan {
        venue: cov.key.venue.clone(),
        label: cov.key.label.clone(),
        source,
        days,
        unavailable,
    }
}

/// Plan against EVERY source, best first — "who can close this hole, and how much of it?".
///
/// Sorted by how many `(day, kind)` fetches each source can contribute, descending, so a caller
/// offering the customer a choice leads with the one that helps most. Sources that can do nothing are
/// dropped; if the result is EMPTY the gap cannot be closed by anything in this workspace, which is
/// the answer for a binance `trade` hole (no venue backfill; the crypto archive is rights-blocked).
pub fn plan_all_sources(cov: &InstrumentCoverage) -> Vec<FillPlan> {
    let mut plans: Vec<FillPlan> =
        Source::ALL.into_iter().map(|s| plan_fill(cov, s)).filter(|p| !p.is_empty()).collect();
    plans.sort_by(|a, b| b.fetch_count().cmp(&a.fetch_count()).then(a.source.cmp(&b.source)));
    plans
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::SeriesId;
    use vike_data::coverage::join_coverage;

    fn cov(venue: &str, rows: &[(&str, Vec<i64>)]) -> InstrumentCoverage {
        let pairs: Vec<(SeriesId, Vec<i64>)> = rows
            .iter()
            .map(|(k, d)| (SeriesId::per_symbol(*k, venue, "TOK", None), d.clone()))
            .collect();
        join_coverage(&pairs).into_iter().next().expect("one instrument")
    }

    /// **The point of the whole module**: fetch the missing days, not the range. A month-long
    /// re-fetch would double-count every day already held.
    #[test]
    fn only_the_missing_days_are_planned() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 2, 3]), ("quote", vec![1, 2, 3]), ("book", vec![1, 3])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert_eq!(p.days.keys().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(p.days[&2], vec!["book"]);
        assert_eq!(p.fetch_count(), 1);
    }

    /// A fully covered instrument plans nothing — and with nothing `unavailable`, that genuinely
    /// means complete.
    #[test]
    fn a_complete_instrument_plans_nothing() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 2]), ("quote", vec![1, 2]), ("book", vec![1, 2])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert!(p.is_empty());
        assert!(p.unavailable.is_empty(), "empty AND complete");
    }

    /// **The honesty requirement.** Polymarket's venue serves no history, so a venue plan is empty —
    /// but that must NOT read as "nothing to do". `unavailable` names every kind it cannot help with.
    #[test]
    fn an_empty_plan_with_unavailable_kinds_is_not_completeness() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3]), ("quote", vec![1, 3]), ("book", vec![1, 3])],
        );
        let p = plan_fill(&c, Source::Venue);
        assert!(p.is_empty(), "the venue can fetch nothing");
        assert_eq!(p.unavailable, vec!["trade", "quote", "book"], "and says so");
    }

    /// The same gap, from a source that CAN serve it.
    #[test]
    fn the_archive_can_close_what_the_venue_cannot() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3]), ("quote", vec![1, 3]), ("book", vec![1, 3])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert_eq!(p.days[&2], vec!["trade", "quote", "book"]);
        assert!(p.unavailable.is_empty());
    }

    /// A kind NEVER recorded is planned across the instrument's whole watched span, not just inside
    /// its own (empty) gap set — there is no span to have a gap inside.
    #[test]
    fn a_never_recorded_kind_is_planned_over_the_whole_span() {
        let c = cov(
            "polymarket",
            &[("trade", vec![5, 6, 7]), ("quote", vec![5, 6, 7]), ("book", vec![])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert_eq!(p.days.keys().copied().collect::<Vec<_>>(), vec![5, 6, 7]);
        assert!(p.days.values().all(|v| v == &vec!["book"]));
    }

    /// **`depth` is never planned and never reported unavailable.** `join_coverage` gives every
    /// instrument a `depth` row (empty here), and no source can ever serve that lane — so iterating
    /// it would append `"depth"` to EVERY plan's `unavailable`, which is true and useless.
    /// [`PLANNABLE_KINDS`] is where that exclusion lives.
    #[test]
    fn depth_is_outside_the_plannable_set() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3]), ("quote", vec![1, 3]), ("book", vec![1, 3])],
        );
        assert!(c.kinds.contains_key("depth"), "the report DOES carry a depth row");
        let p = plan_fill(&c, Source::Venue);
        assert!(!p.unavailable.contains(&"depth"), "got {:?}", p.unavailable);
        assert!(
            p.days.values().all(|v| !v.contains(&"depth")),
            "no day should plan a depth fetch: {:?}",
            p.days
        );
    }

    /// A REAL depth series with a hole is likewise not planned — that hole is permanent (only a
    /// live recorder produces the lane), and the plan says nothing rather than promising a fetch.
    #[test]
    fn a_depth_hole_is_not_plannable_either() {
        let c = cov("binance", &[("trade", vec![1, 2, 3]), ("depth", vec![1, 3])]);
        let p = plan_fill(&c, Source::Archive);
        assert!(p.days.values().all(|v| !v.contains(&"depth")), "{:?}", p.days);
        assert!(!p.unavailable.contains(&"depth"), "{:?}", p.unavailable);
    }

    /// **A binance trade gap cannot be closed by anything here** — no venue trade backfill, and the
    /// crypto archive is rights-blocked (binance/okx ToS forbid resale archives). An empty
    /// `plan_all_sources` IS the answer, and it is why the recorder exists.
    #[test]
    fn a_binance_trade_gap_has_no_source_at_all() {
        let c = cov("binance", &[("trade", vec![1, 3]), ("quote", vec![]), ("book", vec![])]);
        assert!(plan_all_sources(&c).is_empty(), "nothing in this workspace can fetch it");
        assert_eq!(plan_fill(&c, Source::Venue).unavailable, vec!["trade", "quote", "book"]);
    }

    /// Contiguous days collapse into ranges — most fetchers page a range, so four calls become one.
    #[test]
    fn contiguous_days_collapse_into_ranges() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 6]), ("quote", vec![1, 6]), ("book", vec![1, 6])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert_eq!(p.ranges(), vec![(2, 5)], "one range, not four days");
        assert_eq!(p.ms_ranges(), vec![(2 * DAY_MS, 6 * DAY_MS - 1)]);
    }

    #[test]
    fn separated_gaps_stay_separate_ranges() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3, 7]), ("quote", vec![1, 3, 7]), ("book", vec![1, 3, 7])],
        );
        let p = plan_fill(&c, Source::Archive);
        assert_eq!(p.ranges(), vec![(2, 2), (4, 6)]);
    }

    /// Ordering within a day follows PLANNABLE_KINDS, so a plan renders the same way the report
    /// does rather than in map order.
    #[test]
    fn kinds_within_a_day_are_in_report_order() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3]), ("quote", vec![1, 3]), ("book", vec![1, 3])],
        );
        assert_eq!(plan_fill(&c, Source::Archive).days[&2], vec!["trade", "quote", "book"]);
    }

    /// "Who can close this hole?" — best first, and only sources that contribute something.
    #[test]
    fn plan_all_sources_ranks_by_how_much_each_can_do() {
        let c = cov(
            "polymarket",
            &[("trade", vec![1, 3]), ("quote", vec![1, 3]), ("book", vec![1, 3])],
        );
        let plans = plan_all_sources(&c);
        assert_eq!(plans.len(), 2, "archive + clickhouse; never the venue");
        assert!(plans.iter().all(|p| p.source != Source::Venue));
        assert!(plans[0].fetch_count() >= plans[1].fetch_count());
    }
}
