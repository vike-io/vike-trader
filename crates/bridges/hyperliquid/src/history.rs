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

// --- the one-symbol DISPATCH source, and its refusal --------------------------------------------
//
// The ONE dispatch registry (`crates/vike-datahub/src/backfill.rs`'s `KLINE_SOURCES`, folded by
// that file's own `real_backfill_table`) carries ONE symbol per job, so the
// only vike→HL mapping it can express is the identity `coin == symbol`. The one-shot
// `hyperliquid_backfill` program's `--symbols vikeSymbol=coin` grammar could express any mapping;
// docs/decisions/0094 deleted it, so a non-identity symbol has no path into the store until it has
// a canonical spelling — and this is where that is decided rather than guessed at.

/// The HL `coin` for a unified store `symbol` **when, and only when, the two are the same string**
/// — otherwise the reason the identity does not hold.
///
/// PURE: no network, no `spotMeta` read, no store touch. The rule is read off the venue's own
/// symbology rather than invented — `crates/bridges/hyperliquid/src/symbology.rs`'s `load_perps`
/// and `extend_with_perp_dex` both set `coin: name` and `symbol: name` from the SAME field, so
/// **every perp is identity-mapped**, core and HIP-3 alike (`BTC`, `kPEPE`, `test:BTC`).
/// `load_spot` is the one that breaks it: a spot pair's unified `symbol` is `BASE/QUOTE`
/// (`HYPE/USDC`) while its `coin` is `@<pairIndex>` (`@107`). That crate has stated the two
/// namespaces are DISJOINT since its catalog was written —
/// `crates/bridges/hyperliquid/src/catalog.rs` says so in its own module doc — which is what makes
/// a SYNTACTIC test sufficient here:
///
/// * **a `/`** ⇒ a spot pair. Its coin is `@N`, which this seam cannot resolve without a
///   `spotMeta` fetch it does not do. ⚠ `PURR/USDC` is the ONE pair whose coin IS its name
///   (`symbology.rs`'s `spot_asset_id_is_10000_plus_pair_index` pins it), so identity would be
///   correct for it — and it is refused anyway, for a second reason that applies to every pair:
///   `vike_data::DataFusionHist`'s `series_dir` interpolates the symbol into
///   `format!("symbol={symbol}")`, so a `/` becomes a NESTED DIRECTORY in the store. Carving out
///   one venue-data literal would buy one symbol and hand back a partition-path defect.
/// * **an `@`** ⇒ a raw venue coin, not a unified symbol. The fetch would succeed and the rows
///   would land under `symbol=@107`, a partition no other producer and no catalog row ever spells
///   (`symbology.rs`'s `symbol_for_coin("@107")` answers `"HYPE/USDC"`), so the series would be
///   orphaned from every reader that looks it up by name.
///
/// Both refusals are one-sided in the safe direction: a wrongly-refused symbol costs an error
/// message, while a wrongly-admitted one writes rows that `commit_rows`' spent commit key makes
/// permanent (a corrective re-fetch of the same window answers `Ok(0)`).
pub fn identity_coin_for(symbol: &str) -> Result<&str, String> {
    if symbol.contains('/') {
        return Err(format!(
            "hyperliquid: {symbol:?} is a SPOT pair (`BASE/QUOTE`), whose venue `coin` is \
             `@<pairIndex>` rather than the pair name — this seam carries one symbol and can \
             only express `coin == symbol`, so it refuses rather than guessing a coin. A spot \
             pair has no store spelling yet either: the store refuses a `/` in a symbol \
             (`vike_model::paths::store_path::refuse_a_path_hostile_symbol`). Give the pair a canonical \
             spelling in `crates/vike-catalog/src/symbol.rs` first."
        ));
    }
    if symbol.contains('@') {
        return Err(format!(
            "hyperliquid: {symbol:?} is a raw venue `coin`, not a unified symbol — fetching it \
             would succeed and store the rows under a `symbol=` partition nothing else in the \
             store spells. Ask for the unified symbol instead."
        ));
    }
    if symbol.is_empty() {
        return Err("hyperliquid: empty symbol".to_string());
    }
    Ok(symbol)
}

/// This venue's row in the ONE kline registry (`crates/vike-datahub/src/backfill.rs`'s
/// `KLINE_SOURCES`) — the fetch half, store-free, and the only row whose `fetch` can REFUSE before
/// the network.
///
/// ⚠ The refusal is why this row is more than the plain pager: [`fetch_candles_range`] pages a
/// venue `coin`, and a one-symbol seam can only express `coin == symbol`. Guessing one would fetch
/// the wrong book, store it under the requested symbol, and spend the commit key that makes a
/// corrective re-fetch a silent zero-row success.
pub struct HyperliquidKlines;

impl vike_data::source::KlineSource for HyperliquidKlines {
    fn venue(&self) -> &str {
        crate::consts::VENUE
    }

    /// [`identity_coin_for`] first — a `SourceError::Refused`, NOT a `Fetch`, because nothing was
    /// asked of the venue (see that variant's own doc) — then the bridge's paged
    /// `candleSnapshot` pager.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        let coin = identity_coin_for(symbol).map_err(vike_data::source::SourceError::Refused)?;
        fetch_candles_range(coin, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
