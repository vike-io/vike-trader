//! `reap_orphaned_feeds`: what a frame tears down, and what it must leave running.

use super::*;

// ----- reap_orphaned_feeds ----------------------------------------------------------------

/// A window still backing its key keeps its feed; nothing is unsubscribed and no slot moves.
#[test]
fn a_live_window_keeps_its_feed() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
    assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
    assert!(env.spawned.contains("BTCUSDT@1m"));
    assert!(env.charts.contains_key("BTCUSDT@1m"));
    assert!(!env.hidden.contains("BTCUSDT@1m"));
}

/// **The priority scenario.** A key whose last window is gone must be torn down completely —
/// chart dropped, key marked hidden, the live subscription STOPPED on its own venue feed, and
/// the `spawned` slot freed — and then a later [`ensure_feed_on`] must genuinely restart it (a
/// NEW subscription id, unhidden again). Leaving any one slot behind is how a re-added chart
/// comes back permanently dead: `spawned` still set means no resubscribe, `hidden` still set
/// means `sync_from_core` skips it forever.
#[test]
fn teardown_then_re_add_genuinely_restarts_the_feed() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(101));

    env.reap(&[]); // every window closed/deleted
    assert_eq!(
        env.calls(),
        vec!["binance:bars BTCUSDT 1m".to_string(), "binance:unsub 101".to_string()],
        "the live subscription is stopped on the venue it was started on"
    );
    assert!(!env.charts.contains_key("BTCUSDT@1m"), "render series dropped");
    assert!(env.hidden.contains("BTCUSDT@1m"), "marked hidden against a same-frame revival");
    assert!(!env.spawned.contains("BTCUSDT@1m"), "spawned slot freed");
    assert!(!env.subs.contains_key("BTCUSDT@1m"), "subscription id forgotten");

    // Re-add the very same series: it must actually resubscribe, not silently no-op.
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 3, "a real, second subscribe_bars");
    assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(102), "a FRESH subscription id");
    assert!(!env.hidden.contains("BTCUSDT@1m"), "unhidden again, so the fold resumes");
}

/// The unsubscribe is routed by the KEY's venue prefix, not to whichever feed happens to be
/// first: reaping an OKX key must call `unsubscribe` on the OKX feed with the OKX id, leaving
/// the live Binance series untouched.
#[test]
fn the_unsubscribe_is_routed_to_the_keys_own_venue() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("okx", "BTC-USDT", "1m");
    env.reap(&[chart_win("binance", "BTCUSDT", "1m")]); // only the Binance window survives
    assert_eq!(env.calls().last().unwrap(), "okx:unsub 201");
    assert!(env.spawned.contains("BTCUSDT@1m"), "the surviving Binance feed is untouched");
    assert!(!env.spawned.contains("okx:BTC-USDT@1m"));
}

/// A closed (`open = false`) window still counts as live — nothing tears down a merely-hidden
/// window's feed. The GUI-level twin of `orphaned_feed_tests`' pure assertion.
#[test]
fn a_closed_window_does_not_trigger_teardown() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    let mut w = chart_win("binance", "BTCUSDT", "1m");
    w.open = false;
    env.reap(std::slice::from_ref(&w));
    assert_eq!(env.calls().len(), 1, "no unsubscribe");
    assert!(env.spawned.contains("BTCUSDT@1m"));
}

/// The trade-tape half. Two tick charts on the SAME `(venue, symbol)` share ONE trade feed:
/// closing one drops only its aggregator, and the shared feed survives; closing the second
/// drops the last aggregator and only THEN is the feed stopped — exactly once.
#[test]
fn a_shared_trade_feed_is_stopped_only_when_the_last_aggregator_goes() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    env.ensure("binance", "BTCUSDT", "10v");
    assert_eq!(env.calls().len(), 1, "one trade feed for both charts");

    // Close the 10v chart: its aggregator dies, the shared feed lives on.
    env.reap(&[chart_win("binance", "BTCUSDT", "100t")]);
    assert!(!env.aggs.contains_key("BTCUSDT@10v"), "dead aggregator dropped");
    assert!(env.aggs.contains_key("BTCUSDT@100t"));
    assert_eq!(env.calls().len(), 1, "the trade feed is still needed");
    assert!(env.spawned.contains("BTCUSDT@trades"));

    // Close the last one: now the feed is orphaned and stopped.
    env.reap(&[]);
    assert!(env.aggs.is_empty());
    assert_eq!(
        env.calls(),
        vec!["binance:trades BTCUSDT".to_string(), "binance:unsub 101".to_string()]
    );
    assert!(!env.spawned.contains("BTCUSDT@trades"));
    assert!(!env.subs.contains_key("BTCUSDT@trades"));
}

