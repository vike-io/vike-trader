//! The three capped RESEARCH reads — `scan_cohort_capped`, `scan_perp_metrics_capped`,
//! `scan_equity_capped` — and the exec-fill head, `scan_exec_fills_head`, on the real DataFusion
//! backend: each must answer a COMPLETE PREFIX of its unbudgeted read holding AT LEAST its count of
//! rows unless it is the whole range, and must answer it without reading the rest of the range.
//!
//! # Why this file exists
//!
//! The data daemon's `ScanCohort`/`ScanPerpMetrics`/`ScanEquity` read the client's whole range and
//! cut the reply afterwards, so every page of a paged read decoded the rest of the range, and
//! `ScanExecFills` read the whole series
//! (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`, section 1). These
//! four reads are what let the daemon stop early, and this file holds the backend's answer to the
//! two things that make them safe to answer from — the two `crates/vike-data/tests/reads/bars_head.rs`
//! holds for bars, over the same awkward series:
//!
//! 1. **It meets the contract.** [`judge`] compares each bounded answer with the same store's own
//!    unbudgeted read for the same arguments, as a SEQUENCE (order included), over a grid of counts
//!    and of ranges on a series built to be awkward: several UTC days, a re-fetched part overlapping
//!    one of them, a timestamp stored twice inside one part and several across two, ranges that
//!    start in the MIDDLE of a part — the shape a walk that stops after one block fails — and, for
//!    cohort, two rows at EVERY timestamp (two labels of one hour), so a cut that splits a timestamp
//!    shows everywhere. The grid runs again after compaction, when one part holds a timestamp more
//!    than once. Each kind's grid, before and after compaction, is its own test (`per_kind!`), so
//!    nextest spreads them; [`every_kind_has_its_own_grid_tests`] holds that list equal to `KINDS`.
//! 2. **It is NOT a read of the whole range.**
//!    [`a_bounded_read_never_opens_a_part_past_the_one_that_completed_it`] replaces the LAST day's
//!    part with garbage: the unbudgeted read now fails, and a bounded read the earlier days can fill
//!    still answers. Rows alone cannot tell a bounded read from a whole read truncated afterwards —
//!    they return the same rows — but a store that cannot be read whole can.
//!
//! `scan_exec_fills` takes no range, so its oracle is windowed here; the head itself takes one,
//! like every other series read (its doc says why), and runs the same grid.
//!
//! Only compiled/run with `--features hist-datafusion`, like `tests/bars_head.rs`.
#![cfg(feature = "hist-datafusion")]

use std::path::{Path, PathBuf};

use vike_data::{
    CohortRow, CompactionConfig, DataFusionHist, ExecFillRow, HistStore, PerpMetricRow, TsRange,
};
use vike_model::EquitySample;

use crate::common::{DAY_MS, HOUR_MS, open, ranges};

const VENUE: &str = "hyperliquid";
const SYMBOL: &str = "BTC";
/// Timestamps of each day's own part: one every two hours.
const PER_DAY: i64 = 12;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Cohort,
    Perp,
    Equity,
    ExecFill,
}

const KINDS: [Kind; 4] = [Kind::Cohort, Kind::Perp, Kind::Equity, Kind::ExecFill];

impl Kind {
    /// The `kind=` path segment, for compaction and for spoiling a part.
    fn segment(self) -> &'static str {
        match self {
            Kind::Cohort => "cohort",
            Kind::Perp => "perp_metrics",
            Kind::Equity => "equity",
            Kind::ExecFill => "exec_fill",
        }
    }

    /// Stored rows per planted timestamp: a cohort hour is many rows (here two labels), so EVERY
    /// timestamp of that series is a tie a cut could split.
    fn rows_per_ts(self) -> usize {
        match self {
            Kind::Cohort => 2,
            _ => 1,
        }
    }
}

/// One answered row as the judge sees it: its `ts` and its whole `Debug` rendering (every field,
/// floats in their round-trip form), so a sequence comparison sees values AND order.
type Row = (i64, String);

fn rows<T: std::fmt::Debug>(v: Vec<T>, ts: impl Fn(&T) -> i64) -> Vec<Row> {
    v.iter().map(|r| (ts(r), format!("{r:?}"))).collect()
}

