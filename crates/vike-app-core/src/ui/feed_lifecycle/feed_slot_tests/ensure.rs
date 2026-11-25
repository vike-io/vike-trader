//! `ensure_feed_on`, the trade tape and `stop_series`: start, retry and delete one series.

use super::*;

// ----- ensure_feed_on ---------------------------------------------------------------------

/// **A kline series asks no feed** (the CI box, 2026-11-16). It is WANTED (`spawned`, the fold filter the
/// backend's bars go through), its chart is created with the display timezone, and nothing is
/// subscribed, parked as unroutable or recorded for retry: on mounted venues whose feed refuses the bar
/// lane exactly as the datahub wire does, on a venue not mounted yet, and across many frames and a
/// retry pass. Its bars are the backend's (the snapshot's tail, else the store's). The ask it used to
/// make logged "no market-data feed mounted … nothing in this process will produce them" before the
/// datahub's handshake and "subscribe_bars refused … this series stays empty" after it, for a chart
/// that had its bars.
#[test]
fn a_kline_series_is_wanted_and_charted_but_asks_no_feed() {
    let mut env = Env::with_feeds(&[("binance", 100, &["bars!"]), ("okx", 200, &["bars!"])]);
    for _ in 0..5 {
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("okx", "BTC-USDT", "1m");
        env.ensure("bybit", "BTCUSDT", "1m"); // not mounted yet
        env.retry();
    }
    assert!(env.calls().is_empty(), "a feed was asked for a bar lane: {:?}", env.calls());
    for key in ["binance:BTCUSDT@1m", "okx:BTC-USDT@1m", "bybit:BTCUSDT@1m"] {
        assert!(env.spawned.contains(key), "{key}: wanted, so the backend's bars fold into it");
        assert!(env.charts.contains_key(key), "{key}: its chart is created");
    }
    assert!(
        !env.charts.contains_key("binance:BTC-USDT@1m"),
        "okx's key stays out of the Binance keyspace"
    );
    assert!(env.subscriptions.is_empty(), "nothing subscribed, so no id");
    assert!(env.unroutable.is_empty(), "nothing to report as unroutable");
    assert!(env.retries.is_empty(), "nothing to retry");
    assert!(
        env.tick_volume_aggregators.is_empty(),
        "a kline interval creates no client-side aggregator"
    );
    assert_eq!(env.charts["binance:BTCUSDT@1m"].time_zone(), ThemeDisplayTimeZone::Utc);
    env.ensure_at("binance", "BTCUSDT", "1m", ThemeDisplayTimeZone::Local, false);
    assert_eq!(
        env.charts["binance:BTCUSDT@1m"].time_zone(),
        ThemeDisplayTimeZone::Local,
        "the tz is still re-asserted"
    );
}

/// Teardown-safety: once `App::shutdown` is raised, no NEW live feed may start — a fresh
/// subscription would open another blocking socket read the bounded `on_exit` teardown would
/// then have to wait on (PR #589's >6s window-close hang). Nothing at all is touched.
#[test]
fn shutting_down_starts_no_new_feed_and_creates_no_chart() {
    let mut env = Env::new();
    env.ensure_at("binance", "BTCUSDT", "1m", ThemeDisplayTimeZone::Utc, true);
    assert!(env.calls().is_empty());
    assert!(env.spawned.is_empty() && env.subscriptions.is_empty() && env.charts.is_empty());
}

/// An EMPTY chart (no venue, or no symbol yet) names no series: nothing is asked for, nothing is
/// parked as unroutable for the per-frame retry, and no chart state is made.
#[test]
fn a_series_with_no_venue_or_no_symbol_touches_nothing() {
    let mut env = Env::new();
    env.ensure("", "", "1m");
    env.ensure("", "SOMETHING", "1m");
    env.ensure("binance", "", "1m");
    assert!(env.calls().is_empty());
    assert!(env.spawned.is_empty() && env.subscriptions.is_empty() && env.charts.is_empty());
    assert!(env.unroutable.is_empty());
}

