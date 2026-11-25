use super::orphaned_trade_feed_keys;
use std::collections::HashSet;

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
    let sp = spawned(&["binance:BTCUSDT@trades", "binance:ETHUSDT@trades"]);
    // Only BTCUSDT still has an aggregator; ETHUSDT's was the last one removed.
    let need = needed(&[("binance", "BTCUSDT")]);
    assert_eq!(orphaned_trade_feed_keys(&need, &sp), vec!["binance:ETHUSDT@trades".to_string()]);
}

/// Shared-consumer case (the invariant that guards a live feed): two aggregators on the SAME
/// `(venue, symbol)` (e.g. a Tick chart AND a Volume chart, or a tick/vol `tick_volume_aggregators` entry AND an
/// orderflow `orderflow_aggregators` entry) both key the one trade feed. Removing ONE leaves the pair still
/// in `needed`, so the shared feed is NOT torn down while the other aggregator lives.
#[test]
fn a_trade_feed_still_shared_by_another_aggregator_is_not_reaped() {
    let sp = spawned(&["binance:BTCUSDT@trades"]);
    // One consumer removed, but another aggregator on the same (venue,symbol) remains.
    let need = needed(&[("binance", "BTCUSDT")]);
    assert!(
        orphaned_trade_feed_keys(&need, &sp).is_empty(),
        "a trade feed still needed by any aggregator on its (venue,symbol) must survive"
    );
}

/// When NOTHING needs a feed anymore (its window/aggregator gone), it is reaped — the leak the
/// fix closes: an empty `needed` orphans every spawned trade feed.
#[test]
fn no_remaining_aggregators_reaps_every_trade_feed() {
    let sp = spawned(&["binance:BTCUSDT@trades", "okx:BTC-USDT@trades"]);
    let mut got = orphaned_trade_feed_keys(&HashSet::new(), &sp);
    got.sort();
    assert_eq!(got, vec!["binance:BTCUSDT@trades".to_string(), "okx:BTC-USDT@trades".to_string()]);
}

/// Key parsing matches `ensure_trade_feed_on`'s convention exactly: every trade key is
/// `"venue:SYMBOL@trades"`, Binance's included. A dashed symbol (OKX `BTC-USDT`) round-trips
/// because only the FIRST `:` is a venue separator. A key naming no venue is never a trade key.
#[test]
fn keys_parse_to_the_right_venue_and_symbol() {
    // Binance key: needed as (binance, BTCUSDT) → kept; needed under the WRONG venue → reaped.
    let sp = spawned(&["binance:BTCUSDT@trades"]);
    assert!(
        orphaned_trade_feed_keys(&needed(&[("binance", "BTCUSDT")]), &sp).is_empty(),
        "the binance key parses to (binance, BTCUSDT)"
    );
    assert_eq!(
        orphaned_trade_feed_keys(&needed(&[("okx", "BTCUSDT")]), &sp),
        vec!["binance:BTCUSDT@trades".to_string()],
        "a binance key is binance, so an okx need does not keep it alive"
    );

    // OKX namespaced key with a dashed symbol: kept only under (okx, BTC-USDT).
    let sp = spawned(&["okx:BTC-USDT@trades"]);
    assert!(
        orphaned_trade_feed_keys(&needed(&[("okx", "BTC-USDT")]), &sp).is_empty(),
        "namespaced key parses to (okx, BTC-USDT), venue split on the first ':' only"
    );
    assert_eq!(
        orphaned_trade_feed_keys(&needed(&[("binance", "BTC-USDT")]), &sp),
        vec!["okx:BTC-USDT@trades".to_string()],
        "an okx feed is not kept alive by a binance need for the same symbol"
    );
}

/// A kline key (no `@trades` suffix) is NEVER returned by the trade-feed reaper — those are
/// [`super::orphaned_feed_keys`]'s domain. Even with an empty `needed`, kline keys are ignored,
/// and so is a string naming no venue (the retired bare shape): it is no trade key at all.
#[test]
fn kline_keys_are_never_returned() {
    let sp = spawned(&[
        "binance:BTCUSDT@1m",
        "okx:BTC-USDT@5m",
        "binance:ETHUSDT@trades",
        "SOLUSDT@trades",
    ]);
    // Empty needed = reap everything reapable; only the @trades key qualifies.
    assert_eq!(
        orphaned_trade_feed_keys(&HashSet::new(), &sp),
        vec!["binance:ETHUSDT@trades".to_string()]
    );
}
