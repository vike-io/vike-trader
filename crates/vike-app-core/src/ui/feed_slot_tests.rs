use super::*;
use std::sync::{Arc, Mutex};
use vike_chart::{DisplayTz, model};
use vike_data::{LiveDataError, SubscriptionId};
use vike_orderflow::{bar_agg, tickvol};

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

    /// One pass of [`retry_unroutable`] — the per-frame call the shell makes after
    /// `MdSession::sync_mount`.
    fn retry(&mut self) {
        let (mut f, _s) = self.both();
        retry_unroutable(&mut f);
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
    let mut w = workspace::state::chart_window(symbol, interval);
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

#[path = "feed_slot_tests/ensure.rs"]
#[cfg(test)]
mod ensure;

#[path = "feed_slot_tests/reap.rs"]
#[cfg(test)]
mod reap;

#[path = "feed_slot_tests/depth_poly.rs"]
#[cfg(test)]
mod depth_poly;

#[path = "feed_slot_tests/cockpit_streams.rs"]
#[cfg(test)]
mod cockpit_streams;