/// Re-adding a Data-manager-"deleted" series just unhides it (the `hidden.remove` at the top
/// runs before every other decision, so it applies even on the idempotent repeat call).
#[test]
fn ensuring_a_hidden_key_unhides_it() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.hidden.insert("binance:BTCUSDT@1m".to_string()); // Data manager "Delete"
    env.ensure("binance", "BTCUSDT", "1m");
    assert!(!env.hidden.contains("binance:BTCUSDT@1m"));
}

/// A tick/volume interval has no venue kline feed: it subscribes the raw TRADE tape once per
/// `(venue, symbol)` and builds a per-chart-key aggregator. A second tick/volume interval on
/// the SAME symbol reuses the one trade feed and only adds an aggregator.
#[test]
fn a_tick_interval_subscribes_the_trade_tape_and_creates_an_aggregator() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()]);
    assert!(env.spawned.contains("binance:BTCUSDT@trades"));
    assert_eq!(
        env.subscriptions.get("binance:BTCUSDT@trades"),
        Some(&DataClientSubscriptionId(101))
    );
    assert_eq!(env.tick_volume_aggregators["binance:BTCUSDT@100t"].0, "binance");
    assert_eq!(env.tick_volume_aggregators["binance:BTCUSDT@100t"].1, "BTCUSDT");
    assert!(env.charts.contains_key("binance:BTCUSDT@100t"));

    env.ensure("binance", "BTCUSDT", "10v");
    assert_eq!(env.calls().len(), 1, "the trade tape is shared, not resubscribed");
    assert!(env.tick_volume_aggregators.contains_key("binance:BTCUSDT@10v"));
}

/// **The `--observe` guard.** That mode builds an EMPTY `feeds` map and renders charts purely
/// from the remote daemon's streamed bars, which `sync_from_core` folds only for keys present
/// in `spawned`. So every ensured key must still land in `spawned` even when nothing can be
/// subscribed — this is precisely what the "obvious" version of this fix (insert only after a
/// successful lookup) would have broken, silently blanking every observed chart.
#[test]
fn every_ensured_key_is_foldable_even_with_no_feeds_registered() {
    let mut env = Env::with_feeds(&[]); // the --observe shape: no venue clients at all
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("okx", "BTC-USDT", "5m");
    assert!(env.calls().is_empty(), "nothing to subscribe against");
    let mut got: Vec<String> = env.spawned.iter().cloned().collect();
    got.sort();
    assert_eq!(
        got,
        vec!["binance:BTCUSDT@1m".to_string(), "okx:BTC-USDT@5m".to_string()],
        "sync_from_core's fold filter must still admit every wanted series"
    );
}

/// One `ensure_feed_on` with the catalog asset class the Symbol picker sets
/// (`asset_class: Some(l.instrument.asset_class())`), which the `Env::ensure` shorthand leaves
/// `None`.
fn ensure_class(
    env: &mut Env,
    venue: &str,
    symbol: &str,
    interval: &str,
    asset_class: vike_model::AssetClass,
) {
    let (mut f, mut s) = env.both();
    let spec = SeriesSpec {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        asset_class: Some(asset_class),
    };
    ensure_feed_on(&mut f, &mut s, &spec, ThemeDisplayTimeZone::Utc, false);
}

