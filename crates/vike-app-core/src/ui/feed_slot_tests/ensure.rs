//! `ensure_feed_on`, the trade tape and `stop_series`: start, retry and delete one series.

use super::*;

// ----- ensure_feed_on ---------------------------------------------------------------------

/// The happy path: ONE `subscribe_bars` on the named venue, the returned id remembered under
/// the `series_key`, the key marked `spawned`, and a `ChartState` created with the passed
/// display timezone applied.
#[test]
fn a_kline_feed_subscribes_once_and_fills_every_slot() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
    assert_eq!(env.subs.get("BTCUSDT@1m"), Some(&SubscriptionId(101)));
    assert!(env.spawned.contains("BTCUSDT@1m"));
    assert_eq!(env.charts["BTCUSDT@1m"].tz(), DisplayTz::Utc);
    assert!(env.aggs.is_empty(), "a kline interval creates no client-side aggregator");
}

/// Idempotent per key (the `spawned.insert` gate): calling it every frame — which the window
/// loop does — must never open a second socket. The chart's tz is still re-asserted, which is
/// how a menu tz change propagates without a resubscribe.
#[test]
fn a_second_ensure_for_the_same_key_does_not_resubscribe() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure_at("binance", "BTCUSDT", "1m", DisplayTz::Local, false);
    assert_eq!(env.calls().len(), 1, "exactly one subscribe for a repeated ensure");
    assert_eq!(env.charts["BTCUSDT@1m"].tz(), DisplayTz::Local, "tz is still re-asserted");
}

/// A non-Binance venue is namespaced by `series_key` and routed to ITS OWN feed — the
/// cross-exchange bug class this keying exists to prevent (a same-named Binance chart must
/// never receive an OKX subscription, or vice versa).
#[test]
fn a_non_binance_venue_is_namespaced_and_routed_to_its_own_feed() {
    let mut env = Env::new();
    env.ensure("okx", "BTC-USDT", "1m");
    assert_eq!(env.calls(), vec!["okx:bars BTC-USDT 1m".to_string()]);
    assert_eq!(env.subs.get("okx:BTC-USDT@1m"), Some(&SubscriptionId(201)));
    assert!(env.charts.contains_key("okx:BTC-USDT@1m"));
    assert!(!env.charts.contains_key("BTC-USDT@1m"), "must not land in the Binance keyspace");
}

/// Teardown-safety: once `App::shutdown` is raised, no NEW live feed may start — a fresh
/// subscription would open another blocking socket read the bounded `on_exit` teardown would
/// then have to wait on (PR #589's >6s window-close hang). Nothing at all is touched.
#[test]
fn shutting_down_starts_no_new_feed_and_creates_no_chart() {
    let mut env = Env::new();
    env.ensure_at("binance", "BTCUSDT", "1m", DisplayTz::Utc, true);
    assert!(env.calls().is_empty());
    assert!(env.spawned.is_empty() && env.subs.is_empty() && env.charts.is_empty());
}

/// Re-adding a Data-manager-"deleted" series just unhides it (the `hidden.remove` at the top
/// runs before every other decision, so it applies even on the idempotent repeat call).
#[test]
fn ensuring_a_hidden_key_unhides_it() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.hidden.insert("BTCUSDT@1m".to_string()); // Data manager "Delete"
    env.ensure("binance", "BTCUSDT", "1m");
    assert!(!env.hidden.contains("BTCUSDT@1m"));
}

/// A tick/volume interval has no venue kline feed: it subscribes the raw TRADE tape once per
/// `(venue, symbol)` and builds a per-chart-key aggregator. A second tick/volume interval on
/// the SAME symbol reuses the one trade feed and only adds an aggregator.
#[test]
fn a_tick_interval_subscribes_the_trade_tape_and_creates_an_aggregator() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls(), vec!["binance:trades BTCUSDT".to_string()]);
    assert!(env.spawned.contains("BTCUSDT@trades"));
    assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)));
    assert_eq!(env.aggs["BTCUSDT@100t"].0, "binance");
    assert_eq!(env.aggs["BTCUSDT@100t"].1, "BTCUSDT");
    assert!(env.charts.contains_key("BTCUSDT@100t"));

    env.ensure("binance", "BTCUSDT", "10v");
    assert_eq!(env.calls().len(), 1, "the trade tape is shared, not resubscribed");
    assert!(env.aggs.contains_key("BTCUSDT@10v"));
}

