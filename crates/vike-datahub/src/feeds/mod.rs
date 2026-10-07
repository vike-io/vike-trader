//! `feeds` — the ONE owner of venue market-data clients in this process
//! (decision record 0092, one venue client per process).
//!
//! Two planes want venue data here: the recorder and the live market-data hub. Each gets a
//! [`FeedHandle`] — an ordinary [`DataClient`] — and the broker refcounts the REAL subscription
//! behind it per `(venue, symbol, lane)`, so a key both planes want is subscribed once.
//!
//! FEATURE-FREE, like `crate::md`: nothing here names a bridge, so the whole contract runs on the
//! default build in the roster lane. Only the venue table ([`venues`]) is gated, and only its
//! arms.
//!
//! # The three rules this file exists for
//!
//! 1. **`begin_shutdown` raises EVERY flag a client owns** (`vike_data::FeedRegistry::
//!    raise_stops`). A holder's `begin_shutdown` therefore reaches the real client only when that
//!    holder is the sole holder of every live key on it; the client is then DRAINING and every
//!    `acquire` is refused as TRANSIENT until the drain ends, because a new holder would otherwise
//!    share a subscription whose flag is already raised.
//! 2. **The recorder's filter bit is published BEFORE the real subscribe**, so a venue that emits
//!    inside `subscribe_*` cannot lose the first rows — the membership-before-subscribe rule of
//!    `crates/vike-recorder/src/runtime.rs`, one layer down.
//! 3. **Sharing is DECLARED per venue** ([`Sharing`]). A venue whose `unsubscribe` disturbs
//!    co-tenants (polymarket's shards resubscribe their remaining tokens) gets one client per
//!    holder, each built with that holder's own sink.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};

pub(crate) mod routing;
pub mod venues;

/// How the broker obtains one venue client. Same shape as `crate::md::MarketClientBuilder`.
pub type ClientBuilder = Box<
    dyn Fn(&str, Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, String> + Send + Sync,
>;

/// Which plane holds a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holder {
    /// The live market-data hub (`crate::md`).
    Md,
    /// The recording plane (`crate::recorder`).
    Rec,
}

impl Holder {
    const fn idx(self) -> usize {
        match self {
            Holder::Md => 0,
            Holder::Rec => 1,
        }
    }
}

/// A tick lane — the four `DataClient` verbs the broker carries. Bars are not brokered: neither
/// plane subscribes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    Quotes,
    Trades,
    Book,
    Depth,
}

impl Lane {
    pub(crate) const fn bit(self) -> u8 {
        match self {
            Lane::Quotes => 1,
            Lane::Trades => 2,
            Lane::Book => 4,
            Lane::Depth => 8,
        }
    }

    /// A feed's `stream_status` label, back to its lane. `None` for a bar interval.
    pub fn from_stream_label(label: &str) -> Option<Lane> {
        match label {
            "quotes" => Some(Lane::Quotes),
            "trades" => Some(Lane::Trades),
            "book" => Some(Lane::Book),
            "depth" => Some(Lane::Depth),
            _ => None,
        }
    }
}

/// How a venue's client is shared between the two holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    /// One real client, one real subscription per key, events routed by `routing::RoutingSink`.
    Shared,
    /// One real client PER HOLDER, built with that holder's own sink.
    PerHolder,
}

#[derive(Default)]
struct Slot {
    real: Option<SubscriptionId>,
    held: [u32; 2],
}

struct VenueState {
    client: Box<dyn DataClient + Send>,
    slots: HashMap<(String, Lane), Slot>,
    virt: HashMap<SubscriptionId, (Holder, String, Lane)>,
    draining: bool,
}

type ClientKey = (String, Option<Holder>);

pub struct FeedBroker {
    builder: ClientBuilder,
    sharing: fn(&str) -> Sharing,
    clients: Mutex<HashMap<ClientKey, Arc<Mutex<VenueState>>>>,
    routing: Arc<routing::RoutingSink>,
    next_virtual: AtomicU64,
    joined_live_book: AtomicU64,
}

