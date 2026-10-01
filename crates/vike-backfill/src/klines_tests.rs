use super::*;

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

#[test]
fn drops_a_still_forming_final_candle() {
    // 1m interval; a candle open at ts=now-30s has not closed yet (close = ts+60s > now).
    let now = 1_700_000_090_000_i64; // 90s past an arbitrary epoch
    let mut bars =
        vec![bar(1_700_000_000_000 - 3 * 60_000), bar(1_700_000_000_000), bar(1_700_000_060_000)];
    // last bar opened at ts=1_700_000_060_000; close = +60_000 = 1_700_000_120_000 > now (90_000 mark)
    drop_forming_tail(&mut bars, "1m", now);
    assert_eq!(bars.len(), 2, "the still-forming last candle is dropped");
    assert_eq!(bars.last().unwrap().ts, 1_700_000_000_000);
}

#[test]
fn keeps_a_fully_closed_final_candle() {
    // close time (ts + 60_000) exactly equals now: NOT in the future, so it's closed and kept
    // (matches tif_expired-style inclusive-boundary conventions: at the boundary it's done, not
    // still forming).
    let ts = 1_700_000_000_000_i64;
    let now = ts + 60_000;
    let mut bars = vec![bar(ts)];
    drop_forming_tail(&mut bars, "1m", now);
    assert_eq!(bars.len(), 1, "a candle exactly at its close time is closed, not forming");
}

#[test]
fn only_the_final_candle_is_ever_checked() {
    // An interior "gap" candle whose own close would be in the future relative to some OTHER
    // clock is irrelevant — only the LAST row is examined (a venue never serves bars past now,
    // so no earlier row can be forming).
    let now = 1_700_000_090_000_i64;
    let mut bars = vec![bar(1_700_000_000_000)]; // close = +60_000 = 1_700_000_060_000 <= now
    drop_forming_tail(&mut bars, "1m", now);
    assert_eq!(bars.len(), 1, "the sole closed candle is untouched");
}

#[test]
fn empty_bars_is_a_no_op() {
    let mut bars: Vec<Bar> = Vec::new();
    drop_forming_tail(&mut bars, "1m", 1_700_000_000_000);
    assert!(bars.is_empty());
}

/// **THE TEST `docs/decisions/0059-…`'s Phase 1 said had to be argued against, kept — with the
/// argument.**
///
/// 0059 recorded this pin as the third reason bug B's obvious fix was refused: the behaviour it
/// asserts *was* written down as intentional, and "that reason is right for a garbage string and
/// wrong for a real interval three dispatched venues serve". Both halves of that sentence are
/// true, and they are about DIFFERENT layers, which is why the fix is a refusal one layer up
/// rather than a change here:
///
/// * For a string nobody understands (`"not-an-interval"`), declining is still right. A
///   defensive filter that panicked would take down a collector over a typo, and one that
///   guessed a step would drop a row it cannot prove is forming. Neither is an improvement on
///   doing nothing.
/// * For `1w`/`1M`/`1mo` the decline was never the whole answer, because those are REAL steps a
///   venue serves — so the outcome was a still-open candle stored as closed under a spent commit
///   key. [`ingest_klines`]'s refusal is where that is now decided, above this function and
///   before the fetch, so no real request reaches this filter with a step it cannot measure.
///
/// So this pin now covers exactly the case it was always right about, and
/// `an_unmeasurable_interval_is_refused_before_anything_is_fetched` covers the case it was
/// wrongly read as covering. Do not "fix" this one by making it drop or panic: the two tests
/// are a pair, and deleting this one would make the seam's refusal the only thing standing
/// between a garbage string and a popped row.
#[test]
fn unparseable_interval_leaves_bars_untouched() {
    let mut bars = vec![bar(1_700_000_000_000)];
    drop_forming_tail(&mut bars, "not-an-interval", i64::MAX);
    assert_eq!(bars.len(), 1);

    // ...and the three REAL steps, at this layer, still decline — the property the refusal one
    // layer up now makes unreachable from a collector, pinned here so that moving or weakening
    // that refusal changes an observable rather than nothing.
    for interval in ["1w", "1M", "1mo"] {
        let mut bars = vec![bar(1_700_000_000_000)];
        drop_forming_tail(&mut bars, interval, i64::MAX);
        assert_eq!(bars.len(), 1, "{interval} must still be DECLINED here, not acted on");
    }
}

#[test]
fn okx_style_bounded_historical_window_is_unaffected() {
    // OKX's history-candles endpoint never serves an in-progress candle — a bounded historical
    // window (end_ms far in the past relative to "now") has no forming tail to drop; this proves
    // the shared guard is a harmless no-op in that case, not that OKX is special-cased.
    let long_ago_close = 1_600_000_060_000_i64;
    let mut bars = vec![bar(1_600_000_000_000)]; // close = 1_600_000_060_000
    drop_forming_tail(&mut bars, "1m", long_ago_close + 10 * 60_000_000); // "now" far later
    assert_eq!(bars.len(), 1, "a long-closed candle is never dropped");
}

