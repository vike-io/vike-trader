//! The recorder's live tape of a Dukascopy symbol beside the two history lanes (decision 0193): what the
//! `source=recorder` lane keeps the archive safe from, measured against the same tape written into the
//! archive's own leaf. [`Tape::ArchiveLeaf`] is the shape the recorder would write with no lane, so each
//! test holds the damage as numbers and the lane to the store with no tape at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_data::source::FillSource;
use vike_store::{MaintenanceConfig, RetentionPolicy};

use super::archive::{ArchiveDayResult, ArchiveImport};
use super::day_owner::{ArchiveDayClass, DayOwners};
use super::*;
use crate::gapfill::plan_fill;

const SYMBOL: &str = "EURUSD";
/// A settled UTC day: far outside the publication margin.
const DAY: i64 = 19_000 * MS_PER_DAY;
const DAY_END: i64 = DAY + MS_PER_DAY - 1;
const ARCHIVE_TICKS: usize = 600;
const LIVE_TICKS: usize = 600;
/// Enough flushes to reach compaction's default `min_parts` on one date.
const FLUSHES: usize = 5;
const ONE_MINUTE: &str = "1m";

/// Where the recorder's tape of the day goes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tape {
    Absent,
    /// `kind=quote/venue=dukascopy/symbol=EURUSD`, the archive's own leaf.
    ArchiveLeaf,
    /// `kind=quote/venue=dukascopy/source=recorder/symbol=EURUSD`.
    Lane,
}

/// `n` ticks one second apart from `day`, all at one bid, so a read tells the two tapes apart.
fn ticks(day: i64, n: usize, bid: f64) -> Vec<QuoteTick> {
    (0..n as i64)
        .map(|i| QuoteTick {
            ts: day + i * 1_000,
            local_ts: 0,
            bid,
            ask: bid + 0.0001,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        })
        .collect()
}

/// The recorder's tape of `day`, as [`FLUSHES`] commits under the key `RecorderSink` spends.
fn record(hist: &DataFusionHist, tape: Tape, day: i64) {
    let rows = ticks(day, LIVE_TICKS, 1.2);
    for (seq, batch) in rows.chunks(LIVE_TICKS / FLUSHES).enumerate() {
        let (first, last) = (batch[0].ts, batch[batch.len() - 1].ts);
        let key = format!("live-{VENUE}-{SYMBOL}-quote-{first}-{last}-{seq}");
        match tape {
            Tape::Absent => {}
            Tape::ArchiveLeaf => {
                hist.append_quotes(VENUE, SYMBOL, batch, Some(&key)).unwrap();
            }
            Tape::Lane => {
                hist.append_quotes_sourced(VENUE, "recorder", SYMBOL, batch, Some(&key)).unwrap();
            }
        }
    }
}

fn lane_id() -> SeriesId {
    quote_series(SYMBOL).with_source("recorder")
}

fn leaf(hist: &DataFusionHist, lane: Option<&str>) -> PathBuf {
    let venue = hist.root().join("kind=quote").join(format!("venue={VENUE}"));
    match lane {
        Some(lane) => venue.join(format!("source={lane}")).join(format!("symbol={SYMBOL}")),
        None => venue.join(format!("symbol={SYMBOL}")),
    }
}

/// Every file under `dir`, with its bytes: what "the archive's leaf was not touched" is measured on.
fn bytes_under(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(bytes_under(&path));
        } else {
            out.insert(path.clone(), std::fs::read(&path).unwrap());
        }
    }
    out
}

/// What a backtest and a chart read of the symbol: every quote, and every 1m bar as plain numbers.
struct Reads {
    quotes: Vec<QuoteTick>,
    bars: Vec<(i64, f64, f64, f64, f64, f64)>,
}

fn reads(hist: &DataFusionHist) -> Reads {
    let bars = hist.load_bars(VENUE, SYMBOL, ONE_MINUTE, TsRange::all()).unwrap();
    Reads {
        quotes: hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap(),
        bars: bars.iter().map(|b| (b.ts, b.open, b.high, b.low, b.close, b.volume)).collect(),
    }
}

/// The recorder ran through the day, then the operator imports that day's archive file.
fn imported_beside(tape: Tape) -> Reads {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    record(&hist, tape, DAY);
    let mut import =
        ArchiveImport::begin(&hist, SYMBOL, &[ONE_MINUTE.to_string()], vike_model::now_ms())
            .unwrap();
    // A `live-` key owns no day, wherever it sits: which is why a tape in the archive's leaf corrupts it.
    assert_eq!(import.classify(DAY), ArchiveDayClass::Free, "{tape:?}");
    let result = import.import_day(DAY, || Ok(ticks(DAY, ARCHIVE_TICKS, 1.1))).unwrap();
    assert!(matches!(result, ArchiveDayResult::Imported { ticks: ARCHIVE_TICKS, .. }), "{tape:?}");
    drop(import);
    reads(&hist)
}