/// **The regression proof.** This test previously asserted the BUG (as
/// `an_unregistered_venue_still_burns_the_spawned_slot`), whose closing assertion was literally
/// `"the burned `spawned` slot suppresses the retry"`. Inverted here: the miss must be
/// recorded, not suppressed, so a later ensure can still act on it.
///
/// Note what is deliberately NOT inverted — the key IS still in `spawned`. That set doubles as
/// `sync_from_core`'s fold filter, so narrowing it would break `--observe` mode (see
/// [`ensure_feed_on`]'s doc, and `every_ensured_key_is_foldable_even_with_no_feeds_registered`
/// below). `unroutable` is what carries the "never actually subscribed" fact.
///
/// Reachable from the UI: the Symbol picker searches a twelve-venue catalog while only six live
/// clients are registered, so this is what picking an Alpaca/OANDA/cTrader instrument does.
#[test]
fn an_unregistered_venue_does_not_permanently_suppress_the_subscribe() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m");
    assert!(env.calls().is_empty(), "no feed to call");
    assert!(env.subs.is_empty(), "nothing was subscribed, so there is no id to remember");
    assert!(
        env.unroutable.contains("bybit:BTCUSDT@1m"),
        "the miss is RECORDED — this is what reopens the subscribe, and what makes it visible"
    );
    assert!(env.spawned.contains("bybit:BTCUSDT@1m"), "still wanted, so still foldable");
    // The chart is still created, so the window renders empty rather than not at all.
    assert!(env.charts.contains_key("bybit:BTCUSDT@1m"));
}

/// The recovery half, and the exact assertion the old pin made in the negative: registering the
/// venue's feed afterwards and re-ensuring now genuinely subscribes. `unroutable` clears again,
/// so it tracks CURRENT state rather than accumulating tombstones.
#[test]
fn registering_the_venue_later_lets_a_re_ensure_succeed() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m");
    assert!(env.calls().is_empty());

    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.ensure("bybit", "BTCUSDT", "1m");

    assert_eq!(env.calls(), vec!["bybit:bars BTCUSDT 1m".to_string()], "the retry really runs");
    assert_eq!(env.subs.get("bybit:BTCUSDT@1m"), Some(&SubscriptionId(301)));
    assert!(env.unroutable.is_empty(), "no longer unroutable, so the record clears");

    // …and it does not now resubscribe forever: the recovered key is a normal spawned key.
    env.ensure("bybit", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 1, "recovery is once, not once per frame");
}

/// The guard still guards: reopening the subscribe on a MISS must not weaken the once-per-key
/// rule on the HIT. Every caller is per-frame, so a registered venue re-ensured many times must
/// open exactly one socket — the property a careless "just move the insert" fix would break.
#[test]
fn a_registered_venue_still_subscribes_exactly_once_across_many_frames() {
    let mut env = Env::new();
    for _ in 0..5 {
        env.ensure("binance", "BTCUSDT", "1m");
    }
    assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()], "exactly one socket");
    assert_eq!(env.subs.len(), 1, "and exactly one id remembered");
    assert!(env.unroutable.is_empty(), "a routable key never enters the unroutable set");
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
        vec!["BTCUSDT@1m".to_string(), "okx:BTC-USDT@5m".to_string()],
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
    let spec = SeriesSpec { venue, symbol, interval, asset_class: Some(asset_class) };
    ensure_feed_on(&mut f, &mut s, spec, DisplayTz::Utc, false);
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
            let key = workspace::series_key(venue, "BTC-X", "1m");
            assert!(env.spawned.contains(&key), "{venue} / {ac:?}: {key} must be foldable");
            assert!(env.charts.contains_key(&key), "{venue} / {ac:?}: the chart is created");
        }
    }
}

