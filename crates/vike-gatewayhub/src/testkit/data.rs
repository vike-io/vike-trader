//! The scripted DATA lane: a `DataLane` whose venue clients are `DataClient`s a test drives.
//!
//! [`ScriptedData`] serves the venues it was built with. Every `open` hands back a [`ScriptedClient`]
//! that DOES nothing on its own: it records each call the server makes of it (`subscribe_*`,
//! `unsubscribe`, `begin_shutdown`, `shutdown`), answers each `subscribe_*` with a fresh id (or a
//! scripted refusal), and keeps the `LiveDataSink` the server gave it where the test can reach it, as
//! [`ScriptedSession`]. The test plays any sink verb from any thread (`ScriptedSession::emit`), or
//! hooks a call INSIDE `subscribe_*` (a seed a venue emits before the call returns).
//!
//! # What it keeps true to the real thing
//!
//! * `venues()` is fixed at construction and independent of `set_up`: a venue that is down is an
//!   `open` that fails, never a missing advertisement (the lane contract in `crate::data`).
//! * A refused subscription is recorded before it is refused, and a hook runs after the refusal check
//!   and before the id is assigned, as a venue client that emits a seed does.
//! * Ids count from 1 per client, and `unsubscribe` of an id nobody issued is recorded and otherwise
//!   a no-op, as the contract says.
//!
//! # Outside a gatewayhub
//!
//! [`ScriptedData::build`] builds a client with no server in the picture. A twin test puts the SAME
//! scripted client under the datahub's in-process broker (its client table is a closure over this
//! method) and behind the hop, then plays one script into both.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SinkCall, SubscriptionId};

use crate::data::{DataLane, Holder};

use super::events;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Which `subscribe_*` verb a call was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubVerb {
    /// `subscribe_bars`.
    Bars,
    /// `subscribe_quotes`.
    Quotes,
    /// `subscribe_trades`.
    Trades,
    /// `subscribe_book`.
    Book,
    /// `subscribe_depth`.
    Depth,
}

impl SubVerb {
    /// The verb's word as it follows `subscribe_`.
    pub fn as_str(self) -> &'static str {
        match self {
            SubVerb::Bars => "bars",
            SubVerb::Quotes => "quotes",
            SubVerb::Trades => "trades",
            SubVerb::Book => "book",
            SubVerb::Depth => "depth",
        }
    }
}

/// One `subscribe_*` call as the venue client saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeCall {
    /// The verb.
    pub verb: SubVerb,
    /// The symbol it named.
    pub symbol: String,
    /// The bar interval, for `Bars` and for nothing else.
    pub interval: Option<String>,
}

/// One call on a scripted lane or client, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataCall {
    /// The lane was asked to open a client for `venue` (lane log only, and only when it succeeded).
    Open {
        /// The venue.
        venue: String,
    },
    /// A `subscribe_*` call.
    Subscribe(SubscribeCall),
    /// An `unsubscribe` of this id (issued or not).
    Unsubscribe(u64),
    /// `begin_shutdown`.
    BeginShutdown,
    /// `shutdown`.
    Shutdown,
}

impl fmt::Display for DataCall {
    /// `open binance`, `subscribe_bars[1m] BTCUSDT`, `subscribe_quotes BTCUSDT`, `unsubscribe 3`,
    /// `begin_shutdown`, `shutdown`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataCall::Open { venue } => write!(f, "open {venue}"),
            DataCall::Subscribe(SubscribeCall { verb, symbol, interval: Some(interval) }) => {
                write!(f, "subscribe_{}[{interval}] {symbol}", verb.as_str())
            }
            DataCall::Subscribe(SubscribeCall { verb, symbol, interval: None }) => {
                write!(f, "subscribe_{} {symbol}", verb.as_str())
            }
            DataCall::Unsubscribe(id) => write!(f, "unsubscribe {id}"),
            DataCall::BeginShutdown => f.write_str("begin_shutdown"),
            DataCall::Shutdown => f.write_str("shutdown"),
        }
    }
}