fn volume(reads: &Reads) -> f64 {
    reads.bars.iter().map(|b| b.5).sum()
}

/// **Replay and bars read byte-identical beside the lane**, and doubled with the tape in the archive's
/// leaf: 600 quotes against 1200, a total bar volume of 600 against 1200, a high from the other tape.
#[test]
fn replay_and_resample_are_byte_identical_beside_the_lane_and_doubled_in_the_same_leaf() {
    let clean = imported_beside(Tape::Absent);
    let lane = imported_beside(Tape::Lane);
    let same_leaf = imported_beside(Tape::ArchiveLeaf);

    assert_eq!(clean.quotes.len(), ARCHIVE_TICKS);
    assert_eq!(volume(&clean), ARCHIVE_TICKS as f64);
    assert_eq!(lane.quotes, clean.quotes, "the quotes a replay reads");
    assert_eq!(lane.bars, clean.bars, "the bars a backtest and a chart read");

    assert_eq!(same_leaf.quotes.len(), ARCHIVE_TICKS + LIVE_TICKS);
    assert_eq!(
        volume(&same_leaf),
        (ARCHIVE_TICKS + LIVE_TICKS) as f64,
        "a bar's volume is its ticks"
    );
    assert!(same_leaf.bars.iter().all(|b| b.2 > 1.2), "each bar's high is the other tape's");
    assert_eq!(clean.bars.iter().map(|b| b.2).fold(0.0, f64::max), 1.10005, "{:?}", clean.bars[0]);
}

/// **A day only the recorder holds is still fetched and stored by the HTTP lane**: its skip reads the
/// sourceless leaf's keys, so the tape spends none of them.
#[test]
fn the_http_lane_fetches_and_stores_a_day_only_the_recorder_holds() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    record(&hist, Tape::Lane, DAY);

    let mut fetched = Vec::new();
    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, ONE_MINUTE, DAY, DAY_END, &|| false, |_, a, b| {
            fetched.push((a, b));
            Ok(ticks(DAY, ARCHIVE_TICKS, 1.1))
        })
        .unwrap();

    assert_eq!(fetched, [(DAY, DAY_END)], "the day was downloaded");
    assert_eq!(bars, 10, "ten minutes of ticks, ten whole bars");
    assert!(
        hist.series_has_commit(&quote_series(SYMBOL), &quote_commit_key(SYMBOL, DAY, DAY_END))
            .unwrap()
    );
    assert_eq!(volume(&reads(&hist)), ARCHIVE_TICKS as f64);
}

/// **A day only the recorder holds is a hole the gap planner plans.** The archive holds days 0 and 2, the
/// lane days 0, 1 and 2. On the fold this replaced the report's quote row was the lane's three days, so
/// the planner saw no hole and day 1 was never fetched; and `classify` agrees that the day is free.
#[test]
fn the_gap_planner_plans_a_day_only_the_recorder_holds() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    for day in [DAY, DAY + 2 * MS_PER_DAY] {
        let end = day + MS_PER_DAY - 1;
        ingest_quotes(&hist, SYMBOL, day, end, &ticks(day, 10, 1.1)).unwrap();
    }
    for day in [DAY, DAY + MS_PER_DAY, DAY + 2 * MS_PER_DAY] {
        record(&hist, Tape::Lane, day);
    }

    let report = hist.coverage_report().unwrap();
    let cov = report.iter().find(|c| c.key.venue == VENUE && c.key.label == SYMBOL).unwrap();
    let hole = DAY / MS_PER_DAY + 1;
    assert_eq!(cov.kinds[&StoreKindId::Quote].present, [hole - 1, hole + 1]);
    assert_eq!(
        plan_fill(cov, FillSource::Venue).days.get(&hole).map(Vec::as_slice),
        Some(&[StoreKindId::Quote][..]),
        "the planner must fetch the day"
    );

    let (_, keys) = day_owner::quote_series_facts(&hist, SYMBOL).unwrap();
    assert_eq!(
        DayOwners::parse(SYMBOL, &keys).classify(hole * MS_PER_DAY, vike_model::now_ms()),
        ArchiveDayClass::Free,
        "and the archive may store it"
    );
}

