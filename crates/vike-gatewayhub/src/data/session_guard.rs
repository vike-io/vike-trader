//! The SESSION GUARD of a credentialed venue's data stream: one live data session, a ceiling of streams
//! and a minimum spacing between stream opens (decision 0193 verdicts 4 and 6).
//!
//! # Why the gatewayhub, and why per venue
//!
//! The venue sees ONE gatewayhub. Its stream allowance (OANDA's per-IP streams, its new connections per
//! second) is spent by whoever asks this daemon for a `cs_venue=` session, and before the datahub's broker
//! shared one client between its holders nothing here could tell two askers apart from one. Four rules,
//! all keyed on the venue's row of `vike_gatewayhub_client::credentialed_limits` (a venue with no row is
//! not guarded: its client is returned as built, byte for byte):
//!
//! 1. **One live session.** A newer session for the venue REPLACES the older: the older session's streams
//!    are told to stop at once (`DataClient::begin_shutdown`, no join), it discloses a `GapStart` for every
//!    key it held, every later `subscribe_*` of it is refused in words that name the newer session, and one
//!    `warn` names both. The case it exists for is a datahub restarted over a hung old process, or a second
//!    datahub: a second login at the venue, or a second set of streams against its IP allowance. The older
//!    session stays OPEN and silent rather than closed: closing it would make its datahub redial and take
//!    the venue back, and two datahubs would fight over it forever. The replacement lasts as long as the
//!    replacing session does: once THAT session has ended the venue is free again and the older one streams
//!    on its next subscribe (a datahub whose replacement died is the only one left).
//! 2. **A ceiling.** The session holds no more than the row's streams (tick-lane keys: a bar lane is a REST
//!    poll). The refusal carries `CAP_MARK` (a cap, not a link), so the tradehub keeps the venue up and
//!    priceless instead of restarting on it. The datahub's broker refuses first, with the same number; this
//!    is the daemon that really opens the streams counting for itself. A stream is counted from its open to
//!    its RELEASE REQUEST, not to the end of its join (rule 4): the row's margin is what covers a stream
//!    still winding down at the venue.
//! 3. **A spacing.** Two `subscribe_quotes` of one venue, across sessions, start at least the row's spacing
//!    apart: a datahub restart re-opens every key at once, and a venue that limits NEW connections per
//!    second would refuse the burst. The wait is taken on the calling session's reader thread, which the
//!    datahub's per-subscribe acknowledgement already expects (its wait for an ack is far longer than the
//!    ceiling times the spacing).
//! 4. **A release never holds up a subscribe.** `DataClient::unsubscribe` of a stream joins its thread,
//!    which can take the stream's heartbeat (the OANDA bridge's teardown note), and the server's reader
//!    handles one frame at a time: a datahub that let eight keys go and then asked for one would have its
//!    subscribe queue behind eight joins and give up on its acknowledgement. `Guarded::unsubscribe`
//!    therefore only records the release; one reaper thread per session joins the streams one at a time,
//!    and only while no subscribe is in flight or has just finished (`Activity`), so a burst of
//!    subscribes (a returning tradehub) is never interrupted and one that does meet a join in progress
//!    waits for that ONE join. A subscribe of a symbol whose release is still queued makes that release
//!    first, so a symbol is never streamed twice at once. A join takes up to a heartbeat and the datahub
//!    gives up on a subscribe at `GW_SUB_REPLY_TIMEOUT`, so a subscribe starts such a join only while it
//!    has waited less than `JOIN_STARTS_WITHIN`; past it the subscribe is refused as TRANSIENT (the release
//!    stays queued for the reaper) and the caller's next pass finds the join done.
//!
//!    ⚠ **The release is the reaper's, so `WireSink::sub_ended`'s "the feed emits nothing more" holds only
//!    once the reaper has joined the stream.** Until then the released symbol's quotes still arrive and
//!    bind an UNSOLICITED key in the session (one of the session's few such slots), and the datahub drops a
//!    frame for a symbol nobody holds. Nothing is lost that anybody wanted; it is a few frames and a slot
//!    until the join.
//!
//! # Locks
//!
//! There are two kinds and they are never mixed. The session's CLIENT lock is held across a call into the
//! venue client (a thread spawn for a `subscribe_*`, a join for an `unsubscribe`), so it can be held for a
//! stream's heartbeat; nothing is done under it but that call and the reads of `Shared::held` the call
//! needs. Everything else the guard keeps about a session (`Shared`) is behind short locks that are never
//! held across a call into the client: a release, the ceiling check, the guard's admission of a newer session
//! and the replaced-by flag never wait for a join, and `begin_shutdown` only tries the client lock and leaves
//! its request for the reaper if a join holds it. The order is the client lock, then a short lock, and no path
//! holds a short lock while it asks for the client. `venues` is taken before a short lock, and never with a
//! client lock held.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_gatewayhub_client::credentialed_limits::StreamLimits;
use vike_gatewayhub_client::proto::GW_SUB_REPLY_TIMEOUT;
use vike_market_stream::market::{CAP_MARK, TRANSIENT_MARK};
use vike_model::{StreamStatus, now_ms};

