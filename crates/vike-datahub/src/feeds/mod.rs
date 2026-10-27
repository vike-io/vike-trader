//! `feeds` — the ONE owner of venue market-data clients in this process
//! (decision record 0092, one venue client per process).
//!
//! Three holders want venue data here: the recorder, the live market-data hub and (wave 2 of
//! `vike-gatewayhub`) the STRATEGY LANE that serves the tradehub's prices. Each gets a
//! [`FeedHandle`] — an ordinary [`DataClient`] — and the broker refcounts the REAL subscription
//! behind it per `(venue, symbol, lane)`, so a key two holders want is subscribed once.
//!
//! FEATURE-FREE, like `crate::md`: nothing here names a bridge, so the whole contract runs on the
//! default build in the roster lane. Only the venue table ([`venues`]) is gated, and only its
//! arms.
//!
//! # The seven rules this file exists for
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
//! 4. **A key's DELIVERY CLASS is the strongest need of its holders** (`vike_data::delivery`). On a
//!    `Shared` venue ONE real subscription serves both planes, and what a lagging reader may lose
//!    differs: a chart tolerates a superseded depth snapshot, a recorded `kind=depth` series does
//!    not. So a holder's join and release recompute the class (`Slot::needed`) and the broker
//!    calls `DataClient::set_delivery` on the EXISTING subscription when it changes — the recorder
//!    can join a depth key the hub already holds without any new `subscribe_*`, which is exactly the
//!    case a per-subscribe parameter would miss. A key nobody needs stronger than its lane default
//!    never sees a call. A `PerHolder` venue's client is the holder's own, so the same rule gives
//!    it the holder's class.
//! 5. **A subscription its client reports dead is dropped and asked for again**
//!    (`DataClient::is_dead`). The broker refcounts ONE real subscription behind several virtual
//!    ids, so without the question it would see `slot.real` set and never re-ask for a key whose
//!    client gave up on it; the holders' ids keep pointing at the slot, which re-subscribes at the
//!    next `acquire` (the recorder's tick is the retry).
//! 6. **The strategy lane is the THIRD holder and its keys are its own** ([`Holder::Strat`]). It draws on
//!    no md cap (`crate::md::MD_MAX_KEYS_PER_VENUE` is the market-data plane's, and a maker must not be
//!    refused because GUI windows used the venue's cap: `crate::strat::limits` states the lane's own),
//!    it is the one holder that may subscribe BARS (a bar interval is a [`BrokerLane::Bars`] lane — the
//!    recorder and md still get `Unsupported` for it, unchanged), and it never disturbs the others: a
//!    strategy-only unsubscribe leaves a key md or the recorder holds exactly as it was, and its
//!    frames reach it only through the explicit key set `routing::RoutingSink` keeps for it, as the
//!    recorder's reach it through `rec_keys`.
//! 7. **A venue with a STREAM CEILING counts every holder's streams together**
//!    (`vike_gatewayhub_client::credentialed_limits`, decision 0193 verdict 6). A real tick-lane
//!    subscription that would open one stream more than the venue's row allows is refused with the
//!    number and the cap mark, whoever asks; a bar lane is REST and is not counted, a join of a key
//!    another holder already holds opens nothing and is never refused, and a venue with no row has no
//!    ceiling (every venue before OANDA).
//! 8. **A strategy that joins a QUIET key only the recorder holds is given a FRESH stream**, on a venue
//!    whose first price belongs to a new stream ([`venues::first_price_comes_with_a_new_stream`]: OANDA's
//!    on-connect snapshot and the bridge's first-price read). The strategy lane is the first holder of the
//!    key and the recorder the only other, and no quote reached the recorder for [`QUIET_AFTER_MS`] (twenty
//!    seconds; `routing::RoutingSink::rec_quote_quiet`): the key's real subscription is released and asked
//!    for again BEFORE the ceiling is counted, so the swap never holds a stream more than the key had, and
//!    the joiner gets what a first holder always got. A key with recent quotes is joined with no venue
//!    traffic at all, and a quote of the strategy's own path writes nothing for any of this. On a closed
//!    market the fresh stream has no price to give either; the join is then no worse than before.
#[cfg(feature = "record")]
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::delivery::{Delivery, lane_delivery, stronger};
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_gatewayhub_client::credentialed_limits::stream_limits_for;
use vike_market_stream::market::CAP_MARK;
use vike_model::now_ms;

