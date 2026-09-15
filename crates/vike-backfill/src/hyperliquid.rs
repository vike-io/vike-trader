//! Hyperliquid `candleSnapshot` backfill: fetch historical OHLCV candles via `vike_hyperliquid::history`
//! and ingest them into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! The crypto twin of [`crate::okx`] — see it for the fetch→ingest contract (the venue fetcher lives
//! in `vike-hyperliquid` — its own bridge crate — the store in vike-data, this crate the seam
//! between). The fetch half is `vike_hyperliquid::history::fetch_candles_range` (pages HL's keyless
//! `candleSnapshot` `/info` endpoint FORWARD by advancing `startTime` past each page's last candle
//! close, keyless mainnet reads); the ingest half is the `HistStore` seam. HL candles are already
//! OHLCV bars, so there is no resample step.
//!
//! The fetch→ingest orchestration and commit-key format are shared with [`crate::okx`]/
//! [`crate::binance`]/[`crate::bybit`] in `crate::klines` — this module is just the venue binding.
//! The one HL-specific wrinkle: the store `symbol` (the unified vike symbol) may differ from the HL
//! `coin` the fetch pages (the `--symbols vikeSym=coin` mapping; they coincide for perps like
//! `BTC`/`HYPE`), so the fetch closure pages `coin` while the commit-key + append key on `symbol`.
//!
//! This module ALSO owns the realized perp **funding** backfill ([`backfill_hyperliquid_funding`] +
//! the pure [`funding_rows_by_coin`]): the funding analog of the candle backfill — fetch a master
//! account's keyless `userFunding` history and persist it into the `kind=funding` Tier-2 store series
//! (`vike_data::HistStore::append_funding`), keyed `venue=hyperliquid`/`symbol=<coin>`, idempotent per
//! `(account, coin, window)`.

use std::collections::{HashMap, HashSet};

use vike_data::{DataFusionHist, FundingRow, HistStore};
use vike_hyperliquid::config::Network;
use vike_hyperliquid::funding::{FundingPayment, fetch_funding};
use vike_hyperliquid::history::fetch_candles_range;
use vike_hyperliquid::transport::HyperliquidTransport;

use crate::error::CollectError;

/// Venue tag under which Hyperliquid series live in the hist store (`venue=hyperliquid` in the tree).
pub const VENUE: &str = "hyperliquid";

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract). Keys on the unified vike `symbol` + the interval string (e.g. "1m").
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch Hyperliquid candles for HL `coin` over `[start_ms, end_ms]` (paging the 5000/req
/// `candleSnapshot` cap), and `append_bars` them into the store under `(venue=hyperliquid, symbol,
/// interval)` — where `symbol` is the unified vike symbol (the store key) and `coin` is the HL
/// market the fetch pages (they coincide for perps, e.g. `BTC`/`HYPE`). Idempotent by
/// [`klines_commit_key`]. Returns rows written (0 if the window was already ingested).
///
/// NOTE: this DOES network I/O (HL's keyless mainnet `/info`). The offline gate exercises the pure
/// map + ingest path directly against fixtures; only the `#[ignore]`d live path drives this end to
/// end.
pub fn backfill_hyperliquid_klines(
    hist: &DataFusionHist,
    symbol: &str,
    coin: &str,
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
        |_sym, iv, s, e| fetch_candles_range(coin, iv, s, e),
    )
}

// --- realized perp funding backfill (kind=funding) ---------------------------------------------
//
// The funding analog of the candle backfill above: fetch a master account's realized `userFunding`
// history (keyless mainnet `/info` — no signing/creds, just the address) and persist it into the
// `kind=funding` Tier-2 store series (`append_funding`), split by coin, idempotent per window. The
// funding fetcher lives in `vike-hyperliquid` (`funding::fetch_funding`); the store writer in
// vike-data; this module is the seam between, mirroring the klines binding above.

/// Group a flat `userFunding` fetch (payments across ALL coins) into per-coin [`FundingRow`] batches
/// ready for `append_funding`, keyed by the venue `coin` (which becomes the store `symbol`). PURE —
/// no I/O — so this is the fixture-tested seam of [`backfill_hyperliquid_funding`].
///
/// Each payment's `coin` becomes its series `symbol` and is DROPPED from the row (the store keeps it
/// in the partition path, exactly as a quote/trade drops its symbol). Rows are de-duplicated by
/// `hash` (the per-row at-most-once identity — a repeated hash within the fetch is dropped; a blank
/// hash, a parse fallback, is never deduped across coins) and returned ts-ascending within each coin;
/// coins keep first-seen order so the result is deterministic.
pub fn funding_rows_by_coin(payments: Vec<FundingPayment>) -> Vec<(String, Vec<FundingRow>)> {
    let mut order: Vec<String> = Vec::new();
    let mut by_coin: HashMap<String, Vec<FundingRow>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for p in payments {
        // per-row dedup by hash (a blank hash is never deduped — it can't identify a row)
        if !p.hash.is_empty() && !seen.insert(p.hash.clone()) {
            continue;
        }
        let FundingPayment { time_ms, coin, usdc, szi, funding_rate, hash } = p;
        if !by_coin.contains_key(&coin) {
            order.push(coin.clone());
        }
        by_coin.entry(coin).or_default().push(FundingRow {
            ts: time_ms,
            usdc,
            szi,
            funding_rate,
            hash,
        });
    }
    let mut out = Vec::with_capacity(order.len());
    for coin in order {
        let mut rows = by_coin.remove(&coin).unwrap_or_default();
        rows.sort_by_key(|r| r.ts);
        out.push((coin, rows));
    }
    out
}

