//! Aster aggTrade WS/REST mappers + the live `subscribe_trades` feed: the venue face of the shared
//! [`vike_binance::family::trades`].
//!
//! Aster's `@aggTrade` wire shape (`a/p/q/T/m` keys, both the WS push event and the REST array
//! element) is byte-identical to Binance's, so this module's decode + splice + backfill logic —
//! previously a byte-for-byte copy — now lives once in the Binance-wire-grammar core and both
//! venues call in (F9, dedup rung 2). What stays HERE is exactly Aster's own part: the `env`-keyed
//! host table (resolved through [`crate::urls::urls_for`] instead of a hardcoded `binance.com`
//! const) and this venue's own `pub` seam. See the family module for the full field mapping, the
//! non-finite guard, and the Startup/Reconnect contract. The tests below stay HERE, exercising the
//! shared code through Aster's own wrappers: Binance's twin does the same through its own, so the
//! one shared implementation is proven twice.
//!
//! **The one endpoint that is NOT shared grammar:** Aster serves perp aggTrades at
//! `/fapi/v3/aggTrades` where Binance uses `/fapi/v1/aggTrades`. That divergence is carried as data
//! on this crate's [`UrlTable`] row below, not unified.
//!
//! [`UrlTable`]: vike_binance::family::UrlTable

use vike_model::AccountTier;

use vike_binance::family::trades;
use vike_binance::family::{FamilySpec, UrlTable};

use crate::urls;

// Re-exported under this module's existing crate-internal path so `market_feed.rs`'s adapter is
// unchanged.
pub(crate) use trades::run_trades_feed;

/// This venue's row for the shared market-data stack, resolved from `env` (testnet vs mainnet) —
/// the ONE real delta from the binance template, which pins a single mainnet `const` table instead.
pub(crate) fn spec(env: AccountTier) -> FamilySpec {
    let u = urls::urls_for(env);
    FamilySpec {
        venue: "aster",
        display: "Aster",
        urls: UrlTable {
            spot_ws: u.sapi_ws,
            perp_ws: u.fapi_ws,
            // Aster serves every stream on its one `/ws` path (measured 2026-10-31): no class
            // segment, so its URLs are what they were before Binance's futures socket was routed.
            perp_market_path: "",
            perp_public_path: "",
            spot_rest: u.sapi_rest,
            perp_rest: u.fapi_rest,
            spot_agg_trades_path: "/api/v3/aggTrades",
            // Aster's futures aggTrades is v3 — Binance's is v1. A real venue divergence, carried
            // as data rather than unified (module doc).
            perp_agg_trades_path: "/fapi/v3/aggTrades",
            // Aster's futures `@aggTrade` WS stream WORKS (measured 10 frames/15s on
            // fstream.asterdex.com) — unlike Binance's, which is dead. So this venue stays on the
            // aggregate lane, keeping its ids in the same space its backfill pages.
            perp_trades_lane: vike_binance::family::PerpTradesLane::Aggregated,
            // Unread on the Aggregated lane; declared so the row is complete rather than a default.
            perp_raw_trades_path: "/fapi/v1/trades",
        },
    }
}

/// Spot-vs-USDⓈ-M-futures `@aggTrade` WS URL for `api_symbol` (already `.P`-stripped — the
/// EXCHANGE symbol), host resolved from `env` via [`urls::urls_for`]. Spot stays on `sapi_ws`, a
/// perp routes to `fapi_ws`; the frame shape is identical on both.
///
/// `#[cfg(test)]` since rung 2: the production URL is built inside the shared feed straight off
/// [`spec`]'s table, so this wrapper's only remaining job is to pin — from THIS crate, with THIS
/// venue's `env`-resolved hosts — that the shared builder still resolves to the exact URLs Aster
/// used before the extraction. Same builder, same table, so the guard is real and cannot drift from
/// the fetch.
#[cfg(test)]
fn agg_trades_ws_url(api_symbol: &str, is_perp: bool, env: AccountTier) -> String {
    trades::agg_trades_ws_url(&spec(env).urls, api_symbol, is_perp)
}

/// Spot-vs-USDⓈ-M-futures `aggTrades` REST URL (warmup + the `fromId` backfill page share this
/// builder) for `api_symbol` (already `.P`-stripped), host+path resolved from `env` via
/// [`urls::urls_for`]. `from_id: None` builds the warmup's `?symbol=&limit=` query; `Some(id)` adds
/// `&fromId=` for the backward-paging backfill. `#[cfg(test)]` for the same reason as
/// [`agg_trades_ws_url`] above.
#[cfg(test)]
fn agg_trades_rest_url(
    api_symbol: &str,
    is_perp: bool,
    limit: usize,
    from_id: Option<u64>,
    env: AccountTier,
) -> String {
    trades::agg_trades_rest_url(&spec(env).urls, api_symbol, is_perp, limit, from_id)
}

/// Public helper (re-exported via `lib.rs` as `vike_aster::latest_agg_trade`): the current latest
/// aggTrade `(id, ts)` for `symbol`. Exists purely so a smoke test can derive a real,
/// always-current `before_id`/`earliest_ts` pair instead of a hand-picked constant that would go
/// stale; nothing in production calls it yet.
pub fn latest_agg_trade(symbol: &str, env: AccountTier) -> Option<(u64, i64)> {
    trades::backfill::latest_agg_trade(&spec(env), symbol)
}

#[path = "trades_tests.rs"]
#[cfg(test)]
mod trades_tests;