/// Where the guard reads a venue's limits: the production table, or a test's own.
pub type LimitsOf = fn(&str) -> Option<StreamLimits>;

/// How often the reaper looks again while a subscribe is in flight.
const REAP_POLL: Duration = Duration::from_millis(20);

/// A subscribe starts a join of a queued release (its own symbol's, or one that makes room under the
/// ceiling) only while it has waited less than this: four seconds. A join waits for the stream's next line, a
/// heartbeat (about five seconds), and the datahub gives up on a subscribe at [`GW_SUB_REPLY_TIMEOUT`], so a
/// join begun later could outlast the reply and fail a start that a retry would carry.
const JOIN_STARTS_WITHIN: Duration = Duration::from_secs(4);

// The latest start, a heartbeat of join and a second for the open fit inside the datahub's wait.
//
// ⚠ That arithmetic assumes a join takes a heartbeat. The join of a stream that was opened a moment ago and
// is still dialling, or waiting for the response head of a hung open, can run longer: the stream thread
// cannot see its stop flag during the dial or the head wait (up to ten seconds, the market lane of
// `venue_timings::OANDA`), so such a join can run past 4 + 5 + 1 s. The refusal is then the datahub's own
// reply timeout, which is a transient one and is retried.
const _: () =
    assert!(JOIN_STARTS_WITHIN.as_millis() + 5_000 + 1_000 <= GW_SUB_REPLY_TIMEOUT.as_millis());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The venue client of one guarded session. See the module doc's "Locks".
type Client = Mutex<Box<dyn DataClient + Send>>;

/// The subscribes in flight, and when the last one finished: what the reaper waits out.
#[derive(Default)]
struct Activity {
    in_flight: AtomicUsize,
    last_end: Mutex<Option<Instant>>,
}

/// A subscribe in flight; dropping it records when it finished.
struct InFlight<'a>(&'a Activity);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        *lock(&self.0.last_end) = Some(Instant::now());
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Activity {
    fn enter(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        InFlight(self)
    }

    /// No subscribe is in flight and none finished within `quiet`.
    fn quiet_for(&self, quiet: Duration) -> bool {
        self.in_flight.load(Ordering::SeqCst) == 0
            && lock(&self.last_end).is_none_or(|end| end.elapsed() >= quiet)
    }
}

/// What a session holds, behind one short lock.
#[derive(Default)]
struct Held {
    /// The quote streams held, by the client's id.
    quotes: HashMap<SubscriptionId, String>,
    /// The bar lanes held, by the client's id: `(symbol, interval)`.
    bars: HashMap<SubscriptionId, (String, String)>,
    /// Streams the subscriber released that the client has not been asked to stop yet, with their symbol,
    /// oldest first: the reaper's queue.
    releasing: Vec<(SubscriptionId, String)>,
    /// Whether a reaper thread is running (set and cleared under this lock).
    reaping: bool,
}

/// What the guard keeps about one session beside its client: every lock here is short.
struct Shared {
    venue: String,
    /// The daemon's name for the session (`Holder::session`).
    id: u64,
    sink: Arc<dyn LiveDataSink>,
    /// The session is over or about to be: `Guarded::shutdown` ran, or the client was dropped. Read by the
    /// guard's admission WITHOUT waiting for the session, so a session that is still joining its streams
    /// is neither "replaced" (a false warning about two datahubs) nor waited for.
    ending: AtomicBool,
    /// The session that replaced this one, with its id, while it stands (the module doc's rule 1). Weak: the
    /// older session does not keep the newer one's sink alive.
    replaced_by: Mutex<Option<(u64, Weak<Shared>)>>,
    held: Mutex<Held>,
    activity: Activity,
    /// How long the reaper lets a subscribe's burst settle before it joins: two spacings of the venue's row,
    /// since the opens of a burst are at least one spacing apart.
    quiet: Duration,
    /// A `begin_shutdown` that found the client busy with a join: the reaper raises the client's stop flags
    /// as soon as its join is done (`Guarded::begin_shutdown`).
    begin_shutdown_requested: AtomicBool,
}

