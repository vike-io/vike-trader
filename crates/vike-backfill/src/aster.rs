//! Aster klines backfill: fetch historical OHLCV klines via `vike_aster::data` and ingest them into
//! the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! The crypto twin of [`crate::binance`] — see it for the fetch→ingest contract (the venue fetcher
//! lives in `vike-aster`, its own bridge crate; the store in vike-data; this crate the seam between).
//! Aster klines are already OHLCV bars, so — unlike the dukascopy tick collector — there is no
//! resample step, and the fetch→ingest orchestration, commit-key format and CLI body are shared with
//! [`crate::binance`]/[`crate::bybit`]/[`crate::okx`] in `crate::klines`.
//!
//! Two Aster-specific deltas:
//!
//! 1. `vike_aster::data::fetch_klines_range` takes a 5th `env` param that the shared 4-arg fetcher
//!    shape has no slot for, so this seam CLOSES OVER [`Environment::Live`] — the same shape
//!    [`crate::hyperliquid`] uses to close over its HL `coin`. Mainnet is not a risk decision here:
//!    klines are a KEYLESS public read, so the standing "exclude aster from demo/validation runs"
//!    rule — which is about authenticated mainnet account calls — does not reach this path. Testnet
//!    would be the wrong default anyway: it carries no real price history to backfill.
//! 2. Perp vs spot is carried by the SYMBOL, not by a flag: `BTCUSDT.P` routes to the USDⓈ-M futures
//!    host and has its `.P` stripped before the wire, INSIDE the bridge. This seam passes the
//!    caller's symbol through VERBATIM — it never splits — so the suffixed symbol stays the
//!    store/series key and a perp window can never dedup against its spot twin's commit key.

use vike_aster::data::fetch_klines_range;
use vike_bridge_core::Environment;
use vike_data::DataFusionHist;

use crate::error::CollectError;

/// Venue tag under which Aster series live in the hist store (`venue=aster` in the tree).
pub const VENUE: &str = "aster";

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract). Keys on the CALLER's symbol, `.P` suffix included.
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch Aster klines for `(symbol, interval)` over `[start_ms, end_ms]` (paging the 1000/req cap),
/// and `append_bars` them into the store under `(venue=aster, symbol, interval)`. Idempotent by
/// [`klines_commit_key`]. Returns rows written (0 if the window was already ingested).
///
/// A `symbol` carrying the `.P` perp suffix fetches the USDⓈ-M perpetual and stores it under that
/// suffixed key; a bare symbol fetches spot. The routing (and the suffix strip) happens inside
/// `vike_aster::data::fetch_klines_range`, never here.
///
/// NOTE: this DOES network I/O (Aster's public REST, mainnet, keyless) — the offline gate exercises
/// the pure map + ingest path directly against a committed fixture; only the `#[ignore]`d live test
/// drives this end to end.
pub fn backfill_aster_klines(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    crate::klines::backfill_klines(
        hist,
        VENUE,
        symbol,
        interval,
        start_ms,
        end_ms,
        |sym, iv, s, e| fetch_klines_range(sym, iv, s, e, Environment::Live),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A perp window and its spot twin must produce DIFFERENT commit keys, or one would dedup
    /// against the other and silently write nothing. This is the guard for the plan's one silent
    /// failure mode: if this seam ever stripped `.P` before the commit key is built, perp bars would
    /// land on the spot series' key and overwrite its history with no error anywhere.
    #[test]
    fn perp_and_spot_commit_keys_differ() {
        let spot = klines_commit_key("BTCUSDT", "1m", 10, 20);
        let perp = klines_commit_key("BTCUSDT.P", "1m", 10, 20);
        assert_ne!(spot, perp);
        assert_eq!(spot, "aster:BTCUSDT:1m:10-20");
        assert_eq!(perp, "aster:BTCUSDT.P:1m:10-20");
    }
}