/// A provisional part of the day, the recorder's flushes of that day, a maintenance pass, then the settled
/// commit: the answer the settled commit got, and how many rows the series reads afterwards.
fn settled_commit_after_maintenance(tape: Tape) -> (Result<usize, vike_data::DataError>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let provisional = provisional_quote_commit_key(SYMBOL, DAY, DAY_END);
    hist.append_quotes(VENUE, SYMBOL, &ticks(DAY, 100, 1.05), Some(&provisional)).unwrap();
    record(&hist, tape, DAY);

    let archive_before = bytes_under(&leaf(&hist, None));
    hist.run_maintenance(&MaintenanceConfig::default()).unwrap();
    if tape == Tape::Lane {
        assert_eq!(
            bytes_under(&leaf(&hist, None)),
            archive_before,
            "the pass left the archive's leaf"
        );
        let lane = hist.series_coverage(&lane_id()).unwrap();
        assert_eq!((lane.parts, lane.rows), (1, LIVE_TICKS as u64), "and compacted the lane");
    }

    let canonical = quote_commit_key(SYMBOL, DAY, DAY_END);
    let answer = hist.append_quotes_superseding(
        VENUE,
        SYMBOL,
        &ticks(DAY, ARCHIVE_TICKS, 1.1),
        Some(&canonical),
        Some(&provisional),
    );
    (answer, hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap().len())
}

/// **Compaction of the lane leaves the archive's leaf byte-identical and the settled commit goes
/// through.** With the tape in the archive's leaf the pass folds the provisional part into a multi-key
/// part, and the settled commit is refused, for good: its documented remedy is deleting the series.
#[test]
fn compacting_the_lane_never_costs_the_settled_commit_its_supersede() {
    let (clean, clean_rows) = settled_commit_after_maintenance(Tape::Absent);
    let (lane, lane_rows) = settled_commit_after_maintenance(Tape::Lane);
    let (same_leaf, same_leaf_rows) = settled_commit_after_maintenance(Tape::ArchiveLeaf);

    assert_eq!(clean.unwrap(), ARCHIVE_TICKS);
    assert_eq!(clean_rows, ARCHIVE_TICKS, "the provisional rows are gone");
    assert_eq!(lane.unwrap(), ARCHIVE_TICKS);
    assert_eq!(lane_rows, ARCHIVE_TICKS);

    assert!(same_leaf.unwrap_err().is_supersede_refusal(), "the settled commit is refused");
    assert_eq!(
        same_leaf_rows,
        100 + LIVE_TICKS,
        "and the series keeps the early fetch and the tape"
    );
}

/// **Pruning the lane touches only the lane; a kind-wide retention prunes the archive exactly as it does
/// with no lane.** Retention by kind has always reached a backfill's history (`retention_for`'s doc), so
/// the claim is that the lane changes nothing about it, not that the archive is exempt.
#[test]
fn retention_of_the_lane_leaves_the_archive_and_a_kind_wide_pass_is_unchanged_by_it() {
    let old = DAY;
    let new = DAY + 10 * MS_PER_DAY;
    let policy = RetentionPolicy { before_ts: Some(new), max_age_ms: None };
    let store = |tape: Tape| {
        let dir = tempfile::tempdir().unwrap();
        let hist = DataFusionHist::open(dir.path()).unwrap();
        for day in [old, new] {
            let end = day + MS_PER_DAY - 1;
            ingest_quotes(&hist, SYMBOL, day, end, &ticks(day, 10, 1.1)).unwrap();
            record(&hist, tape, day);
        }
        (dir, hist)
    };

    let (_dir, hist) = store(Tape::Lane);
    let archive_before = bytes_under(&leaf(&hist, None));
    hist.apply_retention_at(&leaf(&hist, Some("recorder")), &policy).unwrap();
    assert_eq!(bytes_under(&leaf(&hist, None)), archive_before);
    let lane = hist.series_coverage(&lane_id()).unwrap();
    assert_eq!((lane.dates, lane.rows), (1, LIVE_TICKS as u64), "the lane's old day went");

    let pass = MaintenanceConfig {
        retention_by_kind: BTreeMap::from([(StoreKindId::Quote, policy)]),
        ..MaintenanceConfig::default()
    };
    let (_clean_dir, clean) = store(Tape::Absent);
    clean.run_maintenance(&pass).unwrap();
    hist.run_maintenance(&pass).unwrap();
    assert_eq!(reads(&hist).quotes, reads(&clean).quotes);
    assert_eq!(reads(&clean).quotes.len(), 10, "the kind-wide pass pruned the archive's old day");
}