/// The idempotency guard for an `(account, coin, [start_ms, end_ms])` funding backfill window: a
/// re-run with the same window is a no-op in the store (batch-level dedup — never per-row value
/// dedup, per the store contract). Keys on the master `account` + `coin` so two accounts — or two
/// coins — never share a commit key even though the series itself is keyed only `(venue,
/// symbol=coin)`.
pub fn funding_commit_key(account: &str, coin: &str, start_ms: i64, end_ms: i64) -> String {
    format!("{VENUE}:funding:{account}:{coin}:{start_ms}-{end_ms}")
}

/// Fetch realized perp funding payments for the **master** `account` over `[start_ms, end_ms]` from
/// Hyperliquid's keyless `userFunding` `/info` endpoint on **mainnet** (public reads — NO
/// credentials, just the address), and `append_funding` them into the store split by coin under
/// `(venue=hyperliquid, symbol=<coin>)`. Idempotent per `(account, coin, window)` via
/// [`funding_commit_key`]. Returns total rows written across all coins (0 if every window was already
/// ingested).
///
/// NOTE: this DOES network I/O (HL's keyless mainnet `/info`). The offline gate exercises the pure
/// grouping [`funding_rows_by_coin`] against fixtures; only an `#[ignore]`d live path drives this end
/// to end.
pub fn backfill_hyperliquid_funding(
    hist: &DataFusionHist,
    account: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    let transport = HyperliquidTransport::new(Network::Mainnet);
    let payments =
        fetch_funding(&transport, account, start_ms, end_ms).map_err(CollectError::Fetch)?;
    let mut total = 0usize;
    for (coin, rows) in funding_rows_by_coin(payments) {
        let key = funding_commit_key(account, &coin, start_ms, end_ms);
        total += hist.append_funding(VENUE, &coin, &rows, Some(&key))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payment(time_ms: i64, coin: &str, usdc: f64, szi: f64, hash: &str) -> FundingPayment {
        FundingPayment {
            time_ms,
            coin: coin.to_string(),
            usdc,
            szi,
            funding_rate: 0.000_01,
            hash: hash.to_string(),
        }
    }

    #[test]
    fn groups_by_coin_dedups_hash_and_sorts_ts() {
        let payments = vec![
            payment(3, "BTC", -1.0, 0.5, "0xb2"),
            payment(1, "BTC", -0.5, 0.5, "0xb1"),
            payment(2, "ETH", 0.25, -1.0, "0xe1"),
            payment(1, "BTC", -0.5, 0.5, "0xb1"), // duplicate hash → dropped
        ];
        let out = funding_rows_by_coin(payments);
        // coins in first-seen order: BTC then ETH
        assert_eq!(out.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["BTC", "ETH"]);

        let btc = &out[0].1;
        assert_eq!(btc.len(), 2, "the duplicate-hash BTC row is dropped");
        // ts-ascending within the coin
        assert_eq!(btc[0].ts, 1);
        assert_eq!(btc[1].ts, 3);
        assert_eq!(btc[0].hash, "0xb1");
        // coin is DROPPED from the row (it's the partition symbol); signed fields carried verbatim
        assert_eq!(btc[1].usdc, -1.0);

        let eth = &out[1].1;
        assert_eq!(eth.len(), 1);
        assert_eq!(eth[0].szi, -1.0, "short position size (signed) preserved");
    }

    #[test]
    fn empty_fetch_groups_to_nothing() {
        assert!(funding_rows_by_coin(Vec::new()).is_empty());
    }

    #[test]
    fn blank_hashes_are_not_deduped_across_coins() {
        // a blank hash can't identify a row, so two blank-hash rows on different coins both survive
        let payments = vec![payment(1, "BTC", -1.0, 0.5, ""), payment(1, "ETH", -1.0, 0.5, "")];
        let out = funding_rows_by_coin(payments);
        assert_eq!(out.len(), 2, "both blank-hash rows kept (one per coin)");
    }

    #[test]
    fn commit_key_has_account_coin_and_window() {
        assert_eq!(
            funding_commit_key("0xabc", "BTC", 1000, 2000),
            "hyperliquid:funding:0xabc:BTC:1000-2000"
        );
    }
}
