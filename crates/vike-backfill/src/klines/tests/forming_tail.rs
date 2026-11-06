//! The bar-level forming-tail filter and the per-window commit key, on pure values.

use super::*;

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
