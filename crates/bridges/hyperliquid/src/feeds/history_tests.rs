use super::*;
use serde_json::json;
use std::assert_matches;

/// A synthetic `candleSnapshot` response: two ascending candles in the exact wire shape (`t`/`T`
/// are JSON numbers; `o`/`h`/`l`/`c`/`v` decimal strings; `s`/`i`/`n` metadata).
const TWO_CANDLES: &str = r#"[
    {"t":1700000000000,"T":1700000059999,"s":"BTC","i":"1m","o":"27000.10","c":"27010.25","h":"27050.50","l":"26980.00","v":"12.34567800","n":15},
    {"t":1700000060000,"T":1700000119999,"s":"BTC","i":"1m","o":"27010.25","c":"27080.10","h":"27100.00","l":"27000.00","v":"8.10000000","n":42}
]"#;

#[test]
fn snapshot_array_maps_to_bars_with_exact_bits() {
    let resp: Value = serde_json::from_str(TWO_CANDLES).unwrap();
    let bars = bars_from_snapshot(&resp);
    assert_eq!(bars.len(), 2);
    // Ascending by open-time, mapped exactly (reuses market_data::candle_data_to_bar).
    assert_eq!(bars[0].ts, 1_700_000_000_000);
    assert_eq!(bars[1].ts, 1_700_000_060_000);
    assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
    assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
    assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
    assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
    assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
    // OHLCV bar only — no symbol/funding/bid/ask carried (byte-consistent with the live mapper).
    assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    assert!(bars[0].bid.is_none() && bars[0].ask.is_none());
    // The last candle's close is the paging cursor advance point.
    assert_eq!(last_close_ms(&resp), Some(1_700_000_119_999));
}

#[test]
fn empty_or_non_array_yields_no_bars() {
    assert!(bars_from_snapshot(&json!([])).is_empty());
    assert!(bars_from_snapshot(&json!({"status": "err"})).is_empty());
    assert!(last_close_ms(&json!([])).is_none());
    assert!(last_close_ms(&json!({"status": "err"})).is_none());
}

#[test]
fn malformed_candle_is_skipped_in_place() {
    // Three rows; the middle one is missing `h` (unparseable → dropped by candle_data_to_bar),
    // the two good rows survive — per-element tolerance, same as the live feed.
    let resp = json!([
        {"t":1,"T":1,"o":"1","h":"1","l":"1","c":"1","v":"1"},
        {"t":2,"T":2,"o":"2","l":"2","c":"2","v":"2"},
        {"t":3,"T":3,"o":"3","h":"3","l":"3","c":"3","v":"3"}
    ]);
    let bars = bars_from_snapshot(&resp);
    assert_eq!(bars.len(), 2, "the middle candle missing `h` is dropped");
    assert_eq!(bars[0].ts, 1);
    assert_eq!(bars[1].ts, 3);
}

/// One synthetic well-formed 1m candle row at index `i` (open `i*60_000`, close `+59_999`).
fn good_row(i: i64) -> Value {
    json!({
        "t": i * 60_000,
        "T": i * 60_000 + 59_999,
        "o": "1", "h": "1", "l": "1", "c": "1", "v": "1"
    })
}

#[test]
fn full_raw_page_with_one_malformed_candle_keeps_paging() {
    // THE audit bug: MAX_CANDLES raw rows of which ONE is malformed parse to MAX_CANDLES − 1
    // bars. Exhaustion must read the RAW count — the FILTERED count would misread this full
    // page as "history ran out" and silently drop every older page.
    let mut rows: Vec<Value> = (0..MAX_CANDLES as i64).map(good_row).collect();
    // Row 17 loses `h` → unparseable → dropped by the tolerant per-row parse.
    rows[17] = json!({"t": 17 * 60_000, "T": 17 * 60_000 + 59_999, "o": "1", "l": "1", "c": "1", "v": "1"});
    let resp = Value::Array(rows);

    let (bars, next) = page_outcome(&resp, 0);
    assert_eq!(bars.len(), MAX_CANDLES - 1, "the malformed row is still dropped from the bars");
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    assert_eq!(
        next,
        Some(last_close + 1),
        "a FULL raw page keeps paging: cursor advances one ms past the last close"
    );
}

#[test]
fn full_raw_page_of_unparseable_rows_still_pages_past_itself() {
    // Even a page where EVERY row fails the tolerant parse is not end-of-history: the raw
    // count says the venue had a full page there, so the cursor still advances past its last
    // close and the history beyond it stays reachable. (Pre-fix, the empty FILTERED page
    // ended the loop here.)
    let rows: Vec<Value> = (0..MAX_CANDLES as i64)
        .map(|i| json!({"t": i * 60_000, "T": i * 60_000 + 59_999}))
        .collect();
    let resp = Value::Array(rows);

    let (bars, next) = page_outcome(&resp, 0);
    assert!(bars.is_empty(), "no row parses");
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    assert_eq!(next, Some(last_close + 1));
}

#[test]
fn short_raw_page_ends_paging_with_its_bars() {
    let resp: Value = serde_json::from_str(TWO_CANDLES).unwrap();
    let (bars, next) = page_outcome(&resp, 0);
    assert_eq!(bars.len(), 2, "the short page's bars are still returned");
    assert_eq!(next, None, "a short RAW page = history ran out");
}

