//! Per-captured-day DATA-QUALITY scoring — "which recorded UTC days are backtest-trustworthy?".
//!
//! A pure, READ-ONLY fold over already-scanned rows of one recorded series into a per-(venue,
//! symbol, UTC-day) [`DayQuality`] record. It writes NOTHING and changes no stored data — a
//! [`crate::DataFusionHist`] scored twice yields two identical reports and an untouched store — so
//! it is byte-identical to a plain read. There is no hot-path work here: it is an offline report
//! over a `Vec` the caller already has.
//!
//! Three trust signals fold in, mirroring how the live recorder discloses stream health
//! (`crate::live_rec`) and how day-level presence gaps are already derived
//! (`crate::datafusion_hist::gaps`):
//!
//! - §B status markers (BOOK lane). The recorder persists `GapStart`/`Stale`/`LiveResume` inline in
//!   `kind=book` as zero-level [`vike_model::BookUpdate`]s (see `live_rec::stream_status`). We count
//!   the `GapStart`/`Stale` onsets and total the disclosed outage duration (onset → next
//!   `LiveResume`; an outage still open at day's end extends to the end of the UTC day).
//! - Seq resets (BOOK lane). The recorded book chain carries a per-feed contiguous `seq`; a
//!   REGRESSION (a later data event whose `seq` is smaller — a venue restart that reset the counter,
//!   per `vike_marketdata::orderbook`'s `SeqPolicy::Strict`) is a resync boundary. Status markers carry
//!   `seq == 0` and are excluded from the chain.
//! - Intra-day coverage (ALL lanes). Quote/trade gap provenance is NOT recorded (a noted follow-up
//!   in `live_rec`), so for those lanes a hole can only be INFERRED from inter-row timestamp deltas.
//!   The same inference runs on the book lane too (the §B markers corroborate it). Coverage is a
//!   binary per-segment partition of the UTC day `[day_start, day_start + 24h)`: day start and
//!   day end are coverage boundaries, and any segment between consecutive observations STRICTLY
//!   longer than [`QualityConfig::max_gap_ms`] counts its WHOLE duration as uncovered (a hole);
//!   every shorter segment is normal cadence and fully covered. So `covered + uncovered == 24h`
//!   exactly, and a day sampled within cadence across its full span scores `gap_pct == 0`.
//!
//! The §B disclosed-gap total and the inferred coverage hole are reported SEPARATELY (they are two
//! views of the same outage and would double-count if summed): `gap_pct`/`coverage_fraction` derive
//! from the inferred coverage only, while `disclosed_gap_ms` is the venue-disclosed corroboration.

use std::collections::BTreeMap;

use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use vike_model::time::epoch_ms_to_utc_date;

/// Epoch-ms per UTC day (matches [`vike_model::time::epoch_ms_to_utc_date`]'s day-floor).
const DAY_MS: i64 = 86_400_000;

/// Which recorded lane a [`DayQuality`] scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QualityLane {
    /// `kind=book` — carries §B status markers + a seq chain (all signals populated).
    Book,
    /// `kind=quote` — coverage inferred from row timestamps only.
    Quote,
    /// `kind=trade` — coverage inferred from row timestamps only.
    Trade,
}

/// Tunable scoring thresholds. Cheap to `Copy`; construct with [`QualityConfig::default`] or a
/// literal.
#[derive(Debug, Clone, Copy)]
pub struct QualityConfig {
    /// Inter-row spacing STRICTLY beyond which a segment is a coverage GAP (a hole) rather than
    /// normal cadence. Applies to every lane's inter-row coverage inference.
    pub max_gap_ms: i64,
}

impl Default for QualityConfig {
    fn default() -> Self {
        // 1 minute: on an active recorded feed, a full minute with no row is a real hole; tune per
        // instrument (a sparse market wants a larger window).
        Self { max_gap_ms: 60_000 }
    }
}

