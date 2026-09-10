//! Cross-KIND coverage: "for this instrument, which days do I have trades, quotes AND book for?"
//!
//! [`DataFusionHist::series_gaps`] already answers that per SERIES, and the store partitions by
//! `kind` first — so `kind=trade`, `kind=quote` and `kind=book` are separate series with separate
//! manifests, and a day can be present in one and missing in another. That fact is what makes this
//! module possible without any new detection: the per-kind truth is already on disk. What was
//! missing is the JOIN — nobody asked the three manifests the same question and lined the answers up
//! per instrument.
//!
//! **Why the join is the thing that matters, not the per-series gaps.** The backfill sources are not
//! interchangeable, and their differences are per-kind:
//!
//! | source | trade | quote | book |
//! |---|---|---|---|
//! | Polymarket's own API (as this workspace can use it today) | yes | no | **no** |
//! | the pmxt / `data.vike.io` archives | yes | derived | yes |
//!
//! So "I filled that gap from the venue" can leave a window that has a complete trade tape and **no
//! book at all**. Per-series, both facts are visible and neither is alarming. Joined, the hole is
//! obvious — and a market-making strategy backtested over that window is running on no book while
//! reporting fills.
//!
//! Cheap by construction: this is a manifest fold, exactly like the two functions it composes. NO
//! DataFusion scan, no Parquet read. It is a directory-listing-shaped cost, so the Data Manager can
//! call it on open.
//!
//! ROW-LEVEL trust — a day that is PRESENT but was recorded through a feed outage — is a different
//! question, answered by [`crate::quality`] over scanned rows. This module only knows what the file
//! index knows: which days exist.
//!
//! # Why these types derive serde
//!
//! [`HistStore::coverage_report`](crate::HistStore::coverage_report) is a TRAIT verb, and one of its
//! impls is an RPC one (`vike_datahub_client::RemoteHistStore`), so the report crosses the datahub
//! wire whole — exactly like [`crate::SeriesCoverage`], and for the same reason: this is a manifest
//! FOLD, kilobytes of day indices, not a data slice, so shipping it entire does not breach the
//! compute-to-data rule. The derives live on the value types rather than on a parallel wire DTO so
//! the answer a remote caller folds is bit-identical to the one a local caller folds — there is no
//! second shape to keep in step.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::series::SeriesId;

/// Epoch-ms per UTC day.
const DAY_MS: i64 = 86_400_000;

/// Pure gap-finder: given a set of "present" integer positions (typically UTC-day indices, but the
/// function has no date knowledge, so ms-at-UTC-midnight or any other evenly-stepped unit works too)
/// and the expected step between consecutive positions, return the inclusive `[start,end]` ranges of
/// MISSING positions strictly between the first and last present entry (contiguous runs of absent
/// slots). `present_days` need not be pre-sorted or de-duplicated. Fewer than two distinct entries
/// (nothing to have a gap *between*) or a non-positive step both return `vec![]` rather than
/// erroring — an absent or single-day series has no gaps to report.
///
/// Example: `find_gaps(&[1, 2, 5, 6], 1) == vec![(3, 4)]` — days 3 and 4 are missing between the
/// runs `[1,2]` and `[5,6]`. A fully contiguous input (`[1,2,3,4]`) returns `vec![]`.
///
/// Lives here rather than beside `DataFusionHist::series_gaps` (which uses it, through a
/// `pub(super) use`) because it is pure and BOTH consumers need it: `series_gaps` is inside the
/// `hist-datafusion`-gated tree, and this module must stay ungated like its siblings `quality` and
/// `series`. One rule, one implementation.
pub fn find_gaps(present_days: &[i64], expected_step_days: i64) -> Vec<(i64, i64)> {
    if expected_step_days <= 0 {
        return Vec::new();
    }
    let mut days: Vec<i64> = present_days.to_vec();
    days.sort_unstable();
    days.dedup();
    if days.len() < 2 {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    for w in days.windows(2) {
        let (prev, cur) = (w[0], w[1]);
        if cur - prev > expected_step_days {
            gaps.push((prev + expected_step_days, cur - expected_step_days));
        }
    }
    gaps
}

/// The tick-lane kinds a coverage report lines up. Bars are deliberately excluded: they are derived
/// from trades, so a missing bar day next to a present trade day is a resample that has not been run
/// — not a hole in the tape, and reporting it as one would train an operator to ignore the report.
///
/// `depth` is here because it is where a venue's L2 actually lands for some feeds: the recorder's
/// binance `BTCUSDT.P` records `trade` + `depth` and no `book` at all (`book` is the event lane,
/// `depth` the conflated-snapshot one). Omitting it made the report claim that instrument had no L2
/// while 34 rows of it sat on disk.
pub const TICK_KINDS: [&str; 4] = ["trade", "quote", "book", "depth"];

/// One instrument's identity across kinds: the `(venue, label)` pair, where `label` is the symbol
/// for a per-symbol series and the group for a grouped one.
///
/// Grouped and per-symbol series of the SAME instrument are deliberately different instruments here.
/// They are different directories with different manifests, and a family that was recorded
/// per-symbol before grouping existed genuinely has its history in two places — a report that
/// silently merged them would hide exactly that migration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstrumentKey {
    pub venue: String,
    /// `SeriesId::label()` — the symbol, or the group for a grouped series.
    pub label: String,
    /// `true` when this instrument's series are `group=` directories.
    pub grouped: bool,
}

