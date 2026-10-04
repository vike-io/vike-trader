use super::*;
use std::sync::{Arc, Mutex};

/// Shared call log: one `"venue:verb arg…"` line per `DataClient` call, in order.
type Log = Arc<Mutex<Vec<String>>>;

/// The verbs a [`FakeFeed`] refuses, held behind a shared handle so a test can HEAL the venue
/// mid-run and prove a retry genuinely reaches a working socket (the property the whole
/// [`FeedRetries`] change exists for).
///
/// Two spellings, because [`FeedRetries`] treats them differently and a double that could only
/// produce one of them could not test the other: a bare verb (`"bars"`) answers the RETRYABLE
/// [`LiveDataError::Subscribe`] — the "OS refused a feed thread / socket is down" kind — while
/// a trailing `!` (`"bars!"`) answers the permanent, declared-capability
/// [`LiveDataError::Unsupported`] that `vike_data::require_live_verb` produces from the static
/// `VenueCaps` matrix.
type FailSet = Arc<Mutex<HashSet<String>>>;

fn fail_set(verbs: &[&str]) -> FailSet {
    Arc::new(Mutex::new(verbs.iter().map(|v| v.to_string()).collect()))
}

/// A recording [`DataClient`] double. Hands out ids from a per-venue `base` so a logged
/// `unsub` line names which venue's subscription was actually stopped, and can be told to
/// fail a named verb (`fail`) to exercise the error arms.
struct FakeFeed {
    venue: &'static str,
    log: Log,
    next: u64,
    fail: FailSet,
}

impl FakeFeed {
    fn boxed(
        venue: &'static str,
        log: &Log,
        base: u64,
        fail: &[&str],
    ) -> Box<dyn DataClient + Send> {
        Box::new(FakeFeed { venue, log: Arc::clone(log), next: base, fail: fail_set(fail) })
    }

    /// Same double, but sharing a [`FailSet`] the test keeps a handle to — so the venue can be
    /// healed between two `ensure_*` calls.
    fn boxed_flaky(
        venue: &'static str,
        log: &Log,
        base: u64,
        fail: &FailSet,
    ) -> Box<dyn DataClient + Send> {
        Box::new(FakeFeed { venue, log: Arc::clone(log), next: base, fail: Arc::clone(fail) })
    }

    fn issue(&mut self, verb: &str, args: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.lock().unwrap().push(format!("{}:{verb} {args}", self.venue));
        let (refused, failing) = {
            let f = self.fail.lock().unwrap();
            (f.contains(&format!("{verb}!")), f.contains(verb))
        };
        if refused {
            return Err(LiveDataError::Unsupported("fake feed: verb declared unsupported"));
        }
        if failing {
            return Err(LiveDataError::Subscribe(format!(
                "fake feed: {verb} disabled for this test"
            )));
        }
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for FakeFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.issue("bars", &format!("{symbol} {interval}"))
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue("quotes", symbol)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue("trades", symbol)
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue("book", symbol)
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.issue("depth", symbol)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("{}:unsub {}", self.venue, id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push(format!("{}:shutdown", self.venue));
    }
}

/// The `App` fields these functions borrow, owned in one place so a test can build both
/// bundles from disjoint borrows (which is exactly how `vike-desktop`'s thin wrappers do it).
struct Env {
    feeds: FeedMap,
    subs: HashMap<String, SubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    retries: FeedRetries,
    charts: HashMap<String, model::ChartState>,
    hidden: HashSet<String>,
    aggs: HashMap<String, (String, String, tickvol::TickVolAgg)>,
    of_aggs: HashMap<String, (String, String, bar_agg::OrderflowAgg)>,
    trade_depth: HashMap<(String, String), TradeDepthSubs>,
    poly_subs: HashMap<String, PolyBookSubs>,
    log: Log,
}

impl Env {
    /// Binance (ids 100+) and OKX (ids 200+) feeds registered; nothing subscribed yet.
    fn new() -> Env {
        Env::with_feeds(&[("binance", 100, &[]), ("okx", 200, &[])])
    }