/// ⚠ **THE `NO FEED` REGRESSION, measured live against the CI box with the real desktop.** A chart on
/// `deribit` with `asset_class = Some(Option)` (what the Symbol picker sets) showed `NO FEED` and
/// "this backend publishes no bars for this series" while the log said the chart had been seeded
/// from the backend's store: `venue_bar_instrument` answered `None` for a derivative outside a
/// five-venue allowlist, `ensure_feed_on` returned BEFORE `spawned.insert`, and `sync_from_core`
/// folds only keys in `spawned`. A derivative class must reach `spawned` on EVERY venue — here
/// with no feed registered at all (the observe shape), where `spawned` is the only thing standing
/// between the backend's bars and the chart.
#[test]
fn a_derivative_class_on_any_venue_reaches_the_fold_filter() {
    use vike_model::AssetClass;
    let mut env = Env::with_feeds(&[]);
    for venue in ["deribit", "bybit", "okx", "binance", "polymarket", "some-unwired-venue"] {
        for ac in [AssetClass::Option, AssetClass::CryptoPerp, AssetClass::CryptoFuture] {
            ensure_class(&mut env, venue, "BTC-X", "1m", ac);
            let key = chart_key::series_key(venue, "BTC-X", "1m");
            assert!(env.spawned.contains(&key), "{venue} / {ac:?}: {key} must be foldable");
            assert!(env.charts.contains_key(&key), "{venue} / {ac:?}: the chart is created");
        }
    }
}

/// The same deribit option on a venue whose feed IS mounted and refuses the bar lane as the datahub wire
/// does: still foldable, and the bar lane is never asked.
#[test]
fn a_derivative_on_a_mounted_venue_is_foldable_and_asks_no_bar_lane() {
    use vike_model::AssetClass;
    for venue in ["deribit", "bybit"] {
        let mut env = Env::with_feeds(&[(venue, 300, &["bars!"])]);
        let symbol = "BTC-27JUN26-60000-C";
        for _ in 0..3 {
            ensure_class(&mut env, venue, symbol, "1m", AssetClass::Option);
        }
        let key = chart_key::series_key(venue, symbol, "1m");
        assert!(env.spawned.contains(&key), "{venue}: foldable");
        assert!(env.calls().is_empty(), "{venue}: the bar lane was asked");
        assert!(env.retries.is_empty(), "{venue}: nothing recorded");
    }
}

/// A combo (spread / strategy) stays the one class `venue_bar_instrument` refuses: nothing is
/// wanted, nothing subscribed, no chart created.
#[test]
fn a_combo_is_still_skipped_before_any_slot_is_touched() {
    let mut env = Env::new();
    ensure_class(&mut env, "okx", "BTC-FS", "1m", vike_model::AssetClass::Combo);
    assert!(env.spawned.is_empty() && env.charts.is_empty() && env.calls().is_empty());
}

/// The unroutable record is per KEY, so a miss on one venue never suppresses the report for
/// another — and repeating the miss (the per-frame call) neither re-warns nor grows the set.
#[test]
fn unroutable_is_keyed_per_series_and_repeats_do_not_accumulate() {
    let mut env = Env::new();
    for _ in 0..3 {
        env.ensure("bybit", "BTCUSDT", "100t");
    }
    env.ensure("alpaca", "AAPL", "100t");
    let mut got: Vec<String> = env.unroutable.iter().cloned().collect();
    got.sort();
    assert_eq!(
        got,
        vec!["alpaca:AAPL@trades".to_string(), "bybit:BTCUSDT@trades".to_string()],
        "one entry per key, no duplicates from the per-frame repeats"
    );
    assert!(env.calls().is_empty(), "and no venue was ever called");
}

/// [`ensure_trade_feed_on`] gated its subscribe on `spawned` alone, and its missing-feed path had no
/// `else` arm at all, so a tick/volume or orderflow chart on a feedless venue lost its tape in
/// complete silence. The miss is recorded in `unroutable` now, and the tape starts once the feed does.
#[test]
fn an_unregistered_venue_does_not_permanently_suppress_the_trade_tape() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "100t");
    assert!(env.calls().is_empty(), "no feed to call");
    assert!(env.unroutable.contains("bybit:BTCUSDT@trades"), "the miss is recorded, not silent");
    assert!(
        env.tick_volume_aggregators.contains_key("bybit:BTCUSDT@100t"),
        "the aggregator is still created"
    );

    // Register the venue: the tape now genuinely starts on the next ensure.
    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.ensure("bybit", "BTCUSDT", "100t");
    assert_eq!(env.calls(), vec!["bybit:trades BTCUSDT".to_string()]);
    assert_eq!(env.subscriptions.get("bybit:BTCUSDT@trades"), Some(&DataClientSubscriptionId(301)));
    assert!(env.unroutable.is_empty());

    env.ensure("bybit", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1, "the shared tape is not resubscribed once recovered");
}

