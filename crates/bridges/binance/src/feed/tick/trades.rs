//! Binance aggTrade WS/REST mappers + the live `subscribe_trades` feed (Task B2 — wires
//! `subscribe_trades` of binance's data client): the venue face of the shared [`crate::family::feed::tick::trades`].
//!
//! Aster's `@aggTrade` wire shape is byte-identical to Binance's, so the decode + the
//! WS-buffer→REST-warmup→splice startup dance + the backward-paging backfill live once in the
//! family module and both venues call in, each supplying its own venue string + hosts (F9, dedup
//! rung 2). What stays HERE is exactly the Binance-specific part: the host table
//! ([`crate::feed::BinanceTables`], binance's `venue_url` rows — nothing here is env-resolved) and
//! the `pub` seams: the production backfill (the GUI shell's orderflow thread reached it until that shell's
//! local feed plane went on 2026-09-09; today its smoke test is the one caller), plus the smoke's
//! `latest_agg_trade`. See
//! the family module for the full field mapping, the non-finite guard, and the Startup/Reconnect
//! contract. The tests below stay HERE, exercising the shared code through Binance's own
//! wrappers: Aster's twin does the same through its own, so the one shared implementation is
//! proven twice.

use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

#[cfg(test)]
use crate::family::feed::tick::trades;
use crate::family::{FamilySpec, UrlTable};
use crate::feed::BinanceTables;

/// This venue's row for the shared market-data stack over the host table `urls` (one of
/// [`BinanceTables`]'s, from binance's `venue_url` rows — never env-resolved).
pub(crate) fn spec(urls: UrlTable) -> FamilySpec {
    // No market keepalive: the server pings every book, and binance cuts no socket that answers them.
    FamilySpec { venue: "binance", display: "Binance", urls, market_keepalive: None }
}

/// The release's spot and USDⓈ-M table: the hosts a fresh box's rows name.
#[cfg(test)]
fn seeded() -> UrlTable {
    crate::feed::seeded_tables().linear
}

/// Spot-vs-USDS-M-futures `@aggTrade` WS URL for `api_symbol` (already `.P`-stripped — the
/// EXCHANGE symbol). Spot stays on `stream.binance.com`, a perp routes to the futures stream
/// `fstream.binance.com`; the frame shape is identical on both. Mirrors the kline host split in
/// `family::feed::feed_main`.
///
/// `#[cfg(test)]` since rung 2: the production URL is built inside the shared feed straight off
/// [`spec`]'s table, so this wrapper's only remaining job is to pin — from THIS crate, with THIS
/// venue's hosts — that the shared builder still resolves to the exact URLs Binance used before the
/// extraction. Same builder, same table, so the guard is real and cannot drift from the fetch.
#[cfg(test)]
fn agg_trades_websocket_url(api_symbol: &str, is_perp: bool) -> String {
    trades::agg_trades_websocket_url(&seeded(), api_symbol, is_perp)
}

/// Spot-vs-USDS-M-futures `aggTrades` REST **warmup** URL for `api_symbol` (already `.P`-stripped)
/// — fapi's `/fapi/v1/aggTrades` shares the exact `?symbol=&limit=` params + response JSON shape
/// with spot's `/api/v3/aggTrades`, so the family's `rest_agg_trades` decodes either. `#[cfg(test)]`
/// for the same reason as [`agg_trades_websocket_url`] above.
#[cfg(test)]
fn agg_trades_rest_url(api_symbol: &str, is_perp: bool, limit: usize) -> String {
    trades::agg_trades_rest_url(&seeded(), api_symbol, is_perp, limit, None)
}

/// Why a backfill walk stopped, and how much it got — re-exported at the crate root so a caller of
/// [`agg_trades_backfill_reported`] can name them as `vike_binance::{BackfillOutcome, BackfillStop}`
/// without reaching into `family::`.
pub use crate::family::history::trades::{BackfillOutcome, BackfillStop};

/// Production wrapper (SP3 Task 3): the `pub` seam the GUI shell's background backfill thread
/// called (`vike-app`'s, as this said until 2026-09-28; that thread went with the shell's local
/// feed plane in #1727), re-exported at the crate root (see `lib.rs`) as
/// `vike_binance::agg_trades_backfill_reported`.
/// See [`crate::family::history::trades::agg_trades_backfill_reported`] for the no-double-count/`max_pages`
/// contract.
///
/// Answers with a [`BackfillOutcome`] whose [`BackfillStop`] separates "the window is fully
/// covered" from "the `max_pages` cap fired" from "a REST page failed and older history is
/// missing". Both truncating causes are also `warn!`ed by the shared implementation; the return
/// value exists so a caller can ACT on the third one — notably by reopening a run-once per-symbol
/// spawn guard when `stop.is_retryable()`. There is no `()`-returning variant: the one that
/// existed discarded exactly the value this seam is for, and no caller took it.
///
/// The hosts are binance's `venue_url` rows `urls` (the spot and USDⓈ-M table of [`BinanceTables`]).
///
/// # Errors
/// A row is missing: nothing is asked, the refusal names the row.
pub fn agg_trades_backfill_reported(
    urls: &VenueUrls,
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    emit: &mut dyn FnMut(vike_marketdata::TickTrade),
) -> Result<BackfillOutcome, UrlRefusal> {
    Ok(crate::family::history::trades::agg_trades_backfill_reported(
        &spec(BinanceTables::from_urls(urls)?.linear),
        symbol,
        before_id,
        earliest_ts,
        max_pages,
        emit,
    ))
}

/// Public helper (SP3 Task 4's real-network smoke, re-exported via `lib.rs` as
/// `vike_binance::latest_agg_trade`): the current latest aggTrade `(id, ts)` for `symbol`, on the
/// hosts of binance's `venue_url` rows `urls`. Exists
/// purely so the smoke test can derive a real, always-current `before_id`/`earliest_ts` pair
/// instead of a hand-picked constant that would go stale; nothing in production calls it.
///
/// # Errors
/// A row is missing: nothing is asked, the refusal names the row.
pub fn latest_agg_trade(urls: &VenueUrls, symbol: &str) -> Result<Option<(u64, i64)>, UrlRefusal> {
    Ok(crate::family::history::trades::latest_agg_trade(
        &spec(BinanceTables::from_urls(urls)?.linear),
        symbol,
    ))
}

#[path = "trades_tests.rs"]
#[cfg(test)]
mod trades_tests;