    fn with_feeds(venues: &[(&'static str, u64, &'static [&'static str])]) -> Env {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mut feeds: FeedMap = HashMap::new();
        for &(venue, base, fail) in venues {
            feeds.insert(venue, FakeFeed::boxed(venue, &log, base, fail));
        }
        Env {
            feeds,
            subs: HashMap::new(),
            spawned: HashSet::new(),
            unroutable: HashSet::new(),
            retries: FeedRetries::default(),
            charts: HashMap::new(),
            hidden: HashSet::new(),
            aggs: HashMap::new(),
            of_aggs: HashMap::new(),
            trade_depth: HashMap::new(),
            poly_subs: HashMap::new(),
            log,
        }
    }

    fn both(&mut self) -> (FeedSlots<'_>, SeriesSlots<'_>) {
        (
            FeedSlots {
                feeds: &mut self.feeds,
                subs: &mut self.subs,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                retries: &mut self.retries,
            },
            SeriesSlots {
                charts: &mut self.charts,
                hidden: &mut self.hidden,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
            },
        )
    }

    fn ensure(&mut self, venue: &str, symbol: &str, interval: &str) {
        self.ensure_at(venue, symbol, interval, DisplayTz::Utc, false);
    }

    fn ensure_at(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        tz: DisplayTz,
        shutting_down: bool,
    ) {
        let (mut f, mut s) = self.both();
        let spec = SeriesSpec { venue, symbol, interval, asset_class: None };
        ensure_feed_on(&mut f, &mut s, spec, tz, shutting_down);
    }

    fn reap(&mut self, wins: &[workspace::WinState]) {
        let (mut f, mut s) = self.both();
        reap_orphaned_feeds(&mut f, &mut s, wins);
    }

    /// One Data-manager "Delete" on `key` — the same call `vike-desktop`'s `to_stop` loop makes,
    /// built from the same two bundles that loop builds.
    fn stop(&mut self, key: &str) {
        let (mut f, mut s) = self.both();
        stop_series(&mut f, &mut s, key);
    }

    fn depth(&mut self, venue: &str, inst: &str) {
        ensure_depth(&mut self.feeds, &mut self.trade_depth, &mut self.retries, venue, inst);
    }

    fn poly(&mut self, token: &str) {
        ensure_poly_book(&mut self.feeds, &mut self.poly_subs, &mut self.retries, token);
    }

    fn reap_streams(&mut self, wins: &[workspace::WinState]) {
        reap_orphaned_trade_cockpit_streams(
            &mut self.feeds,
            &mut self.trade_depth,
            &mut self.poly_subs,
            &mut self.retries,
            wins,
        );
    }

    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Simulate the backoff having elapsed, so a retry test never has to sleep for real
    /// seconds. See [`FeedRetries::expire_all`].
    fn elapse(&mut self) {
        self.retries.expire_all();
    }
}

fn chart_win(venue: &str, symbol: &str, interval: &str) -> workspace::WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    let mut w = workspace::WinState::new("t", symbol, interval, workspace::WinKind::Chart, r);
    w.venue = venue.to_string();
    w
}

fn trade_win(venue: &str, symbol: &str) -> workspace::WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
    let mut w = workspace::WinState::tool("t", workspace::WinKind::Trade, r);
    w.venue = venue.to_string();
    w.symbol = symbol.to_string();
    w
}

fn poly_win(token: &str) -> workspace::WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
    let mut w = workspace::WinState::tool("p", workspace::WinKind::Polymarket, r);
    w.symbol = token.to_string();
    w
}

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

// ----- ensure_depth -----------------------------------------------------------------------

/// Binance: depth only. Idempotent via `trade_depth`, so the per-frame call never opens a second
/// stream.
#[test]
fn ensure_depth_on_binance_subscribes_depth_only_and_dedups() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    env.depth("binance", "BTCUSDT");
    assert_eq!(env.calls(), vec!["binance:depth BTCUSDT".to_string()]);
    let sub = &env.trade_depth[&("binance".to_string(), "BTCUSDT".to_string())];
    assert_eq!(sub.depth, SubscriptionId(101), "the minted id is KEPT (not dropped on the floor)");
    assert!(env.retries.is_empty(), "the healthy path records nothing");
}