/// A perp window and its spot twin never share a commit key: aster routes `.P` inside its
/// bridge, and the key carries the caller's symbol verbatim.
#[test]
fn a_perp_and_its_spot_twin_never_share_a_commit_key() {
    let spot = commit_key("aster", "BTCUSDT", "1m", 10, 20);
    let perp = commit_key("aster", "BTCUSDT.P", "1m", 10, 20);
    assert_ne!(spot, perp);
    assert_eq!(spot, "aster:BTCUSDT:1m:10-20");
    assert_eq!(perp, "aster:BTCUSDT.P:1m:10-20");
}

/// The key is the venue tag plus the caller's symbol and window, verbatim: a Deribit instrument
/// name is not rewritten, and the format is the one the CI box's stored windows were spent under.
#[test]
fn a_commit_key_carries_the_instrument_name_and_window_verbatim() {
    assert_eq!(
        commit_key("deribit", "BTC-PERPETUAL", "1m", 10, 20),
        "deribit:BTC-PERPETUAL:1m:10-20"
    );
    assert_eq!(commit_key("binance", "BTCUSDT", "1m", 0, 179_999), "binance:BTCUSDT:1m:0-179999");
}

// ── THE INGEST, OBSERVED ON THE STORE (0059 Phase 3) ────────────────────────────────────────
//
// Phase 3 moved the fetch→ingest body into [`ingest_klines`] and left a `String`-erroring
// `backfill_klines` wrapper beside it, and these tests drove the SAME fixtures through BOTH
// paths. docs/decisions/0094 deleted the wrapper with the one-shot programs that called it; what
// stays is the half that describes [`ingest_klines`] itself, observed on the store rather than
// argued from the code.
//
// Every test gets its own temp store — a shared one would make a second call a commit-key no-op
// and an assertion could pass for the wrong reason.

/// A store over its own temp dir, returned with the dir so the dir outlives it.
fn store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().expect("temp dir");
    let hist = DataFusionHist::open(dir.path()).expect("open temp store");
    (dir, hist)
}

/// Everything observable about one ingest call: what it returned, and what landed in the store.
fn observe(
    hist: &DataFusionHist,
    outcome: Result<usize, CollectError>,
) -> (Result<usize, String>, Vec<Bar>) {
    let bars = hist
        .load_bars("testvenue", "TESTSYM", "1m", vike_data::TsRange::all())
        .expect("read the series back");
    (outcome.map_err(|e| e.to_string()), bars)
}

/// Three 1-minute bars, the last of which is STILL FORMING — so a path that kept the guard
/// writes two and a path that lost it writes three.
///
/// The last bar OPENS at "now", so its close is `now + 60_000` and
/// [`drop_forming_tail`]'s `ts + step > now` holds for any clock the ingest reads afterwards
/// (it re-reads `now_ms()` itself, a few microseconds later, and the margin is a whole minute).
fn three_bars_last_forming() -> Vec<Bar> {
    let t0 = vike_model::now_ms() - 2 * 60_000;
    vec![bar(t0), bar(t0 + 60_000), bar(t0 + 120_000)]
}

/// The ingest's whole contract on a MEASURABLE step: the still-forming tail is dropped, the rest is
/// written, and the window's commit key is spent — the SAME window re-runs to zero rows.
#[test]
fn the_ingest_drops_the_forming_tail_and_spends_the_window_key() {
    let bars = three_bars_last_forming();
    let (_d, hist) = store();

    let (written, stored) = observe(
        &hist,
        ingest_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| Ok(bars.clone())),
    );
    assert_eq!(written, Ok(2), "the still-forming tail must be dropped");
    assert_eq!(stored.len(), 2);

    let again =
        ingest_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| Ok(bars.clone()));
    assert_eq!(again.unwrap(), 0, "the ingest's window key was not spent");
}

/// A fetch failure renders as `CollectError::Fetch` — `venue fetch: …`, the message that tells an
/// operator to retry — and nothing is written.
#[test]
fn a_fetch_failure_renders_as_a_fetch_and_writes_nothing() {
    let (_d, hist) = store();
    let (outcome, stored) = observe(
        &hist,
        ingest_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
            Err(CollectError::Fetch("boom".to_string()))
        }),
    );
    assert_eq!(outcome, Err("venue fetch: boom".to_string()));
    assert!(stored.is_empty(), "a failed fetch writes nothing");
}

/// **The one thing the split BUYS**, and the reason it exists rather than being cosmetic: a
/// `CollectError::Refused` reaches the caller as a refusal instead of being re-rendered as a
/// venue fetch failure. A `String` error channel — which is what the bridges' pagers speak —
/// cannot express it at all: a refusal pushed through one comes back as `venue fetch: …`, which
/// tells an operator to retry a request that never left the box (`CollectError::Refused`'s own doc
/// argues why that matters).
#[test]
fn only_the_ingest_can_carry_a_refusal_as_a_refusal() {
    let (_d1, hist) = store();
    let refused = ingest_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
        Err(CollectError::Refused("cannot express this symbol".to_string()))
    });
    assert!(matches!(refused, Err(CollectError::Refused(_))));
    assert_eq!(refused.unwrap_err().to_string(), "refused: cannot express this symbol");
}

// ── BUG B: THE FORMING-BAR REFUSAL AT THE SEAM (0059 Phase 1) ───────────────────────────────
//
// The seam refuses an interval the step vocabulary cannot measure. These prove the three
// things a refusal has to be: it happens BEFORE the fetch, it writes nothing, and it leaves the
// window's commit key UNSPENT — which is the half that made the old behaviour permanent rather
// than merely wrong.

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