// ----- retry_unroutable -------------------------------------------------------------------

/// **A tape ensured BEFORE its venue's feed existed is subscribed by `retry_unroutable` alone — no
/// second `ensure_*`.** A chart's primary series is ensured once (window opened, symbol picked,
/// workspace restored), and the feeds are mounted from the datahub's handshake, which lands after a
/// restore asked: without this pass the tape of a restored tick/volume chart would stay missed for
/// the life of the process. A kline series on the same venue is never parked here: it asks no feed.
#[test]
fn retry_unroutable_subscribes_a_tape_whose_feed_arrived_after_the_ensure() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m");
    env.ensure("bybit", "BTCUSDT", "100t");
    assert_eq!(
        env.unroutable.iter().cloned().collect::<Vec<_>>(),
        vec!["bybit:BTCUSDT@trades".to_string()],
        "only the tape's miss is recorded"
    );

    env.retry();
    assert!(env.calls().is_empty(), "still no feed: nothing to call, and nothing is lost");
    assert_eq!(env.unroutable.len(), 1, "the record survives a pass that could not act on it");

    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.retry();
    assert_eq!(env.calls(), vec!["bybit:trades BTCUSDT".to_string()], "asked exactly once");
    assert!(env.subscriptions.contains_key("bybit:BTCUSDT@trades"), "the tape's id is remembered");
    assert!(env.unroutable.is_empty(), "routable now, so the record clears");

    env.retry();
    assert_eq!(env.calls().len(), 1, "recovery is once, not once per frame");
}

/// The default venue's keys carry no `venue:` prefix (`BTCUSDT@trades`), and the retry must read
/// that shape back as well as the namespaced one.
#[test]
fn retry_unroutable_reads_the_default_venues_unprefixed_keys() {
    let mut env = Env::with_feeds(&[("okx", 200, &[])]);
    env.ensure("binance", "BTCUSDT", "100t");
    assert!(env.unroutable.contains("binance:BTCUSDT@trades"));

    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 100, &[]));
    env.retry();
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()]);
    assert!(env.unroutable.is_empty());
}

/// **The retry must not resurrect a tape the operator DELETED.** A delete frees the key's `spawned`
/// slot but leaves its window, so the reaper's `unroutable` sweep keeps the record — and a retry
/// keyed on `unroutable` alone would subscribe the very key that was just deleted.
#[test]
fn retry_unroutable_does_not_resurrect_a_deleted_series() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "100t");
    env.stop("bybit:BTCUSDT@trades");
    assert!(!env.spawned.contains("bybit:BTCUSDT@trades"), "the delete freed the slot");
    assert!(
        env.unroutable.contains("bybit:BTCUSDT@trades"),
        "...but not the record, which is exactly what the retry must not trust on its own"
    );

    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.retry();

    assert!(env.calls().is_empty(), "a deleted series was resubscribed");
    assert!(env.subscriptions.is_empty(), "...and its id remembered");
}

/// A venue that STAYS unadvertised costs the per-frame pass a lookup and nothing else: no call, no
/// change to the record (the warn is gated on the insert, so it is not repeated either).
#[test]
fn retry_unroutable_leaves_a_venue_that_is_still_missing_alone() {
    let mut env = Env::new();
    env.ensure("alpaca", "AAPL", "100t");
    for _ in 0..3 {
        env.retry();
    }
    assert!(env.calls().is_empty(), "a venue with no feed was called");
    assert_eq!(env.unroutable.len(), 1, "the record neither grew nor was dropped");
    assert!(env.unroutable.contains("alpaca:AAPL@trades"));
}