/// M-4 of the Trade window's final review (Ruling R6): a Trade window opens NO bar feed, on any
/// venue. The DOM's non-Binance `"1m"` leg ("so that venue's PAPER engine fills") fed an engine the
/// desktop no longer runs, the datahub feed refuses the bar lane, and each instrument a window
/// opened logged one warn for it. On OKX's dashed SWAP-perp inst — the venue-native symbol the
/// Trade window trades, so nothing is translated — the depth stream is the only subscribe.
#[test]
fn ensure_depth_opens_no_bar_feed_on_any_venue() {
    let mut env = Env::new();
    env.depth("okx", "BTC-USDT-SWAP");
    env.depth("okx", "BTC-USDT-SWAP");
    assert_eq!(env.calls(), vec!["okx:depth BTC-USDT-SWAP".to_string()]);
    let sub = &env.trade_depth[&("okx".to_string(), "BTC-USDT-SWAP".to_string())];
    assert_eq!(sub.depth, SubscriptionId(201), "the entry keeps what teardown needs");
    assert!(env.retries.is_empty());
}

/// A FAILED depth subscribe still records no `trade_depth` entry — that is what keeps it
/// retryable at all — but the retry is now THROTTLED rather than fired on the very next frame:
/// `ensure_depth` is a per-frame call, so the old shape re-dialled a live venue socket (and
/// wrote a `warn!`) ~60×/second for as long as the Trade window stayed open.
#[test]
fn a_failed_depth_subscribe_is_retried_after_its_cooldown_not_next_frame() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"])]);
    env.depth("binance", "BTCUSDT");
    assert!(env.trade_depth.is_empty(), "nothing recorded, so it stays retryable");
    for _ in 0..10 {
        env.depth("binance", "BTCUSDT");
    }
    assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");

    env.elapse();
    env.depth("binance", "BTCUSDT");
    assert_eq!(env.calls().len(), 2, "the retry really happens, one per elapsed cooldown");
}

/// An unregistered venue is logged and skipped — never a panic, never a `trade_depth` entry.
/// The miss is now RECORDED instead of re-warned on every frame (the same warn-once treatment
/// `unroutable` gives the series lane), and stays immediately retryable because re-checking a
/// hashmap costs nothing.
#[test]
fn a_missing_depth_feed_is_recorded_once_and_stays_immediately_retryable() {
    let mut env = Env::with_feeds(&[]);
    env.depth("bybit", "BTCUSDT");
    assert!(env.calls().is_empty() && env.trade_depth.is_empty());
    let key = RetryKey::trade_depth("bybit", "BTCUSDT");
    assert!(env.retries.is_pending(&key), "the miss is recorded, so the warn is once");
    assert!(env.retries.is_retry_due(&key), "…and free to re-check every frame");

    // Register the venue: the very next call subscribes, with no cooldown to wait out.
    let log = Arc::clone(&env.log);
    env.feeds.insert("bybit", FakeFeed::boxed("bybit", &log, 300, &[]));
    env.depth("bybit", "BTCUSDT");
    assert_eq!(env.calls()[0], "bybit:depth BTCUSDT");
    assert!(env.trade_depth.contains_key(&("bybit".to_string(), "BTCUSDT".to_string())));
}

// ----- ensure_poly_book -------------------------------------------------------------------

/// The happy path: book + trades on the polymarket feed, token recorded, then deduped.
#[test]
fn ensure_poly_book_subscribes_book_and_trades_once() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1");
    env.poly("tok-1");
    assert_eq!(
        env.calls(),
        vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()]
    );
    assert!(env.poly_subs.contains_key("tok-1"));
    assert_eq!(
        (env.poly_subs["tok-1"].book, env.poly_subs["tok-1"].trades),
        (SubscriptionId(401), Some(SubscriptionId(402))),
        "both minted ids are KEPT for teardown"
    );
    assert!(env.retries.is_empty(), "the healthy path records nothing");
}

