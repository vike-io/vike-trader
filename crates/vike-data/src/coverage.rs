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

// ── coverage of a REQUESTED WINDOW ───────────────────────────────────────────────────────────────
//
// Everything above answers "what does this store hold". This half answers the question a RUN asks,
// which is a different one: "does it hold what I am about to read". Nothing in the tree could
// answer it, and the reason it looks as though something already could is [`find_gaps`] — whose
// holes are strictly INSIDE the recorded span. A window opening a month before a series' first row
// has no gap by that definition, and that silence is the failure this half exists to end: a
// backtest over a window the tape does not reach loads fewer rows, runs to completion and reports
// fills.

/// WHERE a requested window went unanswered — and the distinction is the whole value of the type.
///
/// [`Shortfall::Leading`]/[`Shortfall::Trailing`] are EXISTENCE facts: the series does not reach
/// back to (or forward to) what was asked for, which for an instrument series is a listing or a
/// delisting. [`Shortfall::Interior`] is a COMPLETENESS fact: the tape reaches both ends and has
/// holes in it, which is a recorder outage or a backfill nobody finished. They call for different
/// actions — move the window versus fetch the missing days — and an operator handed one number for
/// both cannot tell which they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shortfall {
    /// The window opens before the series' first recorded row.
    Leading,
    /// A hole strictly inside the recorded span, clipped to the window.
    Interior,
    /// The window closes after the series' last recorded row.
    Trailing,
    /// The store holds no such series at all, so the whole window is missing. A DIFFERENT answer
    /// from a leading plus a trailing shortfall meeting in the middle: "never recorded" and
    /// "recorded outside your window" are the two states [`KindDays::absent`] already separates,
    /// and the cure for the first is a collector rather than a backfill.
    Everything,
}

impl Shortfall {
    /// The word a plan line or a refusal sentence uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Shortfall::Leading => "leading",
            Shortfall::Interior => "interior",
            Shortfall::Trailing => "trailing",
            Shortfall::Everything => "everything",
        }
    }
}

/// One inclusive epoch-ms span a caller asked for and the store does not hold.
///
/// Inclusive on both ends — the convention [`crate::SeriesCoverage`]'s `first_ts`/`last_ts` and
/// [`crate::TsRange`] both carry, so a reader never has to ask which of the three this one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingSpan {
    pub start_ms: i64,
    pub end_ms: i64,
    pub kind: Shortfall,
}

impl MissingSpan {
    /// Span length in ms, inclusive of both ends.
    ///
    /// Saturating, because an [`Shortfall::Everything`] span over an unbounded window is
    /// `i64::MIN..=i64::MAX` and a plain subtraction there panics in debug and wraps in release.
    /// That is the one arithmetic in this file which can reach the type's limits, and it reaches
    /// them on the most ordinary input there is: a profile naming a lane that was never recorded,
    /// with no `from`/`to`.
    pub fn len_ms(&self) -> i64 {
        self.end_ms.saturating_sub(self.start_ms).saturating_add(1)
    }
}