/// A per-(venue, symbol, UTC-day) data-quality record — one row of the trustworthiness table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayQuality {
    pub venue: String,
    pub symbol: String,
    pub lane: QualityLane,
    /// UTC day key (`YYYY-MM-DD`), same partition key the store's `date=` dirs use.
    pub day: String,
    /// Rows scanned for this day (data events + §B markers for the book lane).
    pub rows: usize,
    /// First / last row ts seen in the day (`None` for an empty day).
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    /// The longest single uncovered stretch in the day (0 when nothing exceeds `max_gap_ms`).
    pub largest_gap_ms: i64,
    /// Total uncovered ms across the whole UTC day (Σ of segments longer than `max_gap_ms`).
    pub uncovered_ms: i64,
    /// `covered / 24h`, in `[0, 1]`.
    pub coverage_fraction: f64,
    /// `uncovered / 24h * 100` — the headline gap %.
    pub gap_pct: f64,
    /// §B `GapStart` markers observed (book lane; 0 otherwise).
    pub gap_start_count: usize,
    /// §B `Stale` markers observed (book lane; 0 otherwise).
    pub stale_count: usize,
    /// Total venue-disclosed outage duration (§B onset → resume; book lane; 0 otherwise). Reported
    /// alongside — NOT summed into `gap_pct` — since it corroborates the same hole `uncovered_ms`
    /// already infers.
    pub disclosed_gap_ms: i64,
    /// Seq-chain regressions over the book data events (book lane; 0 otherwise).
    pub seq_resets: usize,
}

impl DayQuality {
    /// True when nothing reduced trust: full inferred intra-day coverage, no disclosed §B outage,
    /// and no seq reset.
    pub fn is_perfect(&self) -> bool {
        self.uncovered_ms == 0 && self.disclosed_gap_ms == 0 && self.seq_resets == 0
    }
}

/// Floor `ts` to the start (`00:00:00.000` UTC) of its UTC day. `div_euclid` floors toward
/// negative infinity, so pre-epoch timestamps bucket correctly too.
pub fn utc_day_start_ms(ts: i64) -> i64 {
    ts.div_euclid(DAY_MS) * DAY_MS
}

/// The book-lane-only trust signals, bundled so [`assemble`] stays inside clippy's argument budget.
#[derive(Debug, Default, Clone, Copy)]
struct Disclosure {
    gap_start_count: usize,
    stale_count: usize,
    disclosed_gap_ms: i64,
    seq_resets: usize,
}

/// `(largest single hole, total uncovered ms)` over the whole UTC day `[day_start, day_start+24h)`.
/// day start and day end are coverage boundaries; a segment STRICTLY longer than `max_gap_ms`
/// counts its whole duration as uncovered (binary per-segment). Timestamps are clamped into the day
/// and sorted internally, so out-of-order or slightly-out-of-range input never distorts the walk.
fn day_coverage(day_start_ms: i64, timestamps: &[i64], max_gap_ms: i64) -> (i64, i64) {
    let day_end = day_start_ms.saturating_add(DAY_MS);
    let mut sorted: Vec<i64> = timestamps.iter().map(|&t| t.clamp(day_start_ms, day_end)).collect();
    sorted.sort_unstable();

    let mut largest = 0i64;
    let mut uncovered = 0i64;
    let mut prev = day_start_ms;
    // interior + leading segments, then the trailing segment to day end
    for &cur in sorted.iter().chain(std::iter::once(&day_end)) {
        let seg = cur - prev; // >= 0: clamped into [day_start, day_end] and sorted ascending
        if seg > max_gap_ms {
            uncovered += seg;
            largest = largest.max(seg);
        }
        prev = cur;
    }
    (largest, uncovered)
}