/// An empty or placeholder token is skipped BEFORE any feed lookup — a cockpit window whose
/// Gamma resolve has not landed yet must never subscribe the literal placeholder id.
#[test]
fn an_empty_or_placeholder_token_never_subscribes() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("");
    env.poly(POLY_PLACEHOLDER_TOKEN);
    assert!(env.calls().is_empty() && env.poly_subs.is_empty());
    assert!(env.retries.is_empty(), "a skipped token is not a failure");
}

/// A failed BOOK subscribe records no `poly_subs` entry (so it stays retryable), but is now
/// throttled rather than re-dialled on every frame.
#[test]
fn a_failed_poly_book_subscribe_is_retried_after_its_cooldown() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["book"])]);
    env.poly("tok-1");
    assert!(env.poly_subs.is_empty(), "book failed -> still retryable");
    for _ in 0..10 {
        env.poly("tok-1");
    }
    assert_eq!(env.calls().len(), 1, "no ~60 Hz re-dial while the cooldown holds");
    env.elapse();
    env.poly("tok-1");
    assert_eq!(env.calls().len(), 2, "the retry really happens");
}

/// **The `poly_subs` burn.** A failed TRADES subscribe still records the token — the book is
/// live and the ladder paints, so re-running `subscribe_book` every frame would be the other
/// bug — but that record used to suppress every later call, leaving the token's trade tape
/// permanently absent behind one `warn!`. The trade leg now carries its own record and is
/// re-attempted ALONE, without touching the live book.
#[test]
fn a_failed_poly_trades_leg_is_retried_without_resubscribing_the_live_book() {
    let mut env = Env::with_feeds(&[]);
    let log = Arc::clone(&env.log);
    let fail = fail_set(&["trades"]);
    env.feeds.insert("polymarket", FakeFeed::boxed_flaky("polymarket", &log, 400, &fail));

    env.poly("tok-1");
    assert!(env.poly_subs.contains_key("tok-1"), "book is live, so the token is recorded");
    let trades_key = RetryKey::poly_trades("tok-1");
    assert!(env.retries.is_pending(&trades_key), "the missing tape is recorded, not dropped");

    for _ in 0..10 {
        env.poly("tok-1");
    }
    assert_eq!(env.calls().len(), 2, "throttled, and the live book is left alone");

    fail.lock().unwrap().clear();
    env.elapse();
    env.poly("tok-1");
    assert_eq!(
        env.calls(),
        vec![
            "polymarket:book tok-1".to_string(),
            "polymarket:trades tok-1".to_string(),
            "polymarket:trades tok-1".to_string(),
        ],
        "ONLY the trade leg is re-attempted — the book is never re-subscribed"
    );
    assert!(env.retries.is_empty(), "recovered: prints resume");
    assert_eq!(
        env.poly_subs["tok-1"].trades,
        Some(SubscriptionId(402)),
        "the recovered leg's id lands on the entry, so teardown can stop it too"
    );

    env.poly("tok-1");
    assert_eq!(env.calls().len(), 3, "and it is once, not once per frame");
}

/// No polymarket feed registered (the default/CI build, where the bridge is feature-gated
/// away): logged and skipped, never a panic — and, like the DOM twin, recorded so the warn is
/// once per token rather than once per frame.
#[test]
fn ensure_poly_book_without_a_polymarket_feed_is_a_no_op() {
    let mut env = Env::new();
    env.poly("tok-1");
    assert!(env.calls().is_empty() && env.poly_subs.is_empty());
    let key = RetryKey::poly_book("tok-1");
    assert!(env.retries.is_pending(&key) && env.retries.is_retry_due(&key));
}

// ----- reap_orphaned_trade_cockpit_streams --------------------------------------------------