/// What one kind has for one instrument.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindDays {
    /// UTC-day indices present (days since the epoch), ascending.
    pub present: Vec<i64>,
    /// Inclusive `[start,end]` UTC-day-index ranges MISSING strictly inside the recorded span.
    pub gaps: Vec<(i64, i64)>,
}

impl KindDays {
    /// `true` when this kind has no series at all for the instrument — distinct from having a series
    /// with gaps in it. "Never recorded" and "recorded with holes" call for different actions.
    pub fn absent(&self) -> bool {
        self.present.is_empty()
    }

    /// Total missing days across every gap range.
    pub fn missing_days(&self) -> usize {
        self.gaps.iter().map(|(a, b)| (b - a + 1).max(0) as usize).sum()
    }
}

/// One instrument's per-kind coverage, joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentCoverage {
    pub key: InstrumentKey,
    /// Keyed by kind; every [`TICK_KINDS`] entry is always present, so an ABSENT kind is a row with
    /// `present: []` rather than a missing map entry. A caller rendering this must not be able to
    /// confuse "no book" with "I forgot to look for book" — and that same distinction is why
    /// [`Self::partial_days`] measures against [`Self::recorded_kinds`] instead of this full set.
    pub kinds: BTreeMap<String, KindDays>,
}

impl InstrumentCoverage {
    /// The union of days any kind has — the instrument's overall recorded span.
    pub fn spanned_days(&self) -> Vec<i64> {
        let mut all: BTreeSet<i64> = BTreeSet::new();
        for k in self.kinds.values() {
            all.extend(k.present.iter().copied());
        }
        all.into_iter().collect()
    }

