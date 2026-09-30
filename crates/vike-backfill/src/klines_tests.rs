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
