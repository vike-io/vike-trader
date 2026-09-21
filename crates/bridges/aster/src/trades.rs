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

use vike_bridge_core::Environment;

use vike_binance::family::trades;
use vike_binance::family::{FamilySpec, UrlTable};

use crate::urls;

// Re-exported under this module's existing crate-internal path so `market_feed.rs`'s adapter is
// unchanged.
pub(crate) use trades::run_trades_feed;

/// This venue's row for the shared market-data stack, resolved from `env` (testnet vs mainnet) —
/// the ONE real delta from the binance template, which pins a single mainnet `const` table instead.
pub(crate) fn spec(env: Environment) -> FamilySpec {
    let u = urls::urls_for(env);
    FamilySpec {
        venue: "aster",
        display: "Aster",
        urls: UrlTable {
            spot_ws: u.sapi_ws,
            perp_ws: u.fapi_ws,
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
fn agg_trades_ws_url(api_symbol: &str, is_perp: bool, env: Environment) -> String {
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
    env: Environment,
) -> String {
    trades::agg_trades_rest_url(&spec(env).urls, api_symbol, is_perp, limit, from_id)
}

/// Public helper (re-exported via `lib.rs` as `vike_aster::latest_agg_trade`): the current latest
/// aggTrade `(id, ts)` for `symbol`. Exists purely so a smoke test can derive a real,
/// always-current `before_id`/`earliest_ts` pair instead of a hand-picked constant that would go
/// stale; nothing in production calls it yet.
pub fn latest_agg_trade(symbol: &str, env: Environment) -> Option<(u64, i64)> {
    trades::latest_agg_trade(&spec(env), symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use vike_binance::family::trades::{
        AggTrade, TRADES_SEED_LIMIT, backfill_agg_trades_backward, handoff, record_earliest_id,
        rest_agg_trades, ws_agg_trade,
    };

    const WS: &str = r#"{"e":"aggTrade","E":1752200000100,"s":"BTCUSDT","a":7001,"p":"60000.50","q":"0.012","f":1,"l":2,"T":1752200000099,"m":true}"#;
    const REST: &str = r#"[{"a":6999,"p":"59999.00","q":"0.5","f":9,"l":9,"T":1752199999000,"m":false},{"a":7000,"p":"60000.00","q":"0.1","f":10,"l":10,"T":1752199999500,"m":true}]"#;
    #[test]
    fn maps_ws_agg_trade() {
        let t = ws_agg_trade("BTCUSDT", WS).unwrap();
        assert_eq!(t.id, 7001);
        assert_eq!(t.tick.ts, 1752200000099);
        assert_eq!(t.tick.price, 60000.50);
        assert_eq!(t.tick.size, 0.012);
        assert!(t.tick.is_buyer_maker);
        assert_eq!(t.tick.symbol, "BTCUSDT");
        assert!(ws_agg_trade("BTCUSDT", r#"{"e":"kline"}"#).is_none());
    }
    #[test]
    fn maps_rest_and_handoff_dedupes_by_id() {
        let warm = rest_agg_trades("BTCUSDT", REST);
        assert_eq!(warm.len(), 2);
        let buffered = vec![
            ws_agg_trade("BTCUSDT", WS).unwrap(),
            // duplicate of warmup tail — must be dropped:
            rest_agg_trades("BTCUSDT", REST).pop().unwrap(),
        ];
        let out = handoff(warm, buffered);
        let ids: Vec<u64> = out.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![6999, 7000, 7001]);
    }

    /// `run_trades_feed` itself needs a live socket (WS connect + REST warmup), so it isn't
    /// unit-testable directly — this exercises the extracted `record_earliest_id` helper in
    /// isolation instead (family module doc's "Earliest-live-id reporting" section); the feed
    /// thread's own wiring is covered by the crate's live-smoke conventions.
    #[test]
    fn record_earliest_id_tracks_the_minimum_across_calls_and_symbols() {
        let ids: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());
        record_earliest_id(&ids, "BTCUSDT", 500);
        record_earliest_id(&ids, "BTCUSDT", 300); // lower id -> replaces
        record_earliest_id(&ids, "BTCUSDT", 400); // higher id -> no-op, stays at the min
        record_earliest_id(&ids, "ETHUSDT", 999); // separate symbol -> independent entry
        let m = ids.lock().unwrap();
        assert_eq!(m.get("BTCUSDT"), Some(&300));
        assert_eq!(m.get("ETHUSDT"), Some(&999));
    }

    /// Mirrors `run_trades_feed`'s actual call shape end to end (minus the socket): the id
    /// recorded is the FIRST (oldest) element of the spliced warmup+buffered sequence, not the
    /// warmup's own oldest id in isolation — same `handoff` splice `maps_rest_and_handoff_dedupes_by_id`
    /// exercises, feeding its result through `record_earliest_id`.
    #[test]
    fn record_earliest_id_matches_the_spliced_first_id() {
        let warm = rest_agg_trades("BTCUSDT", REST); // ids 6999, 7000, ascending
        let buffered = vec![ws_agg_trade("BTCUSDT", WS).unwrap()]; // id 7001
        let spliced = handoff(warm, buffered);
        let ids: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());
        if let Some(first) = spliced.first() {
            record_earliest_id(&ids, "BTCUSDT", first.id);
        }
        assert_eq!(ids.lock().unwrap().get("BTCUSDT"), Some(&6999));
    }

    /// MANDATORY guard carried from the whole-branch review: +Inf/NaN/huge-exponent-overflow/
    /// zero price or size must never reach a `TradeTick` — a downstream volume fold would spin
    /// (on a +Inf size) or silently corrupt (on a zero/garbage size) the running total.
    #[test]
    fn ws_agg_trade_rejects_non_finite_or_non_positive_price_or_size() {
        let inf_size =
            r#"{"e":"aggTrade","a":7002,"p":"60000.50","q":"inf","T":1752200000099,"m":true}"#;
        assert!(ws_agg_trade("BTCUSDT", inf_size).is_none());

        // f64 parse overflow saturates to +Inf rather than erroring — still must be rejected.
        let huge_exponent =
            r#"{"e":"aggTrade","a":7003,"p":"60000.50","q":"1e400","T":1752200000099,"m":true}"#;
        assert!(ws_agg_trade("BTCUSDT", huge_exponent).is_none());

        let zero_size =
            r#"{"e":"aggTrade","a":7004,"p":"60000.50","q":"0","T":1752200000099,"m":true}"#;
        assert!(ws_agg_trade("BTCUSDT", zero_size).is_none());

        let non_finite_price =
            r#"{"e":"aggTrade","a":7005,"p":"nan","q":"0.012","T":1752200000099,"m":true}"#;
        assert!(ws_agg_trade("BTCUSDT", non_finite_price).is_none());
    }

    #[test]
    fn rest_agg_trades_skips_invalid_elements_without_failing_the_batch() {
        let mixed = r#"[{"a":1,"p":"100.0","q":"1.0","T":1,"m":false},
                         {"a":2,"p":"100.0","q":"0","T":2,"m":false},
                         {"a":3,"p":"100.0","q":"2.0","T":3,"m":true}]"#;
        let out = rest_agg_trades("BTCUSDT", mixed);
        let ids: Vec<u64> = out.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    /// The `.P` perp split for the trades feed: `agg_trades_ws_url`/`agg_trades_rest_url` route a
    /// perp (`is_perp = true`, api_symbol already `.P`-stripped) to the USDⓈ-M fapi/fstream hosts
    /// and spot to sapi/sstream, lower-casing the WS path symbol either way; `env` further splits
    /// testnet vs mainnet. Mirrors `data.rs`'s `rest_klines_base_selects_fapi_for_perp_and_testnet_vs_mainnet`.
    /// Also the rung-2 guard that Aster's hosts came through the [`UrlTable`] extraction unchanged.
    #[test]
    fn agg_trades_urls_split_perp_vs_spot_and_testnet_vs_mainnet() {
        // Spot testnet: exchange symbol == series label, is_perp = false.
        assert_eq!(
            agg_trades_ws_url("BTCUSDT", false, Environment::Demo),
            "wss://sstream.asterdex-testnet.com/ws/btcusdt@aggTrade"
        );
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", false, TRADES_SEED_LIMIT, None, Environment::Demo),
            "https://sapi.asterdex-testnet.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000"
        );
        // Perp testnet: `.P` already stripped to `api_symbol` by the caller; routes to fapi/fstream.
        assert_eq!(
            agg_trades_ws_url("BTCUSDT", true, Environment::Demo),
            "wss://fstream.asterdex-testnet.com/ws/btcusdt@aggTrade"
        );
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", true, TRADES_SEED_LIMIT, None, Environment::Demo),
            "https://fapi.asterdex-testnet.com/fapi/v3/aggTrades?symbol=BTCUSDT&limit=1000"
        );
        // Mainnet (Live): same split, different hosts.
        assert_eq!(
            agg_trades_ws_url("BTCUSDT", false, Environment::Live),
            "wss://sstream.asterdex.com/ws/btcusdt@aggTrade"
        );
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", true, TRADES_SEED_LIMIT, None, Environment::Live),
            "https://fapi.asterdex.com/fapi/v3/aggTrades?symbol=BTCUSDT&limit=1000"
        );
        // fromId backfill query (Some(id)):
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", false, 1000, Some(42), Environment::Demo),
            "https://sapi.asterdex-testnet.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000&fromId=42"
        );
    }

    #[test]
    fn backfill_pages_backward_until_earliest_ts() {
        // scripted id-space: trade id i has ts = i*1000 (ms), price=100, size=1, buy.
        let page = |_s: &str, from_id: u64| -> Vec<AggTrade> {
            (from_id..from_id + 1000)
                .map(|id| AggTrade {
                    id,
                    tick: vike_model::TradeTick {
                        ts: id as i64 * 1000,
                        local_ts: 0,
                        price: 100.0,
                        size: 1.0,
                        is_buyer_maker: false,
                        symbol: "BTCUSDT".into(),
                    },
                })
                .collect()
        };
        let mut got: Vec<u64> = Vec::new();
        // before_id 10_000, earliest_ts = 8_000_000 (= id 8000). Expect ids 8000..=9999, oldest-first.
        let (pages, trunc) =
            backfill_agg_trades_backward("BTCUSDT", 10_000, 8_000_000, 300, &page, &mut |t| {
                got.push(t.id)
            });
        assert!(!trunc);
        assert_eq!(*got.first().unwrap(), 8000); // oldest first
        assert_eq!(*got.last().unwrap(), 9999); // strictly < before_id 10_000
        assert!(got.iter().all(|&id| id < 10_000)); // no live overlap
        assert!(got.windows(2).all(|w| w[0] < w[1])); // ascending
        assert_eq!(pages, 2); // [9000..9999] then [8000..8999]
    }

    #[test]
    fn backfill_respects_max_pages_cap() {
        let page = |_s: &str, from_id: u64| -> Vec<AggTrade> {
            (from_id..from_id + 1000)
                .map(|id| AggTrade {
                    id,
                    tick: vike_model::TradeTick {
                        ts: id as i64,
                        local_ts: 0,
                        price: 1.0,
                        size: 1.0,
                        is_buyer_maker: false,
                        symbol: String::new(),
                    },
                })
                .collect()
        };
        let mut n = 0usize;
        let (pages, trunc) =
            backfill_agg_trades_backward("X", 1_000_000, i64::MIN, 3, &page, &mut |_| n += 1);
        assert_eq!(pages, 3);
        assert!(trunc);
        assert_eq!(n, 3000);
    }

    #[test]
    fn backfill_stops_at_id_floor() {
        let page = |_s: &str, from_id: u64| -> Vec<AggTrade> {
            (from_id..from_id + 1000)
                .map(|id| AggTrade {
                    id,
                    tick: vike_model::TradeTick {
                        ts: id as i64,
                        local_ts: 0,
                        price: 1.0,
                        size: 1.0,
                        is_buyer_maker: false,
                        symbol: String::new(),
                    },
                })
                .collect()
        };
        let mut got = Vec::new();
        let (_pages, trunc) =
            backfill_agg_trades_backward("X", 500, i64::MIN, 300, &page, &mut |t| got.push(t.id));
        assert!(!trunc);
        assert!(got.iter().all(|&id| id < 500));
    }
}