    /// The kinds this instrument actually records — those with at least one day, i.e. the
    /// complement of [`KindDays::absent`]. This, not [`TICK_KINDS`], is what [`Self::partial_days`]
    /// holds each day against; see there for why.
    ///
    /// Returned in [`TICK_KINDS`] order rather than the map's alphabetical one, so a rendered
    /// `missing_kinds` list reads in the same order it always has.
    pub fn recorded_kinds(&self) -> Vec<&'static str> {
        TICK_KINDS
            .iter()
            .copied()
            .filter(|k| self.kinds.get(*k).is_some_and(|d| !d.absent()))
            .collect()
    }

    /// Days where some of this instrument's RECORDED kinds have data and others do not — the
    /// report's whole point.
    ///
    /// This is what a half-failed recording looks like: the trade tape keeps flowing while the book
    /// pump is dead. Per-kind neither series looks wrong — each is contiguous on its own — so this
    /// is the only place it shows.
    ///
    /// ## Measured against the instrument's own kinds, NOT against [`TICK_KINDS`]
    ///
    /// A kind that was NEVER recorded is a different fact, and one this type already names:
    /// [`KindDays::absent`], whose doc says as much — *"'never recorded' and 'recorded with holes'
    /// call for different actions"*. Holding every day against the global list conflated the two,
    /// so an instrument that simply has no book series was reported partial on **every day it will
    /// ever have**. That is a permanent ⚠ carrying no day-to-day information, which is precisely
    /// how an operator learns to ignore a warning column.
    ///
    /// Nothing is lost by narrowing it. "This instrument has no book at all" is already visible in
    /// the Data Manager without a warning glyph: there is no `book` row. And the venue-backfill
    /// case that motivated the report — a trade tape restored where no book history exists to
    /// fetch — still reports, so long as the book series exists for any day at all: those days have
    /// trade and no book, and are partial. The only case dropped is the one that was never a *day*
    /// problem to begin with.
    pub fn partial_days(&self) -> Vec<PartialDay> {
        let recorded = self.recorded_kinds();
        // One recorded kind can never disagree with itself, and zero has nothing to compare.
        if recorded.len() < 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for day in self.spanned_days() {
            let missing: Vec<String> = recorded
                .iter()
                .filter(|k| {
                    self.kinds.get(**k).is_none_or(|d| d.present.binary_search(&day).is_err())
                })
                .map(|k| k.to_string())
                .collect();
            if !missing.is_empty() {
                out.push(PartialDay { day, missing_kinds: missing });
            }
        }
        out
    }

    /// `true` when every kind this instrument records covers every spanned day — nothing to explain.
    pub fn is_complete(&self) -> bool {
        self.partial_days().is_empty()
    }
}

/// A day some kinds have and others do not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialDay {
    /// UTC-day index (days since the epoch).
    pub day: i64,
    /// Which of this instrument's RECORDED kinds ([`InstrumentCoverage::recorded_kinds`]) have no
    /// data for this day. Never a kind the instrument does not record at all — that is
    /// [`KindDays::absent`], a different fact.
    pub missing_kinds: Vec<String>,
}

impl PartialDay {
    /// Start of this day, epoch-ms — for a caller that renders timestamps rather than day indices.
    pub fn start_ms(&self) -> i64 {
        self.day * DAY_MS
    }
}