/// **The bug B fix, on both paths into the store.** `1w`, `1M` and `1mo` are refused by
/// [`ingest_klines`] and by [`crate::kline_source::backfill_kline_source`], which is what the wire
/// verb dispatches through. (A third path — the `backfill_klines` wrapper the one-shot
/// `<venue>_backfill` programs reached — went with them, docs/decisions/0094.)
///
/// Named one path at a time rather than folded: the paths are the claim, and a fold over a list
/// that shrank to one would still pass.
///
/// The fetch closures PANIC. A refusal that happened after the fetch would still write nothing
/// (the `?` propagates), so "nothing was written" alone cannot tell the two apart — and the
/// difference is a 24-month paging run against a venue, plus a rate-limit budget, spent to learn
/// something the process knew from its own argv.
#[test]
fn an_unmeasurable_interval_is_refused_before_anything_is_fetched() {
    let (_d, hist) = store();
    for interval in ["1w", "1M", "1mo"] {
        let ingest =
            ingest_klines(&hist, "testvenue", "TESTSYM", interval, 10, 20, |_, _, _, _| {
                panic!("ingest_klines fetched {interval:?}")
            });
        assert!(
            matches!(ingest, Err(CollectError::Refused(_))),
            "ingest_klines answered {interval:?} with {ingest:?}"
        );

        let dispatched = crate::kline_source::backfill_kline_source(
            &hist,
            &NeverFetches,
            "TESTSYM",
            interval,
            10,
            20,
        );
        assert!(
            matches!(dispatched, Err(CollectError::Refused(_))),
            "the registry path answered {interval:?} with {dispatched:?}"
        );

        let bars = hist
            .load_bars("testvenue", "TESTSYM", interval, vike_data::TsRange::all())
            .expect("read the series back");
        assert!(bars.is_empty(), "a refused {interval:?} request wrote rows");
    }
}

/// **The half that made bug B PERMANENT: the window's commit key must be left unspent.**
///
/// Observed directly rather than argued from the code path — after the refusal, the SAME key is
/// offered to `append_bars` and must be accepted. Had the refusal run after the append (or had
/// the old silent-decline behaviour written the forming bar), this call would return 0 rows and
/// the series would be uncorrectable for its whole life: `commit_rows` checks `has_commit`
/// first, and nothing in this workspace retires a single key.
#[test]
fn a_refused_window_leaves_its_commit_key_unspent() {
    let (_d, hist) = store();
    let refused = ingest_klines(&hist, "testvenue", "TESTSYM", "1w", 10, 20, |_, _, _, _| {
        panic!("fetched a refused window")
    });
    assert!(matches!(refused, Err(CollectError::Refused(_))));

    let key = commit_key("testvenue", "TESTSYM", "1w", 10, 20);
    let written = hist
        .append_bars("testvenue", "TESTSYM", "1w", &[bar(1_700_000_000_000)], Some(&key))
        .expect("the store takes the window");
    assert_eq!(written, 1, "the refused window had already spent its commit key");
}

/// The refusal SAYS what is wrong and what it prevents — the two things an operator needs in
/// order to act, and the shape `crates/vike-datahub/src/server.rs`'s `backfill_verb` refusal
/// already had. A refusal that only says "no" sends them to re-run with the same argument.
#[test]
fn the_refusal_names_the_step_the_guard_and_the_way_out() {
    let (_d, hist) = store();
    let err = ingest_klines(&hist, "binance", "BTCUSDT", "1M", 10, 20, |_, _, _, _| {
        unreachable!("refused before the fetch")
    })
    .expect_err("1M is refused")
    .to_string();
    assert!(err.contains("refused:"), "it renders as a refusal, not a fetch failure: {err}");
    assert!(err.contains("\"1M\""), "it names the offending step: {err}");
    assert!(err.contains("binance") && err.contains("BTCUSDT"), "it names the request: {err}");
    assert!(err.contains("commit key"), "it names what it prevents: {err}");
    assert!(err.contains("7d"), "it names a step that would work: {err}");
}

/// A MEASURABLE step is untouched by the refusal — the anti-vacuity half, and the guarantee that
/// this change narrowed exactly the three spellings it argued about.
///
/// `7d` is the interesting one: a whole week, outside nothing, and the answer for an operator
/// who was reaching for `1w`.
#[test]
fn a_measurable_step_still_fetches_and_writes() {
    let (_d, hist) = store();
    for interval in ["1m", "4h", "1d", "7d", "30s"] {
        let n = ingest_klines(&hist, "testvenue", "TESTSYM", interval, 10, 20, |_, _, _, _| {
            Ok(vec![bar(1_600_000_000_000)])
        })
        .unwrap_or_else(|e| panic!("{interval} must still ingest: {e}"));
        assert_eq!(n, 1, "{interval} wrote nothing");
    }
}

// ── THE CHUNKED INGEST (`ingest_klines_chunked`) ───────────────────────────────────────────────
//
// A fake venue that counts and records every fetch, over a real `DataFusionHist` in a temp dir.
// Every test hands the ingest an EXPLICIT `now_ms`, so none of them depends on when it runs: their
// windows live in the 1970s and `LATER` puts the clock far past all of them — except where a test
// aims at the settle boundary and says so, and the one that reads the wall clock
// (`the_public_entry_reads_the_wall_clock_and_leaves_today_alone`), which asserts only what holds at
// any time of any UTC day.