pub(crate) mod routing;
pub mod venues;

/// How long a quote key only the recorder holds may go without a quote before a strategy that joins it is
/// given a fresh stream instead (rule eight): twenty seconds, in milliseconds. A bounce is not cheap: the
/// bridge's own measurement on the CI box is that about two new `/pricing/stream` opens in three hang for the
/// ten-second head wait and are retried three seconds later (`crates/bridges/oanda/src/market_feed/seed.rs`'s
/// module doc), so a pair that ticks every few seconds would be bounced and then wait thirteen seconds or
/// more for a price it was about to get. A major in session quotes several times a second, so a key quiet
/// for twenty seconds is a quiet pair or a closed market, where the open stream has no price for the joiner
/// and a new one has the venue's on-connect snapshot.
pub(crate) const QUIET_AFTER_MS: i64 = 20_000;

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
    /// The strategy lane (`crate::strat`): the tradehub's prices. Its keys are counted by the lane's own
    /// limits and never against the market-data plane's.
    Strat,
}

impl Holder {
    /// How many holders there are (the length of a slot's `held` array).
    const COUNT: usize = 3;

    const fn idx(self) -> usize {
        match self {
            Holder::Md => 0,
            Holder::Rec => 1,
            Holder::Strat => 2,
        }
    }
}

/// What one broker subscription is on: a tick lane (the four `DataClient` verbs, named by the shared
/// [`StreamLabel`] vocabulary) or a bar interval. A bar interval is no `StreamLabel`, so it is the
/// other arm of this enum rather than a fifth label; only [`Holder::Strat`] subscribes one (module doc,
/// rule 6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BrokerLane {
    /// A tick lane.
    Tick(StreamLabel),
    /// A bar lane: the interval word (`"1m"`).
    Bars(String),
}

impl BrokerLane {
    /// The tick lane's label, `None` for a bar lane: delivery classes and the recorder's filter bit
    /// exist for tick lanes only.
    pub(crate) fn label(&self) -> Option<StreamLabel> {
        match self {
            BrokerLane::Tick(label) => Some(*label),
            BrokerLane::Bars(_) => None,
        }
    }

    /// The lane's word for a log line: the label, or the interval.
    pub(crate) fn word(&self) -> &str {
        match self {
            BrokerLane::Tick(label) => label.as_str(),
            BrokerLane::Bars(interval) => interval,
        }
    }
}

/// The recorder-filter bit of one tick lane — the four `DataClient` verbs the broker carries, named
/// by the shared [`StreamLabel`] vocabulary (this module's own `Lane` enum was identical to it and
/// is gone). A bar interval is no `StreamLabel` at all, so the bars [`Holder::Strat`] subscribes are
/// [`BrokerLane::Bars`] and take no bit here. Exhaustive over `StreamLabel`: a new label fails to
/// compile HERE until the broker decides whether it carries it and which bit it takes.
pub(crate) const fn lane_bit(lane: StreamLabel) -> u8 {
    match lane {
        StreamLabel::Quotes => 1,
        StreamLabel::Trades => 2,
        StreamLabel::Book => 4,
        StreamLabel::Depth => 8,
    }
}

/// How a venue's client is shared between the holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sharing {
    /// One real client, one real subscription per key, events routed by `routing::RoutingSink`.
    Shared,
    /// One real client PER HOLDER, built with that holder's own sink.
    PerHolder,
}

