use super::*;
use serde_json::json;
use std::assert_matches;

/// A synthetic `candleSnapshot` response: two ascending candles in the exact wire shape (`t`/`T`
/// are JSON numbers; `o`/`h`/`l`/`c`/`v` decimal strings; `s`/`i`/`n` metadata).
const TWO_CANDLES: &str = r#"[
    {"t":1700000000000,"T":1700000059999,"s":"BTC","i":"1m","o":"27000.10","c":"27010.25","h":"27050.50","l":"26980.00","v":"12.34567800","n":15},
    {"t":1700000060000,"T":1700000119999,"s":"BTC","i":"1m","o":"27010.25","c":"27080.10","h":"27100.00","l":"27000.00","v":"8.10000000","n":42}
]"#;

/// The bars one response's rows carry, as the walk keeps them.
fn bars_of(response: &Value) -> Vec<Bar> {
    candle_rows(response).into_iter().filter_map(|row| row.bar).collect()
}

/// [`walk_candle_pages`] over `pages` (each answered in turn, the last one repeating): the bars, and
/// every cursor the walk asked a page at.
fn walk(start_ms: i64, end_ms: i64, pages: &[Value]) -> (Vec<Bar>, Vec<i64>) {
    let mut asked = Vec::new();
    let bars = walk_candle_pages(start_ms, end_ms, stuck, |cursor| {
        asked.push(cursor);
        Ok(pages[(asked.len() - 1).min(pages.len() - 1)].clone())
    })
    .expect("the walk");
    (bars, asked)
}

#[test]
fn snapshot_array_maps_to_bars_with_exact_bits() {
    let response: Value = serde_json::from_str(TWO_CANDLES).unwrap();
    let bars = bars_of(&response);
    assert_eq!(bars.len(), 2);
    // Ascending by open-time, mapped exactly (reuses market_data::candle_data_to_bar).
    assert_eq!(bars[0].timestamp, 1_700_000_000_000);
    assert_eq!(bars[1].timestamp, 1_700_000_060_000);
    assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
    assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
    assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
    assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
    assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
    // OHLCV bar only — no symbol/funding/bid/ask carried (byte-consistent with the live mapper).
    assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    assert!(bars[0].bid.is_none() && bars[0].ask.is_none());
    // The last candle's close is the paging cursor advance point.
    assert_eq!(last_close_ms(&response), Some(1_700_000_119_999));
}

#[test]
fn empty_or_non_array_yields_no_bars() {
    assert!(bars_of(&json!([])).is_empty());
    assert!(bars_of(&json!({"status": "err"})).is_empty());
    assert!(last_close_ms(&json!([])).is_none());
    assert!(last_close_ms(&json!({"status": "err"})).is_none());
}

#[test]
fn malformed_candle_is_skipped_in_place() {
    // Three rows; the middle one is missing `h` (unparseable → dropped by candle_data_to_bar),
    // the two good rows survive — per-element tolerance, same as the live feed.
    let response = json!([
        {"t":1,"T":1,"o":"1","h":"1","l":"1","c":"1","v":"1"},
        {"t":2,"T":2,"o":"2","l":"2","c":"2","v":"2"},
        {"t":3,"T":3,"o":"3","h":"3","l":"3","c":"3","v":"3"}
    ]);
    let bars = bars_of(&response);
    assert_eq!(bars.len(), 2, "the middle candle missing `h` is dropped");
    assert_eq!(bars[0].timestamp, 1);
    assert_eq!(bars[1].timestamp, 3);
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
    let response = Value::Array(rows);

    let (bars, asked) = walk(0, FAR, &[response, json!([])]);
    assert_eq!(bars.len(), MAX_CANDLES - 1, "the malformed row is still dropped from the bars");
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    assert_eq!(
        asked,
        [0, last_close + 1],
        "a FULL raw page keeps paging: cursor advances one ms past the last close"
    );
}

/// A window end past every synthetic page, so only the pages end a walk.
const FAR: i64 = (MAX_CANDLES as i64 + 10) * 60_000;

#[test]
fn full_raw_page_of_unparseable_rows_still_pages_past_itself() {
    // Even a page where EVERY row fails the tolerant parse is not end-of-history: the raw
    // count says the venue had a full page there, so the cursor still advances past its last
    // close and the history beyond it stays reachable. (Pre-fix, the empty FILTERED page
    // ended the loop here.)
    let rows: Vec<Value> = (0..MAX_CANDLES as i64)
        .map(|i| json!({"t": i * 60_000, "T": i * 60_000 + 59_999}))
        .collect();
    let response = Value::Array(rows);

    let (bars, asked) = walk(0, FAR, &[response, json!([])]);
    assert!(bars.is_empty(), "no row parses");
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    assert_eq!(asked, [0, last_close + 1]);
}