/// Build a [`DayQuality`] from the coverage fold + the (possibly empty) book-lane disclosure.
fn assemble(
    venue: &str,
    symbol: &str,
    lane: QualityLane,
    day_start_ms: i64,
    timestamps: &[i64],
    cfg: &QualityConfig,
    disc: Disclosure,
) -> DayQuality {
    let (largest_gap_ms, uncovered_ms) = day_coverage(day_start_ms, timestamps, cfg.max_gap_ms);
    let covered = (DAY_MS - uncovered_ms).max(0);
    DayQuality {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        lane,
        day: epoch_ms_to_utc_date(day_start_ms),
        rows: timestamps.len(),
        first_ts: timestamps.iter().min().copied(),
        last_ts: timestamps.iter().max().copied(),
        largest_gap_ms,
        uncovered_ms,
        coverage_fraction: covered as f64 / DAY_MS as f64,
        gap_pct: uncovered_ms as f64 / DAY_MS as f64 * 100.0,
        gap_start_count: disc.gap_start_count,
        stale_count: disc.stale_count,
        disclosed_gap_ms: disc.disclosed_gap_ms,
        seq_resets: disc.seq_resets,
    }
}

/// Score one UTC day of recorded BOOK-lane rows (data events + §B status markers), folding the §B
/// disclosure, the seq-chain resets, and the inferred intra-day coverage. `updates` are the day's
/// rows in scan order (`(ts, seq)`-ascending, as `scan_book_updates` returns them).
pub fn score_book_day(
    venue: &str,
    symbol: &str,
    day_start_ms: i64,
    updates: &[BookUpdate],
    cfg: &QualityConfig,
) -> DayQuality {
    let mut gap_start_count = 0usize;
    let mut stale_count = 0usize;
    let mut disclosed_gap_ms = 0i64;
    let mut seq_resets = 0usize;
    // onset ts of the currently-open §B outage (kept at the EARLIEST onset until a resume closes it)
    let mut gap_open: Option<i64> = None;
    // last seq of the data chain (Delta/Snapshot only — status markers carry seq 0 and are skipped)
    let mut prev_seq: Option<u64> = None;

    for u in updates {
        match u.kind {
            BookUpdateKind::GapStart => {
                gap_start_count += 1;
                if gap_open.is_none() {
                    gap_open = Some(u.ts);
                }
            }
            BookUpdateKind::Stale => {
                stale_count += 1;
                if gap_open.is_none() {
                    gap_open = Some(u.ts);
                }
            }
            BookUpdateKind::LiveResume => {
                if let Some(onset) = gap_open.take() {
                    disclosed_gap_ms += (u.ts - onset).max(0);
                }
            }
            BookUpdateKind::Delta | BookUpdateKind::Snapshot => {
                if let Some(prev) = prev_seq
                    && u.seq < prev
                {
                    seq_resets += 1;
                }
                prev_seq = Some(u.seq);
            }
        }
    }
    // an outage still open at day's end had no recorded recovery — it ran to the end of the UTC day
    if let Some(onset) = gap_open {
        disclosed_gap_ms += (day_start_ms.saturating_add(DAY_MS) - onset).max(0);
    }

    let timestamps: Vec<i64> = updates.iter().map(|u| u.ts).collect();
    let disc = Disclosure { gap_start_count, stale_count, disclosed_gap_ms, seq_resets };
    assemble(venue, symbol, QualityLane::Book, day_start_ms, &timestamps, cfg, disc)
}

/// Score one UTC day of TICK-lane rows from their timestamps alone. Quote/trade lanes carry no §B
/// markers or seq chain (gap provenance there is a noted `live_rec` follow-up), so the §B/seq fields
/// stay zero and only inter-row coverage is inferred.
pub fn score_tick_day(
    venue: &str,
    symbol: &str,
    lane: QualityLane,
    day_start_ms: i64,
    timestamps: &[i64],
    cfg: &QualityConfig,
) -> DayQuality {
    assemble(venue, symbol, lane, day_start_ms, timestamps, cfg, Disclosure::default())
}