/// **The B1-flagged leak, closed.** Deleting the last Trade window on a book stops exactly
/// that book's stream — its depth leg, unsubscribed on the entry's OWN venue — while another book's
/// live stream is untouched.
#[test]
fn deleting_a_trade_window_unsubscribes_exactly_its_depth_streams() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // depth id 101
    env.depth("okx", "ETH-USDT-SWAP"); // depth id 201
    assert_eq!(env.calls().len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT")]); // the ETH-USDT-SWAP window is gone
    assert_eq!(
        env.calls()[2..].to_vec(),
        vec!["okx:unsub 201".to_string()],
        "the ETH-USDT-SWAP stream stopped on OKX; the live BTCUSDT stream is untouched"
    );
    assert!(env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
    assert!(!env.trade_depth.contains_key(&("okx".to_string(), "ETH-USDT-SWAP".to_string())));
}

/// A merely-closed (`open = false`) Trade window keeps its streams — the same rule the kline
/// reaper applies ([`orphaned_feed_keys`]'s "regardless of `open`/`minimized`" note): close
/// hides the window off-desktop and the rail can unhide it; only DELETION tears down.
#[test]
fn a_closed_trade_window_keeps_its_depth_stream() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    let mut w = trade_win("binance", "BTCUSDT");
    w.open = false;
    env.reap_streams(std::slice::from_ref(&w));
    assert_eq!(env.calls().len(), 1, "no unsubscribe");
    assert!(env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
}

/// A Trade window names its venue in its `WinState`, so the reaper is venue-SPECIFIC: moving the
/// window to another venue releases the old venue's stream on the next reap. (The DOM's reaper was
/// venue-BLIND because the DOM kept its venue in its tool view, where `wins` could not see it.)
#[test]
fn a_trade_window_that_moves_venue_releases_the_old_venues_stream() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // depth id 101
    env.depth("okx", "BTC-USDT-SWAP"); // depth id 201
    env.reap_streams(&[trade_win("okx", "BTC-USDT-SWAP")]);
    assert_eq!(
        env.calls()[2..].to_vec(),
        vec!["binance:unsub 101".to_string()],
        "the window moved to OKX, so the Binance book is released"
    );
    assert_eq!(env.trade_depth.len(), 1);
    assert!(env.trade_depth.contains_key(&("okx".to_string(), "BTC-USDT-SWAP".to_string())));
}

/// **The reaper matches `(venue, native symbol)` TOGETHER, and counts EVERY Trade window.**
///
/// The test above cannot tell a reaper that matches the whole key from one that matches the symbol
/// alone (the DOM's old matching): its two books have different native symbols. Binance, Bybit and
/// Aster all trade `BTCUSDT`, so here the SAME native symbol lives on two venues. Two Trade
/// windows, one per venue, keep BOTH streams — and a reaper that counted only the first (or only
/// the last) Trade window would release one of them. With only the Bybit window left, exactly
/// Binance's stream goes, even though Bybit's window names the same symbol.
#[test]
fn the_reaper_matches_venue_and_symbol_together_and_counts_every_trade_window() {
    let mut env = Env::with_feeds(&[("binance", 100, &[]), ("bybit", 300, &[])]);
    env.depth("binance", "BTCUSDT"); // depth id 101
    env.depth("bybit", "BTCUSDT"); // depth id 301
    assert_eq!(env.calls().len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), trade_win("bybit", "BTCUSDT")]);
    assert_eq!(env.calls().len(), 2, "two windows, two books: nothing is released");
    assert_eq!(env.trade_depth.len(), 2);

    env.reap_streams(&[trade_win("bybit", "BTCUSDT")]);
    assert_eq!(
        env.calls()[2..].to_vec(),
        vec!["binance:unsub 101".to_string()],
        "the Binance window is gone; the Bybit window names the SAME symbol but not the same book"
    );
    assert!(!env.trade_depth.contains_key(&("binance".to_string(), "BTCUSDT".to_string())));
    assert!(env.trade_depth.contains_key(&("bybit".to_string(), "BTCUSDT".to_string())));
}