/// The same deribit option, with a feed registered that DECLARES the bar lane unsupported (the
/// `DatahubFeed::subscribe_bars` answer, `LiveDataError::Unsupported`): the key still lands in
/// `spawned` (the chart folds the backend's bars), the refusal is recorded so the gap stays
/// visible, and it is asked exactly once and never retried — the same rule a `bybit` kline has
/// always had.
#[test]
fn a_declared_bar_lane_refusal_on_a_derivative_is_recorded_but_still_foldable() {
    use vike_model::AssetClass;
    for venue in ["deribit", "bybit"] {
        let mut env = Env::with_feeds(&[(venue, 300, &["bars!"])]);
        let sym = "BTC-27JUN26-60000-C";
        ensure_class(&mut env, venue, sym, "1m", AssetClass::Option);
        let key = workspace::series_key(venue, sym, "1m");
        assert!(env.spawned.contains(&key), "{venue}: foldable despite the refusal");
        assert_eq!(env.calls().len(), 1, "{venue}: the subscribe WAS attempted");
        let retry_key = RetryKey::series(&key);
        assert!(env.retries.is_pending(&retry_key), "{venue}: recorded");
        assert!(!env.retries.is_retry_due(&retry_key), "{venue}: …and never due");

        env.elapse();
        for _ in 0..5 {
            ensure_class(&mut env, venue, sym, "1m", AssetClass::Option);
        }
        assert_eq!(env.calls().len(), 1, "{venue}: a declared refusal is asked exactly once");
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
        env.ensure("bybit", "BTCUSDT", "1m");
    }
    env.ensure("alpaca", "AAPL", "1m");
    let mut got: Vec<String> = env.unroutable.iter().cloned().collect();
    got.sort();
    assert_eq!(
        got,
        vec!["alpaca:AAPL@1m".to_string(), "bybit:BTCUSDT@1m".to_string()],
        "one entry per key, no duplicates from the per-frame repeats"
    );
    assert!(env.calls().is_empty(), "and no venue was ever called");
}

/// The trade-tape twin: [`ensure_trade_feed_on`] had the same defect in a WORSE form — its
/// missing-feed path had no `else` arm at all, so a tick/volume or orderflow chart on a
/// feedless venue lost its tape in complete silence. Same cure, same recovery.
#[test]
fn an_unregistered_venue_does_not_permanently_suppress_the_trade_tape() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "100t");
    assert!(env.calls().is_empty(), "no feed to call");
    assert!(env.unroutable.contains("bybit:BTCUSDT@trades"), "the miss is recorded, not silent");
    assert!(env.aggs.contains_key("bybit:BTCUSDT@100t"), "the aggregator is still created");

    // Register the venue: the tape now genuinely starts on the next ensure.
    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.ensure("bybit", "BTCUSDT", "100t");
    assert_eq!(env.calls(), vec!["bybit:trades BTCUSDT".to_string()]);
    assert_eq!(env.subs.get("bybit:BTCUSDT@trades"), Some(&SubscriptionId(301)));
    assert!(env.unroutable.is_empty());

    env.ensure("bybit", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1, "the shared tape is not resubscribed once recovered");
}

// ----- retry_unroutable -------------------------------------------------------------------

/// **A series ensured BEFORE its venue's feed existed is subscribed by `retry_unroutable` alone —
/// no second `ensure_*`.** A chart's primary series is ensured once (window opened, symbol picked,
/// workspace restored), and the feeds are mounted from the datahub's handshake, which lands after a
/// restore asked: without this pass the tape of a restored tick/volume chart would stay missed for
/// the life of the process. Both key shapes ride it — the kline key and the shared trade tape.
#[test]
fn retry_unroutable_subscribes_a_series_whose_feed_arrived_after_the_ensure() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m");
    env.ensure("bybit", "BTCUSDT", "100t");
    assert!(env.unroutable.contains("bybit:BTCUSDT@1m"), "the kline miss is recorded");
    assert!(env.unroutable.contains("bybit:BTCUSDT@trades"), "...and so is the tape's");

    env.retry();
    assert!(env.calls().is_empty(), "still no feed: nothing to call, and nothing is lost");
    assert_eq!(env.unroutable.len(), 2, "the record survives a pass that could not act on it");

    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.retry();

    let mut calls = env.calls();
    calls.sort();
    assert_eq!(
        calls,
        vec!["bybit:bars BTCUSDT 1m".to_string(), "bybit:trades BTCUSDT".to_string()],
        "each missed key is asked exactly once"
    );
    assert!(env.subs.contains_key("bybit:BTCUSDT@1m"), "the kline id is remembered");
    assert!(env.subs.contains_key("bybit:BTCUSDT@trades"), "...and so is the tape's");
    assert!(env.unroutable.is_empty(), "both keys are routable now, so the record clears");

    env.retry();
    assert_eq!(env.calls().len(), 2, "recovery is once, not once per frame");
}

