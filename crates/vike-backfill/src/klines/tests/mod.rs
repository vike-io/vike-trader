//! Unit tests of the kline ingest seam: the shared fixtures, and one child per subject.

use super::*;
use std::cell::Cell;
use std::sync::Mutex;

use vike_data::StoreKindId;
use vike_data::source::{KlineSource, SourceError};

#[cfg(test)]
mod chunk_width;
#[cfg(test)]
mod chunked;
#[cfg(test)]
mod empty_days;
#[cfg(test)]
mod forming_tail;
#[cfg(test)]
mod ingest_observed;
#[cfg(test)]
mod oanda_golden;
#[cfg(test)]
mod stop_probe;

/// A fixture bar at `ts` — only `ts` matters for [`drop_forming_tail`].
fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A store over its own temp dir, returned with the dir so the dir outlives it.
fn store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().expect("temp dir");
    let hist = DataFusionHist::open(dir.path()).expect("open temp store");
    (dir, hist)
}

/// A [`vike_data::source::KlineSource`] whose `fetch` PANICS. The registry path's half of the
/// proof: if the refusal ever moved below the dispatch, this stops being a red assertion and
/// becomes a red panic — either way the test fails loudly rather than passing for a new reason.
struct NeverFetches;

impl vike_data::source::KlineSource for NeverFetches {
    fn venue(&self) -> &str {
        "testvenue"
    }
    fn fetch(
        &self,
        _symbol: &str,
        interval: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        panic!("the refusal must answer {interval:?} before any venue is asked")
    }
}

const DAY: i64 = MS_PER_DAY;
const HOUR: i64 = 3_600_000;
/// A "now" far after every window these tests use, so every chunk in them is settled.
const LATER: i64 = 1_000 * DAY;
const CVENUE: &str = "fakevenue";
const CSYM: &str = "EURUSD";