use std::sync::Mutex;

use vike_data::source::{KlineSource, SourceError};

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

/// [`ingest_klines_chunked`] over a [`FakeMarket`], answering the whole outcome rather than just rows.
fn run_chunked(
    hist: &DataFusionHist,
    market: &FakeMarket,
    interval: &str,
    window: (i64, i64),
    now_ms: i64,
) -> Result<ChunkedOutcome, CollectError> {
    ingest_klines_chunked(hist, market.venue(), CSYM, interval, window, now_ms, |sym, iv, s, e| {
        market.fetch(sym, iv, s, e).map_err(CollectError::from)
    })
}

fn chunk_series(interval: &str) -> SeriesId {
    SeriesId::per_symbol("bar", CVENUE, CSYM, Some(interval.to_string()))
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

/// A request that ran to the end partitions its chunks — every one is skipped, fetched, empty or
/// unsettled, and none is counted twice or left out.
fn assert_partitions(out: &ChunkedOutcome) {
    assert_eq!(
        out.skipped + out.fetched + out.empty + out.unsettled,
        out.chunks,
        "the chunks of a finished request partition: {out:?}"
    );
}

/// **Outward rounding, in the store.** A request that starts and ends mid-day is fetched as the
/// three WHOLE days it touches, one call each, and stored under the grid's own keys — never under
/// the request's ragged bounds, and holding the bars before the request's start too: a request for
/// part of a day stores the whole day.
#[test]
fn a_request_is_rounded_outward_to_whole_days_and_fetched_a_day_at_a_time() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);
    let (start, end) = (2 * DAY + 5 * HOUR, 4 * DAY + 2 * HOUR);

    let out = run_chunked(&hist, &market, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        market.calls(),
        vec![(2 * DAY, 3 * DAY - 1), (3 * DAY, 4 * DAY - 1), (4 * DAY, 5 * DAY - 1)],
        "one fetch per whole day, in order — each chunk is one day, and the daemon holds one at a time"
    );
    assert_eq!((out.chunks, out.fetched, out.skipped, out.empty, out.unsettled), (3, 3, 0, 0, 0));
    assert_eq!(out.rows, 72);
    assert_partitions(&out);

    let bars = stored_at(&hist, "1h");
    assert_eq!(bars.len(), 72, "three whole days of hourly bars");
    assert_eq!(bars.first().unwrap().0, 2 * DAY, "the bars BEFORE the request's start are stored");
    assert_eq!(bars.last().unwrap().0, 5 * DAY - HOUR, "...and the bars AFTER its end");

    // The key is the shared format over the chunk's own bounds, spelled out; the request's own
    // bounds mint none.
    assert_eq!(
        commit_key(CVENUE, CSYM, "1h", 2 * DAY, 3 * DAY - 1),
        "fakevenue:EURUSD:1h:172800000-259199999"
    );
    let mut keys = hist.series_commits(&chunk_series("1h")).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "fakevenue:EURUSD:1h:172800000-259199999".to_string(),
            "fakevenue:EURUSD:1h:259200000-345599999".to_string(),
            "fakevenue:EURUSD:1h:345600000-431999999".to_string(),
        ],
        "exactly the three grid chunks' keys"
    );
    assert!(!spent(&hist, "1h", start, end), "a ragged request bound is never a key");
}

/// The grid itself, on the pure helper: a window inside one day is that whole day, exactly one day is
/// one chunk and not two, one millisecond into the next day is the whole next day, a midnight
/// straddled by a millisecond each side is both days — and a pre-1970 stamp rounds DOWN like any
/// other, which is why the grid uses `div_euclid` rather than `/`.
#[test]
fn the_chunk_grid_rounds_outward_and_never_cuts_a_day() {
    assert_eq!(chunk_grid(2 * DAY + 3 * HOUR, 2 * DAY + 4 * HOUR), Some((2 * DAY, 2 * DAY)));
    assert_eq!(chunk_grid(2 * DAY, 3 * DAY - 1), Some((2 * DAY, 2 * DAY)), "one day is one chunk");
    assert_eq!(chunk_grid(2 * DAY, 3 * DAY), Some((2 * DAY, 3 * DAY)), "1 ms into the next day");
    assert_eq!(chunk_grid(2 * DAY - 1, 2 * DAY), Some((DAY, 2 * DAY)), "a midnight straddled");
    assert_eq!(chunk_grid(-1, 0), Some((-DAY, 0)), "pre-1970 rounds DOWN, not toward zero");
    assert_eq!(chunk_grid(-DAY, -1), Some((-DAY, -DAY)));
    assert_eq!(chunk_grid(i64::MIN, 0), None, "the first start would fall below i64::MIN");
    assert_eq!(chunk_grid(0, i64::MAX), None, "the last chunk's END would pass i64::MAX");
    assert!(chunk_grid(0, i64::MAX - DAY).is_some(), "the edge is refused, not the whole range");
}

