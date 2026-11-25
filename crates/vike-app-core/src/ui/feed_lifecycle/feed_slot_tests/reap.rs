//! `reap_orphaned_feeds`: what a frame tears down, and what it must leave running.

use super::*;

// ----- reap_orphaned_feeds ----------------------------------------------------------------

/// A window still backing its key keeps its slots and its tape; nothing is unsubscribed.
#[test]
fn a_live_window_keeps_its_feed() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("binance", "BTCUSDT", "100t");
    env.reap(&[chart_win("binance", "BTCUSDT", "1m"), chart_win("binance", "BTCUSDT", "100t")]);
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()], "no unsubscribe");
    assert!(
        env.spawned.contains("binance:BTCUSDT@1m")
            && env.spawned.contains("binance:BTCUSDT@trades")
    );
    assert!(env.charts.contains_key("binance:BTCUSDT@1m"));
    assert!(!env.hidden.contains("binance:BTCUSDT@1m"));
}

/// **A compare overlay keeps its feed across frames**, on a non-Binance chart too. Each frame the
/// shell ensures every overlay's series (`chart_pane`'s gather, a `SeriesSpec` on the overlay's
/// own venue) and then reaps. The overlay's key must be the one the reaper counts as live, or the
/// reaper drops its chart state every frame and the ensure recreates it empty: the feed churns and
/// never holds a bar. A new overlay takes the chart's own venue.
#[test]
fn a_compare_overlay_keeps_its_feed_across_frames() {
    let mut env = Env::new();
    let mut w = chart_win("bybit", "BTCUSDT", "1m");
    w.add_compare("ETHUSDT");
    assert_eq!(w.compare[0].venue, "bybit", "a new overlay takes the chart's own venue");
    let overlay = chart_key::series_key("bybit", "ETHUSDT", "1m");
    for frame in 0..3 {
        env.ensure(&w.venue, &w.symbol, &w.interval);
        for c in &w.compare {
            env.ensure(&c.venue, &c.symbol, &w.interval);
        }
        if frame == 0 {
            let bar =
                state::bar::ChartBar { t: 0.0, ot: 0, o: 1.0, h: 1.0, l: 1.0, c: 1.0, v: 0.0 };
            env.charts.get_mut(&overlay).expect("the overlay's chart state").bars.push(bar);
        }
        env.reap(std::slice::from_ref(&w));
        assert!(env.spawned.contains(&overlay), "frame {frame}: the overlay is still wanted");
        assert!(!env.hidden.contains(&overlay), "frame {frame}: never torn down");
        assert_eq!(env.charts[&overlay].bars.len(), 1, "frame {frame}: its bar survives the reap");
    }
}

/// **The priority scenario.** A key whose last window is gone must be torn down completely —
/// chart dropped, key marked hidden and the `spawned` slot freed — and a later [`ensure_feed_on`]
/// must genuinely want it again (unhidden). Leaving any one slot behind is how a re-added chart
/// comes back permanently dead: `hidden` still set means `sync_from_core` skips it forever.
#[test]
fn teardown_then_re_add_genuinely_restarts_the_series() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");

    env.reap(&[]); // every window closed/deleted
    assert!(env.calls().is_empty(), "a kline series asked no feed, so none is stopped");
    assert!(!env.charts.contains_key("binance:BTCUSDT@1m"), "render series dropped");
    assert!(
        env.hidden.contains("binance:BTCUSDT@1m"),
        "marked hidden against a same-frame revival"
    );
    assert!(!env.spawned.contains("binance:BTCUSDT@1m"), "spawned slot freed");

    env.ensure("binance", "BTCUSDT", "1m");
    assert!(env.spawned.contains("binance:BTCUSDT@1m"), "wanted again");
    assert!(!env.hidden.contains("binance:BTCUSDT@1m"), "unhidden again, so the fold resumes");
}

/// The unsubscribe is routed by the KEY's venue prefix, not to whichever feed happens to be
/// first: reaping an OKX tape must call `unsubscribe` on the OKX feed with the OKX id, leaving
/// the live Binance tape untouched.
#[test]
fn the_unsubscribe_is_routed_to_the_keys_own_venue() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    env.ensure("okx", "BTC-USDT", "100t");
    env.reap(&[chart_win("binance", "BTCUSDT", "100t")]); // only the Binance window survives
    assert_eq!(env.calls().last().unwrap(), "okx:unsub 201");
    assert!(
        env.spawned.contains("binance:BTCUSDT@trades"),
        "the surviving Binance tape is untouched"
    );
    assert!(!env.spawned.contains("okx:BTC-USDT@trades"));
}