impl std::fmt::Debug for FeedBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedBroker").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn subscribe_real(
    client: &mut (dyn DataClient + Send),
    symbol: &str,
    lane: Lane,
) -> Result<SubscriptionId, LiveDataError> {
    match lane {
        Lane::Quotes => client.subscribe_quotes(symbol),
        Lane::Trades => client.subscribe_trades(symbol),
        Lane::Book => client.subscribe_book(symbol),
        Lane::Depth => client.subscribe_depth(symbol),
    }
}

impl FeedBroker {
    pub fn new(builder: ClientBuilder, sharing: fn(&str) -> Sharing) -> Arc<Self> {
        Arc::new(FeedBroker {
            builder,
            sharing,
            clients: Mutex::new(HashMap::new()),
            routing: Arc::new(routing::RoutingSink::default()),
            next_virtual: AtomicU64::new(1),
            joined_live_book: AtomicU64::new(0),
        })
    }

    /// Attach a holder's sink. Once per holder; a second call is ignored.
    pub fn attach_sink(&self, holder: Holder, sink: Arc<dyn LiveDataSink>) {
        self.routing.attach(holder, sink);
    }

    pub fn handle(self: &Arc<Self>, holder: Holder, venue: &str) -> FeedHandle {
        FeedHandle { broker: Arc::clone(self), venue: venue.to_string(), holder, mine: Vec::new() }
    }

    /// Live REAL subscriptions across every client — the status line, and the tests' witness.
    pub fn real_subscriptions(&self) -> usize {
        lock(&self.clients)
            .values()
            .map(|v| lock(v).slots.values().filter(|s| s.real.is_some()).count())
            .sum()
    }

    /// How many times the recorder joined a LIVE `Book` key — rows before the venue's next
    /// snapshot anchor are deltas with no anchor. Zero by construction under the shipped
    /// [`Sharing`] table (no `Shared` venue has a book lane); a guard, not a feature.
    pub fn joined_live_book(&self) -> u64 {
        self.joined_live_book.load(Ordering::Relaxed)
    }

    /// Phase one of the PROCESS teardown: raise every client's flags, join nothing. Every client
    /// is left DRAINING — the process is going down and nothing may subscribe again.
    pub fn begin_shutdown_all(&self) {
        for v in lock(&self.clients).values() {
            let mut st = lock(v);
            st.draining = true;
            st.client.begin_shutdown();
        }
    }

    fn client_key(&self, holder: Holder, venue: &str) -> ClientKey {
        match (self.sharing)(venue) {
            Sharing::Shared => (venue.to_string(), None),
            Sharing::PerHolder => (venue.to_string(), Some(holder)),
        }
    }

    fn existing(&self, holder: Holder, venue: &str) -> Option<(Arc<Mutex<VenueState>>, bool)> {
        let key = self.client_key(holder, venue);
        let shared = key.1.is_none();
        lock(&self.clients).get(&key).map(|v| (Arc::clone(v), shared))
    }

    fn state(
        &self,
        holder: Holder,
        venue: &str,
    ) -> Result<(Arc<Mutex<VenueState>>, bool), LiveDataError> {
        let key = self.client_key(holder, venue);
        let shared = key.1.is_none();
        let mut map = lock(&self.clients);
        if let Some(v) = map.get(&key) {
            return Ok((Arc::clone(v), shared));
        }
        let sink: Arc<dyn LiveDataSink> = if shared {
            Arc::clone(&self.routing) as Arc<dyn LiveDataSink>
        } else {
            self.routing.sink_of(holder).ok_or_else(|| {
                LiveDataError::Subscribe(format!("{venue}: no {holder:?} sink is attached"))
            })?
        };
        let client = (self.builder)(venue, sink).map_err(LiveDataError::Subscribe)?;
        let st = Arc::new(Mutex::new(VenueState {
            client,
            slots: HashMap::new(),
            virt: HashMap::new(),
            draining: false,
        }));
        map.insert(key, Arc::clone(&st));
        Ok((st, shared))
    }