/// **The rule, on the pure helper.** A chunk is settled when its last possible bar — the one that
/// opens at `c1` — has closed AND the margin has passed, so all three terms are in it, and at the
/// instant itself it IS settled. Pinned here on exact values, then again through the store below.
#[test]
fn the_settle_rule_is_the_close_of_the_last_possible_bar_plus_the_margin() {
    let (c1, step) = (5 * DAY - 1, HOUR);
    let at = c1 + step + SETTLE_MARGIN_MS;
    assert_eq!(settle_instant(c1, step), at);
    assert!(chunk_is_settled(c1, step, at), "AT the instant it is settled");
    assert!(!chunk_is_settled(c1, step, at - 1), "one millisecond before it, it is not");
    assert!(!chunk_is_settled(c1, step, c1 + SETTLE_MARGIN_MS), "the last bar has not closed yet");
    assert!(!chunk_is_settled(c1, step, c1 + step), "the margin has not passed yet");
    assert!(!chunk_is_settled(c1, DAY, at), "a longer step closes later, so it settles later");
    assert!(!chunk_is_settled(i64::MAX, HOUR, i64::MAX - 1), "saturating, never wrapped");
}

/// **A repeat of a stored request fetches NOTHING.** The strongest form of the skip: the second
/// run's fetch panics if it is called at all — not merely counted — and every chunk is answered from
/// the manifest.
#[test]
fn a_repeat_of_a_stored_request_fetches_nothing() {
    let (_d, hist) = store();
    let window = (2 * DAY + 5 * HOUR, 4 * DAY + 2 * HOUR);
    let first = run_chunked(&hist, &FakeMarket::new(HOUR), "1h", window, LATER).unwrap();
    assert_eq!(first.rows, 72);
    let before = stored_at(&hist, "1h");

    let again = ingest_klines_chunked(&hist, CVENUE, CSYM, "1h", window, LATER, |_, _, _, _| {
        panic!("a settled chunk whose commit key is spent must not be fetched at all")
    })
    .unwrap();

    assert_eq!((again.rows, again.skipped, again.fetched, again.chunks), (0, 3, 0, 3));
    assert_partitions(&again);
    assert_eq!(stored_at(&hist, "1h"), before, "and nothing was written twice");
}

/// **Resume.** Chunk 3 of 4 fails: the error names the chunk, the request and the bars already
/// written; the chunks before it stay written; the chunk after it is never asked for. A retry then
/// fetches ONLY the failed chunk and the one after it, and the store ends up holding exactly what an
/// uninterrupted run would have.
#[test]
fn a_failed_chunk_keeps_the_chunks_before_it_and_a_retry_resumes_at_it() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 6 * DAY - 1);
    let flaky = FakeMarket::new(HOUR).failing_at(4 * DAY);

    let failed = run_chunked(&hist, &flaky, "1h", (start, end), LATER);

    let Err(CollectError::Fetch(text)) = &failed else {
        panic!("a fetch failure must answer as a Fetch: {failed:?}")
    };
    for part in [
        "chunk 3 of 4".to_string(),
        format!("[{}, {}]", 4 * DAY, 5 * DAY - 1),
        format!("(request [{start}, {end}]"),
        "after 48 1h bars were written".to_string(),
        "the venue went away".to_string(),
    ] {
        assert!(text.contains(&part), "the error must carry {part:?}: {text}");
    }
    assert_eq!(
        flaky.calls(),
        vec![(2 * DAY, 3 * DAY - 1), (3 * DAY, 4 * DAY - 1), (4 * DAY, 5 * DAY - 1)],
        "the chunk after the failed one is never asked for"
    );
    let kept = stored_at(&hist, "1h");
    assert_eq!(kept.len(), 48, "the chunks before the failure stay written");
    assert_eq!(kept.last().unwrap().0, 4 * DAY - HOUR);

    let healthy = FakeMarket::new(HOUR);
    let retried = run_chunked(&hist, &healthy, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        healthy.calls(),
        vec![(4 * DAY, 5 * DAY - 1), (5 * DAY, 6 * DAY - 1)],
        "a retry skips what was written and resumes at the chunk that failed"
    );
    assert_eq!((retried.rows, retried.skipped, retried.fetched), (48, 2, 2));
    assert_partitions(&retried);
    let (_c, control) = store();
    run_chunked(&control, &FakeMarket::new(HOUR), "1h", (start, end), LATER).unwrap();
    assert_eq!(
        stored_at(&hist, "1h"),
        stored_at(&control, "1h"),
        "interrupted then resumed is what one uninterrupted run stores"
    );
}

/// A `Refused` from the source is a refusal and stays one — at the first chunk and at the third — and
/// the chunks before it stay written. Driven through [`backfill_kline_source_chunked_at`], the
/// `KlineSource` face, so it also proves `CollectError::from` is what carries the source's error in.
#[test]
fn a_refusal_from_the_source_passes_through_unchanged_at_any_chunk() {
    for (refused_at, written) in [(2 * DAY, 0), (4 * DAY, 48)] {
        let (_d, hist) = store();
        let market = FakeMarket::new(HOUR).failing_at(refused_at).refusing();

        let err = crate::kline_source::backfill_kline_source_chunked_at(
            &hist,
            &market,
            CSYM,
            "1h",
            2 * DAY,
            6 * DAY - 1,
            LATER,
        )
        .unwrap_err();

        assert!(
            matches!(&err, CollectError::Refused(why) if why == "the venue went away"),
            "a refusal must come back as the source's own Refused, not a Fetch: {err:?}"
        );
        assert_eq!(stored_at(&hist, "1h").len(), written, "refused at day {}", refused_at / DAY);
    }
}