/// A closed (`open = false`) window still counts as live — nothing tears down a merely-hidden
/// window's feed. The GUI-level twin of `orphaned_feed_tests`' pure assertion.
#[test]
fn a_closed_window_does_not_trigger_teardown() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    let mut w = chart_win("binance", "BTCUSDT", "100t");
    w.open = false;
    env.reap(std::slice::from_ref(&w));
    assert_eq!(env.calls().len(), 1, "no unsubscribe");
    assert!(env.spawned.contains("binance:BTCUSDT@trades"));
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
    assert!(
        !env.tick_volume_aggregators.contains_key("binance:BTCUSDT@10v"),
        "dead aggregator dropped"
    );
    assert!(env.tick_volume_aggregators.contains_key("binance:BTCUSDT@100t"));
    assert_eq!(env.calls().len(), 1, "the trade feed is still needed");
    assert!(env.spawned.contains("binance:BTCUSDT@trades"));

    // Close the last one: now the feed is orphaned and stopped.
    env.reap(&[]);
    assert!(env.tick_volume_aggregators.is_empty());
    assert_eq!(
        env.calls(),
        vec!["binance:trades BTCUSDT".to_string(), "binance:unsub 101".to_string()]
    );
    assert!(!env.spawned.contains("binance:BTCUSDT@trades"));
    assert!(!env.subscriptions.contains_key("binance:BTCUSDT@trades"));
}

/// An ORDERFLOW aggregator on the same `(venue, symbol)` holds the shared trade feed open just
/// like a tick/volume one does — the union in step (2b). Here the tick chart is closed but an
/// orderflow overlay on a still-open kline window remains, so the tape must NOT be stopped.
#[test]
fn an_orderflow_aggregator_alone_keeps_the_trade_feed_alive() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t"); // starts the tape
    env.orderflow_aggregators.insert(
        "binance:BTCUSDT@1m".to_string(),
        ("binance".to_string(), "BTCUSDT".to_string(), bar_agg::OrderflowAgg::new(1.0)),
    );
    // Only the 1m kline window (with the orderflow overlay) survives.
    env.reap(&[chart_win("binance", "BTCUSDT", "1m")]);
    assert!(env.tick_volume_aggregators.is_empty(), "the tick chart's aggregator is gone");
    assert!(
        env.orderflow_aggregators.contains_key("binance:BTCUSDT@1m"),
        "the orderflow aggregator survives"
    );
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()], "tape NOT stopped");
    assert!(env.spawned.contains("binance:BTCUSDT@trades"));
}

/// The `unroutable` record is swept like every other slot, so it cannot grow insert-only: an
/// unroutable `@trades` key is kept only while some remaining aggregator still needs its
/// `(venue, symbol)` — the same rule the trade-feed reap uses, which is why both decode the key
/// through the one shared `trade_key_pair`.
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

/// The SERIES lane of [`DatahubFeedRetries`] is swept by exactly the same two rules (step 2e), for
/// exactly the same reason: otherwise the map grows insert-only for the life of the process.
/// Two properties beyond that: a still-wanted key keeps its record (the venue is still saying
/// no, so the report is still true), and a torn-down key loses it — so a re-added series does
/// NOT inherit a stale attempt count and the inflated backoff that comes with it.
#[test]
fn the_reaper_forgets_retry_records_whose_window_is_gone() {
    let mut env = Env::with_feeds(&[("binance", 100, &["trades"])]);
    env.ensure("binance", "BTCUSDT", "100t"); // subscribe_trades fails -> a series record
    env.ensure("binance", "ETHUSDT", "100t"); // and another
    assert_eq!(env.retries.len(), 2);

    // Only the BTCUSDT tick window survives; the ETHUSDT one (and its aggregator) is gone.
    env.reap(&[chart_win("binance", "BTCUSDT", "100t")]);
    assert!(
        env.retries.is_pending(&DatahubFeedRetryKey::series("binance:BTCUSDT@trades")),
        "still wanted"
    );
    assert!(
        !env.retries.is_pending(&DatahubFeedRetryKey::series("binance:ETHUSDT@trades")),
        "no aggregator needs that tape anymore, so its record goes too"
    );

    env.reap(&[]);
    assert!(env.retries.is_empty(), "nothing wanted, nothing recorded");
}

/// The other half of that sweep: the depth/cockpit lanes are deliberately NOT swept by THIS
/// reaper, which knows nothing about `trade_depth`/`polymarket_subscriptions` — their teardown (and lane sweep)
/// is [`reap_orphaned_trade_cockpit_streams`]'s job, under its own window rules. Sweeping them
/// by the SERIES rules here would silently drop a live report the very next frame (no window
/// ever backs a `"binance:BTCUSDT"` DOM key).
#[test]
fn the_reaper_leaves_the_trade_depth_and_cockpit_retry_lanes_alone() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["trades"])]);
    env.polymarket("tok-1"); // book live, trade leg failed -> a PolymarketTrades record
    let mut depth_env = Env::with_feeds(&[("okx", 200, &["depth"])]);
    depth_env.depth("okx", "BTC-USDT-SWAP"); // depth failed -> a TradeDepth record

    env.reap(&[]);
    depth_env.reap(&[]);
    assert!(
        env.retries.is_pending(&DatahubFeedRetryKey::polymarket_trades("tok-1")),
        "a cockpit token is not a window key; the reaper must not judge it"
    );
    assert!(
        depth_env.retries.is_pending(&DatahubFeedRetryKey::trade_depth("okx", "BTC-USDT-SWAP"))
    );
}