impl Shared {
    fn new(venue: &str, id: u64, sink: Arc<dyn LiveDataSink>, quiet: Duration) -> Self {
        Self {
            venue: venue.to_string(),
            id,
            sink,
            ending: AtomicBool::new(false),
            replaced_by: Mutex::new(None),
            held: Mutex::new(Held::default()),
            activity: Activity::default(),
            quiet,
            begin_shutdown_requested: AtomicBool::new(false),
        }
    }

    /// The id of the session that replaced this one while that session still stands. A replacement that has
    /// ended no longer holds the venue, so this session is free again (and says so once).
    fn replacer(&self) -> Option<u64> {
        let mut by = lock(&self.replaced_by);
        let (id, newer) = by.as_ref()?;
        if newer.upgrade().is_some_and(|newer| !newer.ending.load(Ordering::SeqCst)) {
            return Some(*id);
        }
        tracing::info!(
            venue = %self.venue,
            session = self.id,
            "vike-gatewayhub: the credentialed data session that replaced this one has ended; this \
             session may open streams again (the keys it held when it was replaced were stopped and \
             stay silent until they are subscribed again)"
        );
        *by = None;
        None
    }

    /// The refusal of a `subscribe_*` on a replaced session.
    fn refusal(&self) -> Option<LiveDataError> {
        self.replacer().map(|by| {
            LiveDataError::Subscribe(format!(
                "gatewayhub: this {venue} data session ({id}) was replaced by session {by}: the \
                 gatewayhub keeps ONE live data session per {venue}, so nothing was opened",
                venue = self.venue,
                id = self.id,
            ))
        })
    }
}

/// Wind a replaced session down: stop its streams without waiting for them, and tell the subscriber every key
/// it held has a gap, so its prices are known to have stopped. Takes the client lock, so it runs on a thread
/// of its own (the older session may be inside a join).
///
/// The quote streams it held are stopped from this moment, so they stop counting: they leave `Held::quotes`
/// (the ceiling) for the reaper's queue, which joins them. The datahub behind the session still believes them
/// live (they stay silent after a `GapStart` that no `Live` follows), but a session that is let back in
/// (`Shared::replacer`) does not start with phantom keys against its ceiling.
fn wind_down(shared: &Arc<Shared>, client: &Arc<Client>) {
    lock(client).begin_shutdown();
    let at_ts_ms = now_ms();
    let gap = StreamStatus::GapStart { at_ts_ms };
    let (mut quoted, mut barred, start) = {
        let mut held = lock(&shared.held);
        let stopped: Vec<(SubscriptionId, String)> = held.quotes.drain().collect();
        let mut quoted: Vec<String> = stopped.iter().map(|(_, symbol)| symbol.clone()).collect();
        quoted.sort();
        let barred: Vec<(String, String)> = held.bars.values().cloned().collect();
        held.releasing.extend(stopped);
        let start = !held.releasing.is_empty() && !std::mem::replace(&mut held.reaping, true);
        (quoted, barred, start)
    };
    if start {
        start_reaper(shared, client);
    }
    quoted.dedup();
    for symbol in quoted {
        shared.sink.stream_status(&shared.venue, &symbol, StreamLabel::Quotes.as_str(), gap);
    }
    barred.sort();
    barred.dedup();
    for (symbol, interval) in barred {
        shared.sink.stream_status(&shared.venue, &symbol, &interval, gap);
    }
}

/// Start the reaper of a session: one thread that joins its queued releases. Inline only when no thread can be
/// had (the join then happens on the caller's thread, as a release always did).
fn start_reaper(shared: &Arc<Shared>, client: &Arc<Client>) {
    let quiet = shared.quiet;
    let (thread_shared, thread_client) = (Arc::clone(shared), Arc::clone(client));
    let spawned = std::thread::Builder::new()
        .name(format!("vike-gw-reap-{}", shared.venue))
        .spawn(move || reap(&thread_shared, &thread_client, quiet));
    if spawned.is_err() {
        reap(shared, client, Duration::ZERO);
    }
}

