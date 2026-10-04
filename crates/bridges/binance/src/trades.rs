//! Binance aggTrade WS/REST mappers + the live `subscribe_trades` feed (Task B2 — wires
//! `market_feed::Feeds::subscribe_trades`): the venue face of the shared [`crate::family::trades`].
//!
//! Aster's `@aggTrade` wire shape is byte-identical to Binance's, so the decode + the
//! WS-buffer→REST-warmup→splice startup dance + the backward-paging backfill live once in the
//! family module and both venues call in, each supplying its own venue string + hosts (F9, dedup
//! rung 2). What stays HERE is exactly the Binance-specific part: the mainnet host table
//! ([`crate::market_feed::BINANCE_URLS`], a `const` — nothing here is env-resolved) and the `pub`
//! seams: the production backfill (the GUI shell's orderflow thread reached it until that shell's
//! local feed plane went on 2026-09-09; today its smoke test is the one caller), plus the smoke's
//! `latest_agg_trade`. See
//! the family module for the full field mapping, the non-finite guard, and the Startup/Reconnect
//! contract. The tests below stay HERE, exercising the shared code through Binance's own
//! wrappers: Aster's twin does the same through its own, so the one shared implementation is
//! proven twice.

use crate::family::FamilySpec;
use crate::family::trades;
use crate::market_feed::BINANCE_URLS;

/// This venue's row for the shared market-data stack — a `const`: Binance resolves to exactly one
/// set of mainnet hosts and is deliberately NOT env-resolved (demo→mainnet threading is a separate,
/// separately-gated concern).
pub(crate) const SPEC: FamilySpec =
    FamilySpec { venue: "binance", display: "Binance", urls: BINANCE_URLS };

// Re-exported under this module's existing crate-internal path so `market_feed.rs`'s adapter is
// unchanged.
pub(crate) use trades::run_trades_feed;

/// Spot-vs-USDS-M-futures `@aggTrade` WS URL for `api_symbol` (already `.P`-stripped — the
/// EXCHANGE symbol). Spot stays on `stream.binance.com`, a perp routes to the futures stream
/// `fstream.binance.com`; the frame shape is identical on both. Mirrors the kline host split in
/// `family::market_feed::feed_main`.
///
/// `#[cfg(test)]` since rung 2: the production URL is built inside the shared feed straight off
/// [`SPEC`]'s table, so this wrapper's only remaining job is to pin — from THIS crate, with THIS
/// venue's hosts — that the shared builder still resolves to the exact URLs Binance used before the
/// extraction. Same builder, same table, so the guard is real and cannot drift from the fetch.
#[cfg(test)]
fn agg_trades_ws_url(api_symbol: &str, is_perp: bool) -> String {
    trades::agg_trades_ws_url(&SPEC.urls, api_symbol, is_perp)
}

/// Spot-vs-USDS-M-futures `aggTrades` REST **warmup** URL for `api_symbol` (already `.P`-stripped)
/// — fapi's `/fapi/v1/aggTrades` shares the exact `?symbol=&limit=` params + response JSON shape
/// with spot's `/api/v3/aggTrades`, so the family's `rest_agg_trades` decodes either. `#[cfg(test)]`
/// for the same reason as [`agg_trades_ws_url`] above.
#[cfg(test)]
fn agg_trades_rest_url(api_symbol: &str, is_perp: bool, limit: usize) -> String {
    trades::agg_trades_rest_url(&SPEC.urls, api_symbol, is_perp, limit, None)
}

/// Why a backfill walk stopped, and how much it got — re-exported at the crate root so a caller of
/// [`agg_trades_backfill_reported`] can name them as `vike_binance::{BackfillOutcome, BackfillStop}`
/// without reaching into `family::`.
pub use trades::{BackfillOutcome, BackfillStop};

/// Production wrapper (SP3 Task 3): the `pub` seam the GUI shell's background backfill thread
/// called (`vike-app`'s, as this said until 2026-09-28; that thread went with the shell's local
/// feed plane in #1727), re-exported at the crate root (see `lib.rs`) as
/// `vike_binance::agg_trades_backfill_reported`.
/// See [`crate::family::trades::agg_trades_backfill_reported`] for the no-double-count/`max_pages`
/// contract.
///
/// Answers with a [`BackfillOutcome`] whose [`BackfillStop`] separates "the window is fully
/// covered" from "the `max_pages` cap fired" from "a REST page failed and older history is
/// missing". Both truncating causes are also `warn!`ed by the shared implementation; the return
/// value exists so a caller can ACT on the third one — notably by reopening a run-once per-symbol
/// spawn guard when `stop.is_retryable()`. There is no `()`-returning variant: the one that
/// existed discarded exactly the value this seam is for, and no caller took it.
pub fn agg_trades_backfill_reported(
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    emit: &mut dyn FnMut(vike_model::TradeTick),
) -> BackfillOutcome {
    trades::agg_trades_backfill_reported(&SPEC, symbol, before_id, earliest_ts, max_pages, emit)
}

/// Public helper (SP3 Task 4's real-network smoke, re-exported via `lib.rs` as
/// `vike_binance::latest_agg_trade`): the current latest aggTrade `(id, ts)` for `symbol`. Exists
/// purely so the smoke test can derive a real, always-current `before_id`/`earliest_ts` pair
/// instead of a hand-picked constant that would go stale; nothing in production calls it.
pub fn latest_agg_trade(symbol: &str) -> Option<(u64, i64)> {
    trades::latest_agg_trade(&SPEC, symbol)
}

#[path = "trades_tests.rs"]
#[cfg(test)]
mod trades_tests;
