//! The fake lanes of this crate's integration suites: a scripted `DataLane` and a scripted `RestLane`
//! that a test drives from outside, so the SERVER can be tested against its real peer (the client
//! crate's `RemoteDataClient`, `RemoteKlines`, ...) with no venue and no bridge.
//!
//! * [`FakeData`] hands out [`FakeSession`]s: each is a `DataClient` that RECORDS every call and lets the
//!   test emit any sink verb from any thread (`session.sink`), or from INSIDE `subscribe_*` through a
//!   hook (a seed emitted before the call returns), or refuse a symbol.
//! * [`FakeRest`] answers every REST verb from a closure the test sets (a fetch is a closure over the
//!   request and the cancel context), counts what it was asked, and keeps the options-feed sinks so a
//!   test can push rows.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); a file under
//! `tests/support/` is no test target of its own. Every includer uses a different part, so the
//! includer's `mod` declaration carries the one `#[allow(dead_code, clippy::allow_attributes)]`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_gatewayhub::data::{DataLane, Holder};
use vike_gatewayhub::rest::{
    CatalogTraits, FetchCtx, OptionsFeedHandle, OptionsFeedSink, RestFacts, RestLane,
};
use vike_gatewayhub::session::ConnectionScope;
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::{
    BudgetCounter, FetchFailure, FetchRows, FetchWhat, OptionsCall, OptionsFeedSpec, OptionsReply,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A hook run INSIDE a `subscribe_*` call, before it returns: `(sink, verb, symbol)`.
pub type SubscribeHook = Arc<dyn Fn(&dyn LiveDataSink, &str, &str) + Send + Sync>;

/// How a scripted client answers a `subscribe_*` for one symbol.
#[derive(Clone)]
pub enum Refusal {
    /// `LiveDataError::Subscribe(text)`.
    Subscribe(String),
    /// `LiveDataError::Unsupported(text)`.
    Unsupported(&'static str),
}

/// One session a test can see: what the server asked of the venue client, and the sink it was given.
pub struct FakeSession {
    /// The venue the session was opened for.
    pub venue: String,
    /// The holder the server passed.
    pub holder: Holder,
    /// The sink the server's wire sink is behind: call any verb from the test thread.
    pub sink: Arc<dyn LiveDataSink>,
    /// Every call, in order: `subscribe_quotes BTCUSDT`, `unsubscribe 3`, `begin_shutdown`, `shutdown`.
    pub calls: Mutex<Vec<String>>,
    /// How many times `begin_shutdown` ran.
    pub begin_shutdowns: AtomicUsize,
    /// How many times `shutdown` ran.
    pub shutdowns: AtomicUsize,
    refusals: Mutex<HashMap<String, Refusal>>,
    hook: Mutex<Option<SubscribeHook>>,
}

impl FakeSession {
    /// Refuse every `subscribe_*` for `symbol` with `refusal`.
    pub fn refuse(&self, symbol: &str, refusal: Refusal) {
        lock(&self.refusals).insert(symbol.to_string(), refusal);
    }

    /// Run `hook` inside every later `subscribe_*`.
    pub fn on_subscribe(&self, hook: SubscribeHook) {
        *lock(&self.hook) = Some(hook);
    }

    /// The calls so far.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
}

struct FakeClient {
    session: Arc<FakeSession>,
    next_id: u64,
}

impl FakeClient {
    fn sub(&mut self, verb: &str, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        lock(&self.session.calls).push(format!("subscribe_{verb} {symbol}"));
        let refusal = lock(&self.session.refusals).get(symbol).cloned();
        match refusal {
            Some(Refusal::Subscribe(text)) => return Err(LiveDataError::Subscribe(text)),
            Some(Refusal::Unsupported(text)) => return Err(LiveDataError::Unsupported(text)),
            None => {}
        }
        let hook = lock(&self.session.hook).clone();
        if let Some(hook) = hook {
            hook(self.session.sink.as_ref(), verb, symbol);
        }
        self.next_id += 1;
        Ok(SubscriptionId(self.next_id))
    }
}

impl DataClient for FakeClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.sub(&format!("bars[{interval}]"), symbol)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub("quotes", symbol)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub("trades", symbol)
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub("book", symbol)
    }

    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub("depth", symbol)
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        lock(&self.session.calls).push(format!("unsubscribe {}", id.0));
    }

    fn begin_shutdown(&mut self) {
        lock(&self.session.calls).push("begin_shutdown".to_string());
        self.session.begin_shutdowns.fetch_add(1, Ordering::SeqCst);
    }

    fn shutdown(&mut self) {
        lock(&self.session.calls).push("shutdown".to_string());
        self.session.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

/// The scripted data lane.
pub struct FakeData {
    venues: Mutex<Vec<String>>,
    up: AtomicBool,
    sessions: Mutex<Vec<Arc<FakeSession>>>,
    default_hook: Mutex<Option<SubscribeHook>>,
    default_refusals: Mutex<HashMap<String, Refusal>>,
}

impl FakeData {
    /// A lane serving `venues`.
    pub fn new(venues: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            venues: Mutex::new(venues.iter().map(|v| (*v).to_string()).collect()),
            up: AtomicBool::new(true),
            sessions: Mutex::new(Vec::new()),
            default_hook: Mutex::new(None),
            default_refusals: Mutex::new(HashMap::new()),
        })
    }

    /// Make `open` fail (a venue that is down) - the advertisement must not change.
    pub fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }

    /// Run `hook` inside every `subscribe_*` of every session opened from now on.
    pub fn set_default_hook(&self, hook: SubscribeHook) {
        *lock(&self.default_hook) = Some(hook);
    }

    /// Refuse every `subscribe_*` for `symbol` on every session opened from now on.
    pub fn refuse_all(&self, symbol: &str, refusal: Refusal) {
        lock(&self.default_refusals).insert(symbol.to_string(), refusal);
    }

    /// How many sessions have been opened.
    pub fn opened(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// The `n`th session, waiting up to five seconds for it to open.
    pub fn session(&self, n: usize) -> Arc<FakeSession> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(s) = lock(&self.sessions).get(n) {
                return Arc::clone(s);
            }
            assert!(std::time::Instant::now() < deadline, "session {n} never opened");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl DataLane for FakeData {
    fn venues(&self) -> Vec<String> {
        lock(&self.venues).clone()
    }

    fn open(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        if !self.up.load(Ordering::SeqCst) {
            return Err(format!(
                "fake lane: the {venue} client cannot be built (the venue is down)"
            ));
        }
        let session = Arc::new(FakeSession {
            venue: venue.to_string(),
            holder: holder.clone(),
            sink,
            calls: Mutex::new(Vec::new()),
            begin_shutdowns: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            refusals: Mutex::new(lock(&self.default_refusals).clone()),
            hook: Mutex::new(lock(&self.default_hook).clone()),
        });
        lock(&self.sessions).push(Arc::clone(&session));
        Ok(Box::new(FakeClient { session, next_id: 0 }))
    }
}

/// The closure a fetch is answered by.
pub type FetchFn = dyn Fn(&ConnectionScope, &FetchWhat, bool, &FetchCtx) -> Result<FetchRows, FetchFailure>
    + Send
    + Sync;

/// A recorded options feed: the sink the server handed over and whether it was shut down.
pub struct FakeFeed {
    /// Push rows through this.
    pub sink: Arc<dyn OptionsFeedSink>,
    /// The spec the feed was opened with.
    pub spec: OptionsFeedSpec,
    /// Set by the handle's `shutdown`.
    pub shut_down: Arc<AtomicBool>,
}

struct FakeFeedHandle(Arc<AtomicBool>);

impl OptionsFeedHandle for FakeFeedHandle {
    fn shutdown(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The scripted REST lane.
pub struct FakeRest {
    facts: Mutex<RestFacts>,
    fetch: Mutex<Arc<FetchFn>>,
    /// Every fetch the lane was asked, as `lane credentialed?`.
    pub fetches: Mutex<Vec<String>>,
    presence: Mutex<HashMap<String, CredentialPresence>>,
    families: Mutex<HashMap<String, Result<Vec<String>, String>>>,
    options: Mutex<Option<Result<OptionsReply, String>>>,
    /// Every options REST call the lane was asked.
    pub options_calls: Mutex<Vec<OptionsCall>>,
    describe: Mutex<Option<Result<CatalogTraits, String>>>,
    /// Every options feed opened.
    pub feeds: Mutex<Vec<Arc<FakeFeed>>>,
    feed_error: Mutex<Option<String>>,
    budget: Mutex<Vec<BudgetCounter>>,
}

impl FakeRest {
    /// A lane serving `facts`; every fetch fails until a test sets one.
    pub fn new(facts: RestFacts) -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(facts),
            fetch: Mutex::new(Arc::new(
                |_: &ConnectionScope, what: &FetchWhat, _: bool, _: &FetchCtx| {
                    Err(FetchFailure::Fetch(format!(
                        "fake lane: no script for a {} fetch",
                        what.lane()
                    )))
                },
            )),
            fetches: Mutex::new(Vec::new()),
            presence: Mutex::new(HashMap::new()),
            families: Mutex::new(HashMap::new()),
            options: Mutex::new(None),
            options_calls: Mutex::new(Vec::new()),
            describe: Mutex::new(None),
            feeds: Mutex::new(Vec::new()),
            feed_error: Mutex::new(None),
            budget: Mutex::new(Vec::new()),
        })
    }

    /// Script every fetch.
    pub fn set_fetch(
        &self,
        f: impl Fn(&ConnectionScope, &FetchWhat, bool, &FetchCtx) -> Result<FetchRows, FetchFailure>
        + Send
        + Sync
        + 'static,
    ) {
        *lock(&self.fetch) = Arc::new(f);
    }

    /// Script a presence word.
    pub fn set_presence(&self, venue: &str, word: CredentialPresence) {
        lock(&self.presence).insert(venue.to_string(), word);
    }

    /// Script a family resolve.
    pub fn set_family(&self, family: &str, answer: Result<Vec<String>, String>) {
        lock(&self.families).insert(family.to_string(), answer);
    }

    /// Script the options REST answer.
    pub fn set_options(&self, answer: Result<OptionsReply, String>) {
        *lock(&self.options) = Some(answer);
    }

    /// Script the catalog describe answer.
    pub fn set_describe(&self, answer: Result<CatalogTraits, String>) {
        *lock(&self.describe) = Some(answer);
    }

    /// Make the next options feed open fail.
    pub fn fail_feed(&self, text: &str) {
        *lock(&self.feed_error) = Some(text.to_string());
    }

    /// Script the budget rows the status reports.
    pub fn set_budget(&self, rows: Vec<BudgetCounter>) {
        *lock(&self.budget) = rows;
    }

    /// The `n`th options feed, waiting up to five seconds for it.
    pub fn feed(&self, n: usize) -> Arc<FakeFeed> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(f) = lock(&self.feeds).get(n) {
                return Arc::clone(f);
            }
            assert!(std::time::Instant::now() < deadline, "options feed {n} never opened");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl RestLane for FakeRest {
    fn facts(&self) -> RestFacts {
        lock(&self.facts).clone()
    }

    fn fetch(
        &self,
        scope: &ConnectionScope,
        what: &FetchWhat,
        credentialed: bool,
        ctx: &FetchCtx,
    ) -> Result<FetchRows, FetchFailure> {
        lock(&self.fetches).push(format!(
            "{}{}",
            what.lane(),
            if credentialed { " (credentialed)" } else { "" }
        ));
        let script = Arc::clone(&lock(&self.fetch));
        script(scope, what, credentialed, ctx)
    }

    fn catalog_describe(
        &self,
        _scope: &ConnectionScope,
        venue: &str,
    ) -> Result<CatalogTraits, String> {
        lock(&self.describe)
            .clone()
            .unwrap_or_else(|| Err(format!("fake lane: no describe script for {venue}")))
    }

    fn options(
        &self,
        _scope: &ConnectionScope,
        call: &OptionsCall,
    ) -> Result<OptionsReply, String> {
        lock(&self.options_calls).push(call.clone());
        lock(&self.options)
            .clone()
            .unwrap_or_else(|| Err("fake lane: no options script".to_string()))
    }

    fn open_options_feed(
        &self,
        _scope: &ConnectionScope,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String> {
        if let Some(text) = lock(&self.feed_error).take() {
            return Err(text);
        }
        let shut_down = Arc::new(AtomicBool::new(false));
        lock(&self.feeds).push(Arc::new(FakeFeed {
            sink,
            spec: spec.clone(),
            shut_down: Arc::clone(&shut_down),
        }));
        Ok(Box::new(FakeFeedHandle(shut_down)))
    }

    fn resolve_family(
        &self,
        _scope: &ConnectionScope,
        _venue: &str,
        family: &str,
        _now_ms: i64,
    ) -> Result<Vec<String>, String> {
        lock(&self.families)
            .get(family)
            .cloned()
            .unwrap_or_else(|| Err(format!("fake lane: no family script for {family}")))
    }

    fn credential_presence(&self, venue: &str) -> CredentialPresence {
        lock(&self.presence).get(venue).copied().unwrap_or(CredentialPresence::NotNeeded)
    }

    fn budget_counters(&self) -> Vec<BudgetCounter> {
        lock(&self.budget).clone()
    }
}