/// The default venue's keys carry no `venue:` prefix (`BTCUSDT@1m`, `BTCUSDT@trades`), and the
/// retry must read that shape back as well as the namespaced one.
#[test]
fn retry_unroutable_reads_the_default_venues_unprefixed_keys() {
    let mut env = Env::with_feeds(&[("okx", 200, &[])]);
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("binance", "BTCUSDT", "100t");
    assert!(env.unroutable.contains("BTCUSDT@1m") && env.unroutable.contains("BTCUSDT@trades"));

    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 100, &[]));
    env.retry();

    let mut calls = env.calls();
    calls.sort();
    assert_eq!(
        calls,
        vec!["binance:bars BTCUSDT 1m".to_string(), "binance:trades BTCUSDT".to_string()]
    );
    assert!(env.unroutable.is_empty());
}

/// **The retry must not resurrect a series the operator DELETED.** A Data-manager Delete frees the
/// key's `spawned` slot but leaves its window, so the reaper's `unroutable` sweep (which asks "does
/// a window still back this key?") keeps the record — and a retry keyed on `unroutable` alone would
/// subscribe the very series that was just deleted, behind a `hidden` flag that does not stop it.
#[test]
fn retry_unroutable_does_not_resurrect_a_deleted_series() {
    let mut env = Env::new();
    env.ensure("bybit", "BTCUSDT", "1m");
    env.stop("bybit:BTCUSDT@1m");
    assert!(!env.spawned.contains("bybit:BTCUSDT@1m"), "the delete freed the slot");
    assert!(
        env.unroutable.contains("bybit:BTCUSDT@1m"),
        "...but not the record, which is exactly what the retry must not trust on its own"
    );

    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.retry();

    assert!(env.calls().is_empty(), "a deleted series was resubscribed");
    assert!(env.subs.is_empty(), "...and its id remembered");
}

/// A venue that STAYS unadvertised costs the per-frame pass a lookup and nothing else: no call, no
/// change to the record (the warn is gated on the insert, so it is not repeated either).
#[test]
fn retry_unroutable_leaves_a_venue_that_is_still_missing_alone() {
    let mut env = Env::new();
    env.ensure("alpaca", "AAPL", "1m");
    for _ in 0..3 {
        env.retry();
    }
    assert!(env.calls().is_empty(), "a venue with no feed was called");
    assert_eq!(env.unroutable.len(), 1, "the record neither grew nor was dropped");
    assert!(env.unroutable.contains("alpaca:AAPL@1m"));
}

/// A `subscribe_bars` FAILURE leaves no `subs` entry (there is no id to stop) but still HOLDS
/// the `spawned` slot — unchanged, because `spawned` doubles as `sync_from_core`'s fold filter
/// (the `--observe` constraint). It is NOT recorded `unroutable` either: the venue is routable,
/// it just said no. What IS now recorded is the [`FeedRetries`] entry, which is the whole point
/// — before it, this exact state was permanent for the life of the process. The chart is still
/// created, so the window renders empty rather than not at all.
#[test]
fn a_failed_bar_subscribe_records_no_subscription_id() {
    let mut env = Env::with_feeds(&[("binance", 100, &["bars"])]);
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls(), vec!["binance:bars BTCUSDT 1m".to_string()]);
    assert!(env.subs.is_empty());
    assert!(env.spawned.contains("BTCUSDT@1m"));
    assert!(env.charts.contains_key("BTCUSDT@1m"));
    assert!(env.unroutable.is_empty(), "a reachable venue is routable even when it says no");
    assert!(
        env.retries.is_pending(&RetryKey::series("BTCUSDT@1m")),
        "the failed attempt is RECORDED — this is what makes it retryable and visible"
    );
}