struct Slot {
    real: Option<SubscriptionId>,
    held: [u32; Holder::COUNT],
    /// How many of the strategy lane's holds on this key asked for the lossless class (a depth lane a
    /// strategy wants snapshots of: `FeedHandle::set_delivery`). The recorder's need is its `held`
    /// count; this is the same need stated by a strategy.
    strat_lossless: u32,
    /// The delivery class the client runs this key at: the lane's default until a holder needs more
    /// (rule 4). Meaningless for a bar lane, which has no class to change.
    class: Delivery,
}

impl Slot {
    fn fresh(lane: &BrokerLane) -> Self {
        Slot {
            real: None,
            held: [0; Holder::COUNT],
            strat_lossless: 0,
            class: lane.label().map_or(Delivery::Conflatable, |l| lane_delivery(l, false)),
        }
    }

    /// The class the holders of this slot need: the strongest over the holders that hold it, each at
    /// its own need ([`lane_delivery`]), never below the lane's default. The recorder holding a depth
    /// key makes it lossless, and so does a strategy that asked for snapshots; with only the
    /// market-data hub left it falls back to conflatable.
    fn needed(&self, lane: StreamLabel) -> Delivery {
        let lossless_need = self.held[Holder::Rec.idx()] > 0 || self.strat_lossless > 0;
        let mut class = lane_delivery(lane, false);
        if lossless_need {
            class = stronger(class, lane_delivery(lane, true));
        }
        class
    }
}