/// What a requested window asked for and one series does not hold — the leading shortfall, the
/// interior holes clipped to the window, and the trailing shortfall, in time order.
///
/// # The rule that makes this honest: an UNBOUNDED window side can never be short
///
/// `window_start: None` means "whatever the store holds", so there is nothing that side asked for
/// and did not get. Reporting the whole of time as leading shortfall for a profile with no
/// `data.from` would make the answer useless on exactly the profiles that are easiest to write,
/// and it would also be false — nobody asked for 1970. The same holds for `window_end`. That is why
/// both bounds are `Option` here rather than the `i64::MIN`/`MAX` fallbacks a [`crate::TsRange`] is
/// usually flattened into at a comparison site: the flattening cannot express the distinction at
/// all.
///
/// # `recorded: None` is not `recorded: Some((0, 0))`
///
/// A store that holds no such series yields one [`Shortfall::Everything`] span; a series that
/// exists holding rows at the epoch yields leading/trailing spans against them. Those two look
/// alike once folded into a millisecond total, and they are different findings — the same lesson
/// `crates/vike-data/src/datafusion_hist.rs`'s `series_dir_of` records, wearing a second shape: an
/// absent manifest folds to an all-zero [`crate::SeriesCoverage`], so a caller that passes
/// `Some(coverage)` unconditionally reports every never-recorded lane as a 1970 tape. Decide
/// presence from the store's INVENTORY and pass `None`.
///
/// # Total, never fallible
///
/// An inverted window (`start > end`) asked for nothing, so nothing is missing: `vec![]`. The
/// refusal for an inverted window belongs to whoever built it — the backtest profile's own
/// `data.from <= data.to` rule already owns that sentence — and a second refusal here would report
/// a window mistake as a data mistake.
///
/// `interior_gaps` is [`crate::HistStore::series_gaps`]'s answer verbatim. Order and overlap are
/// not assumed: each is clipped to the window independently, and one falling entirely outside it is
/// dropped.
pub fn window_shortfall(
    window_start: Option<i64>,
    window_end: Option<i64>,
    recorded: Option<(i64, i64)>,
    interior_gaps: &[(i64, i64)],
) -> Vec<MissingSpan> {
    // An inverted window asked for nothing. Checked before `recorded` is consulted, so a profile
    // with `from > to` gets ONE refusal (its own) rather than that one plus a data finding.
    if let (Some(a), Some(b)) = (window_start, window_end)
        && a > b
    {
        return Vec::new();
    }
    let whole_window = || {
        vec![MissingSpan {
            start_ms: window_start.unwrap_or(i64::MIN),
            end_ms: window_end.unwrap_or(i64::MAX),
            kind: Shortfall::Everything,
        }]
    };
    let Some((first, last)) = recorded else {
        return whole_window();
    };
    // A series whose own span is inverted cannot be reasoned about — `first > last` is what a
    // partially-written manifest looks like — so it is reported as wholly missing rather than as a
    // shortfall of negative length.
    if first > last {
        return whole_window();
    }
    let mut out = Vec::new();
    if let Some(start) = window_start
        && first > start
    {
        out.push(MissingSpan {
            start_ms: start,
            // `first - 1`: the span is INCLUSIVE and `first` itself is present. Saturating for the
            // reason [`MissingSpan::len_ms`] is.
            end_ms: first.saturating_sub(1),
            kind: Shortfall::Leading,
        });
    }
    for (a, b) in interior_gaps {
        let (mut a, mut b) = (*a, *b);
        if let Some(start) = window_start {
            a = a.max(start);
        }
        if let Some(end) = window_end {
            b = b.min(end);
        }
        if a <= b {
            out.push(MissingSpan { start_ms: a, end_ms: b, kind: Shortfall::Interior });
        }
    }
    if let Some(end) = window_end
        && last < end
    {
        // ⚠ CLAMPED to the window, exactly as the interior arm above is — and unlike the leading
        // arm, which is clamped by construction because it STARTS at `window_start`. The recorded
        // span may begin before the window (a series holding one row at the epoch against a
        // window opening later is the worked case), and `last + 1` alone then reports time
        // OUTSIDE the window as missing — a finding about data nobody asked for, which reads to a
        // caller as a much larger hole than the window actually has.
        let start_ms = match window_start {
            Some(start) => last.saturating_add(1).max(start),
            None => last.saturating_add(1),
        };
        out.push(MissingSpan { start_ms, end_ms: end, kind: Shortfall::Trailing });
    }
    out
}