/// **The regression proof for the failed-subscribe half**, the twin of
/// `an_unregistered_venue_does_not_permanently_suppress_the_subscribe`. A transient
/// `subscribe_bars` error used to hold the `spawned` slot shut forever: the chart was dead for
/// the life of the process behind one `warn!`. Now the failure is recorded, the per-frame
/// callers are throttled by the backoff (so a live socket is not re-dialled ~60×/second), and
/// once the cooldown elapses the retry runs and genuinely succeeds against a healed venue.
#[test]
fn a_transient_bar_subscribe_error_is_retried_and_eventually_succeeds() {
    let mut env = Env::with_feeds(&[]);
    let log = Arc::clone(&env.log);
    let fail = fail_set(&["bars"]);
    env.feeds.insert("binance", FakeFeed::boxed_flaky("binance", &log, 100, &fail));

    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 1, "the first attempt really happened");
    assert!(env.subs.is_empty(), "and really failed");

    // The per-frame callers must NOT re-dial while the cooldown holds. No sleeping needed:
    // the floor is one second and this test runs in microseconds.
    for _ in 0..10 {
        env.ensure("binance", "BTCUSDT", "1m");
    }
    assert_eq!(env.calls().len(), 1, "the backoff throttles the per-frame retry");

    // Cooldown elapsed, venue still broken: exactly ONE more attempt, then quiet again.
    env.elapse();
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 2, "one retry per elapsed cooldown, not a loop");

    // Heal the venue and let the next cooldown elapse: the chart comes back.
    fail.lock().unwrap().clear();
    env.elapse();
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 3);
    assert_eq!(env.subs.get("BTCUSDT@1m"), Some(&SubscriptionId(101)), "genuinely subscribed");
    assert!(env.retries.is_empty(), "the record clears on success");

    // …and having recovered, it is an ordinary spawned key again — not a resubscribe loop.
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 3, "recovery is once, not once per frame");
}

/// The trade-tape twin of the test above: the tape a tick/volume chart and every orderflow
/// overlay on a `(venue, symbol)` share is their ONLY source, so one transient
/// `subscribe_trades` error used to leave all of them permanently empty.
#[test]
fn a_transient_trade_subscribe_error_is_retried_and_eventually_succeeds() {
    let mut env = Env::with_feeds(&[]);
    let log = Arc::clone(&env.log);
    let fail = fail_set(&["trades"]);
    env.feeds.insert("binance", FakeFeed::boxed_flaky("binance", &log, 100, &fail));

    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1);
    assert!(env.subs.is_empty(), "no tape");
    assert!(env.aggs.contains_key("BTCUSDT@100t"), "the aggregator exists, waiting for prints");
    assert!(env.retries.is_pending(&RetryKey::series("BTCUSDT@trades")));

    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.calls().len(), 1, "throttled while the cooldown holds");

    fail.lock().unwrap().clear();
    env.elapse();
    env.ensure("binance", "BTCUSDT", "100t");
    assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)), "the tape starts");
    assert!(env.retries.is_empty());
}

/// The one error kind that must NOT be retried. `LiveDataError::Unsupported` is a DECLARED-
/// capability refusal — `vike_data::require_live_verb` derives it from the static
/// `VenueCaps.live_data` matrix — so waiting cannot make it true and re-asking is pure noise.
/// It is still RECORDED, so the resulting empty chart is visible rather than forgotten: that is
/// the difference between "not retried" and the old "silently burnt".
#[test]
fn a_declared_capability_refusal_is_recorded_and_never_retried() {
    let mut env = Env::with_feeds(&[("binance", 100, &["bars!"])]);
    env.ensure("binance", "BTCUSDT", "1m");
    assert_eq!(env.calls().len(), 1);
    let key = RetryKey::series("BTCUSDT@1m");
    assert!(env.retries.is_pending(&key), "recorded, so the gap stays visible");
    assert!(!env.retries.is_retry_due(&key), "…but never due");

    env.elapse(); // even letting every cooldown lapse changes nothing
    for _ in 0..5 {
        env.ensure("binance", "BTCUSDT", "1m");
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
        vec!["binance:bars BTCUSDT 1m".to_string(), "okx:trades BTC-USDT".to_string()],
        "exactly one socket each"
    );
    assert!(env.unroutable.is_empty() && env.retries.is_empty(), "healthy path records nothing");
}

/// The backoff schedule itself: doubling from one second, capped at the 60s ceiling and flat
/// forever after (there is no attempt limit — a venue down for an hour must still recover on
/// its own), and saturating so an absurd attempt count can neither shift- nor multiply-overflow.
#[test]
fn retry_backoff_doubles_then_flattens_at_the_ceiling() {
    let secs = |n| retry_backoff(n).as_secs();
    assert_eq!([secs(1), secs(2), secs(3), secs(4)], [1, 2, 4, 8]);
    assert_eq!([secs(5), secs(6)], [16, 32]);
    assert_eq!(secs(7), 60, "capped, not 64");
    assert_eq!(secs(100), 60);
    assert_eq!(secs(u32::MAX), 60, "saturating, not a panic");
    assert_eq!(secs(0), 1, "a defensive 0 reads as the first failure");
}

// ----- ensure_trade_feed_on ---------------------------------------------------------------