/// **The settled-only policy, through the store, on and around the exact boundary.** Days 2..=5 are
/// requested. Day 3's last possible bar (the 23:00 one) closes at day 4's midnight and the chunk is
/// settled a margin after that: AT that instant days 2 and 3 are written, one millisecond earlier
/// only day 2 is, and long before it nothing is. The unsettled chunk and everything after it are
/// never fetched and never spend a key; the call still answers `Ok`, and the outcome — which is what
/// the info log is built from — says where storage stops and when the first unwritten chunk settles.
#[test]
fn an_unsettled_tail_is_not_written_and_is_reported() {
    let (start, end) = (2 * DAY, 6 * DAY - 1);
    let day3_settles = 4 * DAY - 1 + HOUR + SETTLE_MARGIN_MS;
    for (now, written) in [(LATER, 4_i64), (day3_settles, 2), (day3_settles - 1, 1), (2 * DAY, 0)] {
        let (_d, hist) = store();
        let market = FakeMarket::new(HOUR);
        let context = format!("now = {now}");

        let out = run_chunked(&hist, &market, "1h", (start, end), now).unwrap();

        assert_eq!(out.rows as i64, written * 24, "{context}");
        assert_eq!((out.chunks, out.fetched, out.skipped, out.empty), (4, written as u64, 0, 0));
        assert_eq!(out.unsettled, 4 - written as u64, "{context}");
        assert_partitions(&out);
        assert_eq!(
            market.calls().len() as i64,
            written,
            "unsettled chunks are never fetched: {context}"
        );
        let ts: Vec<i64> = stored_at(&hist, "1h").iter().map(|b| b.0).collect();
        assert_eq!(ts.first().copied(), (written > 0).then_some(2 * DAY), "{context}");
        assert_eq!(
            ts.last().copied(),
            (written > 0).then_some((2 + written) * DAY - HOUR),
            "{context}"
        );
        if written < 4 {
            let first_unsettled = (2 + written) * DAY;
            assert_eq!(out.stops_before, Some(first_unsettled), "{context}");
            assert_eq!(
                out.settles_at,
                Some(first_unsettled + DAY - 1 + HOUR + SETTLE_MARGIN_MS),
                "the instant the first unwritten chunk settles: {context}"
            );
            assert!(
                !spent(&hist, "1h", first_unsettled, first_unsettled + DAY - 1),
                "an unsettled chunk spends no key: {context}"
            );
        } else {
            assert_eq!((out.stops_before, out.settles_at), (None, None), "{context}");
        }
    }
}

/// **The log lines say what the outcome knows, in the words an operator reads.** The same request as
/// the settle-boundary test, with "now" at noon on day 4: days 2 and 3 are stored, days 4 and 5 are
/// not. The line names where storage stops (day 4's midnight, in UTC and in milliseconds), how many of
/// how many chunks were left, "now", and the instant the first of them settles — a step and a margin
/// past day 4's last millisecond, truncated to the second. A fully settled request has no such line,
/// and the one summary line carries every count.
#[test]
fn the_log_says_where_storage_stops_and_when_the_rest_settles() {
    let (_d, hist) = store();
    let now = 4 * DAY + 12 * HOUR; // 1970-01-05T12:00:00Z
    let out =
        run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (2 * DAY, 6 * DAY - 1), now).unwrap();
    let label = "fakevenue EURUSD 1h";

    let line = unsettled_line(&out, label, now).expect("two chunks were left unwritten");

    assert!(
        line.starts_with(
            "fakevenue EURUSD 1h: storage stops before 1970-01-05T00:00:00Z (345600000)."
        ),
        "{line}"
    );
    assert!(
        line.contains("2 of 4 day chunks are not settled at 1970-01-05T12:00:00Z (388800000)"),
        "{line}"
    );
    assert!(line.contains("the first settles at 1970-01-06T01:09:59Z (436199999)"), "{line}");
    assert!(line.contains("live feed"), "it says who owns the recent edge: {line}");

    let everything =
        run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (2 * DAY, 6 * DAY - 1), LATER).unwrap();
    assert_eq!(unsettled_line(&everything, label, LATER), None, "nothing was left unwritten");

    let window = (2 * DAY, 6 * DAY - 1);
    let summary = summary_line(&out, label, window, std::time::Duration::from_millis(1500));
    assert_eq!(
        summary,
        "fakevenue EURUSD 1h [172800000, 518399999]: 4 day chunks: 0 skipped (already stored), \
         2 fetched, 0 empty, 2 unsettled; 48 bars written in 1.5s"
    );
    let with_strays = ChunkedOutcome { out_of_range: 6, ..ChunkedOutcome::default() };
    let elapsed = std::time::Duration::ZERO;
    assert!(
        summary_line(&with_strays, label, window, elapsed)
            .ends_with("; 6 out-of-range bars dropped")
    );
    assert!(!summary.contains("out-of-range"), "a request with no strays says nothing of them");
}