/// [`wind_down`] on a thread of its own; inline only when no thread can be had.
fn wind_down_in_background(shared: Arc<Shared>, client: Weak<Client>) {
    let (inline_shared, inline_client) = (Arc::clone(&shared), client.clone());
    let spawned = std::thread::Builder::new()
        .name(format!("vike-gw-replace-{}", shared.venue))
        .spawn(move || {
            if let Some(client) = client.upgrade() {
                wind_down(&shared, &client);
            }
        });
    if spawned.is_err()
        && let Some(client) = inline_client.upgrade()
    {
        wind_down(&inline_shared, &client);
    }
}

/// Join the streams the subscriber released, one at a time, each only once no subscribe is in flight or has
/// just finished (`quiet`). Returns when the queue is empty; a release that arrives after that starts a new
/// reaper (`Held::reaping` is cleared under the queue's lock).
///
/// ⚠ The client lock is taken BEFORE the queue is read and kept across the join: the entry leaves the queue and
/// its stream is stopped in one step as far as the session's own subscribes can tell (they need the client
/// lock too), so a symbol released and taken again is never streamed twice. Lock order everywhere: the client
/// lock, then `Shared::held`; no path holds `held` while it asks for the client.
fn reap(shared: &Shared, client: &Client, quiet: Duration) {
    // A bridge that panics in a join must not leave the session believing a reaper still runs: `reaping` stays
    // true for ever and no release would be joined again. The panic path clears it.
    struct ClearOnPanic<'a>(&'a Shared);
    impl Drop for ClearOnPanic<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                lock(&self.0.held).reaping = false;
            }
        }
    }
    let _clear = ClearOnPanic(shared);
    loop {
        while !shared.activity.quiet_for(quiet) {
            std::thread::sleep(REAP_POLL);
        }
        let mut venue_client = lock(client);
        honour_begin_shutdown(shared, &mut venue_client);
        let next = {
            let mut held = lock(&shared.held);
            if held.releasing.is_empty() {
                held.reaping = false;
                None
            } else {
                Some(held.releasing.remove(0))
            }
        };
        let Some((id, _symbol)) = next else { return };
        venue_client.unsubscribe(id);
        honour_begin_shutdown(shared, &mut venue_client);
    }
}

/// Raise the client's stop flags if a `begin_shutdown` found it busy with a join and left its request.
fn honour_begin_shutdown(shared: &Shared, client: &mut Box<dyn DataClient + Send>) {
    if shared.begin_shutdown_requested.swap(false, Ordering::SeqCst) {
        client.begin_shutdown();
    }
}

/// What the daemon keeps per guarded venue.
#[derive(Default)]
struct Slot {
    /// The sessions that have not ended: the guard's admission replaces every one that still holds the venue.
    live: Vec<Live>,
    /// The earliest instant the next stream open may start.
    next_open: Option<Instant>,
}

/// One session the venue's slot knows: weak, so a session that ended is nobody's to keep alive.
struct Live {
    shared: Weak<Shared>,
    client: Weak<Client>,
}

/// The guard: one per daemon, shared by every credentialed session. See the module doc.
pub struct SessionGuard {
    limits: LimitsOf,
    venues: Arc<Mutex<HashMap<String, Slot>>>,
    /// How long a subscribe may have waited and still start a join ([`JOIN_STARTS_WITHIN`]; a test's own).
    join_budget: Duration,
}

impl SessionGuard {
    /// A guard over `limits` (production: `vike_gatewayhub_client::credentialed_limits::stream_limits_for`).
    pub fn new(limits: LimitsOf) -> Self {
        Self { limits, venues: Arc::default(), join_budget: JOIN_STARTS_WITHIN }
    }