    fn acquire(
        &self,
        holder: Holder,
        venue: &str,
        symbol: &str,
        lane: Lane,
    ) -> Result<SubscriptionId, LiveDataError> {
        let (vs, shared) = self.state(holder, venue)?;
        let mut guard = lock(&vs);
        let VenueState { client, slots, virt, draining } = &mut *guard;
        if *draining {
            return Err(LiveDataError::Subscribe(format!(
                "{venue}: the client is draining (a holder began its shutdown) — retried next pass"
            )));
        }
        let k = (symbol.to_string(), lane);
        let first_for_holder = slots.get(&k).is_none_or(|s| s.held[holder.idx()] == 0);
        let live = slots.get(&k).is_some_and(|s| s.real.is_some());
        let filtered = shared && holder == Holder::Rec && first_for_holder;
        if filtered {
            self.routing.set_rec(venue, symbol, lane, true); // rule 2: BEFORE the real call
        }
        if !live {
            match subscribe_real(client.as_mut(), symbol, lane) {
                Ok(id) => slots.entry(k.clone()).or_default().real = Some(id),
                Err(e) => {
                    if filtered {
                        self.routing.set_rec(venue, symbol, lane, false);
                    }
                    return Err(e);
                }
            }
        } else if holder == Holder::Rec && first_for_holder && lane == Lane::Book {
            self.joined_live_book.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                venue,
                symbol,
                "feed broker: the recorder joined a LIVE book subscription — its first rows are \
                 deltas until the venue's next snapshot anchor"
            );
        }
        slots.entry(k).or_default().held[holder.idx()] += 1;
        let vid = SubscriptionId(self.next_virtual.fetch_add(1, Ordering::Relaxed));
        virt.insert(vid, (holder, symbol.to_string(), lane));
        Ok(vid)
    }

    fn release(&self, holder: Holder, venue: &str, vid: SubscriptionId) {
        let Some((vs, shared)) = self.existing(holder, venue) else { return };
        let mut guard = lock(&vs);
        let VenueState { client, slots, virt, draining } = &mut *guard;
        let Some((h, symbol, lane)) = virt.remove(&vid) else { return };
        let k = (symbol, lane);
        let Some(slot) = slots.get_mut(&k) else { return };
        slot.held[h.idx()] = slot.held[h.idx()].saturating_sub(1);
        if shared && h == Holder::Rec && slot.held[h.idx()] == 0 {
            self.routing.set_rec(venue, &k.0, lane, false);
        }
        if slot.held == [0, 0] {
            if let Some(id) = slot.real.take() {
                client.unsubscribe(id);
            }
            slots.remove(&k);
        }
        if *draining && !slots.values().any(|s| s.real.is_some()) {
            *draining = false;
        }
    }

    fn begin_shutdown_if_sole(&self, holder: Holder, venue: &str) {
        let Some((vs, _)) = self.existing(holder, venue) else { return };
        let mut st = lock(&vs);
        let other = 1 - holder.idx();
        let mut live = st.slots.values().filter(|s| s.real.is_some()).peekable();
        let any = live.peek().is_some();
        if any && live.all(|s| s.held[other] == 0) {
            st.draining = true;
            st.client.begin_shutdown();
        }
    }
}

/// One holder's view of one venue — an ordinary [`DataClient`].
pub struct FeedHandle {
    broker: Arc<FeedBroker>,
    venue: String,
    holder: Holder,
    mine: Vec<SubscriptionId>,
}

impl FeedHandle {
    fn take(&mut self, symbol: &str, lane: Lane) -> Result<SubscriptionId, LiveDataError> {
        let id = self.broker.acquire(self.holder, &self.venue, symbol, lane)?;
        self.mine.push(id);
        Ok(id)
    }
}

impl DataClient for FeedHandle {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("the feed broker carries tick lanes only"))
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, Lane::Quotes)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, Lane::Trades)
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, Lane::Book)
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, Lane::Depth)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some(p) = self.mine.iter().position(|m| *m == id) {
            self.mine.swap_remove(p);
            self.broker.release(self.holder, &self.venue, id);
        }
    }
    /// Rule 1 — see the module doc.
    fn begin_shutdown(&mut self) {
        self.broker.begin_shutdown_if_sole(self.holder, &self.venue);
    }
    fn shutdown(&mut self) {
        for id in std::mem::take(&mut self.mine) {
            self.broker.release(self.holder, &self.venue, id);
        }
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