/// An ORDERFLOW aggregator on the same `(venue, symbol)` holds the shared trade feed open just
/// like a tick/volume one does — the union in step (2b). Here the tick chart is closed but an
/// orderflow overlay on a still-open kline window remains, so the tape must NOT be stopped.
#[test]
fn an_orderflow_aggregator_alone_keeps_the_trade_feed_alive() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t"); // starts the tape
    env.of_aggs.insert(
        "BTCUSDT@1m".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), bar_agg::OrderflowAgg::new(1.0)),
    );
    // Only the 1m kline window (with the orderflow overlay) survives.
    env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
    assert!(env.aggs.is_empty(), "the tick chart's aggregator is gone");
    assert!(env.of_aggs.contains_key("BTCUSDT@1m"), "the orderflow aggregator survives");
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()], "tape NOT stopped");
    assert!(env.spawned.contains("BTCUSDT@trades"));
}

/// The `unroutable` record is swept by the reaper like every other slot, so it cannot become a
/// new insert-only set. A key whose window is gone is forgotten; a key whose window is still
/// open stays (the venue is still missing, so the report is still true).
#[test]
fn the_reaper_forgets_unroutable_keys_whose_window_is_gone() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m"); // no bybit feed -> unroutable
    env.ensure("bybit", "ETHUSDT", "1m");
    assert_eq!(env.unroutable.len(), 2);

    // Only the BTCUSDT window survives.
    env.reap(&[chart_win("bybit", "BTCUSDT", "1m")]);
    assert_eq!(
        env.unroutable.iter().cloned().collect::<Vec<_>>(),
        vec!["bybit:BTCUSDT@1m".to_string()],
        "the live window's key stays reported; the closed one is forgotten"
    );

    env.reap(&[]);
    assert!(env.unroutable.is_empty(), "no windows left, nothing to report");
}

/// The trade-key half of that sweep: an unroutable `@trades` key is kept only while some
/// remaining aggregator still needs its `(venue, symbol)` — the same rule the trade-feed reap
/// uses, which is why both decode the key through the one shared `trade_key_pair`.
#[test]
fn the_reaper_sweeps_unroutable_trade_keys_by_remaining_aggregators() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "100t"); // no bybit feed -> unroutable trade key
    assert!(env.unroutable.contains("bybit:BTCUSDT@trades"));

    // The window still exists, so its aggregator survives and the report stays.
    env.reap(&[chart_win("bybit", "BTCUSDT", "100t")]);
    assert!(env.unroutable.contains("bybit:BTCUSDT@trades"), "aggregator still needs it");

    // Close it: the aggregator dies, so the trade-key report is dropped too.
    env.reap(&[]);
    assert!(env.unroutable.is_empty());
}

/// The SERIES lane of [`FeedRetries`] is swept by exactly the same two rules (step 2e), for
/// exactly the same reason: otherwise the map grows insert-only for the life of the process.
/// Two properties beyond that: a still-wanted key keeps its record (the venue is still saying
/// no, so the report is still true), and a torn-down key loses it — so a re-added series does
/// NOT inherit a stale attempt count and the inflated backoff that comes with it.
#[test]
fn the_reaper_forgets_retry_records_whose_window_is_gone() {
    let mut env = Env::with_feeds(&[("binance", 100, &["bars", "trades"])]);
    env.ensure("binance", "BTCUSDT", "1m"); // subscribe_bars fails -> a series record
    env.ensure("binance", "ETHUSDT", "100t"); // subscribe_trades fails -> another
    assert_eq!(env.retries.len(), 2);

    // Only the BTCUSDT kline window survives; the tick window (and its aggregator) is gone.
    env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
    assert!(env.retries.is_pending(&RetryKey::series("BTCUSDT@1m")), "still wanted");
    assert!(
        !env.retries.is_pending(&RetryKey::series("ETHUSDT@trades")),
        "no aggregator needs that tape anymore, so its record goes too"
    );

    env.reap(&[]);
    assert!(env.retries.is_empty(), "nothing wanted, nothing recorded");
}

/// The other half of that sweep: the depth/cockpit lanes are deliberately NOT swept by THIS
/// reaper, which knows nothing about `trade_depth`/`poly_subs` — their teardown (and lane sweep)
/// is [`reap_orphaned_trade_cockpit_streams`]'s job, under its own window rules. Sweeping them
/// by the SERIES rules here would silently drop a live report the very next frame (no window
/// ever backs a `"binance:BTCUSDT"` DOM key).
#[test]
fn the_reaper_leaves_the_trade_depth_and_cockpit_retry_lanes_alone() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["trades"])]);
    env.poly("tok-1"); // book live, trade leg failed -> a PolyTrades record
    let mut depth_env = Env::with_feeds(&[("okx", 200, &["depth"])]);
    depth_env.depth("okx", "BTC-USDT-SWAP"); // depth failed -> a TradeDepth record

    env.reap(&[]);
    depth_env.reap(&[]);
    assert!(
        env.retries.is_pending(&RetryKey::poly_trades("tok-1")),
        "a cockpit token is not a window key; the reaper must not judge it"
    );
    assert!(depth_env.retries.is_pending(&RetryKey::trade_depth("okx", "BTC-USDT-SWAP")));
}