/// How a scripted client answers a `subscribe_*` for one symbol instead of issuing an id.
#[derive(Debug, Clone)]
pub enum Refusal {
    /// `LiveDataError::Subscribe(text)`.
    Subscribe(String),
    /// `LiveDataError::Unsupported(text)`.
    Unsupported(&'static str),
}

impl Refusal {
    fn error(&self) -> LiveDataError {
        match self {
            Refusal::Subscribe(text) => LiveDataError::Subscribe(text.clone()),
            Refusal::Unsupported(text) => LiveDataError::Unsupported(text),
        }
    }
}

/// A hook run INSIDE a `subscribe_*` call, before it returns: the session's sink and the call.
pub type SubscribeHook = Arc<dyn Fn(&dyn LiveDataSink, &SubscribeCall) + Send + Sync>;

/// The calls of every session, in arrival order: `(session index, call)`.
type LaneLog = Arc<Mutex<Vec<(usize, DataCall)>>>;

/// One client a test can see: what was asked of it and the sink it was given.
pub struct ScriptedSession {
    /// The venue the client was built for.
    pub venue: String,
    /// The holder the server passed; `None` for a client built outside a server.
    pub holder: Option<Holder>,
    /// The sink the server's wire sink is behind: call any verb from the test thread.
    pub sink: Arc<dyn LiveDataSink>,
    index: usize,
    calls: Mutex<Vec<DataCall>>,
    log: LaneLog,
    begin_shutdowns: AtomicUsize,
    shutdowns: AtomicUsize,
    /// How long `unsubscribe` and `shutdown` take after they are recorded, in milliseconds: a real client joins
    /// the stream's thread, which can take its heartbeat.
    join_delay_ms: AtomicU64,
    refusals: Mutex<HashMap<String, Refusal>>,
    hook: Mutex<Option<SubscribeHook>>,
}

impl ScriptedSession {
    /// Make every later `unsubscribe` and `shutdown` of this client take `delay` AFTER it is recorded (as the
    /// join of a stream's thread does).
    pub fn set_join_delay(&self, delay: Duration) {
        self.join_delay_ms
            .store(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    fn join(&self) {
        let delay = self.join_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
    }

    /// This session's index in the lane (the order the lane opened them in).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The calls this client received, in order.
    pub fn calls(&self) -> Vec<DataCall> {
        lock(&self.calls).clone()
    }

    /// How many times `begin_shutdown` ran.
    pub fn begin_shutdowns(&self) -> usize {
        self.begin_shutdowns.load(Ordering::SeqCst)
    }

    /// How many times `shutdown` ran.
    pub fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }

    /// Refuse every later `subscribe_*` for `symbol` with `refusal`.
    pub fn refuse(&self, symbol: &str, refusal: Refusal) {
        lock(&self.refusals).insert(symbol.to_string(), refusal);
    }

    /// Run `hook` inside every later `subscribe_*` of this client.
    pub fn on_subscribe(&self, hook: SubscribeHook) {
        *lock(&self.hook) = Some(hook);
    }

    /// Play one sink verb into the sink the server gave this client.
    pub fn emit(&self, call: &SinkCall) {
        events::emit(self.sink.as_ref(), call);
    }

    /// Play `calls`, in order.
    pub fn emit_all(&self, calls: &[SinkCall]) {
        events::emit_all(self.sink.as_ref(), calls);
    }

    /// Block (up to five seconds) until this client has received a call `matches` accepts, and return
    /// it. Panics with the calls seen otherwise: the server reaches the client from its own thread, so
    /// a test that needs "the unsubscribe arrived" waits for it here.
    pub fn wait_for_call(&self, matches: impl Fn(&DataCall) -> bool) -> DataCall {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let seen = self.calls();
            if let Some(found) = seen.iter().find(|c| matches(c)) {
                return found.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the awaited call never reached the {} client; it saw {:?}",
                self.venue,
                seen.iter().map(ToString::to_string).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn record(&self, call: DataCall) {
        lock(&self.calls).push(call.clone());
        lock(&self.log).push((self.index, call));
    }
}

/// The `DataClient` a [`ScriptedSession`] fronts. Built by [`ScriptedData::build`].
pub struct ScriptedClient {
    session: Arc<ScriptedSession>,
    next_id: u64,
}

impl ScriptedClient {
    /// The session this client reports to.
    pub fn session(&self) -> &Arc<ScriptedSession> {
        &self.session
    }

    fn sub(&mut self, call: SubscribeCall) -> Result<SubscriptionId, LiveDataError> {
        self.session.record(DataCall::Subscribe(call.clone()));
        let refusal = lock(&self.session.refusals).get(&call.symbol).cloned();
        if let Some(refusal) = refusal {
            return Err(refusal.error());
        }
        let hook = lock(&self.session.hook).clone();
        if let Some(hook) = hook {
            hook(self.session.sink.as_ref(), &call);
        }
        self.next_id += 1;
        Ok(SubscriptionId(self.next_id))
    }
}

impl DataClient for ScriptedClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.sub(SubscribeCall {
            verb: SubVerb::Bars,
            symbol: symbol.to_string(),
            interval: Some(interval.to_string()),
        })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub(SubscribeCall {
            verb: SubVerb::Quotes,
            symbol: symbol.to_string(),
            interval: None,
        })
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub(SubscribeCall {
            verb: SubVerb::Trades,
            symbol: symbol.to_string(),
            interval: None,
        })
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub(SubscribeCall { verb: SubVerb::Book, symbol: symbol.to_string(), interval: None })
    }

    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sub(SubscribeCall { verb: SubVerb::Depth, symbol: symbol.to_string(), interval: None })
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.session.record(DataCall::Unsubscribe(id.0));
        self.session.join();
    }