/// One bar of the fake venue — every field a function of `ts`, so two bars never compare equal by
/// accident and a stored bar can be told from its neighbour.
fn market_bar(ts: i64, step: i64) -> Bar {
    let n = (ts / step) as f64;
    Bar {
        ts,
        open: n,
        high: n + 0.5,
        low: n - 0.5,
        close: n + 0.25,
        volume: 1.0 + ts.rem_euclid(7) as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A venue with a history endpoint, in memory: it serves a bar at every multiple of `step` from
/// epoch 0 inside whatever window it is asked for, records every window it was asked for, and can
/// be made to misbehave in the ways the ingest guards against.
struct FakeMarket {
    step: i64,
    /// UTC day indices on which it serves nothing — a Saturday, say.
    closed_days: Vec<i64>,
    /// Extra bars it serves either side of the window it was asked for, as a sloppy venue would.
    strays: i64,
    /// A fetch whose window OPENS here fails.
    fail_at: Option<i64>,
    /// ...and fails as a `Refused` rather than a `Fetch`.
    refuse: bool,
    calls: Mutex<Vec<(i64, i64)>>,
}

impl FakeMarket {
    fn new(step: i64) -> Self {
        FakeMarket {
            step,
            closed_days: Vec::new(),
            strays: 0,
            fail_at: None,
            refuse: false,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn closed_on(mut self, day: i64) -> Self {
        self.closed_days.push(day);
        self
    }
    fn straying(mut self, bars: i64) -> Self {
        self.strays = bars;
        self
    }
    fn failing_at(mut self, window_start: i64) -> Self {
        self.fail_at = Some(window_start);
        self
    }
    fn refusing(mut self) -> Self {
        self.refuse = true;
        self
    }
    /// Every `(start, end)` window it was asked for, in order.
    fn calls(&self) -> Vec<(i64, i64)> {
        self.calls.lock().unwrap().clone()
    }
}

impl KlineSource for FakeMarket {
    fn venue(&self) -> &str {
        CVENUE
    }
    fn fetch(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        self.calls.lock().unwrap().push((start_ms, end_ms));
        if self.fail_at == Some(start_ms) {
            let why = "the venue went away".to_string();
            return Err(if self.refuse {
                SourceError::Refused(why)
            } else {
                SourceError::Fetch(why)
            });
        }
        let (from, to) = (start_ms - self.strays * self.step, end_ms + self.strays * self.step);
        // The first multiple of `step` at or after `from`.
        let first = from + (self.step - from.rem_euclid(self.step)) % self.step;
        Ok((first..=to)
            .step_by(self.step as usize)
            .filter(|ts| !self.closed_days.contains(&ts.div_euclid(DAY)))
            .map(|ts| market_bar(ts, self.step))
            .collect())
    }
}

/// [`ingest_klines_chunked`] over a [`FakeMarket`], answering the whole outcome rather than just rows
/// — with a stop probe that never fires, which is every test below except the ones about stopping.
fn run_chunked(
    hist: &DataFusionHist,
    market: &FakeMarket,
    interval: &str,
    window: (i64, i64),
    now_ms: i64,
) -> Result<ChunkedOutcome, CollectError> {
    run_chunked_stoppable(hist, market, interval, window, now_ms, &|| false)
}

/// [`run_chunked`] with the caller's stop probe.
fn run_chunked_stoppable(
    hist: &DataFusionHist,
    market: &FakeMarket,
    interval: &str,
    window: (i64, i64),
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<ChunkedOutcome, CollectError> {
    run_chunked_wide(hist, market, interval, window, 1, now_ms, should_stop)
}

/// [`run_chunked_stoppable`] at a chunk width of `chunk_days` whole days — one, for every test that
/// predates the knob, which is the proof that one changed nothing.
fn run_chunked_wide(
    hist: &DataFusionHist,
    market: &FakeMarket,
    interval: &str,
    window: (i64, i64),
    chunk_days: u32,
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<ChunkedOutcome, CollectError> {
    ingest_klines_chunked(
        hist,
        market.venue(),
        CSYM,
        interval,
        window,
        chunk_days,
        now_ms,
        should_stop,
        |sym, iv, s, e| market.fetch(sym, iv, s, e).map_err(CollectError::from),
    )
}

fn chunk_series(interval: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, CVENUE, CSYM, Some(interval.to_string()))
}

/// Whether the day chunk `[c0, c1]` has spent its commit key in `hist`.
fn spent(hist: &DataFusionHist, interval: &str, c0: i64, c1: i64) -> bool {
    let key = commit_key(CVENUE, CSYM, interval, c0, c1);
    hist.series_has_commit(&chunk_series(interval), &key).expect("read the manifest")
}

/// Every bar of the fake venue's series, ascending, as the bit patterns of its stored fields — the
/// full-fidelity form two stores are compared in.
fn stored_at(hist: &DataFusionHist, interval: &str) -> Vec<(i64, [u64; 5])> {
    let bars = hist
        .load_bars(CVENUE, CSYM, interval, vike_data::TsRange::all())
        .expect("read the series back");
    bars.iter()
        .map(|b| (b.ts, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

/// A request partitions its chunks — every one is skipped, fetched, empty, unsettled or left
/// unreached by a stop, and none is counted twice or left out.
fn assert_partitions(out: &ChunkedOutcome) {
    assert_eq!(
        out.skipped + out.fetched + out.empty + out.unsettled + out.stopped,
        out.chunks,
        "the chunks of a request partition: {out:?}"
    );
}

/// A probe that answers `false` to its first `k` asks and `true` from then on — "fires after chunk
/// `k`", since the ingest asks once at the top of every chunk. It counts its asks, and it latches the
/// way the datahub's does: once it has said stop, it keeps saying it.
struct FiresAfter {
    k: usize,
    asked: Cell<usize>,
}

impl FiresAfter {
    fn new(k: usize) -> Self {
        FiresAfter { k, asked: Cell::new(0) }
    }
    fn ask(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.asked.get() > self.k
    }
}