/// Score one UTC day of recorded QUOTE rows (coverage inferred from `QuoteTick::ts`).
pub fn score_quote_day(
    venue: &str,
    symbol: &str,
    day_start_ms: i64,
    quotes: &[QuoteTick],
    cfg: &QualityConfig,
) -> DayQuality {
    let ts: Vec<i64> = quotes.iter().map(|q| q.ts).collect();
    score_tick_day(venue, symbol, QualityLane::Quote, day_start_ms, &ts, cfg)
}

/// Score one UTC day of recorded TRADE rows (coverage inferred from `TradeTick::ts`).
pub fn score_trade_day(
    venue: &str,
    symbol: &str,
    day_start_ms: i64,
    trades: &[TradeTick],
    cfg: &QualityConfig,
) -> DayQuality {
    let ts: Vec<i64> = trades.iter().map(|t| t.ts).collect();
    score_tick_day(venue, symbol, QualityLane::Trade, day_start_ms, &ts, cfg)
}

/// Score a whole recorded BOOK series into per-UTC-day records, day-ascending. Rows are grouped by
/// UTC day (scan order preserved within a day, so the seq chain reads correctly). Expects the rows
/// in `scan_book_updates` order.
pub fn score_book_series(
    venue: &str,
    symbol: &str,
    updates: &[BookUpdate],
    cfg: &QualityConfig,
) -> Vec<DayQuality> {
    let mut by_day: BTreeMap<i64, Vec<BookUpdate>> = BTreeMap::new();
    for u in updates {
        by_day.entry(utc_day_start_ms(u.ts)).or_default().push(u.clone());
    }
    by_day.into_iter().map(|(day, rows)| score_book_day(venue, symbol, day, &rows, cfg)).collect()
}

/// Score a whole recorded QUOTE series into per-UTC-day records, day-ascending.
pub fn score_quote_series(
    venue: &str,
    symbol: &str,
    quotes: &[QuoteTick],
    cfg: &QualityConfig,
) -> Vec<DayQuality> {
    let mut by_day: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for q in quotes {
        by_day.entry(utc_day_start_ms(q.ts)).or_default().push(q.ts);
    }
    by_day
        .into_iter()
        .map(|(day, ts)| score_tick_day(venue, symbol, QualityLane::Quote, day, &ts, cfg))
        .collect()
}

/// Score a whole recorded TRADE series into per-UTC-day records, day-ascending.
pub fn score_trade_series(
    venue: &str,
    symbol: &str,
    trades: &[TradeTick],
    cfg: &QualityConfig,
) -> Vec<DayQuality> {
    let mut by_day: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for t in trades {
        by_day.entry(utc_day_start_ms(t.ts)).or_default().push(t.ts);
    }
    by_day
        .into_iter()
        .map(|(day, ts)| score_tick_day(venue, symbol, QualityLane::Trade, day, &ts, cfg))
        .collect()
}

/// The store-scanning convenience: read a recorded series out of a [`crate::DataFusionHist`] and
/// score it per day. Gated with the store engine itself — the pure scorers above need no DataFusion.
/// These methods READ ONLY (one `scan_*` each); they write nothing.
#[cfg(feature = "hist-datafusion")]
mod store {
    use super::{
        DayQuality, QualityConfig, score_book_series, score_quote_series, score_trade_series,
    };
    use crate::DataFusionHist;
    use crate::hist::{DataError, HistStore, TsRange};

    impl DataFusionHist {
        /// Per-UTC-day data-quality report for the recorded BOOK series `(venue, symbol)`.
        pub fn book_series_quality(
            &self,
            venue: &str,
            symbol: &str,
            cfg: &QualityConfig,
        ) -> Result<Vec<DayQuality>, DataError> {
            let rows = self.scan_book_updates(venue, symbol, TsRange::all())?;
            Ok(score_book_series(venue, symbol, &rows, cfg))
        }