#[test]
fn empty_or_non_array_page_ends_paging() {
    let (bars, next) = page_outcome(&json!([]), 0);
    assert!(bars.is_empty());
    assert_eq!(next, None, "an empty RAW page = no history in the window");
    let (bars, next) = page_outcome(&json!({"status": "err"}), 0);
    assert!(bars.is_empty());
    assert_eq!(next, None, "a non-array body = no history");
}

#[test]
fn full_page_that_cannot_advance_the_cursor_ends_paging() {
    let rows: Vec<Value> = (0..MAX_CANDLES as i64).map(good_row).collect();
    let resp = Value::Array(rows);
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    // A cursor already at/past the advance point must not move backward/stall → defensive stop
    // (never re-fetches the same page forever).
    let (_, next) = page_outcome(&resp, last_close + 1);
    assert_eq!(next, None, "a stalled cursor ends the loop");
    // And a full page whose last `T` is unreadable cannot advance either.
    let mut rows: Vec<Value> = (0..MAX_CANDLES as i64).map(good_row).collect();
    *rows.last_mut().unwrap() = json!({"t": 0, "o": "1"});
    let (_, next) = page_outcome(&Value::Array(rows), 0);
    assert_eq!(next, None, "an unreadable last close ends the loop");
}

#[test]
fn snapshot_body_has_the_hl_shape() {
    let body = candle_snapshot_body("BTC", "1m", 1000, 2000);
    assert_eq!(body["type"], "candleSnapshot");
    assert_eq!(body["req"]["coin"], "BTC");
    assert_eq!(body["req"]["interval"], "1m");
    assert_eq!(body["req"]["startTime"], 1000);
    assert_eq!(body["req"]["endTime"], 2000);
}

// --- the dispatch adapter's refusal (moved from `vike-backfill`'s `venues/hyperliquid.rs`,
// docs/decisions/0094) ----------------------------------------------------------------------------

/// Every PERP spelling is identity-mapped, core and HIP-3 alike — read off `symbology.rs`'s
/// `load_perps`/`extend_with_perp_dex`, both of which set `coin` and `symbol` from one field.
#[test]
fn a_perp_symbol_is_its_own_coin() {
    for sym in ["BTC", "ETH", "HYPE", "kPEPE", "test:BTC"] {
        assert_eq!(identity_coin_for(sym).expect("a perp is identity-mapped"), sym);
    }
}

/// A SPOT pair is refused rather than fetched under a guessed coin — the wrong-book failure
/// this adapter exists to prevent. The message must name the way forward (a canonical spelling
/// in `vike-catalog`), because "refused" with no way forward is how an operator ends up
/// hand-editing a roster.
#[test]
fn a_spot_pair_is_refused_and_the_message_names_the_way_forward() {
    for sym in ["HYPE/USDC", "PURR/USDC", "BTC/USDC"] {
        let why = identity_coin_for(sym).expect_err("a spot pair is not identity-mapped");
        assert!(why.contains("SPOT"), "{why}");
        assert!(why.contains("vike-catalog"), "names the way forward: {why}");
    }
}

/// ⚠ `PURR/USDC` is the one pair whose venue coin IS its name, so identity WOULD fetch the
/// right candles — and it is still refused, because `series_dir` interpolates the symbol into
/// `symbol={symbol}` and a `/` is a directory separator. Pinned as its own case so a later
/// "but that one works" edit has to argue with the store-layout half rather than only the
/// symbology half.
#[test]
fn the_one_identity_spot_pair_is_refused_too() {
    assert!(identity_coin_for("PURR/USDC").is_err());
}

/// A raw venue coin is refused: it would fetch correctly and store under a partition no
/// catalog row and no other producer ever spells.
#[test]
fn a_raw_venue_coin_is_refused() {
    let why = identity_coin_for("@107").expect_err("@N is a coin, not a unified symbol");
    assert!(why.contains("raw venue"), "{why}");
}

#[test]
fn an_empty_symbol_is_refused() {
    assert!(identity_coin_for("").is_err());
}

/// The REGISTRY ROW ([`HyperliquidKlines`], what the datahub's wire verb dispatches through)
/// surfaces the refusal as a `SourceError::Refused`, offline, before the venue is asked
/// anything — never as a `Fetch`, which would tell an operator to retry a request that never
/// left the box.
///
/// The registry-row half of the pinned disagreement that sat here while the one-shot
/// `hyperliquid_backfill` program could still express a `SYM=coin` mapping; docs/decisions/0094
/// deleted the program, which settled the disagreement in the registry's favour.
#[test]
fn the_registry_row_refuses_a_spot_pair_before_the_venue_is_asked() {
    use vike_data::source::KlineSource;

    let refused = HyperliquidKlines
        .fetch("HYPE/USDC", "1h", 0, 1)
        .expect_err("a non-identity symbol is refused by the one-symbol seam");
    assert_matches!(
        refused,
        vike_data::source::SourceError::Refused(_),
        "it must be a REFUSAL, not a fetch failure: {refused}"
    );
    assert!(refused.to_string().contains("vike-catalog"), "names the way forward: {refused}");
}