/// A transient trade-tape error is retried on its backoff: the tape a tick/volume chart and every
/// orderflow overlay on a `(venue, symbol)` share is their ONLY source, so one transient
/// `subscribe_trades` error used to leave all of them permanently empty.
#[test]
fn a_transient_trade_subscribe_error_is_retried_and_eventually_succeeds() {
    let mut env = Env::with_feeds(&[]);
    let log = Arc::clone(&env.log);
    let fail = fail_set(&["trades"]);
    env.feeds.insert("binance", FakeFeed::boxed_flaky("binance", &log, 100, &fail));

    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1);
    assert!(env.subscriptions.is_empty(), "no tape");
    assert!(
        env.tick_volume_aggregators.contains_key("binance:BTCUSDT@100t"),
        "the aggregator exists, waiting for prints"
    );
    assert!(env.retries.is_pending(&DatahubFeedRetryKey::series("binance:BTCUSDT@trades")));

    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1, "throttled while the cooldown holds");

    fail.lock().unwrap().clear();
    env.elapse();
    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(
        env.subscriptions.get("binance:BTCUSDT@trades"),
        Some(&DataClientSubscriptionId(101)),
        "the tape starts"
    );
    assert!(env.retries.is_empty());
}

/// The one error kind that must NOT be retried. `DataClientError::Unsupported` is a DECLARED-
/// capability refusal — `vike_data::require_live_verb` derives it from the static
/// `VenueCapabilities.live_data` matrix — so waiting cannot make it true and re-asking is pure noise.
/// It is still RECORDED, so the resulting empty chart is visible rather than forgotten: that is
/// the difference between "not retried" and the old "silently burnt".
#[test]
fn a_declared_capability_refusal_is_recorded_and_never_retried() {
    let mut env = Env::with_feeds(&[("binance", 100, &["trades!"])]);
    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1);
    let key = DatahubFeedRetryKey::series("binance:BTCUSDT@trades");
    assert!(env.retries.is_pending(&key), "recorded, so the gap stays visible");
    assert!(!env.retries.is_retry_due(&key), "…but never due");

    env.elapse(); // even letting every cooldown lapse changes nothing
    for _ in 0..5 {
        env.ensure("binance", "BTCUSDT", "100t");
    }
    assert_eq!(env.calls().len(), 1, "a provably-permanent refusal is asked exactly once");
}

/// A healthy venue must be untouched by all of the above: exactly one subscribe across many
/// frames, and NOTHING recorded in either failure set.
#[test]
fn a_healthy_subscribe_still_happens_exactly_once_and_records_no_retry() {
    let mut env = Env::new();
    for _ in 0..10 {
        env.ensure("binance", "BTCUSDT", "1m");
        env.ensure("okx", "BTC-USDT", "100t");
    }
    assert_eq!(
        env.calls(),
        vec!["okx:trades BTC-USDT".to_string()],
        "exactly one socket, for the tape; the kline asks none"
    );
    assert!(env.unroutable.is_empty() && env.retries.is_empty(), "healthy path records nothing");
}

/// The backoff schedule itself: doubling from one second, capped at the 60s ceiling and flat
/// forever after (there is no attempt limit — a venue down for an hour must still recover on
/// its own), counting each failure, and starting over once the run is reset.
#[test]
fn retry_backoff_doubles_then_flattens_at_the_ceiling() {
    let mut run = crate::data::datahub::feed_retry::RetryRun::default();
    let secs: Vec<u64> = (0..8).map(|_| run.failed().as_secs()).collect();
    assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60], "capped, not 64");
    assert_eq!(run.attempts(), 8);
    assert!((0..1_000).all(|_| run.failed().as_secs() == 60), "flat for ever, never a panic");
    run.reset();
    assert_eq!((run.failed().as_secs(), run.attempts()), (1, 1), "a reset run starts over");
}

