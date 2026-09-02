//! Binance aggTrade WS/REST mappers + the live `subscribe_trades` feed (Task B2 — wires
//! `market_feed::Feeds::subscribe_trades`): the venue face of the shared [`crate::family::trades`].
//!
//! Aster's `@aggTrade` wire shape is byte-identical to Binance's, so the decode + the
//! WS-buffer→REST-warmup→splice startup dance + the backward-paging backfill live once in the
//! family module and both venues call in, each supplying its own venue string + hosts (F9, dedup
//! rung 2). What stays HERE is exactly the Binance-specific part: the mainnet host table
//! ([`crate::market_feed::BINANCE_URLS`], a `const` — nothing here is env-resolved) and the `pub`
//! seams: the production backfill `vike-app` reaches, plus the smoke's `latest_agg_trade`. See
//! the family module for the full field mapping, the non-finite guard, and the Startup/Reconnect
//! contract. The tests below stay HERE, exercising the shared code through Binance's own
//! wrappers: Aster's twin does the same through its own, so the one shared implementation is
//! proven twice.

use crate::family::trades;
use crate::family::FamilySpec;
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

/// Production wrapper (SP3 Task 3): the `pub` seam `vike-app`'s background backfill thread calls,
/// re-exported at the crate root (see `lib.rs`) as `vike_binance::agg_trades_backfill_reported`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::trades::{
        backfill_agg_trades_backward, backfill_agg_trades_backward_reported, handoff,
        parse_agg_trades_page, record_earliest_id, rest_agg_trades, ws_agg_trade, AggTrade,
        TRADES_SEED_LIMIT,
    };
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::Mutex;

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

    /// SP3 T2: `run_trades_feed` itself needs a live socket (WS connect + REST warmup), so it
    /// isn't unit-testable directly — this exercises the extracted `record_earliest_id` helper
    /// in isolation instead (family module doc's "Earliest-live-id reporting" section); the feed
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
    /// perp (`is_perp = true`, api_symbol already `.P`-stripped) to the USDS-M fstream/fapi hosts
    /// and spot to stream.binance.com/api.binance.com, lower-casing the WS path symbol either way.
    /// Mirrors `data.rs`'s `klines_url_perp_uses_fapi_host_spot_uses_api_host`. Also the rung-2
    /// guard that Binance's hosts came through the `UrlTable` extraction byte-identical.
    #[test]
    fn agg_trades_urls_split_perp_vs_spot() {
        // Spot: exchange symbol == series label, is_perp = false.
        assert_eq!(
            agg_trades_ws_url("BTCUSDT", false),
            "wss://stream.binance.com:9443/ws/btcusdt@aggTrade"
        );
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", false, TRADES_SEED_LIMIT),
            "https://api.binance.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000"
        );
        // Perp: `.P` already stripped to `api_symbol` by the caller; routes to fstream + fapi.
        //
        // ⚠ The STREAM changed and this pin changed with it: binance's futures `@aggTrade` is DEAD
        // — it accepts the socket and pushes nothing (measured 0 frames/60s on BTCUSDT AND ETHUSDT
        // while `@trade` pushed 770) — so the perp lane rides `@trade`. See `PerpTradesLane`. The
        // HOST half is unchanged, and still guards the rung-2 `UrlTable` extraction.
        assert_eq!(
            agg_trades_ws_url("BTCUSDT", true),
            "wss://fstream.binance.com/ws/btcusdt@trade"
        );
        // The paged BACKFILL still pages aggTrades — only the LIVE lane moved, and the two id
        // spaces stay deliberately separate (see `run_trades_feed`'s earliest-live-id note).
        assert_eq!(
            agg_trades_rest_url("BTCUSDT", true, TRADES_SEED_LIMIT),
            "https://fapi.binance.com/fapi/v1/aggTrades?symbol=BTCUSDT&limit=1000"
        );
        // …while the WARMUP follows the live lane, so the seed's ids land in the stream's space.
        assert_eq!(
            crate::family::trades::trades_rest_url(
                &crate::market_feed::BINANCE_URLS,
                "BTCUSDT",
                true,
                TRADES_SEED_LIMIT
            ),
            "https://fapi.binance.com/fapi/v1/trades?symbol=BTCUSDT&limit=1000"
        );
    }

    /// The paged-backfill `fromId` URL. **Rung 2 reordered this query string** — it was
    /// `?symbol=&fromId=&limit=` when built by this module's own inline `format!`, and is now
    /// `?symbol=&limit=&fromId=` because the family shares ONE builder with Aster, whose twin
    /// already emitted that order. Host, path and every value are unchanged, and aggTrades is a
    /// PUBLIC unsigned endpoint (no signature over the query string, and HTTP query params are
    /// order-independent), so this is inert on the wire — pinned here so the reorder is explicit
    /// and guarded rather than an unrecorded diff.
    #[test]
    fn backfill_from_id_url_is_spot_mainnet_with_the_shared_param_order() {
        assert_eq!(
            crate::family::trades::agg_trades_rest_url(
                &SPEC.urls,
                "BTCUSDT",
                false,
                1000,
                Some(42)
            ),
            "https://api.binance.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000&fromId=42"
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

    // ---------------------------------------------------------------------------------------
    // Stop-reason tests: a REST error must not be able to impersonate end-of-history.
    //
    // The pager stops on an empty page, and the production fetch used to reach it through
    // `unwrap_or_default()` — so any transport fault / 429 / 5xx became a clean "end of history",
    // with `truncated` (the ONLY signal, and the only `warn!`) set solely by the `max_pages` cap.
    // Combined with `vike-app`'s insert-only `bf_spawned` run-once guard, that symbol then never
    // refetched. These pin the three outcomes apart. Aster proves the same shared code through its
    // own wrappers.
    // ---------------------------------------------------------------------------------------

    /// A scripted contiguous id space: trade id `i` has `ts = i` (ms), price 1, size 1 — the page a
    /// venue would serve for `?fromId={from_id}&limit=1000`.
    fn full_page(from_id: u64) -> Vec<AggTrade> {
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
    }

    /// An infallible page source, as a fallible one (the shape production can no longer use).
    fn ok_pages(_s: &str, from_id: u64) -> Result<Vec<AggTrade>, String> {
        Ok(full_page(from_id))
    }

    /// THE bug: page 2 fails, and the walk must report `Failed` — not the `Complete` an empty page
    /// would produce. The page that DID arrive is still emitted (dropping it would only add a second
    /// loss), oldest-first and strictly below `before_id` as always.
    #[test]
    fn an_errored_page_stops_as_failed_and_is_not_mistaken_for_end_of_history() {
        let calls = Cell::new(0u32);
        let fetch = |_s: &str, from_id: u64| -> Result<Vec<AggTrade>, String> {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Ok(full_page(from_id))
            } else {
                Err("binance aggTrades HTTP 500: <html>bad gateway".to_string())
            }
        };
        let mut got: Vec<u64> = Vec::new();
        let out = backfill_agg_trades_backward_reported(
            "X",
            1_000_000,
            i64::MIN,
            300,
            &fetch,
            &mut |t| got.push(t.id),
        );

        assert_eq!(out.pages, 1, "only the page that succeeded counts");
        assert_eq!(
            out.stop,
            BackfillStop::Failed("binance aggTrades HTTP 500: <html>bad gateway".to_string()),
            "the fetch's error must reach the caller verbatim"
        );
        assert!(out.stop.is_truncated(), "an errored walk is NOT a complete one");
        assert!(out.stop.is_retryable(), "the venue still has this history — refetch is the fix");
        // The good page survives, unchanged in shape.
        assert_eq!(got.len(), 1000);
        assert!(got.windows(2).all(|w| w[0] < w[1]), "still oldest-first");
        assert!(got.iter().all(|&id| id < 1_000_000), "no live overlap");
    }

    /// The worst case of the same bug: the FIRST page fails. Nothing is emitted, and the walk must
    /// still not look like "this symbol has no history older than `before_id`".
    #[test]
    fn a_first_page_error_reports_failed_with_zero_pages_and_emits_nothing() {
        let fetch = |_s: &str, _f: u64| -> Result<Vec<AggTrade>, String> {
            Err("binance aggTrades GET: dns error".to_string())
        };
        let mut n = 0usize;
        let out = backfill_agg_trades_backward_reported(
            "X",
            1_000_000,
            i64::MIN,
            300,
            &fetch,
            &mut |_| n += 1,
        );
        assert_eq!(out.pages, 0);
        assert_eq!(n, 0);
        assert!(matches!(out.stop, BackfillStop::Failed(_)));
        assert!(out.stop.is_truncated(), "a total failure must not read as a complete walk");
        assert!(out.stop.is_retryable());
    }

    /// The other side of the contract: a genuinely empty page — the venue answering 200 with `[]`,
    /// which on a contiguous id space means there is nothing older — still ends the walk cleanly.
    #[test]
    fn a_genuinely_empty_page_still_ends_paging_cleanly_as_complete() {
        let fetch = |_s: &str, _f: u64| -> Result<Vec<AggTrade>, String> { Ok(Vec::new()) };
        let mut n = 0usize;
        let out = backfill_agg_trades_backward_reported(
            "X",
            1_000_000,
            i64::MIN,
            300,
            &fetch,
            &mut |_| n += 1,
        );
        assert_eq!(out.pages, 0);
        assert_eq!(out.stop, BackfillStop::Complete);
        assert_eq!(n, 0);
        assert!(!out.stop.is_truncated());
        assert!(!out.stop.is_retryable(), "nothing to retry — the venue has no older trades");
    }

    /// A walk that reaches back to `earliest_ts` is `Complete`, and the emitted window is exactly
    /// the pre-existing one (`backfill_pages_backward_until_earliest_ts`'s twin, through the
    /// reporting API).
    #[test]
    fn a_walk_that_reaches_earliest_ts_reports_complete() {
        let mut got: Vec<u64> = Vec::new();
        let out =
            backfill_agg_trades_backward_reported("X", 10_000, 8_000, 300, &ok_pages, &mut |t| {
                got.push(t.id)
            });
        assert_eq!(out.pages, 2);
        assert_eq!(out.stop, BackfillStop::Complete);
        assert!(!out.stop.is_truncated());
        assert_eq!(*got.first().unwrap(), 8_000);
        assert_eq!(*got.last().unwrap(), 9_999);
    }

    /// The cap still behaves — and reports `Capped`, which is deliberately NOT retryable: an
    /// identical re-run would stop at the identical page.
    #[test]
    fn the_max_pages_cap_reports_capped_not_failed_and_is_not_retryable() {
        let mut n = 0usize;
        let out = backfill_agg_trades_backward_reported(
            "X",
            1_000_000,
            i64::MIN,
            3,
            &ok_pages,
            &mut |_| n += 1,
        );
        assert_eq!(out.pages, 3);
        assert_eq!(n, 3000);
        assert_eq!(out.stop, BackfillStop::Capped);
        assert!(out.stop.is_truncated());
        assert!(!out.stop.is_retryable(), "raise the cap, don't re-run the same request");
    }

    /// The requirement the old `(pages, truncated)` pair could not meet: BOTH truncating causes look
    /// identical in every observable except `stop`. Same page count, same "history is missing"
    /// verdict — opposite operator responses (raise the cap vs refetch the symbol).
    #[test]
    fn capped_and_errored_truncations_are_distinguishable_by_the_caller() {
        let mut sink = |_t: AggTrade| {};
        let capped = backfill_agg_trades_backward_reported(
            "X",
            1_000_000,
            i64::MIN,
            3,
            &ok_pages,
            &mut sink,
        );

        let calls = Cell::new(0u32);
        let flaky = |_s: &str, from_id: u64| -> Result<Vec<AggTrade>, String> {
            calls.set(calls.get() + 1);
            if calls.get() <= 3 {
                Ok(full_page(from_id))
            } else {
                Err("binance aggTrades rate-limited: HTTP 429 (rate limited) (after 6 retries)"
                    .to_string())
            }
        };
        let failed =
            backfill_agg_trades_backward_reported("X", 1_000_000, i64::MIN, 300, &flaky, &mut sink);

        assert_eq!(capped.pages, failed.pages, "identical page counts by construction");
        assert!(capped.stop.is_truncated() && failed.stop.is_truncated());
        assert_ne!(capped.stop, failed.stop, "the CAUSE must survive");
        assert!(!capped.stop.is_retryable());
        assert!(failed.stop.is_retryable());
    }

    /// The infallible compat wrapper both venues' tests use is exactly the reporting walk with the
    /// stop flattened to the historical cap-only `bool` — same pages, same emitted ids. Pins that
    /// nothing about the pre-existing behavior moved when the error path was added.
    #[test]
    fn the_infallible_wrapper_is_the_reported_walk_with_the_stop_flattened() {
        for (max_pages, before_id) in [(3u32, 1_000_000u64), (300, 5_000), (300, 500), (300, 1)] {
            let mut via_pair: Vec<u64> = Vec::new();
            let pair = backfill_agg_trades_backward(
                "X",
                before_id,
                i64::MIN,
                max_pages,
                &|_s: &str, f: u64| full_page(f),
                &mut |t| via_pair.push(t.id),
            );
            let mut via_outcome: Vec<u64> = Vec::new();
            let out = backfill_agg_trades_backward_reported(
                "X",
                before_id,
                i64::MIN,
                max_pages,
                &ok_pages,
                &mut |t| via_outcome.push(t.id),
            );
            assert_eq!(pair, (out.pages, out.stop == BackfillStop::Capped), "{before_id}");
            assert_eq!(via_pair, via_outcome, "{before_id}");
            assert_ne!(out.stop, BackfillStop::Failed(String::new()), "unreachable without errors");
        }
    }

    /// The fourth silent path, closed alongside the other three: a `200` whose body is not an
    /// aggTrades array. `rest_agg_trades` is deliberately lenient there (an unusable body is an
    /// empty vec — right for the live warmup seed, which the buffered live trades make up for), but
    /// on the BACKFILL path an empty page is a decision that stops the walk. `parse_agg_trades_page`
    /// is the strict twin the backfill fetch uses, so a garbled body can no longer impersonate the
    /// end of history.
    #[test]
    fn a_garbled_200_body_errors_on_the_backfill_path_but_stays_lenient_on_the_warmup_path() {
        let garbled = r#"{"code":-1121,"msg":"Invalid symbol."}"#;
        assert!(rest_agg_trades("BTCUSDT", garbled).is_empty(), "warmup seed stays lenient");
        let err = parse_agg_trades_page("binance", "BTCUSDT", garbled).unwrap_err();
        assert!(err.starts_with("binance aggTrades json: "), "got: {err}");
        assert!(err.contains("Invalid symbol"), "the body head must survive into the error: {err}");

        // A genuinely empty ARRAY is still an empty page on both paths — that is real end-of-history.
        assert!(parse_agg_trades_page("binance", "BTCUSDT", "[]").unwrap().is_empty());

        // Element-wise tolerance is unchanged from `rest_agg_trades`: a non-positive size is skipped
        // in place, not batch-failed.
        let mixed = r#"[{"a":1,"p":"100.0","q":"1.0","T":1,"m":false},
                         {"a":2,"p":"100.0","q":"0","T":2,"m":false},
                         {"a":3,"p":"100.0","q":"2.0","T":3,"m":true}]"#;
        let ids: Vec<u64> = parse_agg_trades_page("binance", "BTCUSDT", mixed)
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![1, 3]);
        assert_eq!(ids, rest_agg_trades("BTCUSDT", mixed).iter().map(|t| t.id).collect::<Vec<_>>());
    }
}