struct VenueState {
    client: Box<dyn DataClient + Send>,
    slots: HashMap<(String, BrokerLane), Slot>,
    /// A virtual id to what it stands for: the holder, the symbol, the lane, and whether THIS hold
    /// asked for the lossless class (only a strategy hold ever does besides the recorder's own).
    virt: HashMap<SubscriptionId, (Holder, String, BrokerLane, bool)>,
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
    lane: &BrokerLane,
) -> Result<SubscriptionId, LiveDataError> {
    match lane {
        BrokerLane::Tick(StreamLabel::Quotes) => client.subscribe_quotes(symbol),
        BrokerLane::Tick(StreamLabel::Trades) => client.subscribe_trades(symbol),
        BrokerLane::Tick(StreamLabel::Book) => client.subscribe_book(symbol),
        BrokerLane::Tick(StreamLabel::Depth) => client.subscribe_depth(symbol),
        BrokerLane::Bars(interval) => client.subscribe_bars(symbol, interval),
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
        FeedHandle {
            broker: Arc::clone(self),
            venue: venue.to_string(),
            holder,
            mine: Vec::new(),
            floors: HashMap::new(),
        }
    }

    /// Live REAL subscriptions across every client — the status line, and the tests' witness.
    pub fn real_subscriptions(&self) -> usize {
        lock(&self.clients)
            .values()
            .map(|v| lock(v).slots.values().filter(|s| s.real.is_some()).count())
            .sum()
    }

    /// The symbols the STRATEGY LANE holds on `venue` in tick lane `lane` right now: the key set
    /// `routing::RoutingSink` admits the lane's frames by. The recorder's "held by strategies"
    /// resolver reads it (`crate::recording`), so a strategy's quote key is recorded without the
    /// operator naming it. Only `Holder::Strat` publishes into it, so the recorder's own hold of a key
    /// never makes the key look strategy-held.
    #[cfg(feature = "record")]
    pub(crate) fn strat_held(&self, venue: &str, lane: StreamLabel) -> BTreeSet<String> {
        self.routing.strat_symbols(venue, lane)
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

    /// Publish (`on`) or withdraw the routing filter of the holder that has one for `lane`: the
    /// recorder's tick-lane bit, the strategy lane's key set. md has none.
    fn set_filter(&self, holder: Holder, venue: &str, symbol: &str, lane: &BrokerLane, on: bool) {
        match holder {
            Holder::Rec => {
                if let Some(label) = lane.label() {
                    self.routing.set_rec(venue, symbol, label, on);
                }
            }
            Holder::Strat => self.routing.set_strat(venue, symbol, lane, on),
            Holder::Md => {}
        }
    }

    fn acquire(
        &self,
        holder: Holder,
        venue: &str,
        symbol: &str,
        lane: &BrokerLane,
        lossless: bool,
    ) -> Result<SubscriptionId, LiveDataError> {
        let (vs, shared) = self.state(holder, venue)?;
        let mut guard = lock(&vs);
        let VenueState { client, slots, virt, draining } = &mut *guard;
        if *draining {
            return Err(LiveDataError::Subscribe(format!(
                "{venue}: the client is draining (a holder began its shutdown) — retried next pass"
            )));
        }
        let k = (symbol.to_string(), lane.clone());
        let label = lane.label();
        let first_for_holder = slots.get(&k).is_none_or(|s| s.held[holder.idx()] == 0);
        let default = label.map_or(Delivery::Conflatable, |l| lane_delivery(l, false));
        // Rule 5: a real subscription its client reports dead is dropped and asked for afresh, by
        // whoever asks next; the other holders' ids keep pointing at the slot, which now re-subscribes.
        if let Some(id) = slots.get(&k).and_then(|s| s.real)
            && client.is_dead(id)
        {
            tracing::warn!(
                venue,
                symbol,
                lane = lane.word(),
                "feed broker: the venue client reports a held subscription dead — dropping it and \
                 subscribing afresh"
            );
            client.unsubscribe(id);
            if let Some(s) = slots.get_mut(&k) {
                s.real = None;
                s.class = default;
            }
        }
        // Rule 8: the first strategy holder of a quote key only the recorder holds, on a venue whose first price
        // comes with a NEW stream, finds the key quiet: release the open stream and ask for it afresh below, so
        // the joiner gets the on-connect snapshot and the first-price read. The slot is freed here, before the
        // ceiling is counted, so the swap never needs a stream more than the key already had. The class is
        // reset as rule 5 does: the client forgets a key's class with its subscription.
        let mut swapped = false;
        if holder == Holder::Strat
            && first_for_holder
            && *lane == BrokerLane::Tick(StreamLabel::Quotes)
            && venues::first_price_comes_with_a_new_stream(venue)
            && slots.get(&k).is_some_and(|s| {
                s.real.is_some() && s.held[Holder::Rec.idx()] > 0 && s.held[Holder::Md.idx()] == 0
            })
            && self.routing.rec_quote_quiet(venue, symbol, now_ms(), QUIET_AFTER_MS)
            && let Some(slot) = slots.get_mut(&k)
            && let Some(old) = slot.real.take()
        {
            tracing::info!(
                venue,
                symbol,
                "feed broker: a strategy joined a quote key only the recorder holds and no quote came for a \
                 while - releasing the open stream and asking for it afresh, so the joiner gets the venue's \
                 first price"
            );
            client.unsubscribe(old);
            slot.class = default;
            swapped = true;
        }
        let live = slots.get(&k).is_some_and(|s| s.real.is_some());
        // Rule 7: the stream ceiling, checked before anything is published so a refusal has nothing to
        // undo. A key that is already live opens no stream and is never refused here.
        if !live
            && matches!(lane, BrokerLane::Tick(_))
            && let Some(limits) = stream_limits_for(venue)
        {
            let open = slots
                .iter()
                .filter(|((_, l), s)| matches!(l, BrokerLane::Tick(_)) && s.real.is_some())
                .count();
            if open >= limits.max_streams {
                return Err(LiveDataError::Subscribe(format!(
                    "{venue}: {open} streams are already open and the ceiling for all holders \
                     together is {cap}; {CAP_MARK}: `{symbol}` ({lane}) was not subscribed - stop a \
                     strategy or a recorded symbol that holds a stream and try again (the ceiling \
                     is built into this release: `credentialed_limits`)",
                    cap = limits.max_streams,
                    lane = lane.word(),
                )));
            }
        }
        // The filter a holder's frames are admitted through is published BEFORE the real subscribe
        // (rule 2), for the holders that have one and only on a shared client: a PerHolder client
        // delivers into the holder's own sink and needs none.
        let filtered = shared && holder != Holder::Md && first_for_holder;
        if filtered {
            self.set_filter(holder, venue, symbol, lane, true);
        }
        // Rule 4: the class this subscription must run at once this holder joins. The client is
        // told only when it differs from what the client already runs (the lane's default for a
        // fresh key), so a key nobody needs stronger sees no call at all. A bar lane has no class.
        let mut class = default;
        if let Some(label) = label {
            let own = lane_delivery(label, holder == Holder::Rec || lossless);
            let (running, needed) = match slots.get(&k) {
                Some(s) => (s.class, stronger(s.needed(label), own)),
                None => (default, stronger(default, own)),
            };
            class = running;
            if needed != running {
                match client.set_delivery(symbol, label, needed) {
                    Ok(()) => class = needed,
                    // Declined (an older daemon, a client with no class to change): the key runs at what
                    // it ran at before this change, as every key did before the class existed.
                    Err(LiveDataError::Unsupported(why)) => tracing::warn!(
                        venue,
                        symbol,
                        lane = lane.word(),
                        why,
                        "feed broker: the venue client declined a delivery class — the key keeps its \
                         previous one"
                    ),
                    Err(e) => {
                        if filtered {
                            self.set_filter(holder, venue, symbol, lane, false);
                        }
                        return Err(e);
                    }
                }
            }
        }
        if !live {
            let mut started = subscribe_real(client.as_mut(), symbol, lane);
            // A swap released the recorder's stream: a fresh subscribe that fails (a link that is down, the
            // gatewayhub's transient refusal behind a slow join) would leave the recorder holding a key with
            // no stream until the strategy retries. ONE more ask carries the common case; a second failure is
            // the joiner's refusal, not a loop with the venue's mutex held.
            if swapped && let Err(first) = &started {
                tracing::warn!(
                    venue,
                    symbol,
                    error = %first,
                    "feed broker: the fresh stream a swap asked for was refused - asking once more, the \
                     recorder still holds the key"
                );
                started = subscribe_real(client.as_mut(), symbol, lane);
            }
            match started {
                Ok(id) => {
                    let slot = slots.entry(k.clone()).or_insert_with(|| Slot::fresh(lane));
                    slot.real = Some(id);
                    slot.class = class;
                }
                Err(e) => {
                    if filtered {
                        self.set_filter(holder, venue, symbol, lane, false);
                    }
                    if let Some(label) = label
                        && class != default
                    {
                        // Best effort: do not leave the client remembering a class for a key it
                        // never opened.
                        let _ = client.set_delivery(symbol, label, default);
                    }
                    return Err(e);
                }
            }
        } else if let Some(slot) = slots.get_mut(&k) {
            slot.class = class;
            if holder == Holder::Rec && first_for_holder && label == Some(StreamLabel::Book) {
                self.joined_live_book.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    venue,
                    symbol,
                    "feed broker: the recorder joined a LIVE book subscription — its first rows are \
                     deltas until the venue's next snapshot anchor"
                );
            }
        }
        let slot = slots.entry(k).or_insert_with(|| Slot::fresh(lane));
        slot.held[holder.idx()] += 1;
        let lossless_hold = holder == Holder::Strat && lossless && label.is_some();
        if lossless_hold {
            slot.strat_lossless += 1;
        }
        let vid = SubscriptionId(self.next_virtual.fetch_add(1, Ordering::Relaxed));
        virt.insert(vid, (holder, symbol.to_string(), lane.clone(), lossless_hold));
        Ok(vid)
    }

    fn release(&self, holder: Holder, venue: &str, vid: SubscriptionId) {
        let Some((vs, shared)) = self.existing(holder, venue) else { return };
        let mut guard = lock(&vs);
        let VenueState { client, slots, virt, draining } = &mut *guard;
        let Some((h, symbol, lane, lossless_hold)) = virt.remove(&vid) else { return };
        let k = (symbol, lane);
        let Some(slot) = slots.get_mut(&k) else { return };
        slot.held[h.idx()] = slot.held[h.idx()].saturating_sub(1);
        if lossless_hold {
            slot.strat_lossless = slot.strat_lossless.saturating_sub(1);
        }
        if shared && h != Holder::Md && slot.held[h.idx()] == 0 {
            self.set_filter(h, venue, &k.0, &k.1, false);
            // The strategy lane let a quote key go to the recorder alone: the key was live up to now, so its
            // quote clock starts here (rule eight) and a strategy that is back within the quiet window joins
            // the open stream with no venue traffic.
            if h == Holder::Strat
                && k.1 == BrokerLane::Tick(StreamLabel::Quotes)
                && slot.held[Holder::Rec.idx()] > 0
                && venues::first_price_comes_with_a_new_stream(venue)
            {
                self.routing.stamp_rec_quote(venue, &k.0, now_ms());
            }
        }
        if slot.held.iter().all(|n| *n == 0) {
            if let Some(id) = slot.real.take() {
                client.unsubscribe(id);
            }
            slots.remove(&k);
        } else if let Some(label) = k.1.label()
            && slot.real.is_some()
            && !*draining
        {
            // Rule 4, the other direction: the strongest holder left, so the key may relax (the
            // recorder released a depth key the hub still holds: back to conflatable). Not while the
            // client is DRAINING: the process is going down and a class change would only race the stop.
            let needed = slot.needed(label);
            if needed != slot.class {
                match client.set_delivery(&k.0, label, needed) {
                    Ok(()) => slot.class = needed,
                    // A release cannot fail: the key keeps running at the stronger class, which
                    // loses nothing, and the next change retries.
                    Err(e) => tracing::warn!(
                        venue,
                        symbol = k.0.as_str(),
                        lane = label.as_str(),
                        error = %e,
                        "feed broker: the venue client could not relax a delivery class — the key \
                         keeps the stronger one"
                    ),
                }
            }
        }
        if *draining && !slots.values().any(|s| s.real.is_some()) {
            *draining = false;
        }
    }

    /// A strategy hold's lossless need CHANGED while it is held (`FeedHandle::set_delivery` after the
    /// subscribe): move the slot's count and re-run rule 4 in whichever direction it points. A hold
    /// that is not the strategy lane's, or on a bar lane, has no need to state and is left alone.
    fn restate(
        &self,
        holder: Holder,
        venue: &str,
        vid: SubscriptionId,
        lossless: bool,
    ) -> Result<(), LiveDataError> {
        if holder != Holder::Strat {
            return Ok(());
        }
        let Some((vs, _)) = self.existing(holder, venue) else { return Ok(()) };
        let mut guard = lock(&vs);
        let VenueState { client, slots, virt, draining } = &mut *guard;
        let Some(entry) = virt.get_mut(&vid) else { return Ok(()) };
        let (_, symbol, lane, held_lossless) = entry;
        let Some(label) = lane.label() else { return Ok(()) };
        if *held_lossless == lossless {
            return Ok(());
        }
        let k = (symbol.clone(), lane.clone());
        let Some(slot) = slots.get_mut(&k) else { return Ok(()) };
        *held_lossless = lossless;
        if lossless {
            slot.strat_lossless += 1;
        } else {
            slot.strat_lossless = slot.strat_lossless.saturating_sub(1);
        }
        if slot.real.is_none() || *draining {
            return Ok(());
        }
        let needed = slot.needed(label);
        if needed == slot.class {
            return Ok(());
        }
        match client.set_delivery(symbol, label, needed) {
            Ok(()) => {
                slot.class = needed;
                Ok(())
            }
            // A relax that fails keeps the stronger class, which loses nothing; a raise that fails
            // is the caller's to hear about (it asked for a stronger rule than the key runs at).
            Err(e) if needed < slot.class => {
                tracing::warn!(
                    venue,
                    symbol = symbol.as_str(),
                    lane = label.as_str(),
                    error = %e,
                    "feed broker: the venue client could not relax a delivery class — the key keeps \
                     the stronger one"
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn begin_shutdown_if_sole(&self, holder: Holder, venue: &str) {
        let Some((vs, _)) = self.existing(holder, venue) else { return };
        let mut st = lock(&vs);
        let mut live = st.slots.values().filter(|s| s.real.is_some()).peekable();
        let any = live.peek().is_some();
        if any
            && live
                .all(|s| s.held.iter().enumerate().all(|(i, held)| i == holder.idx() || *held == 0))
        {
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
    mine: Vec<(SubscriptionId, String, BrokerLane)>,
    /// The delivery floors the strategy lane stated by `set_delivery`, per `(symbol, lane)`: read at
    /// the next subscribe on the pair, and re-applied to a hold already taken.
    floors: HashMap<(String, StreamLabel), Delivery>,
}

impl FeedHandle {
    fn take(&mut self, symbol: &str, lane: BrokerLane) -> Result<SubscriptionId, LiveDataError> {
        let lossless = lane.label().is_some_and(|label| {
            self.floors
                .get(&(symbol.to_string(), label))
                .is_some_and(|class| *class >= Delivery::Lossless)
        });
        let id = self.broker.acquire(self.holder, &self.venue, symbol, &lane, lossless)?;
        self.mine.push((id, symbol.to_string(), lane));
        Ok(id)
    }
}

impl DataClient for FeedHandle {
    /// Bars are the strategy lane's alone: the market-data hub and the recorder never subscribe a bar
    /// lane through the broker (rule 6), and a handle held by either still says so.
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        if self.holder == Holder::Strat {
            self.take(symbol, BrokerLane::Bars(interval.to_string()))
        } else {
            Err(LiveDataError::Unsupported("the feed broker carries tick lanes only"))
        }
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, BrokerLane::Tick(StreamLabel::Quotes))
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, BrokerLane::Tick(StreamLabel::Trades))
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, BrokerLane::Tick(StreamLabel::Book))
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.take(symbol, BrokerLane::Tick(StreamLabel::Depth))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        if let Some(p) = self.mine.iter().position(|(m, ..)| *m == id) {
            self.mine.swap_remove(p);
            self.broker.release(self.holder, &self.venue, id);
        }
    }
    /// A STRATEGY handle states its lane's floor (a strategy that wants depth snapshots): the broker
    /// raises the key's class for as long as the hold stands (rule 4), and a floor stated before the
    /// subscribe rides it. Any other holder's class is its own ([`lane_delivery`] for the recorder,
    /// the lane's default for md), so it declines.
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        if self.holder != Holder::Strat {
            return Err(LiveDataError::Unsupported(
                "only the strategy lane states a delivery floor through the broker",
            ));
        }
        let key = (symbol.to_string(), lane);
        let lossless = class >= Delivery::Lossless;
        if lossless {
            self.floors.insert(key, class);
        } else {
            self.floors.remove(&key);
        }
        let held: Vec<SubscriptionId> = self
            .mine
            .iter()
            .filter(|(_, s, l)| s == symbol && *l == BrokerLane::Tick(lane))
            .map(|(id, ..)| *id)
            .collect();
        for id in held {
            self.broker.restate(self.holder, &self.venue, id, lossless)?;
        }
        Ok(())
    }
    /// Rule 1 — see the module doc.
    fn begin_shutdown(&mut self) {
        self.broker.begin_shutdown_if_sole(self.holder, &self.venue);
    }
    fn shutdown(&mut self) {
        for (id, ..) in std::mem::take(&mut self.mine) {
            self.broker.release(self.holder, &self.venue, id);
        }
    }
}

impl Drop for FeedHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