    /// Admit session `id` of `venue` with the `client` the lane built for it: the client back as it is for
    /// a venue with no row, otherwise wrapped in the guard, after replacing the venue's older live sessions.
    pub fn admit(
        &self,
        venue: &str,
        id: u64,
        sink: Arc<dyn LiveDataSink>,
        client: Box<dyn DataClient + Send>,
    ) -> Box<dyn DataClient + Send> {
        let Some(limits) = (self.limits)(venue) else { return client };
        let shared = Arc::new(Shared::new(venue, id, sink, limits.min_open_spacing * 2));
        let client: Arc<Client> = Arc::new(Mutex::new(client));
        {
            let mut venues = lock(&self.venues);
            let slot = venues.entry(venue.to_string()).or_default();
            slot.live.retain(|live| live.shared.strong_count() > 0);
            for older in &slot.live {
                let Some(older_shared) = older.shared.upgrade() else { continue };
                // An ending session is not wound down (it is joining its streams and nobody is left to hear
                // it: a warning about two datahubs would be false), and one already replaced is not replaced
                // twice. Neither waits on the session: only short locks are taken here.
                if older_shared.ending.load(Ordering::SeqCst) || older_shared.replacer().is_some() {
                    continue;
                }
                *lock(&older_shared.replaced_by) = Some((id, Arc::downgrade(&shared)));
                tracing::warn!(
                    venue,
                    replaced_session = older_shared.id,
                    by_session = id,
                    "vike-gatewayhub: a second credentialed data session opened for a venue that \
                     allows one; the older session's streams are stopped and it refuses every \
                     later subscribe (two datahubs, or a restarted one over a hung old process)"
                );
                wind_down_in_background(older_shared, older.client.clone());
            }
            slot.live
                .push(Live { shared: Arc::downgrade(&shared), client: Arc::downgrade(&client) });
        }
        Box::new(Guarded {
            limits,
            venues: Arc::clone(&self.venues),
            join_budget: self.join_budget,
            shared,
            client,
        })
    }
}

/// The client the guard hands the server in place of the bridge's.
struct Guarded {
    limits: StreamLimits,
    venues: Arc<Mutex<HashMap<String, Slot>>>,
    join_budget: Duration,
    shared: Arc<Shared>,
    client: Arc<Client>,
}

impl Drop for Guarded {
    fn drop(&mut self) {
        self.shared.ending.store(true, Ordering::SeqCst);
    }
}

impl Guarded {
    /// Reserve the next stream-open slot of the venue and wait for it. The reservation is taken under the
    /// lock and the sleep is not, so concurrent sessions queue in order.
    fn pace(&self) {
        let wait = {
            let mut venues = lock(&self.venues);
            let slot = venues.entry(self.shared.venue.clone()).or_default();
            let now = Instant::now();
            let at = slot.next_open.map_or(now, |t| t.max(now));
            slot.next_open = Some(at + self.limits.min_open_spacing);
            at.saturating_duration_since(now)
        };
        if wait > Duration::ZERO {
            std::thread::sleep(wait);
        }
    }

    /// Join, on this subscribe's own thread and with the client lock held, the queued releases that must come
    /// before its open: the symbol's own (so a symbol is never streamed twice at once) and, while the held
    /// streams and the winding-down ones together fill the ceiling, the oldest others (the ceiling bounds what
    /// the venue sees, not what the subscriber still holds). A join takes up to a heartbeat, so one is begun
    /// only while the subscribe has waited less than the guard's budget ([`JOIN_STARTS_WITHIN`]); past it the
    /// subscribe is refused as TRANSIENT with every release still queued for the reaper, and the caller's
    /// next pass finds the joins done.
    fn join_ahead(
        &self,
        client: &mut (dyn DataClient + Send),
        symbol: &str,
        cap: usize,
        began: Instant,
    ) -> Result<(), LiveDataError> {
        loop {
            let next = {
                let mut held = lock(&self.shared.held);
                let own = held.releasing.iter().position(|(_, queued)| queued == symbol);
                let oldest = (held.quotes.len() + held.releasing.len() >= cap
                    && !held.releasing.is_empty())
                .then_some(0);
                let Some(at) = own.or(oldest) else { return Ok(()) };
                let waited = began.elapsed();
                if waited > self.join_budget {
                    return Err(LiveDataError::Subscribe(format!(
                        "gatewayhub: {venue}: `{symbol}` waited {waited:?} behind the join of a stream that \
                         was being released, too long to start another; the subscription was not started \
                         and is {TRANSIENT_MARK}",
                        venue = self.shared.venue,
                    )));
                }
                held.releasing.remove(at).0
            };
            client.unsubscribe(next);
        }
    }
}