/// Append one batch, every `(ts, value)` as `kind`'s row(s), under `key`.
fn append(store: &DataFusionHist, kind: Kind, batch: &[(i64, f64)], key: &str) {
    let key = Some(key);
    let written = match kind {
        Kind::Cohort => {
            let rows: Vec<CohortRow> = batch
                .iter()
                .flat_map(|&(ts, v)| {
                    ["4xWhale", "Shrimp"].into_iter().map(move |label| CohortRow {
                        ts,
                        asset: SYMBOL.into(),
                        axis: "size".into(),
                        cohort: label.into(),
                        grading: "realized".into(),
                        label_basis: "point_in_time".into(),
                        long_usd: v,
                        total_usd: v + 1.0,
                    })
                })
                .collect();
            store.append_cohort(VENUE, SYMBOL, &rows, key).unwrap()
        }
        Kind::Perp => {
            let rows: Vec<PerpMetricRow> = batch
                .iter()
                .map(|&(ts, v)| PerpMetricRow { ts, premium: v, open_interest: None })
                .collect();
            store.append_perp_metrics(VENUE, SYMBOL, &rows, key).unwrap()
        }
        Kind::Equity => {
            let rows: Vec<EquitySample> = batch
                .iter()
                .map(|&(ts, v)| EquitySample {
                    ts,
                    venue: SYMBOL.to_string(),
                    equity: v,
                    realized: 1.0,
                    unrealized: 2.0,
                    missing_prices: 0,
                })
                .collect();
            store.append_equity(VENUE, SYMBOL, &rows, key).unwrap()
        }
        Kind::ExecFill => {
            let rows: Vec<ExecFillRow> = batch
                .iter()
                .map(|&(ts, v)| ExecFillRow {
                    ts,
                    trade_id: format!("t{ts}-{v}"),
                    client_order_id: "c1".to_string(),
                    venue: VENUE.to_string(),
                    symbol: SYMBOL.to_string(),
                    side: 1,
                    qty: 0.5,
                    px: v,
                    commission: 0.01,
                    mark_price: None,
                    liquidity_side: "maker".to_string(),
                    commission_asset: "USDC".to_string(),
                })
                .collect();
            store.append_exec_fills(VENUE, SYMBOL, &rows, key).unwrap()
        }
    };
    assert_eq!(written, batch.len() * kind.rows_per_ts(), "{kind:?}: one row per planted ts");
}

/// The unbudgeted read — the oracle every bounded answer is judged against.
fn full(store: &DataFusionHist, kind: Kind, range: TsRange) -> Result<Vec<Row>, String> {
    let e = |e: vike_data::DataError| e.to_string();
    Ok(match kind {
        Kind::Cohort => rows(store.scan_cohort(VENUE, SYMBOL, range).map_err(e)?, |r| r.ts),
        Kind::Perp => rows(store.scan_perp_metrics(VENUE, SYMBOL, range).map_err(e)?, |r| r.ts),
        Kind::Equity => rows(store.scan_equity(VENUE, SYMBOL, range).map_err(e)?, |r| r.ts),
        // The exec read takes no range, so the oracle windows it here — written out rather than
        // taken from the trait default the head's own default is built on.
        Kind::ExecFill => {
            let mut all = store.scan_exec_fills(VENUE, SYMBOL).map_err(e)?;
            all.retain(|r| {
                range.start.is_none_or(|lo| r.ts >= lo) && range.end.is_none_or(|hi| r.ts <= hi)
            });
            rows(all, |r| r.ts)
        }
    })
}

/// The bounded read under test: a `Some(n)` budget for the three capped reads, the count `n` for
/// the exec-fill head.
fn bounded(
    store: &DataFusionHist,
    kind: Kind,
    range: TsRange,
    n: usize,
) -> Result<Vec<Row>, String> {
    let e = |e: vike_data::DataError| e.to_string();
    let b = Some(n);
    Ok(match kind {
        Kind::Cohort => {
            rows(store.scan_cohort_capped(VENUE, SYMBOL, range, b).map_err(e)?, |r| r.ts)
        }
        Kind::Perp => {
            rows(store.scan_perp_metrics_capped(VENUE, SYMBOL, range, b).map_err(e)?, |r| r.ts)
        }
        Kind::Equity => {
            rows(store.scan_equity_capped(VENUE, SYMBOL, range, b).map_err(e)?, |r| r.ts)
        }
        Kind::ExecFill => {
            rows(store.scan_exec_fills_head(VENUE, SYMBOL, range, n).map_err(e)?, |r| r.ts)
        }
    })
}