/// The trade-key convention [`orphaned_trade_feed_keys`] parses back out: bare for
/// [`DEFAULT_VENUE`], `"venue:"`-namespaced otherwise. Idempotent per key.
#[test]
fn trade_feed_keys_follow_the_default_venue_convention() {
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
    assert_eq!(env.subs.get("BTCUSDT@trades"), Some(&SubscriptionId(101)));
    assert_eq!(env.subs.get("okx:BTC-USDT@trades"), Some(&SubscriptionId(201)));
}

// ----- stop_series (the Data-manager "Delete" path) ----------------------------------------

/// **The Data-manager Delete leak, asserted instead of commented.** The GUI shell used to carry
/// its own copy of this teardown and route every unsubscribe to `feeds["binance"]`, under a
/// comment claiming both key kinds were binance-only. For a `series_key`-namespaced row the
/// binance lookup misses, so the id minted by OKX was dropped on the floor while the `spawned`
/// slot was freed anyway — a socket running with nothing left that names it. That comment was
/// the only thing standing where this test now stands, and it was false: the id must be handed
/// back to the venue that MINTED it.
#[test]
fn deleting_a_non_binance_series_unsubscribes_on_its_own_venue() {
    let mut env = Env::new();
    env.ensure("binance", "BTCUSDT", "1m");
    env.ensure("okx", "BTC-USDT", "1m");
    assert_eq!(env.subs["okx:BTC-USDT@1m"], SubscriptionId(201), "minted by the OKX feed");

    env.stop("okx:BTC-USDT@1m");
    assert_eq!(
        env.calls().last().unwrap(),
        "okx:unsub 201",
        "the unsubscribe must reach the venue the key names, not the default one"
    );
    assert!(
        !env.calls().iter().any(|c| c.starts_with("binance:unsub")),
        "the binance feed must never be offered another venue's subscription id"
    );
    assert!(!env.subs.contains_key("okx:BTC-USDT@1m"), "the id is forgotten");
    assert!(!env.spawned.contains("okx:BTC-USDT@1m"), "the spawned slot is freed");
    assert!(!env.charts.contains_key("okx:BTC-USDT@1m"), "the render series is dropped");
    assert!(env.hidden.contains("okx:BTC-USDT@1m"), "and the key is marked hidden");
    assert!(env.spawned.contains("BTCUSDT@1m"), "the untouched binance series survives");
    assert_eq!(env.subs["BTCUSDT@1m"], SubscriptionId(101));
}

/// The other half, and the reason a misroute here is PERMANENT rather than merely late: a
/// Data-manager Delete does not remove the window (it stays with `open = false`), so the key is
/// still in [`live_window_keys`] and [`reap_orphaned_feeds`] never considers it — the stop
/// above is the only chance that stream ever gets. Driven as the real sequence — delete, the
/// per-frame reap, then the operator re-adding the chart — and asserted on the venue's own call
/// log: one subscribe, one unsubscribe, one FRESH subscribe. Under the old hand copy the middle
/// line was absent and the last one stacked a second live stream on a first nothing had
/// stopped.
#[test]
fn a_deleted_non_binance_series_leaves_no_orphan_and_re_adds_as_one_stream() {
    let mut env = Env::new();
    env.ensure("okx", "BTC-USDT", "1m");
    let mut w = chart_win("okx", "BTC-USDT", "1m");
    w.open = false; // exactly what a Data-manager Delete leaves behind

    env.stop("okx:BTC-USDT@1m");
    let after_stop = env.calls();
    env.reap(std::slice::from_ref(&w));
    assert_eq!(
        env.calls(),
        after_stop,
        "the per-frame reaper adds nothing: the key has left `subs`/`spawned` and its window is \
             still live, so a stop that failed to unsubscribe could never be repaired"
    );

    env.ensure("okx", "BTC-USDT", "1m");
    assert_eq!(
        env.calls(),
        vec![
            "okx:bars BTC-USDT 1m".to_string(),
            "okx:unsub 201".to_string(),
            "okx:bars BTC-USDT 1m".to_string(),
        ],
        "one stream at a time on the venue's own socket, never two"
    );
    assert_eq!(env.subs["okx:BTC-USDT@1m"], SubscriptionId(202), "a FRESH subscription id");
    assert!(!env.hidden.contains("okx:BTC-USDT@1m"), "unhidden again, so the fold resumes");
}