        /// Per-UTC-day data-quality report for the recorded QUOTE series `(venue, symbol)`.
        pub fn quote_series_quality(
            &self,
            venue: &str,
            symbol: &str,
            cfg: &QualityConfig,
        ) -> Result<Vec<DayQuality>, DataError> {
            let rows = self.scan_quotes(venue, symbol, TsRange::all())?;
            Ok(score_quote_series(venue, symbol, &rows, cfg))
        }

        /// Per-UTC-day data-quality report for the recorded TRADE series `(venue, symbol)`.
        pub fn trade_series_quality(
            &self,
            venue: &str,
            symbol: &str,
            cfg: &QualityConfig,
        ) -> Result<Vec<DayQuality>, DataError> {
            let rows = self.scan_trades(venue, symbol, TsRange::all())?;
            Ok(score_trade_series(venue, symbol, &rows, cfg))
        }
    }
}

#[path = "quality_tests.rs"]
#[cfg(test)]
mod quality_tests;

/// The store-backed read-only proof: scoring a real recorded series changes NOTHING on disk
/// (byte-identical scans + series listing before/after) while producing the expected report.
#[cfg(all(test, feature = "hist-datafusion"))]
mod store_tests {
    use super::*;
    use crate::{DataFusionHist, HistStore, TsRange};
    use vike_model::BookLevel;
    use vike_model::{BookUpdate, BookUpdateKind};

    const STEP: i64 = 1_800_000;
    const HOUR: i64 = 3_600_000;

    fn bu(ts: i64, seq: u64, kind: BookUpdateKind) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts,
            seq,
            kind,
            tick_size: 0.01,
            bids: vec![BookLevel::new(0.45, 10.0)],
            asks: vec![BookLevel::new(0.46, 5.0)],
            symbol: String::new(),
        }
    }

    fn marker(ts: i64, kind: BookUpdateKind) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts,
            seq: 0,
            kind,
            tick_size: 0.0,
            bids: Vec::new(),
            asks: Vec::new(),
            symbol: String::new(),
        }
    }

    #[test]
    fn scoring_reads_store_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        // one UTC day: pre-gap deltas, a §B GapStart..LiveResume one-hour hole, a seq reset, resume
        let mut rows = Vec::new();
        for k in 0..=9i64 {
            let kind = if k == 0 { BookUpdateKind::Snapshot } else { BookUpdateKind::Delta };
            rows.push(bu(k * STEP, (k + 1) as u64, kind));
        }
        rows.push(marker(10 * STEP, BookUpdateKind::GapStart));
        rows.push(marker(12 * STEP, BookUpdateKind::LiveResume));
        rows.push(bu(12 * STEP + 1, 1, BookUpdateKind::Snapshot));
        for k in 13..=47i64 {
            rows.push(bu(k * STEP, (k - 11) as u64, BookUpdateKind::Delta));
        }
        store.append_book_updates("polymarket", "TOK", &rows, Some("day0")).unwrap();

        // snapshot the persisted state BEFORE scoring
        let before = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
        let series_before = store.list_series().unwrap();

        // score (read-only)
        let cfg = QualityConfig { max_gap_ms: STEP };
        let report = store.book_series_quality("polymarket", "TOK", &cfg).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].day, "1970-01-01");
        assert_eq!(report[0].gap_start_count, 1);
        assert_eq!(report[0].disclosed_gap_ms, HOUR);
        assert_eq!(report[0].seq_resets, 1);
        assert_eq!(report[0].largest_gap_ms, HOUR);
        assert_eq!(report[0].uncovered_ms, HOUR);

        // the store is byte-identical after scoring — the API wrote nothing
        let after = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
        let series_after = store.list_series().unwrap();
        assert_eq!(series_before, series_after);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert_eq!(a.ts, b.ts);
            assert_eq!(a.seq, b.seq);
            assert_eq!(a.kind, b.kind);
        }
    }
}