/// The awkward series, as parts — `crates/vike-data/tests/reads/bars_head.rs`'s `plant`, per kind:
///
/// * days 0..=3, one part each, `PER_DAY` timestamps every two hours (value = hour + 100 · day);
/// * day 1 RE-FETCHED over 06:00..=16:00 under a second key — a second part of the same date whose
///   span sits INSIDE the first one's, every one of its six timestamps stored a second time with a
///   different value, so the ORDER of a timestamp's rows is observable;
/// * day 2's own batch carrying 04:00 twice, so one part holds a timestamp twice from the start.
///
/// 55 planted timestamps in all (48 + 6 + 1), times [`Kind::rows_per_ts`].
fn plant(store: &DataFusionHist, kind: Kind) -> usize {
    for day in 0..4i64 {
        let mut batch: Vec<(i64, f64)> = (0..PER_DAY)
            .map(|k| (day * DAY_MS + 2 * k * HOUR_MS, (2 * k + 100 * day) as f64))
            .collect();
        if day == 2 {
            batch.push((2 * DAY_MS + 4 * HOUR_MS, 999.0));
        }
        append(store, kind, &batch, &format!("day{day}"));
    }
    let refetch: Vec<(i64, f64)> =
        (3..=8i64).map(|k| (DAY_MS + 2 * k * HOUR_MS, (2 * k + 100) as f64 + 0.5)).collect();
    append(store, kind, &refetch, "day1-refetch");
    let all = full(store, kind, TsRange::all()).unwrap();
    assert_eq!(all.len(), 55 * kind.rows_per_ts(), "{kind:?}: the fixture must hold its doc");
    all.len()
}

/// The contract, judged against an already-read `full`. Written out here rather than taken from
/// any production helper, so a defect in the walk cannot be repeated in the thing that judges it.
fn judge(at: &str, head: &[Row], full: &[Row], n: usize) {
    assert!(head.len() <= full.len(), "{at}: a bounded answer cannot hold rows the range does not");
    assert_eq!(head, &full[..head.len()], "{at}: a PREFIX of the whole read, rows and order alike");
    if let (Some(last), Some(next)) = (head.last(), full.get(head.len())) {
        assert!(
            next.0 > last.0,
            "{at}: COMPLETE — the cut split ts {}, so a caller continuing past it loses rows",
            last.0
        );
    }
    assert!(
        head.len() >= n || head.len() == full.len(),
        "{at}: AT LEAST n — {} rows of {} while the range goes on; a pager reads a short or EMPTY \
         answer as the end",
        head.len(),
        full.len()
    );
}

const NS: [usize; 22] =
    [1, 2, 3, 5, 7, 11, 12, 13, 17, 20, 24, 25, 30, 43, 54, 55, 56, 86, 109, 110, 111, usize::MAX];

fn grid(store: &DataFusionHist, kind: Kind) {
    for range in ranges() {
        let whole = full(store, kind, range).unwrap();
        for n in NS {
            let head = bounded(store, kind, range, n).unwrap();
            judge(&format!("{kind:?}, range {range:?}, n {n}"), &head, &whole, n);
        }
    }
}

#[test]
fn an_unknown_series_has_an_empty_bounded_answer() {
    let (_dir, store) = open();
    for kind in KINDS {
        for n in [1, 100] {
            assert!(bounded(&store, kind, TsRange::all(), n).unwrap().is_empty(), "{kind:?}");
        }
    }
}

/// The grid over one kind's own planted series. One test per kind (`per_kind!` below), so nextest
/// spreads the four grids instead of running them as one long test.
fn bounded_reads_meet_the_contract(kind: Kind) {
    let (_dir, store) = open();
    plant(&store, kind);
    grid(&store, kind);
}

/// The capped reads' reading of a budget, which is not the head's reading of a count: `None` and
/// `Some(0)` are the WHOLE range for the three capped reads (the `scan_quotes_capped` contract),
/// while a head of `0` is nothing.
#[test]
fn no_budget_is_the_whole_range_and_a_head_of_nothing_is_empty() {
    for kind in [Kind::Cohort, Kind::Perp, Kind::Equity] {
        let (_dir, store) = open();
        let total = plant(&store, kind);
        let whole = full(&store, kind, TsRange::all()).unwrap();
        assert_eq!(bounded(&store, kind, TsRange::all(), 0).unwrap(), whole, "{kind:?}: Some(0)");
        let none = match kind {
            Kind::Cohort => {
                store.scan_cohort_capped(VENUE, SYMBOL, TsRange::all(), None).unwrap().len()
            }
            Kind::Perp => {
                store.scan_perp_metrics_capped(VENUE, SYMBOL, TsRange::all(), None).unwrap().len()
            }
            Kind::Equity => {
                store.scan_equity_capped(VENUE, SYMBOL, TsRange::all(), None).unwrap().len()
            }
            Kind::ExecFill => unreachable!(),
        };
        assert_eq!(none, total, "{kind:?}: None");
    }
    let (_dir, store) = open();
    plant(&store, Kind::ExecFill);
    assert!(store.scan_exec_fills_head(VENUE, SYMBOL, TsRange::all(), 0).unwrap().is_empty());
}