    fn begin_shutdown(&mut self) {
        self.session.record(DataCall::BeginShutdown);
        self.session.begin_shutdowns.fetch_add(1, Ordering::SeqCst);
    }

    fn shutdown(&mut self) {
        self.session.record(DataCall::Shutdown);
        self.session.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.session.join();
    }
}

struct Inner {
    venues: Vec<String>,
    /// The credentialed-stream venues (`cs_venue=`) this lane serves; empty unless configured.
    credentialed: Vec<String>,
    up: AtomicBool,
    sessions: Mutex<Vec<Arc<ScriptedSession>>>,
    log: LaneLog,
    refused_opens: Mutex<Vec<String>>,
    default_hook: Mutex<Option<SubscribeHook>>,
    default_refusals: Mutex<HashMap<String, Refusal>>,
}

/// The scripted data lane. Cheap to clone: every clone is a handle on the same lane, so a test keeps
/// one to script and inspect after the server took another.
#[derive(Clone)]
pub struct ScriptedData {
    inner: Arc<Inner>,
}

impl ScriptedData {
    /// A lane serving `venues`, in this order (the advertisement's).
    pub fn new(venues: &[&str]) -> Self {
        Self {
            inner: Arc::new(Inner {
                venues: venues.iter().map(|v| (*v).to_string()).collect(),
                credentialed: Vec::new(),
                up: AtomicBool::new(true),
                sessions: Mutex::new(Vec::new()),
                log: Arc::new(Mutex::new(Vec::new())),
                refused_opens: Mutex::new(Vec::new()),
                default_hook: Mutex::new(None),
                default_refusals: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// This lane serving `venues` as CREDENTIALED streams (`cs_venue=`, `Scope::Write`) too: each is
    /// advertised and `open_credentialed` builds the same scripted client `open` does. Call it before the
    /// lane is cloned or handed to a server (a configured lane is a fresh one).
    ///
    /// # Panics
    /// When the lane has already been cloned.
    #[must_use]
    pub fn with_credentialed(mut self, venues: &[&str]) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("a scripted lane is configured before it is shared")
            .credentialed = venues.iter().map(|v| (*v).to_string()).collect();
        self
    }

    /// Make `open` fail (a venue that is down). The advertisement does not change.
    pub fn set_up(&self, up: bool) {
        self.inner.up.store(up, Ordering::SeqCst);
    }

    /// Run `hook` inside every `subscribe_*` of every client built from now on.
    pub fn set_default_hook(&self, hook: SubscribeHook) {
        *lock(&self.inner.default_hook) = Some(hook);
    }

    /// Refuse every `subscribe_*` for `symbol` on every client built from now on.
    pub fn refuse_all(&self, symbol: &str, refusal: Refusal) {
        lock(&self.inner.default_refusals).insert(symbol.to_string(), refusal);
    }

    /// Build one client for `venue` delivering into `sink`, and record it as a session. This is what
    /// [`DataLane::open`] does (with the server's `holder`); a test that wants the same scripted client
    /// under an in-process broker calls it directly with `None`. `Err` is the lane's words for a venue
    /// that is down, and is recorded in [`ScriptedData::refused_opens`].
    pub fn build(
        &self,
        venue: &str,
        holder: Option<&Holder>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<ScriptedClient, String> {
        if !self.inner.up.load(Ordering::SeqCst) {
            lock(&self.inner.refused_opens).push(venue.to_string());
            return Err(format!(
                "scripted data lane: the {venue} client cannot be built (the venue is down)"
            ));
        }
        let mut sessions = lock(&self.inner.sessions);
        let index = sessions.len();
        let session = Arc::new(ScriptedSession {
            venue: venue.to_string(),
            holder: holder.cloned(),
            sink,
            index,
            calls: Mutex::new(Vec::new()),
            log: Arc::clone(&self.inner.log),
            begin_shutdowns: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
            join_delay_ms: AtomicU64::new(0),
            refusals: Mutex::new(lock(&self.inner.default_refusals).clone()),
            hook: Mutex::new(lock(&self.inner.default_hook).clone()),
        });
        sessions.push(Arc::clone(&session));
        lock(&self.inner.log).push((index, DataCall::Open { venue: venue.to_string() }));
        drop(sessions);
        Ok(ScriptedClient { session, next_id: 0 })
    }

    /// How many clients have been built.
    pub fn opened(&self) -> usize {
        lock(&self.inner.sessions).len()
    }

    /// The venues of every `open` that failed because the lane was down, in order.
    pub fn refused_opens(&self) -> Vec<String> {
        lock(&self.inner.refused_opens).clone()
    }

    /// The `n`th session, waiting up to five seconds for it to open (the server opens it from its own
    /// thread, after the client's `subscribe_*` or `new` returned).
    pub fn session(&self, n: usize) -> Arc<ScriptedSession> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(s) = lock(&self.inner.sessions).get(n) {
                return Arc::clone(s);
            }
            assert!(Instant::now() < deadline, "scripted data lane: session {n} never opened");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Every session so far.
    pub fn sessions(&self) -> Vec<Arc<ScriptedSession>> {
        lock(&self.inner.sessions).clone()
    }

    /// Every call of every session in arrival order, `(session index, call)`; an `Open` is each
    /// session's first entry.
    pub fn log(&self) -> Vec<(usize, DataCall)> {
        lock(&self.inner.log).clone()
    }
}

impl fmt::Debug for ScriptedData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedData")
            .field("venues", &self.inner.venues)
            .field("opened", &self.opened())
            .finish_non_exhaustive()
    }
}

impl DataLane for ScriptedData {
    fn venues(&self) -> Vec<String> {
        self.inner.venues.clone()
    }

    fn open(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.build(venue, Some(holder), sink).map(|c| Box::new(c) as Box<dyn DataClient + Send>)
    }

    fn credentialed_venues(&self) -> Vec<String> {
        self.inner.credentialed.clone()
    }

    fn open_credentialed(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.open(venue, holder, sink)
    }
}