/// **An empty chunk writes nothing and spends no key** — so a re-run asks for it again, and ONLY it.
/// Day 3 has no candles (a Saturday, say): the first run stores days 2 and 4, the second re-fetches
/// day 3 alone, and finds it empty again. That is the accepted cost, pinned so it stays a declared
/// one.
#[test]
fn an_empty_chunk_writes_nothing_spends_no_key_and_is_asked_again() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 5 * DAY - 1);

    let first =
        run_chunked(&hist, &FakeMarket::new(HOUR).closed_on(3), "1h", (start, end), LATER).unwrap();

    assert_eq!((first.chunks, first.fetched, first.empty, first.rows), (3, 2, 1, 48));
    assert_partitions(&first);
    assert!(spent(&hist, "1h", 2 * DAY, 3 * DAY - 1));
    assert!(!spent(&hist, "1h", 3 * DAY, 4 * DAY - 1), "the empty day spent no key");
    assert!(spent(&hist, "1h", 4 * DAY, 5 * DAY - 1));

    let again = FakeMarket::new(HOUR).closed_on(3);
    let second = run_chunked(&hist, &again, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        again.calls(),
        vec![(3 * DAY, 4 * DAY - 1)],
        "only the empty day is asked for again"
    );
    assert_eq!((second.skipped, second.empty, second.fetched, second.rows), (2, 1, 0, 0));
    assert_partitions(&second);
}

/// A step the store cannot measure — and a zero-width one, which measures — is refused BEFORE any
/// fetch and before any manifest is touched: the fetch panics if it is called, and no series exists
/// afterwards. Through both entry points, the `KlineSource` face with a source that panics too. The
/// two refusals say different things, and the test holds them to it: a zero-width step is ALSO
/// something `interval_ms` cannot make a bar of, so without the message the predicate's own check
/// could be deleted and this would stay green.
#[test]
fn an_unmeasurable_or_zero_width_interval_is_refused_before_anything_is_fetched_chunked() {
    let (_d, hist) = store();
    let cases = [
        ("1w", "no bar width"),
        ("1M", "no bar width"),
        ("1mo", "no bar width"),
        ("", "no bar width"),
        ("x", "no bar width"),
        ("5", "no bar width"),
        ("0m", "zero width"),
        ("0s", "zero width"),
        ("00d", "zero width"),
    ];
    for (interval, says) in cases {
        let refused = ingest_klines_chunked(
            &hist,
            CVENUE,
            CSYM,
            interval,
            (0, DAY - 1),
            LATER,
            |_, _, _, _| panic!("the chunked ingest fetched {interval:?}"),
        );
        let Err(CollectError::Refused(why)) = &refused else {
            panic!("{interval:?} must be refused, got {refused:?}")
        };
        assert!(why.contains(says), "{interval:?} must be refused as {says:?}: {why}");
        let dispatched = crate::kline_source::backfill_kline_source_chunked_at(
            &hist,
            &NeverFetches,
            CSYM,
            interval,
            0,
            DAY - 1,
            LATER,
        );
        assert!(
            matches!(dispatched, Err(CollectError::Refused(_))),
            "the KlineSource face answered {interval:?} with {dispatched:?}"
        );
    }
    assert!(hist.list_series().unwrap().is_empty(), "a refused request touched the store");
}

/// The other refusals, also before any fetch: a symbol that cannot be a directory name, an inverted
/// window, and a window the day grid cannot express at the edge of `i64`.
#[test]
fn a_hostile_symbol_an_inverted_window_and_an_unrepresentable_window_are_refused_first() {
    let (_d, hist) = store();
    let panics = |_: &str, _: &str, _: i64, _: i64| -> Result<Vec<Bar>, CollectError> {
        panic!("a refused request fetched")
    };
    for symbol in ["../x", "a/b", "a\\b", "EUR:USD", "EUR?"] {
        let refused =
            ingest_klines_chunked(&hist, CVENUE, symbol, "1h", (0, DAY - 1), LATER, panics);
        assert!(matches!(refused, Err(CollectError::Refused(_))), "{symbol:?}: {refused:?}");
    }
    for window in [(DAY, 0), (0, i64::MAX), (i64::MIN, 0)] {
        let refused = ingest_klines_chunked(&hist, CVENUE, CSYM, "1h", window, LATER, panics);
        assert!(matches!(refused, Err(CollectError::Refused(_))), "{window:?}: {refused:?}");
    }
    assert!(hist.list_series().unwrap().is_empty(), "a refused request touched the store");
}

/// **Bars outside the chunk are dropped.** A venue that serves three bars past each end of the window
/// it was asked for would, undropped, store the seam bars in TWO adjacent chunks — the store dedups by
/// commit key, never by row. Dropped, the store holds exactly what a well-behaved venue's run holds,
/// every timestamp once, and the strays are counted.
#[test]
fn bars_outside_the_chunk_are_dropped_counted_and_never_stored_twice() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 5 * DAY - 1);

    let out =
        run_chunked(&hist, &FakeMarket::new(HOUR).straying(3), "1h", (start, end), LATER).unwrap();

    assert_eq!(out.out_of_range, 3 * (3 + 3), "three either side of each of the three chunks");
    assert_eq!(out.rows, 72);
    let stored = stored_at(&hist, "1h");
    assert!(stored.windows(2).all(|pair| pair[0].0 < pair[1].0), "a bar was stored twice");
    let (_c, clean) = store();
    run_chunked(&clean, &FakeMarket::new(HOUR), "1h", (start, end), LATER).unwrap();
    assert_eq!(stored, stored_at(&clean, "1h"), "the strays left no trace in the store");
}

