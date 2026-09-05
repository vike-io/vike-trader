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
//!   per `vike_model::orderbook`'s `SeqPolicy::Strict`) is a resync boundary. Status markers carry
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

    const STEP: i64 = 1_800_000; // 30 min — the test cadence AND the coverage threshold
    const HOUR: i64 = 3_600_000;

    fn cfg() -> QualityConfig {
        QualityConfig { max_gap_ms: STEP }
    }

    /// A book data event (2 levels) at `(ts, seq)`.
    fn bu(ts: i64, seq: u64, kind: BookUpdateKind) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts,
            seq,
            kind,
            tick_size: 0.01,
            bids: vec![(0.45, 10.0)],
            asks: vec![(0.46, 5.0)],
            symbol: String::new(),
        }
    }

    /// A §B status marker (zero levels, seq 0) — exactly what `live_rec::stream_status` records.
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

    fn q_at(ts: i64) -> QuoteTick {
        QuoteTick {
            ts,
            local_ts: ts,
            bid: 0.45,
            ask: 0.46,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        }
    }

    fn t_at(ts: i64) -> TradeTick {
        TradeTick {
            ts,
            local_ts: ts,
            price: 0.45,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        }
    }

    /// A single day (`1970-01-01`) that is otherwise fully covered but has a §B `GapStart`..
    /// `LiveResume` outage of exactly one hour (no rows in between), a seq reset at the re-anchor,
    /// and therefore a one-hour coverage hole.
    fn book_day_with_gap() -> Vec<BookUpdate> {
        let mut rows = Vec::new();
        // slots 0..=9: a snapshot then deltas, seq 1..10, at the 30-min cadence
        for k in 0..=9i64 {
            let kind = if k == 0 { BookUpdateKind::Snapshot } else { BookUpdateKind::Delta };
            rows.push(bu(k * STEP, (k + 1) as u64, kind));
        }
        // slot 10 (ts 18_000_000): §B GapStart; NO rows until recovery a full hour later
        rows.push(marker(10 * STEP, BookUpdateKind::GapStart));
        // slot 12 (ts 21_600_000): recovery — LiveResume then a re-anchor Snapshot whose seq RESETS
        rows.push(marker(12 * STEP, BookUpdateKind::LiveResume));
        rows.push(bu(12 * STEP + 1, 1, BookUpdateKind::Snapshot)); // seq 10 -> 1 regression
        // slots 13..=47: post-gap deltas, seq 2..36, back on cadence to the end of the day
        for k in 13..=47i64 {
            rows.push(bu(k * STEP, (k - 11) as u64, BookUpdateKind::Delta));
        }
        rows
    }

    #[test]
    fn clean_continuous_book_day_scores_perfect() {
        // 48 events at the 30-min cadence spanning the whole UTC day, monotonic seq, no markers.
        let rows: Vec<BookUpdate> = (0..48i64)
            .map(|k| {
                let kind = if k == 0 { BookUpdateKind::Snapshot } else { BookUpdateKind::Delta };
                bu(k * STEP, (k + 1) as u64, kind)
            })
            .collect();
        let q = score_book_day("polymarket", "TOK", 0, &rows, &cfg());
        assert_eq!(q.lane, QualityLane::Book);
        assert_eq!(q.day, "1970-01-01");
        assert_eq!(q.rows, 48);
        assert_eq!(q.first_ts, Some(0));
        assert_eq!(q.last_ts, Some(47 * STEP));
        assert_eq!(q.uncovered_ms, 0);
        assert_eq!(q.largest_gap_ms, 0);
        assert_eq!(q.gap_start_count, 0);
        assert_eq!(q.stale_count, 0);
        assert_eq!(q.disclosed_gap_ms, 0);
        assert_eq!(q.seq_resets, 0);
        assert!(q.is_perfect());
        assert!((q.coverage_fraction - 1.0).abs() < 1e-12);
        assert!(q.gap_pct.abs() < 1e-12);
    }

    #[test]
    fn book_day_folds_gap_seqreset_and_hole() {
        let q = score_book_day("polymarket", "TOK", 0, &book_day_with_gap(), &cfg());
        // §B disclosure: one GapStart, no Stale, one hour of disclosed outage (18.0M -> 21.6M)
        assert_eq!(q.gap_start_count, 1);
        assert_eq!(q.stale_count, 0);
        assert_eq!(q.disclosed_gap_ms, HOUR);
        // seq chain: 1..10 then a reset to 1 then 2..36 -> exactly one regression
        assert_eq!(q.seq_resets, 1);
        // inferred coverage: exactly one 1-hour hole, nothing else exceeds the 30-min threshold
        assert_eq!(q.largest_gap_ms, HOUR);
        assert_eq!(q.uncovered_ms, HOUR);
        let expect_gap_pct = HOUR as f64 / DAY_MS as f64 * 100.0;
        assert!((q.gap_pct - expect_gap_pct).abs() < 1e-9);
        let expect_cov = (DAY_MS - HOUR) as f64 / DAY_MS as f64;
        assert!((q.coverage_fraction - expect_cov).abs() < 1e-12);
        assert!(!q.is_perfect());
    }

    #[test]
    fn quote_lane_infers_coverage_from_inter_row_deltas() {
        // clean: 48 half-hourly quotes across the day -> perfect (cadence == threshold)
        let clean: Vec<QuoteTick> = (0..48i64).map(|k| q_at(k * STEP)).collect();
        let cq = score_quote_day("polymarket", "TOK", 0, &clean, &cfg());
        assert_eq!(cq.lane, QualityLane::Quote);
        assert_eq!(cq.uncovered_ms, 0);
        assert!(cq.is_perfect());

        // holed: drop the k=10 sample -> a 1-hour hole between k=9 (16.2M) and k=11 (19.8M)
        let holed: Vec<QuoteTick> =
            (0..48i64).filter(|&k| k != 10).map(|k| q_at(k * STEP)).collect();
        let hq = score_quote_day("polymarket", "TOK", 0, &holed, &cfg());
        assert_eq!(hq.rows, 47);
        assert_eq!(hq.largest_gap_ms, HOUR);
        assert_eq!(hq.uncovered_ms, HOUR);
        assert!((hq.gap_pct - HOUR as f64 / DAY_MS as f64 * 100.0).abs() < 1e-9);
        // §B/seq signals are inert on the quote lane (no provenance recorded there)
        assert_eq!(hq.gap_start_count, 0);
        assert_eq!(hq.stale_count, 0);
        assert_eq!(hq.disclosed_gap_ms, 0);
        assert_eq!(hq.seq_resets, 0);
        assert!(!hq.is_perfect());
    }

    #[test]
    fn trade_lane_infers_coverage_like_quotes() {
        // drop the k=20 sample -> a 1-hour hole between k=19 (34.2M) and k=21 (37.8M)
        let holed: Vec<TradeTick> =
            (0..48i64).filter(|&k| k != 20).map(|k| t_at(k * STEP)).collect();
        let hq = score_trade_day("polymarket", "TOK", 0, &holed, &cfg());
        assert_eq!(hq.lane, QualityLane::Trade);
        assert_eq!(hq.uncovered_ms, HOUR);
        assert_eq!(hq.largest_gap_ms, HOUR);
        assert_eq!(hq.seq_resets, 0);
        assert!(!hq.is_perfect());
    }

    #[test]
    fn empty_day_is_fully_uncovered() {
        let q = score_tick_day("v", "s", QualityLane::Quote, 0, &[], &cfg());
        assert_eq!(q.rows, 0);
        assert_eq!(q.first_ts, None);
        assert_eq!(q.last_ts, None);
        assert_eq!(q.uncovered_ms, DAY_MS);
        assert_eq!(q.largest_gap_ms, DAY_MS);
        assert!(q.coverage_fraction.abs() < 1e-12);
        assert!((q.gap_pct - 100.0).abs() < 1e-9);
        assert!(!q.is_perfect());
    }

    #[test]
    fn stale_marker_counted_and_unclosed_gap_extends_to_day_end() {
        let rows = vec![
            bu(1_000, 1, BookUpdateKind::Snapshot),
            marker(HOUR, BookUpdateKind::Stale), // data stopped at 1h and never resumed this day
        ];
        let q = score_book_day("v", "s", 0, &rows, &cfg());
        assert_eq!(q.stale_count, 1);
        assert_eq!(q.gap_start_count, 0);
        // unclosed outage runs from the Stale onset (HOUR) to the end of the UTC day
        assert_eq!(q.disclosed_gap_ms, DAY_MS - HOUR);
        assert!(!q.is_perfect());
    }

    #[test]
    fn seq_resets_ignore_status_markers() {
        // markers carry seq 0; they must NOT read as a regression in the data seq chain.
        let across_markers = vec![
            bu(1_000, 5, BookUpdateKind::Delta),
            marker(2_000, BookUpdateKind::GapStart),
            marker(3_000, BookUpdateKind::LiveResume),
            bu(4_000, 6, BookUpdateKind::Delta),
        ];
        let q = score_book_day("v", "s", 0, &across_markers, &cfg());
        assert_eq!(q.seq_resets, 0); // 5 -> 6 across the markers is monotonic

        // a genuine backwards jump in the data chain IS counted
        let regression =
            vec![bu(1_000, 5, BookUpdateKind::Delta), bu(2_000, 3, BookUpdateKind::Delta)];
        let q2 = score_book_day("v", "s", 0, &regression, &cfg());
        assert_eq!(q2.seq_resets, 1);
    }

    #[test]
    fn quote_series_buckets_into_per_day_records() {
        let quotes = vec![
            q_at(1_000),
            q_at(2_000), // 1970-01-01
            q_at(DAY_MS + 1_000),
            q_at(DAY_MS + 2_000), // 1970-01-02
        ];
        let out = score_quote_series("polymarket", "TOK", &quotes, &cfg());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].day, "1970-01-01");
        assert_eq!(out[1].day, "1970-01-02");
        assert_eq!(out[0].rows, 2);
        assert_eq!(out[1].rows, 2);
        assert_eq!(out[0].lane, QualityLane::Quote);
    }

    #[test]
    fn book_series_buckets_and_scores_each_day() {
        let mut rows = book_day_with_gap(); // day 0: hole + reset + disclosed gap
        rows.push(bu(DAY_MS + 1_000, 1, BookUpdateKind::Snapshot)); // day 1: clean-ish
        rows.push(bu(DAY_MS + 2_000, 2, BookUpdateKind::Delta));
        let out = score_book_series("polymarket", "TOK", &rows, &cfg());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].day, "1970-01-01");
        assert_eq!(out[0].seq_resets, 1);
        assert_eq!(out[0].gap_start_count, 1);
        assert_eq!(out[0].disclosed_gap_ms, HOUR);
        assert_eq!(out[1].day, "1970-01-02");
        assert_eq!(out[1].seq_resets, 0);
        assert_eq!(out[1].gap_start_count, 0);
    }

    #[test]
    fn utc_day_start_floors_to_midnight() {
        assert_eq!(utc_day_start_ms(0), 0);
        assert_eq!(utc_day_start_ms(1), 0);
        assert_eq!(utc_day_start_ms(DAY_MS - 1), 0);
        assert_eq!(utc_day_start_ms(DAY_MS), DAY_MS);
        assert_eq!(utc_day_start_ms(DAY_MS + 5), DAY_MS);
        // pre-epoch floors toward negative infinity
        assert_eq!(utc_day_start_ms(-1), -DAY_MS);
    }
}

/// The store-backed read-only proof: scoring a real recorded series changes NOTHING on disk
/// (byte-identical scans + series listing before/after) while producing the expected report.
#[cfg(all(test, feature = "hist-datafusion"))]
mod store_tests {
    use super::*;
    use crate::{DataFusionHist, HistStore, TsRange};
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
            bids: vec![(0.45, 10.0)],
            asks: vec![(0.46, 5.0)],
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