/// The shapes the grid covers, pinned in ABSOLUTE numbers too, so a walk and a judge that were
/// wrong the same way would still fail.
#[test]
fn the_bounded_answer_is_the_shape_the_contract_names() {
    let h = HOUR_MS;
    for kind in KINDS {
        let (_dir, store) = open();
        plant(&store, kind);
        let r = kind.rows_per_ts();
        // From the middle of day 0: two of day 0's timestamps, short of five, so the walk goes on
        // and takes ALL of day 1 — its own part and the re-fetch overlap and come as one block.
        let range = TsRange { start: Some(20 * h), end: None };
        let head = bounded(&store, kind, range, 5 * r).unwrap();
        assert_eq!(head.len(), (2 + 18) * r, "{kind:?}: two of day 0, then all of day 1");
        assert_eq!(head.first().map(|x| x.0), Some(20 * h), "{kind:?}");
        assert_eq!(head.last().map(|x| x.0), Some(DAY_MS + 22 * h), "{kind:?}");
        // One on the twice-stored timestamp of day 2 holds BOTH of its stamps.
        let range = TsRange { start: Some(2 * DAY_MS + 4 * h), end: None };
        let head = bounded(&store, kind, range, 1).unwrap();
        assert!(head.len() >= 2 * r, "{kind:?}: a ts is never split: {head:?}");
        assert!(head[..2 * r].iter().all(|x| x.0 == 2 * DAY_MS + 4 * h), "{kind:?}");
        // A count of one over the whole series is day 0's part: the first block, whole.
        let head = bounded(&store, kind, TsRange::all(), 1).unwrap();
        assert_eq!(head.len(), PER_DAY as usize * r, "{kind:?}: one block, day 0's part");
        // A count wider than the series answers all of it, and says so by being short.
        assert_eq!(bounded(&store, kind, TsRange::all(), 1_000).unwrap().len(), 55 * r, "{kind:?}");
    }
}

/// Maintenance merges day 1's two parts into one, which then holds six timestamps twice — the case
/// a row-cutting `LIMIT` could split and this read must not. One test per kind (`per_kind!` below).
fn the_contract_survives_compaction_for(kind: Kind) {
    let (_dir, store) = open();
    plant(&store, kind);
    let whole = full(&store, kind, TsRange::all()).unwrap();
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let report = store.compact_series(kind.segment(), VENUE, SYMBOL, None, &cfg).unwrap();
    assert_eq!(
        report.parts_merged, 2,
        "{kind:?}: day 1's two parts must merge, or this proves nothing"
    );
    assert_eq!(
        full(&store, kind, TsRange::all()).unwrap(),
        whole,
        "{kind:?} control: compaction keeps every row, duplicates and their order included"
    );
    grid(&store, kind);
}

/// One `#[test]` per `(grid, kind)`, and the list of them as data — `PER_KIND_TESTS` — so
/// [`every_kind_has_its_own_grid_tests`] can hold it equal to [`KINDS`] for BOTH grids: a kind added
/// to `KINDS`, or a line deleted here, fails that test rather than going unread.
macro_rules! per_kind {
    ($($test:ident => $f:ident($kind:expr)),* $(,)?) => {
        const PER_KIND_TESTS: &[(&str, Kind)] = &[$((stringify!($f), $kind)),*];
        $(
            #[test]
            fn $test() {
                $f($kind);
            }
        )*
    };
}

