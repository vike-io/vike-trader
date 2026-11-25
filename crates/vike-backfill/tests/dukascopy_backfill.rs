//! Offline gate for the Dukascopy → hist-store ingest + resample (docs/decisions/0094):
//! build a synthetic `Vec<TickQuote>` directly, ingest into a `tempdir` `HistoryStoreDataFusion`, and assert
//! bit-exact round-trips (`to_bits`). Then resample and assert the stored bars match
//! `vike_marketdata::consolidate_quotes` bit-exactly, plus commit-key idempotency — and drive the whole
//! `TickBars` lane, `backfill_quotes_then_bars`, over aligned and unaligned windows, over
//! windows long enough to be cut into day-sized chunks, over empty days (marked on order evidence)
//! and over chunks the store REFUSES to
//! supersede; and (`archive`) the archive import lane's store half against that lane. The fetch
//! half (and its own `#[ignore]`d live test) moved to `crates/bridges/dukascopy/tests/quotes.rs`;
//! this file is deterministic + offline, and — since it no longer names the bridge — ungated.

use std::assert_matches;
use vike_backfill::CollectError;
use vike_backfill::venues::dukascopy::{
    VENUE, backfill_quotes_then_bars, ingest_quotes, provisional_quote_commit_key,
    provisional_resample_key, quote_commit_key, resample_and_store_bars, resample_commit_key,
};
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::{TickQuote, consolidate_quotes};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_store::{CompactionConfig, HistoryStoreDataFusion};

const SYMBOL: &str = "EURUSD";
const STEP_MS: i64 = 60_000;
const INTERVAL: &str = "1m";

// The tests, one child per section; each child's own `//!` line names its section.
#[path = "dukascopy_backfill/archive.rs"]
mod archive;
#[path = "dukascopy_backfill/chunk_grid.rs"]
mod chunk_grid;
#[path = "dukascopy_backfill/empty_days.rs"]
mod empty_days;
#[path = "dukascopy_backfill/provisional.rs"]
mod provisional;
#[path = "dukascopy_backfill/recent_chunks.rs"]
mod recent_chunks;
#[path = "dukascopy_backfill/refused_supersede.rs"]
mod refused_supersede;
#[path = "dukascopy_backfill/roundtrip.rs"]
mod roundtrip;
#[path = "dukascopy_backfill/stop_probe.rs"]
mod stop_probe;

fn open() -> (tempfile::TempDir, HistoryStoreDataFusion) {
    let dir = tempfile::tempdir().unwrap();
    let hist = HistoryStoreDataFusion::open(dir.path()).unwrap();
    (dir, hist)
}