/// Join per-series day sets into per-instrument coverage.
///
/// Takes `(SeriesId, present_days)` pairs rather than a store handle so the join is a pure function
/// — the whole reason this file has no `DataFusionHist` in it. `DataFusionHist::coverage_report`
/// supplies the pairs from each series' manifest.
///
/// Series whose `kind` is not in [`TICK_KINDS`] are ignored (bars, properties, equity …).
pub fn join_coverage(series: &[(SeriesId, Vec<i64>)]) -> Vec<InstrumentCoverage> {
    let mut by_instrument: BTreeMap<InstrumentKey, BTreeMap<String, KindDays>> = BTreeMap::new();

    for (id, days) in series {
        if !TICK_KINDS.contains(&id.kind.as_str()) {
            continue;
        }
        let key = InstrumentKey {
            venue: id.venue.clone(),
            label: id.label().to_string(),
            grouped: id.group.is_some(),
        };
        let mut present: Vec<i64> = days.clone();
        present.sort_unstable();
        present.dedup();
        let gaps = find_gaps(&present, 1);
        by_instrument.entry(key).or_default().insert(id.kind.clone(), KindDays { present, gaps });
    }

    by_instrument
        .into_iter()
        .map(|(key, mut kinds)| {
            // Every TICK_KINDS entry always exists, so "no book" is a row and not an omission.
            for k in TICK_KINDS {
                kinds.entry(k.to_string()).or_default();
            }
            InstrumentCoverage { key, kinds }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- find_gaps (moved here with the function, so they now run in the DEFAULT lane too rather
    // ---- than only under `hist-datafusion`) ------------------------------------------------------

    #[test]
    fn no_gap_for_contiguous_days() {
        assert_eq!(find_gaps(&[1, 2, 3, 4], 1), Vec::new());
    }

    #[test]
    fn empty_for_empty_or_single_day_input() {
        assert_eq!(find_gaps(&[], 1), Vec::new());
        assert_eq!(find_gaps(&[5], 1), Vec::new());
    }

    #[test]
    fn one_gap_between_two_runs() {
        assert_eq!(find_gaps(&[1, 2, 5, 6], 1), vec![(3, 4)]);
    }

    #[test]
    fn multiple_gaps_from_unsorted_duplicated_input() {
        // unsorted + duplicated input still finds every gap, sorted ascending.
        assert_eq!(find_gaps(&[10, 1, 1, 5, 3, 10, 7], 1), vec![(2, 2), (4, 4), (6, 6), (8, 9)]);
    }

    #[test]
    fn zero_or_negative_step_is_a_noop() {
        assert_eq!(find_gaps(&[1, 5], 0), Vec::new());
        assert_eq!(find_gaps(&[1, 5], -1), Vec::new());
    }

    // ---- the cross-kind join ---------------------------------------------------------------------

    fn quote(venue: &str, sym: &str) -> SeriesId {
        SeriesId::per_symbol("quote", venue, sym, None)
    }
    fn trade(venue: &str, sym: &str) -> SeriesId {
        SeriesId::per_symbol("trade", venue, sym, None)
    }
    fn book(venue: &str, sym: &str) -> SeriesId {
        SeriesId::per_symbol("book", venue, sym, None)
    }
    fn depth(venue: &str, sym: &str) -> SeriesId {
        SeriesId::per_symbol("depth", venue, sym, None)
    }

    #[test]
    fn a_fully_covered_instrument_reports_nothing_to_explain() {
        let got = join_coverage(&[
            (trade("polymarket", "TOK"), vec![10, 11, 12]),
            (quote("polymarket", "TOK"), vec![10, 11, 12]),
            (book("polymarket", "TOK"), vec![10, 11, 12]),
        ]);
        assert_eq!(got.len(), 1);
        assert!(got[0].is_complete());
        assert!(got[0].partial_days().is_empty());
    }

    /// **The case this module exists for.** A Polymarket venue-backfill restores the trade tape and
    /// nothing else — no book history exists to fetch. Per-kind, neither series looks wrong: trades
    /// are contiguous, and the book series simply has no rows for those days. Joined, the hole is
    /// unmissable, and a market-making backtest over day 11 would otherwise run on no book at all.
    #[test]
    fn a_trades_only_backfill_shows_up_as_a_partial_day() {
        let got = join_coverage(&[
            (trade("polymarket", "TOK"), vec![10, 11, 12]),
            (quote("polymarket", "TOK"), vec![10, 12]),
            (book("polymarket", "TOK"), vec![10, 12]),
        ]);

        let partial = got[0].partial_days();
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].day, 11);
        assert_eq!(
            partial[0].missing_kinds,
            vec!["quote".to_string(), "book".to_string()],
            "in TICK_KINDS order, not the kinds map's alphabetical one"
        );
        assert!(!got[0].is_complete());
    }

    /// A kind that was NEVER recorded is a row with no days, not a missing map entry — a renderer
    /// must not be able to confuse "no book" with "I did not look for book".
    #[test]
    fn a_never_recorded_kind_is_present_as_an_empty_row() {
        let got = join_coverage(&[(trade("binance", "BTCUSDT"), vec![5, 6])]);

        assert_eq!(got[0].kinds.len(), TICK_KINDS.len(), "every kind always appears");
        assert!(got[0].kinds["book"].absent());
        assert!(got[0].kinds["quote"].absent());
        assert!(got[0].kinds["depth"].absent());
        assert!(!got[0].kinds["trade"].absent());
    }

    /// **A trade-only instrument is not "partial" — it is complete at what it records.**
    ///
    /// Held against the global [`TICK_KINDS`] this was partial on EVERY day it would ever have: a
    /// permanent ⚠ carrying no day-to-day information, which is how an operator learns to ignore a
    /// warning column. "There is no book here" is a different fact, already named by
    /// [`KindDays::absent`] and already visible in the Data Manager as the absence of a book row.
    #[test]
    fn an_instrument_recording_one_kind_is_never_partial() {
        let got = join_coverage(&[(trade("binance", "BTCUSDT"), vec![5, 6])]);
        assert_eq!(got[0].recorded_kinds(), vec!["trade"]);
        assert!(got[0].partial_days().is_empty(), "{:?}", got[0].partial_days());
        assert!(got[0].is_complete());
    }

    /// The live case that exposed this: the recorder writes binance L2 to `depth` (the conflated
    /// snapshot lane), never to `book` (the event lane). Both kinds cover both days, so there is
    /// nothing to report — before, this instrument was flagged "missing book, quote" forever.
    #[test]
    fn a_trade_plus_depth_instrument_is_complete() {
        let got = join_coverage(&[
            (trade("binance", "BTCUSDT.P"), vec![5, 6]),
            (depth("binance", "BTCUSDT.P"), vec![5, 6]),
        ]);
        assert_eq!(got[0].recorded_kinds(), vec!["trade", "depth"], "TICK_KINDS order");
        assert!(got[0].is_complete(), "{:?}", got[0].partial_days());
    }

    /// …and the same instrument IS reported the day one of its two lanes stops. This is the signal
    /// the column exists for, and narrowing to recorded kinds does not weaken it.
    #[test]
    fn a_depth_outage_beside_a_live_trade_tape_is_partial() {
        let got = join_coverage(&[
            (trade("binance", "BTCUSDT.P"), vec![5, 6, 7]),
            (depth("binance", "BTCUSDT.P"), vec![5, 7]),
        ]);
        let partial = got[0].partial_days();
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].day, 6);
        assert_eq!(partial[0].missing_kinds, vec!["depth".to_string()]);
    }

    /// Gaps INSIDE one kind's own span are still reported per kind — the join adds the cross-kind
    /// view, it does not replace `series_gaps`.
    #[test]
    fn a_gap_within_one_kind_is_reported_on_that_kind() {
        let got = join_coverage(&[
            (trade("okx", "BTC-USDT"), vec![1, 2, 5]),
            (quote("okx", "BTC-USDT"), vec![1, 2, 5]),
            (book("okx", "BTC-USDT"), vec![1, 2, 5]),
        ]);
        assert_eq!(got[0].kinds["trade"].gaps, vec![(3, 4)]);
        assert_eq!(got[0].kinds["trade"].missing_days(), 2);
        // Days 3-4 are absent from EVERY kind, so they are not in the spanned union and are not
        // "partial" — they are a plain gap, which `gaps` already reports.
        assert!(got[0].is_complete(), "a hole in all three kinds is a gap, not a partial day");
    }

    /// Grouped and per-symbol series of the same name are separate instruments: different
    /// directories, different manifests. Merging them would hide a mid-migration split.
    #[test]
    fn grouped_and_per_symbol_series_do_not_merge() {
        let got = join_coverage(&[
            (SeriesId::per_symbol("trade", "polymarket", "fam", None), vec![1]),
            (SeriesId::grouped("trade", "polymarket", "fam"), vec![2]),
        ]);
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got.iter().any(|c| !c.key.grouped));
        assert!(got.iter().any(|c| c.key.grouped));
    }

    /// Bars are derived from trades, so a missing bar day is an un-run resample, not a hole in the
    /// tape. Including them would train an operator to ignore the report.
    #[test]
    fn non_tick_kinds_are_ignored() {
        let got = join_coverage(&[
            (SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into())), vec![1, 2]),
            (SeriesId::per_symbol("properties", "binance", "BTCUSDT", None), vec![1]),
            (trade("binance", "BTCUSDT"), vec![1, 2]),
        ]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kinds.len(), TICK_KINDS.len(), "only the tick kinds");
        assert!(!got[0].kinds.contains_key("bar"));
        assert!(!got[0].kinds.contains_key("properties"));
    }

    #[test]
    fn instruments_are_venue_scoped() {
        let got = join_coverage(&[
            (trade("binance", "BTCUSDT"), vec![1]),
            (trade("okx", "BTCUSDT"), vec![1]),
        ]);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn a_day_index_converts_to_its_utc_midnight() {
        let p = PartialDay { day: 2, missing_kinds: vec!["book".into()] };
        assert_eq!(p.start_ms(), 2 * 86_400_000);
    }
}