per_kind! {
    every_bounded_cohort_read_meets_the_contract => bounded_reads_meet_the_contract(Kind::Cohort),
    every_bounded_perp_read_meets_the_contract => bounded_reads_meet_the_contract(Kind::Perp),
    every_bounded_equity_read_meets_the_contract => bounded_reads_meet_the_contract(Kind::Equity),
    every_bounded_exec_fill_read_meets_the_contract => bounded_reads_meet_the_contract(Kind::ExecFill),
    the_cohort_contract_survives_compaction => the_contract_survives_compaction_for(Kind::Cohort),
    the_perp_contract_survives_compaction => the_contract_survives_compaction_for(Kind::Perp),
    the_equity_contract_survives_compaction => the_contract_survives_compaction_for(Kind::Equity),
    the_exec_fill_contract_survives_compaction => the_contract_survives_compaction_for(Kind::ExecFill),
}

#[test]
fn every_kind_has_its_own_grid_tests() {
    // `KINDS` is the file's own roster; a kind added there must get its two lines above.
    for family in ["bounded_reads_meet_the_contract", "the_contract_survives_compaction_for"] {
        let tested: Vec<Kind> =
            PER_KIND_TESTS.iter().filter(|(f, _)| *f == family).map(|(_, k)| *k).collect();
        assert_eq!(tested.len(), KINDS.len(), "{family}: one test per kind, no more: {tested:?}");
        for kind in KINDS {
            assert!(tested.contains(&kind), "{family}: {kind:?} has no test in `per_kind!`");
        }
    }
    assert_eq!(
        PER_KIND_TESTS.len(),
        2 * KINDS.len(),
        "every `per_kind!` line names one of the grids"
    );
}

/// The one observable difference between a bounded read and a whole read cut down afterwards: the
/// bounded read never opens a part past the block that completed its count.
///
/// The LAST day's part is replaced, in place and under the same name, by bytes that are not Parquet
/// at all. The unbudgeted read cannot survive it — the control below proves that — so a bounded read
/// the first three days can fill must answer without having opened it, and one that needs the
/// fourth day must fail, which proves the part IS read when the count reaches it.
#[test]
fn a_bounded_read_never_opens_a_part_past_the_one_that_completed_it() {
    for kind in KINDS {
        let (dir, store) = open();
        plant(&store, kind);
        let r = kind.rows_per_ts();
        let whole = full(&store, kind, TsRange::all()).unwrap();
        let mid = TsRange { start: Some(20 * HOUR_MS), end: None };
        let mid_whole = full(&store, kind, mid).unwrap();

        spoil(&last_day_part(dir.path(), kind));

        assert!(
            full(&store, kind, TsRange::all()).is_err(),
            "{kind:?} control: the spoiled part must defeat a whole-range read, or nothing below \
             proves a bound"
        );
        // Days 0..=2 hold 12 + 18 + 13 = 43 timestamps: any count up to that is answered from them.
        for ts_n in [1, 5, 12, 13, 30, 31, 43] {
            let n = ts_n * r;
            let head = bounded(&store, kind, TsRange::all(), n).unwrap_or_else(|e| {
                panic!("{kind:?}, n {n}: a read the first three days fill must answer: {e}")
            });
            judge(&format!("{kind:?} spoiled, n {n}"), &head, &whole, n);
            assert!(
                head.iter().all(|x| x.0 < 3 * DAY_MS),
                "{kind:?}, n {n}: no row of the spoiled day"
            );
        }
        // ...and starting in the middle of day 0, which takes more than one block to fill.
        let head = bounded(&store, kind, mid, 25 * r).unwrap();
        judge(&format!("{kind:?} spoiled, mid"), &head, &mid_whole, 25 * r);
        assert!(
            bounded(&store, kind, TsRange::all(), 43 * r + 1).is_err(),
            "{kind:?} control: a read that NEEDS the fourth day must read it — the bound is the \
             count, not a fixed set of parts"
        );
    }
}

/// The one part of the LAST day the fixture writes — day 3 is appended once, so it is one part.
fn last_day_part(root: &Path, kind: Kind) -> PathBuf {
    let day = root
        .join(format!("kind={}", kind.segment()))
        .join(format!("venue={VENUE}"))
        .join(format!("symbol={SYMBOL}"))
        .join("date=1970-01-04");
    let mut parts: Vec<PathBuf> = std::fs::read_dir(&day)
        .unwrap_or_else(|e| panic!("{}: {e}", day.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
        .collect();
    assert_eq!(parts.len(), 1, "{kind:?}: one append on day 3 seals one part: {parts:?}");
    parts.pop().unwrap()
}

/// Overwrite a part with bytes no Parquet reader accepts.
fn spoil(path: &Path) {
    std::fs::write(path, b"this is not a parquet file").unwrap();
}