/// What `consolidate_quotes` makes of `tape` at `step`, each bar as its fields' bit patterns (the
/// form the members' stored-bar readers answer in): the bars of a store that holds `tape` exactly
/// once and nothing beside it — so a bar left beside its settled twin, or a `volume` counting a tick
/// twice, cannot compare equal.
fn bars_of(tape: &[TickQuote], step: i64) -> Vec<(i64, [u64; 5])> {
    consolidate_quotes(tape, step)
        .iter()
        .map(|b| (b.timestamp, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

/// The `ts` of every tick in `tape`, in its order.
fn ts_of(tape: &[TickQuote]) -> Vec<i64> {
    tape.iter().map(|q| q.timestamp).collect()
}

/// The `ts` of every bar a reader reads of the lane's series (`load_bars`: one bar per timestamp), ascending.
fn stored_bar_timestamp(hist: &HistoryStoreDataFusion) -> Vec<i64> {
    let bars = hist.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).unwrap();
    bars.iter().map(|b| b.timestamp).collect()
}

/// A tape of three ticks every UTC day — 10 s and 70 s into the day (its first two minutes) and
/// 30 s before its end (its last minute) — cut to `[from, to]`: a fake fetch that honours its
/// bounds, for any days at all. Values are arbitrary — these tests count rows and read `ts`.
fn tape_between(from: i64, to: i64) -> Vec<TickQuote> {
    (from.div_euclid(MS_PER_DAY)..=to.div_euclid(MS_PER_DAY))
        .flat_map(|day| [10_000, 70_000, MS_PER_DAY - 30_000].map(|at| day * MS_PER_DAY + at))
        .filter(|ts| (from..=to).contains(ts))
        .map(|ts| TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid: 1.10001,
            ask: 1.10003,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SYMBOL.into(),
        })
        .collect()
}

/// Whether the lane spent `key` on its quote series, asked the way
/// `append_is_idempotent_by_commit_key` pins: an append under a spent key writes nothing. Only ever
/// asserted TRUE — under an unspent key the one-tick probe would land.
fn quote_key_is_spent(hist: &HistoryStoreDataFusion, key: &str) -> bool {
    hist.append_quotes(VENUE, SYMBOL, &tape_between(0, 10_000), Some(key)).unwrap() == 0
}

/// The bars of `interval` the store holds, ascending, each as its stored fields' bit patterns: the
/// full-fidelity form two runs are compared in.
fn stored_bars_at(hist: &HistoryStoreDataFusion, interval: &str) -> Vec<(i64, [u64; 5])> {
    let bars = hist.load_bars(VENUE, SYMBOL, interval, TimestampRange::all()).unwrap();
    bars.iter()
        .map(|b| {
            let fields = [b.open, b.high, b.low, b.close, b.volume];
            (b.timestamp, fields.map(f64::to_bits))
        })
        .collect()
}

/// The `interval` bar series' twin of [`quote_series_has_commit`]: asked of the manifest, so the
/// answer may be NO.
fn bar_series_has_commit(hist: &HistoryStoreDataFusion, interval: &str, key: &str) -> bool {
    let id = SeriesId::per_symbol(StoreKindId::Bar, VENUE, SYMBOL, Some(interval.to_string()));
    hist.series_has_commit(&id, key).unwrap()
}

/// [`stored_bars_at`], narrowed to the bars whose `ts` lies inside `[from, to]`.
fn stored_bars_within(
    hist: &HistoryStoreDataFusion,
    interval: &str,
    (from, to): (i64, i64),
) -> Vec<(i64, [u64; 5])> {
    stored_bars_at(hist, interval).into_iter().filter(|(ts, _)| (from..=to).contains(ts)).collect()
}

/// The `ts` of every stored quote inside `[from, to]`, ascending: a tick stored twice is there
/// twice.
fn stored_quote_timestamp_within(
    hist: &HistoryStoreDataFusion,
    (from, to): (i64, i64),
) -> Vec<i64> {
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TimestampRange::all()).unwrap();
    quotes.iter().map(|q| q.timestamp).filter(|ts| (from..=to).contains(ts)).collect()
}

/// Plant what compaction leaves once it has folded a chunk's PROVISIONAL quote entry into a
/// neighbour: that key sharing ONE part with an unrelated commit's — so the store finds it, cannot
/// remove it exactly, and refuses. Both writes land on the chunk's own UTC day, which is what lets
/// compaction merge them. ⚠ The neighbour's key carries the chunk's start: a commit key is spent
/// once per series, so a fixed one would make the SECOND chunk planted in a store write no neighbour
/// at all — one part on its day, nothing to fold, and the plant would say so.
fn fold_provisional_quotes_into_a_neighbour(hist: &HistoryStoreDataFusion, (from, to): (i64, i64)) {
    let neighbour = format!("a-neighbour-of-{from}");
    hist.append_quotes(VENUE, SYMBOL, &tape_between(from, from + 20_000), Some(&neighbour))
        .unwrap();
    hist.append_quotes(
        VENUE,
        SYMBOL,
        &tape_between(from + 60_000, from + 80_000),
        Some(&provisional_quote_commit_key(SYMBOL, from, to)),
    )
    .unwrap();
    let merged = hist
        .compact_series(
            StoreKindId::Quote,
            VENUE,
            SYMBOL,
            None,
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(merged.parts_merged, 2, "the two same-day parts must fold together: {merged:?}");
}

/// Whether `key` is spent on the QUOTE series — asked of the manifest, which is the one way to ask
/// when the answer may be NO: [`quote_key_is_spent`] probes with an append, and under an unspent key
/// that probe would land.
fn quote_series_has_commit(hist: &HistoryStoreDataFusion, key: &str) -> bool {
    let id = SeriesId::per_symbol(StoreKindId::Quote, VENUE, SYMBOL, None);
    hist.series_has_commit(&id, key).unwrap()
}