/// Total missing milliseconds across `spans`, saturating.
///
/// A SUM rather than a `max` because the question a coverage gate asks is how much of the window is
/// absent, not how big the worst hole is — and a tape with fifty one-hour holes is worse than one
/// with a single two-hour hole, which a `max` would rank the other way round. A gate on the LARGEST
/// hole is a different and equally legitimate rule, reachable as
/// `spans.iter().map(MissingSpan::len_ms).max()`; this function deliberately does not pick between
/// them, and a caller that wants the other one says so at its own site.
pub fn missing_ms(spans: &[MissingSpan]) -> i64 {
    spans.iter().fold(0i64, |acc, s| acc.saturating_add(s.len_ms()))
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

    // ---- window_shortfall ------------------------------------------------------------------------

    fn span(start_ms: i64, end_ms: i64, kind: Shortfall) -> MissingSpan {
        MissingSpan { start_ms, end_ms, kind }
    }

    /// The case `find_gaps` is structurally blind to, and the reason this function exists: the tape
    /// is perfectly contiguous and still does not reach what the window asked for.
    #[test]
    fn a_contiguous_tape_that_starts_late_is_a_leading_shortfall() {
        let got = window_shortfall(Some(100), Some(900), Some((500, 900)), &[]);
        assert_eq!(got, vec![span(100, 499, Shortfall::Leading)]);
        assert_eq!(missing_ms(&got), 400);
    }

    #[test]
    fn a_tape_that_ends_early_is_a_trailing_shortfall() {
        let got = window_shortfall(Some(100), Some(900), Some((100, 500)), &[]);
        assert_eq!(got, vec![span(501, 900, Shortfall::Trailing)]);
    }

    #[test]
    fn a_fully_covered_window_is_short_of_nothing() {
        assert!(window_shortfall(Some(100), Some(900), Some((50, 1000)), &[]).is_empty());
        assert!(window_shortfall(Some(100), Some(900), Some((100, 900)), &[]).is_empty());
    }

    /// The load-bearing rule: an unbounded side ASKED for nothing, so it cannot be short. Without
    /// this, every profile with no `data.from` reports the whole of time as missing.
    #[test]
    fn an_unbounded_window_side_is_never_short() {
        assert!(window_shortfall(None, None, Some((500, 600)), &[]).is_empty());
        assert_eq!(
            window_shortfall(None, Some(900), Some((500, 600)), &[]),
            vec![span(601, 900, Shortfall::Trailing)]
        );
        assert_eq!(
            window_shortfall(Some(100), None, Some((500, 600)), &[]),
            vec![span(100, 499, Shortfall::Leading)]
        );
    }

    /// An ABSENT series and an all-zero one are different findings, and the type says which.
    #[test]
    fn an_absent_series_is_everything_not_a_leading_plus_trailing_pair() {
        assert_eq!(
            window_shortfall(Some(100), Some(900), None, &[]),
            vec![span(100, 900, Shortfall::Everything)]
        );
        // The same window against a series that really does hold one row at the epoch reads as the
        // two EXISTENCE facts instead — which is the answer `series_dir_of`'s all-zero manifest
        // fold would otherwise have impersonated.
        assert_eq!(
            window_shortfall(Some(100), Some(900), Some((0, 0)), &[]),
            vec![span(100, 900, Shortfall::Trailing)]
        );
    }

    #[test]
    fn an_absent_series_over_an_unbounded_window_does_not_overflow() {
        let got = window_shortfall(None, None, None, &[]);
        assert_eq!(got, vec![span(i64::MIN, i64::MAX, Shortfall::Everything)]);
        // Saturating rather than panicking in debug / wrapping in release.
        assert_eq!(got[0].len_ms(), i64::MAX);
        assert_eq!(missing_ms(&got), i64::MAX);
    }

    #[test]
    fn interior_gaps_are_clipped_to_the_window_and_ones_outside_it_are_dropped() {
        let got = window_shortfall(
            Some(200),
            Some(800),
            Some((100, 900)),
            &[(120, 250), (400, 450), (850, 880)],
        );
        assert_eq!(
            got,
            vec![span(200, 250, Shortfall::Interior), span(400, 450, Shortfall::Interior)],
            "the first gap is clipped to the window's start and the last falls outside it"
        );
    }

    #[test]
    fn all_three_shortfalls_come_back_in_time_order() {
        let got = window_shortfall(Some(0), Some(1000), Some((100, 900)), &[(400, 500)]);
        assert_eq!(
            got,
            vec![
                span(0, 99, Shortfall::Leading),
                span(400, 500, Shortfall::Interior),
                span(901, 1000, Shortfall::Trailing),
            ]
        );
    }

    /// An inverted window is the PROFILE's mistake and already has its own refusal; reporting it
    /// again here would name a data fault that does not exist.
    #[test]
    fn an_inverted_window_asked_for_nothing() {
        assert!(window_shortfall(Some(900), Some(100), None, &[(1, 2)]).is_empty());
        assert!(window_shortfall(Some(900), Some(100), Some((0, 10)), &[]).is_empty());
    }

    #[test]
    fn an_inverted_recorded_span_is_reported_as_wholly_missing() {
        assert_eq!(
            window_shortfall(Some(100), Some(900), Some((900, 100)), &[]),
            vec![span(100, 900, Shortfall::Everything)]
        );
    }
}