#[test]
fn short_raw_page_ends_paging_with_its_bars() {
    let response: Value = serde_json::from_str(TWO_CANDLES).unwrap();
    let (bars, asked) = walk(1_700_000_000_000, 1_700_000_119_999, &[response]);
    assert_eq!(bars.len(), 2, "the short page's bars are still returned");
    assert_eq!(asked.len(), 1, "a short RAW page = history ran out");
}

#[test]
fn empty_or_non_array_page_ends_paging() {
    let (bars, asked) = walk(0, FAR, &[json!([])]);
    assert!(bars.is_empty());
    assert_eq!(asked.len(), 1, "an empty RAW page = no history in the window");
    let (bars, asked) = walk(0, FAR, &[json!({"status": "err"})]);
    assert!(bars.is_empty());
    assert_eq!(asked.len(), 1, "a non-array body = no history");
}

/// The walk's error for a full page that does not move the cursor, in the shared sentence.
fn stuck(at: PageWalkStuck) -> String {
    at.text("hyperliquid", "BTC", "1m")
}

/// [`walk_candle_pages`] over one page answered every time: the walk's answer and every cursor asked.
fn walk_one(start_ms: i64, page: &Value) -> (Result<Vec<Bar>, String>, Vec<i64>) {
    let mut asked = Vec::new();
    let got = walk_candle_pages(start_ms, FAR, stuck, |cursor| {
        asked.push(cursor);
        Ok(page.clone())
    });
    (got, asked)
}

#[test]
fn full_page_that_cannot_advance_the_cursor_fails_the_walk() {
    let rows: Vec<Value> = (0..MAX_CANDLES as i64).map(good_row).collect();
    let response = Value::Array(rows);
    let last_close = (MAX_CANDLES as i64 - 1) * 60_000 + 59_999;
    // A cursor already at/past the advance point must not move backward/stall: the walk ends
    // (never re-fetches the same page forever) WITH AN ERROR naming the point (owner ruling H-D6),
    // never a short history returned as success.
    let (got, asked) = walk_one(last_close + 1, &response);
    assert_eq!(asked, [last_close + 1], "a stalled cursor ends the loop");
    assert_eq!(got, Err(stuck(PageWalkStuck::Forward { cursor_ms: last_close + 1 })));
    // And a full page whose last `T` is unreadable cannot advance either, even though every row
    // before it states one (the cursor is the page's LAST close).
    let mut rows: Vec<Value> = (0..MAX_CANDLES as i64).map(good_row).collect();
    *rows.last_mut().unwrap() = json!({"t": 0, "o": "1"});
    let (got, asked) = walk_one(0, &Value::Array(rows));
    assert_eq!(asked, [0], "an unreadable last close ends the loop");
    assert_eq!(got, Err(stuck(PageWalkStuck::Forward { cursor_ms: 0 })));
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
    for symbol in ["BTC", "ETH", "HYPE", "kPEPE", "test:BTC"] {
        assert_eq!(identity_coin_for(symbol).expect("a perp is identity-mapped"), symbol);
    }
}

/// A SPOT pair is refused rather than fetched under a guessed coin — the wrong-book failure
/// this adapter exists to prevent. The message must name the way forward (a canonical spelling
/// in `vike-catalog`), because "refused" with no way forward is how an operator ends up
/// hand-editing a roster.
#[test]
fn a_spot_pair_is_refused_and_the_message_names_the_way_forward() {
    for symbol in ["HYPE/USDC", "PURR/USDC", "BTC/USDC"] {
        let why = identity_coin_for(symbol).expect_err("a spot pair is not identity-mapped");
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

/// The REGISTRY ROW ([`KlineSourceHyperliquid`], what the datahub's wire verb dispatches through)
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

    let seed = vike_venues::venue_urls::VenueUrls::seed(crate::transport::consts::VENUE);
    let row = KlineSourceHyperliquid::from_urls(&seed).expect("the seed's live history row");
    assert_eq!(
        row.rest, "https://api.hyperliquid.xyz",
        "mainnet, byte for byte, whatever the tier"
    );
    let refused = row
        .fetch("HYPE/USDC", "1h", 0, 1)
        .expect_err("a non-identity symbol is refused by the one-symbol seam");
    assert_matches!(
        refused,
        vike_data::source::SourceError::Refused(_),
        "it must be a REFUSAL, not a fetch failure: {refused}"
    );
    assert!(refused.to_string().contains("vike-catalog"), "names the way forward: {refused}");
}
