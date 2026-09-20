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
use vike_model::Bar;

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

// --- the one-symbol DISPATCH source, and its refusal --------------------------------------------
//
// The ONE dispatch registry (`crate::kline_source::KLINE_SOURCES`, folded by both the supervisor
// and `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`) carries ONE symbol per job, so
// the only vike→HL mapping it can express is the identity `coin == symbol`. The bin above it
// (`src/bin/hyperliquid_backfill.rs`) carries the `--symbols vikeSymbol=coin` grammar and can
// express any mapping; the seam cannot, and this is where that difference is decided rather than
// guessed at.
//
// ⚠ It said "the two dispatch rosters" until 0059 Phase 3, and the plural is what the phase
// removed — not the refusal, which is unchanged and is now `HyperliquidKlines::fetch`'s first act.

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
             `@<pairIndex>` rather than the pair name — this dispatch seam carries one symbol and \
             can only express `coin == symbol`, so it refuses rather than guessing a coin. Run \
             `hyperliquid_backfill --symbols {symbol}=@<pairIndex>`, which carries the mapping."
        ));
    }
    if symbol.contains('@') {
        return Err(format!(
            "hyperliquid: {symbol:?} is a raw venue `coin`, not a unified symbol — fetching it \
             would succeed and store the rows under a `symbol=` partition nothing else in the \
             store spells. Name the unified symbol and map it: `hyperliquid_backfill --symbols \
             <BASE/QUOTE>={symbol}`."
        ));
    }
    if symbol.is_empty() {
        return Err("hyperliquid: empty symbol".to_string());
    }
    Ok(symbol)
}

/// [`backfill_hyperliquid_klines`] behind the ONE-SYMBOL dispatch signature every other venue's
/// entry point already has, refusing every symbol for which `coin == symbol` is not the right
/// mapping — see [`identity_coin_for`].
///
/// ⚠ **0059 Phase 3 made good on this function's own prediction.** It used to say that when the
/// rosters collapsed into one trait "this body is already the `fetch` half of an impl"; it is now
/// literally that — [`HyperliquidKlines::fetch`] — and this function is the thin one-symbol entry
/// point that dispatches through it, matching `backfill_binance_klines` and its four siblings.
/// The refusal has ONE spelling and every path reaches it.
pub fn backfill_hyperliquid_klines_by_symbol(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    crate::kline_source::backfill_kline_source(
        hist,
        &HyperliquidKlines,
        symbol,
        interval,
        start_ms,
        end_ms,
    )
}

/// This venue's row in the ONE kline registry (`crate::kline_source::KLINE_SOURCES`) — the fetch
/// half, store-free, and the only row whose `fetch` can REFUSE before the network.
///
/// ⚠ The refusal is why this row exists as a distinct thing rather than as the plain pager:
/// [`backfill_hyperliquid_klines`] takes a separate venue `coin`, and a one-symbol seam can only
/// express `coin == symbol`. Guessing one would fetch the wrong book, store it under the requested
/// symbol, and spend the commit key that makes a corrective re-fetch a silent zero-row success.
pub struct HyperliquidKlines;

impl crate::kline_source::KlineSource for HyperliquidKlines {
    fn collector_name(&self) -> &str {
        "hyperliquid_klines"
    }

    fn venue(&self) -> &str {
        VENUE
    }

    /// [`identity_coin_for`] first — a `CollectError::Refused`, NOT a `Fetch`, because nothing was
    /// asked of the venue (see that variant's own doc) — then the bridge's paged
    /// `candleSnapshot` pager.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, CollectError> {
        let coin = identity_coin_for(symbol).map_err(CollectError::Refused)?;
        fetch_candles_range(coin, interval, start_ms, end_ms).map_err(CollectError::Fetch)
    }
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

    // --- the dispatch adapter's refusal ---------------------------------------------------------

    /// Every PERP spelling is identity-mapped, core and HIP-3 alike — read off `symbology.rs`'s
    /// `load_perps`/`extend_with_perp_dex`, both of which set `coin` and `symbol` from one field.
    #[test]
    fn a_perp_symbol_is_its_own_coin() {
        for sym in ["BTC", "ETH", "HYPE", "kPEPE", "test:BTC"] {
            assert_eq!(identity_coin_for(sym).expect("a perp is identity-mapped"), sym);
        }
    }

    /// A SPOT pair is refused rather than fetched under a guessed coin — the wrong-book failure
    /// this adapter exists to prevent. The message must name the bin that CAN express the mapping,
    /// because "refused" with no way forward is how an operator ends up hand-editing a roster.
    #[test]
    fn a_spot_pair_is_refused_and_the_message_names_the_mapping_bin() {
        for sym in ["HYPE/USDC", "PURR/USDC", "BTC/USDC"] {
            let why = identity_coin_for(sym).expect_err("a spot pair is not identity-mapped");
            assert!(why.contains("SPOT"), "{why}");
            assert!(why.contains("hyperliquid_backfill --symbols"), "names the way out: {why}");
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

    /// **PINNED DISAGREEMENT — the one-shot bin can express a symbol its own registry row
    /// REFUSES.** Both sides are live; neither is wrong; nothing here resolves it.
    ///
    /// * The REGISTRY ROW ([`HyperliquidKlines`], what the supervisor and the datahub's wire verb
    ///   dispatch through) carries ONE symbol, so it can only ever mean `coin == symbol`. It
    ///   refuses anything else through [`identity_coin_for`] — before any I/O, which is what lets
    ///   this test assert it offline.
    /// * The BIN (`crates/vike-backfill/src/bin/hyperliquid_backfill.rs`) takes
    ///   `--symbols vikeSymbol=coin`, so it CAN say `HYPE/USDC=@107` and store the result under
    ///   `symbol=HYPE/USDC`. [`identity_coin_for`]'s own refusal message names that bin as the
    ///   place to go.
    ///
    /// Why this is pinned rather than fixed: routing the bin through the registry would turn every
    /// non-identity symbol it fetches today into a refusal — a collector that quietly stops
    /// collecting what it collected yesterday, which is the failure mode a deduplication is
    /// supposed to avoid. Widening the seam to carry a source symbol is the other direction, and it
    /// is a change to a trait five other venues implement. Either is an argument of its own.
    #[test]
    fn the_bins_symbol_grammar_expresses_what_the_registry_row_refuses() {
        use crate::kline_source::KlineSource;

        // The registry row's side — REFUSED, offline, before the venue is asked anything.
        let refused = HyperliquidKlines
            .fetch("HYPE/USDC", "1h", 0, 1)
            .expect_err("a non-identity symbol is refused by the one-symbol seam");
        assert!(
            matches!(refused, CollectError::Refused(_)),
            "it must be a REFUSAL, not a fetch failure: {refused}"
        );
        assert!(
            refused.to_string().contains("hyperliquid_backfill"),
            "the refusal names the bin that CAN express the mapping: {refused}"
        );

        // The bin's side — the same request, expressible.
        let pairs = crate::cli::parse_symbol_pairs("HYPE/USDC=@107")
            .expect("the bin's grammar carries a source coin beside the vike symbol");
        assert_eq!(pairs, vec![("HYPE/USDC".to_string(), "@107".to_string())]);
    }
}
