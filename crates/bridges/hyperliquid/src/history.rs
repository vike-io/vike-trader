//! Hyperliquid historical candle backfill — pages the keyless `candleSnapshot` `/info` endpoint
//! into [`vike_model::Bar`]s (the offline twin of the live [`crate::market_feed`] candle stream).
//!
//! Endpoint: `POST /info` with `{"type":"candleSnapshot","req":{"coin","interval","startTime",
//! "endTime"}}` → a JSON array of candles sharing the SAME field shape as the live WS `candle`
//! frames (`t`/`T`/`s`/`i`/`o`/`c`/`h`/`l`/`v`/`n`, decimal-string OHLCV). The candle→`Bar` mapping
//! REUSES [`crate::market_data::candle_data_to_bar`] verbatim (the same fn the live feed decodes
//! each `candle` frame with), so an offline-backfilled bar is byte-identical to the live-streamed
//! one.
//!
//! [`fetch_candles_range`] is the entry point: HL caps a snapshot at [`MAX_CANDLES`] rows, so it
//! PAGES by advancing `startTime` past each page's last candle CLOSE (`T`) until `endTime`, de-duping
//! the boundary candle, and returns [`Bar`]s ascending by open-time. Keyless mainnet reads (no
//! credentials); the transport's own `RateGate` self-throttles and retries transient failures.

use serde_json::Value;

use vike_model::Bar;

use crate::config::Network;
use crate::market_data::candle_data_to_bar;
use crate::transport::HyperliquidTransport;

/// Max candles Hyperliquid returns in one `candleSnapshot` response (the venue caps a snapshot at
/// 5000 rows), so a wider `[start, end]` window must be paged.
const MAX_CANDLES: usize = 5000;

/// The `candleSnapshot` `/info` request body for one page: `{"type":"candleSnapshot","req":{coin,
/// interval,startTime,endTime}}`. `startTime`/`endTime` are epoch-ms. Pure — the network-free half,
/// unit-tested.
fn candle_snapshot_body(coin: &str, interval: &str, start_ms: i64, end_ms: i64) -> Value {
    serde_json::json!({
        "type": "candleSnapshot",
        "req": {
            "coin": coin,
            "interval": interval,
            "startTime": start_ms,
            "endTime": end_ms,
        }
    })
}

/// Map a `candleSnapshot` response body (a JSON ARRAY of candle objects — each the SAME shape as a
/// live WS `candle` frame's `data`: `t`/`T`/`s`/`i`/`o`/`c`/`h`/`l`/`v`/`n`) → `Vec<Bar>` in wire
/// order (ascending by open-time). REUSES the live mapper [`candle_data_to_bar`] verbatim, so an
/// offline-backfilled bar is byte-identical to the live-streamed one. A non-array body yields an
/// empty vec; a malformed candle is skipped in place (mirrors the live feed's per-frame tolerance).
/// Because of that tolerance the OUTPUT length under-counts what the venue actually sent — never
/// use it as the paging-exhaustion signal; [`page_outcome`] reads the RAW array length for that.
fn bars_from_snapshot(resp: &Value) -> Vec<Bar> {
    resp.as_array()
        .map(|rows| rows.iter().filter_map(candle_data_to_bar).collect())
        .unwrap_or_default()
}

/// The last candle's CLOSE time (`T`, epoch-ms) in a `candleSnapshot` response array — the paging
/// cursor advances PAST this so the next page begins at the following candle. `None` for an
/// empty/malformed array (which ends the paging loop).
fn last_close_ms(resp: &Value) -> Option<i64> {
    resp.as_array()?.last()?.get("T").and_then(Value::as_i64)
}

/// Fold one `candleSnapshot` response into `(bars, next_cursor)`: this page's tolerantly-parsed
/// [`Bar`]s plus the cursor for the NEXT page (`None` ends the paging loop). Pure — the
/// network-free paging decision, unit-tested.
///
/// Exhaustion is judged on the **RAW response-array length**, BEFORE the tolerant per-row parse:
/// [`bars_from_snapshot`] drops a malformed candle in place, so a full [`MAX_CANDLES`]-row page
/// with one bad row parses to `MAX_CANDLES − 1` bars — comparing THAT filtered count to
/// [`MAX_CANDLES`] would misread the full page as "history ran out" and silently truncate every
/// older page (and the empty-page no-history check reads the raw length for the same reason).
/// Paging continues only when the raw page is FULL and the cursor advances strictly forward (one
/// ms past the last candle's CLOSE `T`); a stalled cursor or an unreadable last `T` ends the loop
/// defensively (never re-fetches the same page).
fn page_outcome(resp: &Value, cursor: i64) -> (Vec<Bar>, Option<i64>) {
    let raw_len = resp.as_array().map_or(0, |rows| rows.len());
    let bars = bars_from_snapshot(resp);
    // A short RAW page means history ran out within the window — nothing more to fetch.
    if raw_len < MAX_CANDLES {
        return (bars, None);
    }
    let next = last_close_ms(resp).map(|close| close.saturating_add(1)).filter(|&n| n > cursor);
    (bars, next)
}

/// Fetch closed-candle history for the inclusive `[start_ms, end_ms]` window from Hyperliquid's
/// keyless `candleSnapshot` `/info` endpoint on **mainnet** (public reads), returning [`Bar`]s
/// ascending by open-time, de-duplicated.
///
/// HL caps a snapshot at [`MAX_CANDLES`], so this PAGES: each request asks `[cursor, end_ms]`; the
/// cursor then advances to one ms past the last returned candle's CLOSE (`T`) so the next page begins
/// at the following candle (no boundary re-fetch), and the loop stops once a page comes back short —
/// judged on the RAW response row count, BEFORE the tolerant per-row parse (see [`page_outcome`] for
/// why the filtered count would truncate history) — or the cursor passes `end_ms`. A trailing sort +
/// `dedup_by_key` drops any boundary duplicate regardless of the venue's start/end inclusivity.
/// Network I/O (the transport's own `RateGate` self-throttles and retries transient rate-limit/server
/// failures); the pure paging decision [`page_outcome`] (over the pure map [`bars_from_snapshot`]) is
/// the fixture-tested seam. A transport error is surfaced as a `String` (matching the sibling
/// `fetch_klines_range`s the collect layer binds).
pub fn fetch_candles_range(
    coin: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let transport = HyperliquidTransport::new(Network::Mainnet);
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = start_ms;
    while cursor <= end_ms {
        let body = candle_snapshot_body(coin, interval, cursor, end_ms);
        let resp = transport
            .info(&body)
            .map_err(|e| format!("hyperliquid candleSnapshot {coin} {interval}: {e}"))?;
        let (page, next) = page_outcome(&resp, cursor);
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }
        // `page_outcome` reads the RAW row count: `None` = short/empty page (history ran out) or a
        // cursor that failed to move strictly forward (defensive — a candle's close is always >=
        // its open >= cursor, so this never stalls a well-formed stream).
        match next {
            Some(n) => cursor = n,
            None => break,
        }
    }
    // Pages are ascending already; sort + dedup is cheap insurance against a boundary-candle dup.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
