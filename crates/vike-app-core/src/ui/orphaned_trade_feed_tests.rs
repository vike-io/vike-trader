use super::orphaned_trade_feed_keys;
use std::collections::HashSet;

const DEFAULT_VENUE: &str = "binance";

fn spawned(keys: &[&str]) -> HashSet<String> {
    keys.iter().map(|s| s.to_string()).collect()
}

fn needed(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
    pairs.iter().map(|(v, s)| (v.to_string(), s.to_string())).collect()
}

/// The core rule: a trade feed whose `(venue, symbol)` is NOT needed by any remaining
/// aggregator is orphaned and returned; one that IS needed is kept.
#[test]
fn a_trade_feed_no_aggregator_needs_is_orphaned_and_one_still_needed_is_kept() {
    let sp = spawned(&["BTCUSDT@trades", "ETHUSDT@trades"]);
    // Only BTCUSDT still has an aggregator; ETHUSDT's was the last one removed.
    let need = needed(&[(DEFAULT_VENUE, "BTCUSDT")]);
    assert_eq!(
        orphaned_trade_feed_keys(&need, &sp, DEFAULT_VENUE),
        vec!["ETHUSDT@trades".to_string()]
    );
}

/// Shared-consumer case (the invariant that guards a live feed): two aggregators on the SAME
/// `(venue, symbol)` (e.g. a Tick chart AND a Volume chart, or a tick/vol `aggs` entry AND an
/// orderflow `of_aggs` entry) both key the one trade feed. Removing ONE leaves the pair still
/// in `needed`, so the shared feed is NOT torn down while the other aggregator lives.
#[test]
fn a_trade_feed_still_shared_by_another_aggregator_is_not_reaped() {
    let sp = spawned(&["BTCUSDT@trades"]);
    // One consumer removed, but another aggregator on the same (venue,symbol) remains.
    let need = needed(&[(DEFAULT_VENUE, "BTCUSDT")]);
    assert!(
        orphaned_trade_feed_keys(&need, &sp, DEFAULT_VENUE).is_empty(),
        "a trade feed still needed by any aggregator on its (venue,symbol) must survive"
    );
}

/// When NOTHING needs a feed anymore (its window/aggregator gone), it is reaped — the leak the
/// fix closes: an empty `needed` orphans every spawned trade feed.
#[test]
fn no_remaining_aggregators_reaps_every_trade_feed() {
    let sp = spawned(&["BTCUSDT@trades", "okx:BTC-USDT@trades"]);
    let mut got = orphaned_trade_feed_keys(&HashSet::new(), &sp, DEFAULT_VENUE);
    got.sort();
    assert_eq!(got, vec!["BTCUSDT@trades".to_string(), "okx:BTC-USDT@trades".to_string()]);
}

/// Key parsing matches `ensure_trade_feed_on`'s convention exactly: a bare `"SYMBOL@trades"`
/// is the DEFAULT_VENUE (Binance); a `"venue:SYMBOL@trades"` is namespaced. A dashed symbol
/// (OKX `BTC-USDT`) round-trips because only the FIRST `:` is a venue separator.
#[test]
fn keys_parse_to_the_right_venue_and_symbol() {
    // Binance bare key: needed as (binance, BTCUSDT) → kept; needed under the WRONG venue → reaped.
    let sp = spawned(&["BTCUSDT@trades"]);
    assert!(
        orphaned_trade_feed_keys(&needed(&[("binance", "BTCUSDT")]), &sp, DEFAULT_VENUE).is_empty(),
        "bare key parses to (binance, BTCUSDT)"
    );
    assert_eq!(
        orphaned_trade_feed_keys(&needed(&[("okx", "BTCUSDT")]), &sp, DEFAULT_VENUE),
        vec!["BTCUSDT@trades".to_string()],
        "a bare key is binance, so an okx need does not keep it alive"
    );

    // OKX namespaced key with a dashed symbol: kept only under (okx, BTC-USDT).
    let sp = spawned(&["okx:BTC-USDT@trades"]);
    assert!(
        orphaned_trade_feed_keys(&needed(&[("okx", "BTC-USDT")]), &sp, DEFAULT_VENUE).is_empty(),
        "namespaced key parses to (okx, BTC-USDT), venue split on the first ':' only"
    );
    assert_eq!(
        orphaned_trade_feed_keys(&needed(&[(DEFAULT_VENUE, "BTC-USDT")]), &sp, DEFAULT_VENUE),
        vec!["okx:BTC-USDT@trades".to_string()],
        "an okx feed is not kept alive by a binance need for the same symbol"
    );
}

/// A kline key (no `@trades` suffix) is NEVER returned by the trade-feed reaper — those are
/// [`super::orphaned_feed_keys`]'s domain. Even with an empty `needed`, kline keys are ignored.
#[test]
fn kline_keys_are_never_returned() {
    let sp = spawned(&["BTCUSDT@1m", "okx:BTC-USDT@5m", "ETHUSDT@trades"]);
    // Empty needed = reap everything reapable; only the @trades key qualifies.
    assert_eq!(
        orphaned_trade_feed_keys(&HashSet::new(), &sp, DEFAULT_VENUE),
        vec!["ETHUSDT@trades".to_string()]
    );
}