/// Teardown then re-open genuinely restarts the stream — a FRESH subscription id, the DOM
/// twin of `teardown_then_re_add_genuinely_restarts_the_feed`. The idempotency entry must
/// have been freed, or a re-opened Trade window would paint a book nothing feeds.
#[test]
fn teardown_then_reopen_genuinely_restarts_the_depth_stream() {
    let mut env = Env::new();
    let key = ("binance".to_string(), "BTCUSDT".to_string());
    env.depth("binance", "BTCUSDT");
    assert_eq!(env.trade_depth[&key].depth, SubscriptionId(101));

    env.reap_streams(&[]);
    assert!(env.trade_depth.is_empty());

    env.depth("binance", "BTCUSDT");
    env.depth("binance", "BTCUSDT"); // still idempotent after the round trip
    assert_eq!(
        env.calls(),
        vec![
            "binance:depth BTCUSDT".to_string(),
            "binance:unsub 101".to_string(),
            "binance:depth BTCUSDT".to_string(),
        ],
        "a real second subscribe — and exactly one"
    );
    assert_eq!(env.trade_depth[&key].depth, SubscriptionId(102), "a FRESH id");
}

/// Teardown with the venue's client gone (a de-registered venue; every `--observe` session)
/// is tolerated: the entry is dropped with no unsubscribe and no panic, and a later re-reap
/// finds nothing left to stop — an id is never unsubscribed twice.
#[test]
fn teardown_without_the_venues_client_is_tolerated_and_never_double_unsubscribes() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT");
    env.poly("tok-1"); // no polymarket feed registered -> a retry record, no entry

    env.feeds.remove("binance");
    env.reap_streams(&[]);
    assert!(env.trade_depth.is_empty() && env.poly_subs.is_empty());
    assert!(!env.calls().iter().any(|c| c.contains("unsub")), "no client, no unsubscribe");

    // Re-register the venue and re-reap: the torn-down entry stays down.
    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
    env.reap_streams(&[]);
    assert!(!env.calls().iter().any(|c| c.contains("unsub")), "nothing left to stop");
}

/// An unsubscribe for an id the CLIENT no longer knows is tolerated — the venue client was
/// replaced mid-session (a reconnect), so the teardown's id was never issued by the client
/// that receives it. [`DataClient::unsubscribe`]'s contract pins unknown ids as a no-op
/// ("never panics"); this proves the teardown path leans on exactly that and nothing more.
#[test]
fn an_unsubscribe_for_an_id_the_client_no_longer_knows_is_tolerated() {
    let mut env = Env::new();
    env.depth("binance", "BTCUSDT"); // id 101, minted by the ORIGINAL client
    let log = Arc::clone(&env.log);
    env.feeds.insert("binance", FakeFeed::boxed("binance", &log, 500, &[]));
    env.reap_streams(&[]); // must not panic
    assert_eq!(env.calls().last().unwrap(), "binance:unsub 101");
    assert!(env.trade_depth.is_empty());
}

/// The cockpit twin of the DOM teardown: deleting the last window on a token stops its book
/// AND trade streams on the polymarket feed, another token's cockpit is untouched, and a
/// re-opened window genuinely resubscribes with fresh ids.
#[test]
fn deleting_a_cockpit_window_unsubscribes_its_book_and_trade_streams() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1"); // book 401 + trades 402
    env.poly("tok-2"); // book 403 + trades 404

    env.reap_streams(&[poly_win("tok-1")]); // tok-2's window is gone
    assert_eq!(
        env.calls()[4..].to_vec(),
        vec!["polymarket:unsub 403".to_string(), "polymarket:unsub 404".to_string()],
        "both tok-2 legs stopped; the live tok-1 streams are untouched"
    );
    assert!(env.poly_subs.contains_key("tok-1") && !env.poly_subs.contains_key("tok-2"));

    env.reap_streams(&[]);
    assert!(env.poly_subs.is_empty());

    env.poly("tok-1");
    assert_eq!(
        env.calls()[8..].to_vec(),
        vec!["polymarket:book tok-1".to_string(), "polymarket:trades tok-1".to_string()],
        "a re-opened cockpit genuinely resubscribes"
    );
    assert_eq!(env.poly_subs["tok-1"].book, SubscriptionId(405), "a FRESH id");
}

