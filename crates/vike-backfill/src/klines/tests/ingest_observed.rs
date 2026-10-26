//! `ingest_klines` observed on the store, and the forming-bar refusal at the seam (bug B).

use super::*;
use std::assert_matches;

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
    assert_matches!(refused, Err(CollectError::Refused(_)));
    assert_eq!(refused.unwrap_err().to_string(), "refused: cannot express this symbol");
}

// ── BUG B: THE FORMING-BAR REFUSAL AT THE SEAM (0059 Phase 1) ───────────────────────────────
//
// The seam refuses an interval the step vocabulary cannot measure. These prove the three
// things a refusal has to be: it happens BEFORE the fetch, it writes nothing, and it leaves the
// window's commit key UNSPENT — which is the half that made the old behaviour permanent rather
// than merely wrong.

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
        assert_matches!(
            ingest,
            Err(CollectError::Refused(_)),
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
        assert_matches!(
            dispatched,
            Err(CollectError::Refused(_)),
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
    assert_matches!(refused, Err(CollectError::Refused(_)));

    let key = commit_key("testvenue", "TESTSYM", "1w", 10, 20);
    let written = hist
        .append_bars("testvenue", "TESTSYM", "1w", &[bar(1_700_000_000_000)], Some(&key))
        .expect("the store takes the window");
    assert_eq!(written, 1, "the refused window had already spent its commit key");
}

/// The refusal SAYS what is wrong and what it prevents — the two things an operator needs in
/// order to act, and the shape `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` refusal
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