// ----- ensure_trade_feed_on ---------------------------------------------------------------

/// The trade-key convention [`orphaned_trade_feed_keys`] parses back out: `"venue:SYMBOL@trades"`
/// for every venue, Binance included. Idempotent per key.
#[test]
fn trade_feed_keys_name_their_venue() {
    let mut env = Env::new();
    {
        let (mut f, _) = env.both();
        ensure_trade_feed_on(&mut f, "binance", "BTCUSDT");
        ensure_trade_feed_on(&mut f, "binance", "BTCUSDT"); // idempotent
        ensure_trade_feed_on(&mut f, "okx", "BTC-USDT");
    }
    assert_eq!(
        env.calls(),
        vec!["binance:trades BTCUSDT".to_string(), "okx:trades BTC-USDT".to_string()]
    );
    assert_eq!(
        env.subscriptions.get("binance:BTCUSDT@trades"),
        Some(&DataClientSubscriptionId(101))
    );
    assert_eq!(env.subscriptions.get("okx:BTC-USDT@trades"), Some(&DataClientSubscriptionId(201)));
}

// ----- stop_series (the Data-manager "Delete" path) ----------------------------------------

/// **The Data-manager Delete leak, asserted instead of commented.** The GUI shell used to carry
/// its own copy of this teardown and route every unsubscribe to `feeds["binance"]`, under a
/// comment claiming every key kind was binance-only. For a `"venue:"`-namespaced key the binance
/// lookup misses, so the id minted by OKX was dropped on the floor while the `spawned` slot was
/// freed anyway — a socket running with nothing left that names it. The id must be handed back to
/// the venue that MINTED it.
#[test]
fn deleting_a_non_binance_series_unsubscribes_on_its_own_venue() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    env.ensure("okx", "BTC-USDT", "100t");
    assert_eq!(
        env.subscriptions["okx:BTC-USDT@trades"],
        DataClientSubscriptionId(201),
        "minted by the OKX feed"
    );

    env.stop("okx:BTC-USDT@trades");
    assert_eq!(
        env.calls().last().unwrap(),
        "okx:unsub 201",
        "the unsubscribe must reach the venue the key names, not the default one"
    );
    assert!(
        !env.calls().iter().any(|c| c.starts_with("binance:unsub")),
        "the binance feed must never be offered another venue's subscription id"
    );
    assert!(!env.subscriptions.contains_key("okx:BTC-USDT@trades"), "the id is forgotten");
    assert!(!env.spawned.contains("okx:BTC-USDT@trades"), "the spawned slot is freed");
    assert!(env.hidden.contains("okx:BTC-USDT@trades"), "and the key is marked hidden");
    assert!(env.spawned.contains("binance:BTCUSDT@trades"), "the untouched binance tape survives");
    assert_eq!(env.subscriptions["binance:BTCUSDT@trades"], DataClientSubscriptionId(101));
}

/// A kline series asks no feed, so deleting one frees its slots and dials nothing.
#[test]
fn deleting_a_kline_series_frees_its_slots_and_dials_nothing() {
    let mut env = Env::new();
    env.ensure("okx", "BTC-USDT", "1m");
    env.stop("okx:BTC-USDT@1m");
    assert!(env.calls().is_empty(), "nothing was subscribed, so nothing is unsubscribed");
    assert!(
        !env.spawned.contains("okx:BTC-USDT@1m") && !env.charts.contains_key("okx:BTC-USDT@1m")
    );
    assert!(env.hidden.contains("okx:BTC-USDT@1m"));

    env.ensure("okx", "BTC-USDT", "1m");
    assert!(env.spawned.contains("okx:BTC-USDT@1m"), "re-added: wanted again");
    assert!(!env.hidden.contains("okx:BTC-USDT@1m"), "unhidden again, so the fold resumes");
    assert!(env.calls().is_empty());
}