/// A still-resolving cockpit window (placeholder token) holds nothing alive: it never
/// subscribed anything ([`ensure_poly_book`] refuses the placeholder), so it cannot pin
/// another token's stream either.
#[test]
fn a_placeholder_cockpit_window_pins_no_stream() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &[])]);
    env.poly("tok-1");
    env.reap_streams(&[poly_win(POLY_PLACEHOLDER_TOKEN)]);
    assert!(env.poly_subs.is_empty(), "tok-1's window is gone; the placeholder is not it");
}

/// The lane sweep — [`reap_orphaned_feeds`]'s step (2e), applied to the depth/cockpit lanes
/// this teardown owns: a torn-down entry's failing SECOND-leg record (the cockpit's trade tape)
/// goes with it (the retry arm lives behind the entry, so nothing could ever re-attempt it), while
/// a live window's record stays (the venue is still saying no, so the report is still true).
#[test]
fn the_stream_reaper_sweeps_the_trade_depth_and_cockpit_retry_lanes() {
    let mut env = Env::with_feeds(&[("polymarket", 400, &["trades"])]);
    env.poly("tok-1"); // book live, trade leg failing
    let trades_key = RetryKey::poly_trades("tok-1");
    env.reap_streams(&[poly_win("tok-1")]);
    assert!(env.retries.is_pending(&trades_key), "window live: the record is still true");

    env.reap_streams(&[]);
    assert!(!env.retries.is_pending(&trades_key), "torn down: nothing can re-attempt the leg");
    assert!(env.retries.is_empty());
}

/// The entry-LESS half of that sweep: a failing depth/book subscribe records no entry (that
/// is what keeps it retryable), so when its window is deleted only the retry record remains —
/// with no per-frame `ensure_*` caller left, it could only sit idle forever. It is swept;
/// a still-open window's record survives.
#[test]
fn the_stream_reaper_sweeps_failing_subscribes_whose_window_is_gone() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"]), ("polymarket", 400, &["book"])]);
    env.depth("binance", "BTCUSDT");
    env.poly("tok-1");
    assert_eq!(env.retries.len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), poly_win("tok-1")]);
    assert_eq!(env.retries.len(), 2, "both windows live: both records stay");

    env.reap_streams(&[trade_win("binance", "BTCUSDT")]);
    assert!(env.retries.is_pending(&RetryKey::trade_depth("binance", "BTCUSDT")));
    assert!(!env.retries.is_pending(&RetryKey::poly_book("tok-1")), "cockpit gone");

    env.reap_streams(&[]);
    assert!(env.retries.is_empty());
}

/// The retry-lane twin of the reaper test above that matches venue and symbol together.
/// A failing depth subscribe records no entry, only a `RetryLane::TradeDepth` record keyed
/// `"{venue}:{inst}"`, and the lane sweep judges THAT against the Trade windows' `(venue, symbol)`
/// pairs too: with Binance and Bybit both failing on `BTCUSDT`, deleting the Binance window drops
/// Binance's record and leaves Bybit's, whose window names the same symbol.
#[test]
fn the_stream_reaper_sweeps_a_failing_depth_record_by_venue_and_symbol_together() {
    let mut env = Env::with_feeds(&[("binance", 100, &["depth"]), ("bybit", 300, &["depth"])]);
    env.depth("binance", "BTCUSDT");
    env.depth("bybit", "BTCUSDT");
    assert!(env.trade_depth.is_empty(), "both subscribes failed: records only, no entries");
    assert_eq!(env.retries.len(), 2);

    env.reap_streams(&[trade_win("binance", "BTCUSDT"), trade_win("bybit", "BTCUSDT")]);
    assert_eq!(env.retries.len(), 2, "two windows live: both records stay");

    env.reap_streams(&[trade_win("bybit", "BTCUSDT")]);
    assert!(env.retries.is_pending(&RetryKey::trade_depth("bybit", "BTCUSDT")));
    assert!(
        !env.retries.is_pending(&RetryKey::trade_depth("binance", "BTCUSDT")),
        "the Binance window is gone, so its record goes — whatever symbol Bybit's window names"
    );
    assert_eq!(env.retries.len(), 1);
}