impl DataClient for Guarded {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let _busy = self.shared.activity.enter();
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        let id = lock(&self.client).subscribe_bars(symbol, interval)?;
        lock(&self.shared.held).bars.insert(id, (symbol.to_string(), interval.to_string()));
        Ok(id)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let _busy = self.shared.activity.enter();
        let began = Instant::now();
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        let cap = self.limits.max_streams;
        let held = lock(&self.shared.held).quotes.len();
        if held >= cap {
            return Err(LiveDataError::Subscribe(format!(
                "gatewayhub: {venue} already streams {held} symbols, the ceiling of its one data \
                 session ({cap}); {CAP_MARK}: `{symbol}` was not subscribed - stop a strategy that \
                 holds a stream and try again (the ceiling is built into this release: \
                 `credentialed_limits`)",
                venue = self.shared.venue,
            )));
        }
        // No client lock across the wait: a newer session winding this one down must not queue behind it.
        self.pace();
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        // The client lock from here to the open: it waits out a join in progress (at most one: the reaper
        // starts none while this subscribe is in flight), then joins what must precede the open.
        let id = {
            let mut client = lock(&self.client);
            self.join_ahead(&mut **client, symbol, cap, began)?;
            client.subscribe_quotes(symbol)?
        };
        let streams = {
            let mut held = lock(&self.shared.held);
            held.quotes.insert(id, symbol.to_string());
            held.quotes.len()
        };
        // The acceptance of the first slice counts opens from the journal (one line per stream, at the instant
        // the spacing allowed it): never per message, never a value.
        tracing::info!(
            venue = %self.shared.venue,
            session = self.shared.id,
            symbol,
            streams,
            "vike-gatewayhub: credentialed stream opened"
        );
        Ok(id)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        lock(&self.client).subscribe_trades(symbol)
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        lock(&self.client).subscribe_book(symbol)
    }

    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if let Some(refused) = self.shared.refusal() {
            return Err(refused);
        }
        lock(&self.client).subscribe_depth(symbol)
    }

    /// Records the release and returns: the join is the reaper's (module doc, rule 4). The stream stops being
    /// counted at once.
    fn unsubscribe(&mut self, id: SubscriptionId) {
        let mut held = lock(&self.shared.held);
        let Some(symbol) = held.quotes.remove(&id) else {
            // A bar lane (a REST poll whose join is quick) or an id the guard never handed out: straight to
            // the client, as before.
            held.bars.remove(&id);
            drop(held);
            lock(&self.client).unsubscribe(id);
            return;
        };
        held.releasing.push((id, symbol.clone()));
        let start = !std::mem::replace(&mut held.reaping, true);
        let streams = held.quotes.len();
        drop(held);
        tracing::info!(
            venue = %self.shared.venue,
            session = self.shared.id,
            symbol,
            streams,
            "vike-gatewayhub: credentialed stream released"
        );
        if start {
            start_reaper(&self.shared, &self.client);
        }
    }

    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        lock(&self.client).set_delivery(symbol, lane, class)
    }

    fn is_dead(&self, id: SubscriptionId) -> bool {
        lock(&self.client).is_dead(id)
    }

    /// Not the end of the session (the datahub's drain sends it over the wire and keeps using the session), so
    /// it does not set `Shared::ending`. It never waits for a join: the session's single reader calls it, and a
    /// reaper mid-join holds the client for up to a heartbeat. When the client is busy the request is left for
    /// the reaper, which raises the stop flags the moment its join is done.
    fn begin_shutdown(&mut self) {
        match self.client.try_lock() {
            Ok(mut client) => client.begin_shutdown(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().begin_shutdown(),
            Err(TryLockError::WouldBlock) => {
                self.shared.begin_shutdown_requested.store(true, Ordering::SeqCst);
            }
        }
    }

    /// The session is over: flagged BEFORE the client lock is taken (the join below can take a heartbeat), so
    /// a newer session's admission does not wait for it or take it for a live older one.
    fn shutdown(&mut self) {
        self.shared.ending.store(true, Ordering::SeqCst);
        let mut client = lock(&self.client);
        // The client stops every stream still registered, released or not.
        lock(&self.shared.held).releasing.clear();
        client.shutdown();
    }
}

#[cfg(test)]
#[path = "session_guard_tests.rs"]
mod session_guard_tests;