/// A chunk the source answers ONLY with strays — every bar it served is outside the day it was asked
/// for — is an empty chunk: nothing written, no key spent, every stray counted.
#[test]
fn a_chunk_answered_only_with_strays_is_empty_and_spends_no_key() {
    let (_d, hist) = store();
    let elsewhere = vec![market_bar(10 * DAY, HOUR), market_bar(10 * DAY + HOUR, HOUR)];

    let out = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        (2 * DAY, 3 * DAY - 1),
        LATER,
        |_, _, _, _| Ok(elsewhere.clone()),
    )
    .unwrap();

    assert_eq!((out.chunks, out.empty, out.fetched, out.rows, out.out_of_range), (1, 1, 0, 0, 2));
    assert_partitions(&out);
    assert!(!spent(&hist, "1h", 2 * DAY, 3 * DAY - 1), "an all-strays chunk spent a key");
    assert!(stored_at(&hist, "1h").is_empty(), "a stray was stored");
}

/// **Content equality with one big fetch.** For steps that divide a day, steps that do not, a step of
/// a day and a step longer than one, the chunked store holds bit for bit what a single per-window
/// ingest of the same rounded window holds — a bar belongs to the chunk holding its OPEN time, and the
/// union of the chunks is the window.
#[test]
fn the_chunked_store_holds_what_one_big_fetch_holds() {
    for interval in ["1m", "1h", "7h", "1d", "2d"] {
        let step = vike_model::time::interval_ms(interval).unwrap();
        let market = FakeMarket::new(step);
        let (_d, chunked) = store();
        run_chunked(&chunked, &market, interval, (3 * DAY + 5 * HOUR, 7 * DAY + 2 * HOUR), LATER)
            .unwrap();

        let (_c, control) = store();
        ingest_klines(&control, CVENUE, CSYM, interval, 3 * DAY, 8 * DAY - 1, |sym, iv, s, e| {
            market.fetch(sym, iv, s, e).map_err(CollectError::from)
        })
        .unwrap();

        let got = stored_at(&chunked, interval);
        assert!(!got.is_empty(), "{interval}: nothing was stored, so nothing was compared");
        assert_eq!(got, stored_at(&control, interval), "{interval}");
    }
}

/// One request that meets every kind of chunk: day 2 was stored by an earlier request (skipped),
/// day 3 has no candles (empty), days 4..=6 are fetched, days 7 and 8 are not yet settled. The tally
/// says so, and says where storage stops.
#[test]
fn a_mixed_request_tallies_every_kind_of_chunk() {
    let (_d, hist) = store();
    run_chunked(&hist, &FakeMarket::new(HOUR).closed_on(3), "1h", (2 * DAY, 3 * DAY - 1), LATER)
        .unwrap();
    // Day 6 settles a step and a margin after its last millisecond; day 7 does not.
    let now = 7 * DAY - 1 + HOUR + SETTLE_MARGIN_MS;
    let market = FakeMarket::new(HOUR).closed_on(3);

    let out = run_chunked(&hist, &market, "1h", (2 * DAY, 9 * DAY - 1), now).unwrap();

    assert_eq!(
        (out.chunks, out.skipped, out.empty, out.fetched, out.unsettled),
        (7, 1, 1, 3, 2),
        "{out:?}"
    );
    assert_partitions(&out);
    assert_eq!(out.rows, 72);
    assert_eq!(out.stops_before, Some(7 * DAY));
    assert_eq!(out.settles_at, Some(8 * DAY - 1 + HOUR + SETTLE_MARGIN_MS));
    let asked: Vec<i64> = market.calls().iter().map(|(from, _)| from / DAY).collect();
    assert_eq!(
        asked,
        vec![3, 4, 5, 6],
        "day 2 skipped, days 7 and 8 unsettled, the rest asked for"
    );
}

/// **The public entry point reads a clock** — the wall clock, once — and takes its venue from the
/// source. Placed against today's UTC midnight and asserting only what is true at any moment of any
/// day: the two days before yesterday are always settled and always written, and today, which cannot
/// be settled yet, is never fetched. (Yesterday is not asserted: it settles a step and a margin past
/// midnight, so which side of it a run lands on depends on when the test starts.)
#[test]
fn the_public_entry_reads_the_wall_clock_and_leaves_today_alone() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(DAY);

    let written = crate::kline_source::backfill_kline_source_chunked(
        &hist,
        &market,
        CSYM,
        "1h",
        today - 3 * DAY,
        today + DAY - 1,
    )
    .unwrap();

    let asked: Vec<i64> = market.calls().iter().map(|(from, _)| *from).collect();
    assert!(asked.contains(&(today - 3 * DAY)) && asked.contains(&(today - 2 * DAY)), "{asked:?}");
    assert!(!asked.contains(&today), "today is not settled and must never be fetched: {asked:?}");
    assert_eq!(written, 24 * asked.len(), "every fetched day was a full day of bars");
    let stored = hist
        .load_bars(CVENUE, CSYM, "1h", vike_data::TsRange::all())
        .expect("the rows are under the SOURCE's venue");
    assert_eq!(stored.len(), written);
}
