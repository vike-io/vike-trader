//! `StratFeed` - the tradehub's PRICE path to the datahub's strategy lane: a `vike_data::DataClient` whose
//! frames come from `Request::StratOpen`'s socket instead of a venue socket
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md` verdict 2,
//! `docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
//!
//! # What it is, and what it is not
//!
//! The RECEIVING ENGINE is the tube's `vike_market_stream::market::RemoteDataClient`, unchanged: it owns
//! the reconnecting reader, the in-band `Sub`/`Unsub`, the replay of the held set under its original
//! ids, the sequence checks and the gap disclosure. This module supplies the two things the engine asks
//! its host for - the dial ([`StratOpener`], an `Opener`) and the sink it delivers into
//! ([`LinkSink`]) - and one rule on top (the next section). There is no `StratUpdate` verb and no
//! `Response::Strat` frame: the lane's grammar after the open reply IS the tube's.
//!
//! # Loss is disclosed under its OWN word, at the instant it happens
//!
//! The tradehub's link dead-man keeps one window per disclosing SOURCE
//! (`crates/vike-core/src/runtime/link_deadman.rs`'s `LinkDeadMan`), derived from the status's `stream`
//! word (`vike_venues::link_deadman::LinkSource::of`). When the engine loses the session it discloses a
//! `GapStart` for every held lane under that lane's word, which is the VENUE's word: a `Live` from the
//! venue's own socket (forwarded by the datahub) would close it, and this link's `Live` would close an
//! outage the venue's socket disclosed. [`LinkSink`] therefore recognises the engine's link-loss
//! `GapStart` (the session is marked down when it is called, and the peer forwards nothing on a down
//! session: [`LinkSink`]'s doc), WITHHOLDS it and discloses ONE `GapStart` per held symbol under
//! `STRAT_LINK_STREAM`, stamped with when the gap OPENED (the engine's own stamp: the last frame
//! delivered, never the arrival clock). Everything else the engine says (a status the peer forwarded, a
//! hole disclosure, a dead lane) passes through untouched, and `Stale` is never synthesized.
//!
//! The gap CLOSES with a `Live` under the same word, for a symbol: at the re-acknowledgement of the LAST
//! of its lanes after the reconnect (each lane's engine `Live` is withheld in turn; a sibling that came
//! back cannot vouch for a lane the peer refused for good), when nothing is owed any more because the
//! last owed lane was unsubscribed during the outage ([`StratFeed::unsubscribe`]; a lane the engine ended
//! on its own was never owed), and when the feed is shut down or dropped: **a `Disconnected` disclosed for a key is always
//! followed by a `Live` from the SAME source, or by process exit** (the dead-man's gap-closure
//! invariant). A lane the peer refuses for good on replay leaves its symbol's gap open until its owner
//! gives it up, loudly, exactly as the engine leaves its own. A link-loss `GapStart` for a symbol the feed
//! no longer holds (the engine's list was computed before an unsubscribe took the symbol away) is not
//! disclosed at all: nothing could ever close it.
//!
//! # A holed book lane waits for its anchor
//!
//! The pump sends the book as an anchor and DELTAS ([`BookUpdate`]), and a lost frame (the bounded queue
//! at either hop; the engine discloses it as a `GapStart`/`Live` pair under the book word, not the link's)
//! leaves a delta stream that no longer applies. The tube has no resync frame, so [`LinkSink`] does the
//! one safe thing locally: from the hole until the next snapshot it WITHHOLDS the symbol's deltas and
//! counts them ([`StratFeed::book_deltas_withheld`]), and [`StratFeed::book_needs_anchor`] says the book
//! the consumer holds is stale. W12's consumer: poll it (or treat the book-word `GapStart` as the same
//! fact) and trust the book again only after an anchor.
//!
//! # Numbers
//!
//! The frame ceilings, the symbol guard and the heartbeat range are `crate::proto`'s `SL_*` (the datahub
//! states them for its own side); the waits of this link are [`StratTimings`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_hub_proto::auth::HubKeys;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::opener::{DataStream, Opener, StreamLimits, StreamTimings};
use vike_market_stream::wire::LINK_STREAM_WORD;
use vike_marketdata::{Bar, BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};
use vike_venues::link_deadman::STRAT_LINK_STREAM;

use crate::client::DatahubClient;
use crate::proto::{
    SL_FRAME_CEILING_BYTES, SL_HEARTBEAT_MAX, SL_HEARTBEAT_MIN, SL_MAX_SYMBOL_BYTES, SL_READ_FLOOR,
    SL_UP_FRAME_CEILING_BYTES,
};

/// How long the open reply may stay silent. It is a pure lookup on the daemon (a table read and a slot
/// reserved; nothing waits on a venue), so a wedged daemon is called dead in seconds.
pub const STRAT_OPEN_REPLY: Duration = Duration::from_secs(15);

/// How long a `subscribe_*` waits for its `SubOk`/`SubErr`. The datahub acks after the hop-one
/// subscribe it forwards returned, which is milliseconds; this bounds a half-open socket, and the
/// tradehub's per-venue feed mutex is held for at most this long.
///
/// **It must be the LONGEST wait of the chain** `tradehub > datahub > gatewayhub`: the datahub's own
/// subscribe at the gatewayhub waits for a dial in flight and then the gatewayhub's ack
/// (`GW_SUB_REPLY_TIMEOUT`, 10 s). The dial wait is `FIRST_CONNECT_WAIT` (2 s, once) for a keyless stream, so
/// 12 s, and the per-opener `GW_CREDENTIALED_FIRST_DIAL_WAIT` (13 s: the venue handshake inside the open) for a
/// credentialed one, so 23 s: 25 s outlasts both. At 10 s the tradehub gave up at about the moment the datahub
/// did and an operator read a bare timeout where the refusal's words were on their way.
/// `crates/vike-datahub/src/strat/limits.rs` holds the relations in `const _`s.
pub const STRAT_SUB_REPLY: Duration = Duration::from_secs(25);

/// The write timeout of the lane socket for its life: a client frame is tens of bytes, so a write
/// blocked this long is a peer that has stopped reading.
pub const STRAT_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The first reconnect delay.
pub const STRAT_RECONNECT_BASE: Duration = Duration::from_millis(250);

/// The longest reconnect delay: a flapping datahub is retried at this cadence for ever.
pub const STRAT_RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Every wait this link makes, in one value a test can narrow. The defaults are the constants above and
/// `crate::proto::SL_READ_FLOOR`; ⚠ they are floors in production's mouth (a window shorter than the
/// daemon's heartbeat tears a healthy lane down), which is why nothing but a test narrows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StratTimings {
    /// The idle bound of the `StratOpen` reply.
    pub open_reply: Duration,
    /// How long a `subscribe_*` waits for its ack.
    pub sub_reply: Duration,
    /// The floor of the read deadline against the next frame: `max(floor, 3 x heartbeat)`.
    pub stream_read_floor: Duration,
    /// The first reconnect delay.
    pub reconnect_base: Duration,
    /// The longest reconnect delay.
    pub reconnect_max: Duration,
}

impl Default for StratTimings {
    fn default() -> Self {
        Self {
            open_reply: STRAT_OPEN_REPLY,
            sub_reply: STRAT_SUB_REPLY,
            stream_read_floor: SL_READ_FLOOR,
            reconnect_base: STRAT_RECONNECT_BASE,
            reconnect_max: STRAT_RECONNECT_MAX,
        }
    }
}

/// A stated heartbeat period as a `Duration`, clamped into
/// [`SL_HEARTBEAT_MIN`]`..=`[`SL_HEARTBEAT_MAX`]: a period outside the range is a daemon bug, and the
/// client runs the nearest legal deadline rather than believe it.
pub fn stated_heartbeat(heartbeat_ms: u64) -> Duration {
    Duration::from_millis(heartbeat_ms).clamp(SL_HEARTBEAT_MIN, SL_HEARTBEAT_MAX)
}

/// An error as a `datahub <addr>: ...` sentence, without doubling a prefix the error already has.
fn datahub_text(addr: &str, e: &dyn std::fmt::Display) -> String {
    let text = e.to_string();
    if text.starts_with("datahub") {
        text
    } else {
        format!("datahub {addr}: strategy lane: {text}")
    }
}

/// The datahub's strategy lane behind one address and one Trade key, as the tube's [`Opener`]: it dials,
/// authenticates under `Scope::Trade`, sends `Request::StratOpen{venue, network}` and reads
/// `Response::StratOpened{session, heartbeat_ms}` - the last positional frame the socket carries.
///
/// `Debug` shows the address, the network and which scopes hold a key, never a key (`HubKeys` redacts).
#[derive(Debug, Clone)]
pub struct StratOpener {
    addr: String,
    keys: Arc<HubKeys>,
    network: Option<String>,
    timings: StratTimings,
}

impl StratOpener {
    /// An opener for the datahub at `addr` (`host:port`), signing with `keys` (which must hold the Trade
    /// key: without it every dial is refused locally, nothing dialled), on the venue's main network.
    pub fn new(addr: impl Into<String>, keys: HubKeys, timings: StratTimings) -> Self {
        Self { addr: addr.into(), keys: Arc::new(keys), network: None, timings }
    }

    /// The same opener for a venue's other network (Hyperliquid's `STRAT_NETWORK_TESTNET`, the one
    /// venue whose feed follows the account tier). `None` is the main network.
    #[must_use]
    pub fn on_network(mut self, network: Option<&str>) -> Self {
        self.network = network.map(str::to_string);
        self
    }
}

impl Opener for StratOpener {
    fn addr(&self) -> &str {
        &self.addr
    }

    fn limits(&self) -> StreamLimits {
        StreamLimits {
            up_frame_ceiling_bytes: SL_UP_FRAME_CEILING_BYTES,
            down_frame_ceiling_bytes: SL_FRAME_CEILING_BYTES,
            max_symbol_bytes: SL_MAX_SYMBOL_BYTES,
        }
    }

    fn timings(&self) -> StreamTimings {
        let t = self.timings;
        StreamTimings {
            sub_reply: t.sub_reply,
            stream_read_floor: t.stream_read_floor,
            reconnect_base: t.reconnect_base,
            reconnect_max: t.reconnect_max,
        }
    }

    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        datahub_text(&self.addr, e)
    }

    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream> {
        let client = DatahubClient::connect_strat_lane(&self.addr, &self.keys, |s| observe(s))?;
        let (info, stream) = client.strat_open(
            venue,
            self.network.as_deref(),
            self.timings.open_reply,
            STRAT_STREAM_WRITE_TIMEOUT,
        )?;
        Ok(DataStream {
            stream,
            session: info.session,
            // The datahub states no epoch on this lane: a restart is a dropped socket, and the engine
            // replays the held set on the next session.
            epoch: 0,
            heartbeat: stated_heartbeat(info.heartbeat_ms),
            delivery_class: info.delivery_class,
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Ledger {
    /// Symbol -> when its strat-link gap OPENED (disclosed under `STRAT_LINK_STREAM`, not yet closed).
    gaps: BTreeMap<String, i64>,
    /// `(symbol, lane word)` whose engine-side link-loss `GapStart` was withheld: the engine's `Live`
    /// for the pair is withheld in turn, and the LAST one of a symbol's pairs closes its strat-link gap
    /// (a lane the peer refuses for good never gets one, so a live sibling cannot mask it).
    withheld: BTreeSet<(String, String)>,
    /// Symbol -> how many subscriptions the feed holds on it (counted BEFORE the engine is asked, and
    /// given back when a subscribe fails or an unsubscribe returns). The engine's link-loss `GapStart` of
    /// a symbol the feed no longer holds is not disclosed: it is the pre-computed list of a loss racing
    /// the unsubscribe that took the symbol away, and a gap opened for it would never get its `Live`.
    holding: BTreeMap<String, usize>,
    /// Symbols whose book lane was HOLED (a `GapStart` under the book word that is not the link's) and
    /// has not been re-anchored since: book DELTAS are withheld until a snapshot arrives.
    book_stale: BTreeSet<String>,
    /// How many book deltas were withheld that way, ever.
    book_deltas_withheld: u64,
}

/// The sink the tube's engine delivers into: every verb is forwarded to the host's sink unchanged,
/// except the link-loss `GapStart` and the `Live` that closes it (the module doc).
///
/// ⚠ **The discriminator is the engine's own state.** `down` answers "is the session marked down
/// right now"; the reader calls `stream_status` for a loss after marking the session down and before it
/// redials, holding no lock, while a status the PEER forwards is only ever read off an open session. So a
/// `GapStart` seen while `down()` is the link's, and one seen while it is not (a forwarded venue status,
/// a hole disclosure, a dead lane) is not, whatever its word. A sink that cannot tell treats nothing as
/// a link loss (`down` unset), which is the engine's plain behaviour.
///
/// ⚠ **It forwards TWELVE verbs by hand, and a verb added to `LiveDataSink` with a default body would
/// compile here and be silently dropped.** `strat_tests.rs`'s `every_sink_verb_is_forwarded` lists them.
pub struct LinkSink {
    inner: Arc<dyn LiveDataSink>,
    down: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    ledger: Mutex<Ledger>,
}

impl LinkSink {
    /// A sink delivering into `inner`.
    pub fn new(inner: Arc<dyn LiveDataSink>) -> Self {
        Self { inner, down: OnceLock::new(), ledger: Mutex::new(Ledger::default()) }
    }

    fn is_down(&self) -> bool {
        self.down.get().is_some_and(|down| down())
    }

    /// How many symbols have a strat-link gap disclosed and not yet closed (a diagnostic).
    pub fn open_gaps(&self) -> usize {
        lock(&self.ledger).gaps.len()
    }

    /// The feed is about to ask the engine for a subscription on `symbol`: count it, so a link-loss
    /// `GapStart` the engine delivers from inside (or right after) the call finds the symbol held.
    fn hold(&self, symbol: &str) {
        *lock(&self.ledger).holding.entry(symbol.to_string()).or_insert(0) += 1;
    }

    /// Give back one count of `symbol` (a subscribe that failed, or an unsubscribe that returned).
    fn unhold(st: &mut Ledger, symbol: &str) {
        if let Some(n) = st.holding.get_mut(symbol) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                st.holding.remove(symbol);
            }
        }
    }

    /// A subscribe the engine refused: give its count back.
    fn hold_failed(&self, symbol: &str) {
        Self::unhold(&mut lock(&self.ledger), symbol);
    }

    /// Close `symbol`'s strat-link gap, if one is open, with the stamp it opened at. The caller holds the
    /// ledger: a `Live` closing a gap must not overtake the `GapStart` it closes, and both are sent under
    /// the one lock.
    fn close_gap(&self, st: &mut Ledger, venue: &str, symbol: &str) {
        if let Some(at) = st.gaps.remove(symbol) {
            self.inner.stream_status(
                venue,
                symbol,
                STRAT_LINK_STREAM,
                StreamStatus::Live { gap_started_ts_ms: Some(at) },
            );
        }
    }

    /// A held subscription of `(symbol, word)` was given up. `others_on_pair` says whether another held
    /// subscription remains on the pair, `others_on_symbol` whether one remains on the symbol that the
    /// engine still serves (a lane it ended is no reason to keep a symbol's gap open). The withheld
    /// `Live` of a pair nobody holds any more is forgotten (a later genuine `Live` for it must pass), and
    /// a symbol nobody serves any more has its open strat-link gap CLOSED here and now: the engine drops
    /// an unsubscribed lane from its replay and says nothing, so without this the dead-man's window for
    /// the key would stay open until it tripped. A gap whose remaining lanes have ALL been
    /// re-acknowledged is closed too (the given-up lane was the one still owed).
    fn released(
        &self,
        venue: &str,
        symbol: &str,
        word: &str,
        others_on_pair: bool,
        others_on_symbol: bool,
    ) {
        let mut st = lock(&self.ledger);
        Self::unhold(&mut st, symbol);
        if !others_on_pair {
            st.withheld.remove(&(symbol.to_string(), word.to_string()));
        }
        if !others_on_symbol {
            st.withheld.retain(|(s, _)| s != symbol);
            st.book_stale.remove(symbol);
            self.close_gap(&mut st, venue, symbol);
        } else if !st.withheld.iter().any(|(s, _)| s == symbol) {
            self.close_gap(&mut st, venue, symbol);
        }
    }

    /// The feed is going away: close every open strat-link gap. A removed feed has no lane left to be in
    /// doubt about, and a window left open for it would trip the dead-man on a feed nobody reads (the
    /// gap-closure invariant: a `GapStart` is followed by its `Live` or by process exit). Idempotent.
    fn close_all(&self, venue: &str) {
        let mut st = lock(&self.ledger);
        st.withheld.clear();
        st.book_stale.clear();
        st.holding.clear();
        let symbols: Vec<String> = st.gaps.keys().cloned().collect();
        for symbol in symbols {
            self.close_gap(&mut st, venue, &symbol);
        }
    }

    /// Whether `symbol`'s book lane was holed and no anchor has arrived since.
    fn book_stale(&self, symbol: &str) -> bool {
        lock(&self.ledger).book_stale.contains(symbol)
    }

    /// How many book deltas were withheld while a lane waited for its anchor, ever.
    fn book_deltas_withheld(&self) -> u64 {
        lock(&self.ledger).book_deltas_withheld
    }
}

impl LiveDataSink for LinkSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }

    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }

    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }

    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }

    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }

    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        // A whole book is a re-anchor, whoever sent it.
        lock(&self.ledger).book_stale.remove(symbol);
        self.inner.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }

    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.inner.quote(venue, symbol, quote);
    }

    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.inner.trade(venue, symbol, trade);
    }

    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        // A whole folded book is a re-anchor, whoever sent it.
        lock(&self.ledger).book_stale.remove(symbol);
        self.inner.book(venue, symbol, book);
    }

    /// An ANCHOR (`BookUpdateKind::Snapshot`) always passes and re-bases the lane; a DELTA passes unless
    /// the lane was holed since its last anchor, in which case it is withheld and counted: applied to a
    /// book that is missing a frame it would silently corrupt it, and the hole was disclosed already (the
    /// book lane's `GapStart`/`Live` pair, [`StratFeed::book_needs_anchor`]).
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        {
            let mut st = lock(&self.ledger);
            if update.kind == BookUpdateKind::Snapshot {
                st.book_stale.remove(symbol);
            } else if st.book_stale.contains(symbol) {
                st.book_deltas_withheld += 1;
                return;
            }
        }
        self.inner.book_update(venue, symbol, update);
    }

    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        match status {
            StreamStatus::GapStart { .. }
                if !self.is_down() && stream == StreamLabel::Book.as_str() =>
            {
                // A hole in the book lane that is not the link's: a delta may be missing, and the tube
                // has no resync frame, so deltas wait for the next anchor.
                lock(&self.ledger).book_stale.insert(symbol.to_string());
                self.inner.stream_status(venue, symbol, stream, status);
            }
            StreamStatus::GapStart { at_ts_ms } if self.is_down() => {
                let mut st = lock(&self.ledger);
                if !st.holding.contains_key(symbol) {
                    return;
                }
                st.withheld.insert((symbol.to_string(), stream.to_string()));
                if !st.gaps.contains_key(symbol) {
                    st.gaps.insert(symbol.to_string(), at_ts_ms);
                    // Under the lock: a `Live` closing this gap (an unsubscribe on a caller's thread)
                    // must not overtake the `GapStart` it closes.
                    self.inner.stream_status(
                        venue,
                        symbol,
                        STRAT_LINK_STREAM,
                        StreamStatus::GapStart { at_ts_ms },
                    );
                }
            }
            StreamStatus::Live { .. } => {
                let mut st = lock(&self.ledger);
                if st.withheld.remove(&(symbol.to_string(), stream.to_string())) {
                    // The gap closes at the symbol's LAST owed lane: a sibling lane that came back
                    // cannot vouch for one the peer refused for good.
                    if !st.withheld.iter().any(|(s, _)| s == symbol) {
                        self.close_gap(&mut st, venue, symbol);
                    }
                } else {
                    drop(st);
                    self.inner.stream_status(venue, symbol, stream, status);
                }
            }
            _ => self.inner.stream_status(venue, symbol, stream, status),
        }
    }

    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
}

/// A `vike_data::DataClient` over the datahub's strategy lane for ONE venue (and network): the tradehub
/// holds one per distinct venue. See the module doc.
///
/// Same contract as the gatewayhub's remote client, because it is the same engine: it never blocks the
/// caller's mutex (a down link answers a transient `Err` with no wait; the only waits are the peer's ack
/// and one in-flight first dial), construction never dials, the held set replays under its original ids
/// after a reconnect, `begin_shutdown` is a frame, and `Unsupported(&'static str)` crosses by interning.
pub struct StratFeed {
    client: RemoteDataClient,
    sink: Arc<LinkSink>,
    /// The subscriptions this feed holds: id -> `(symbol, lane word)`. The pair is what the sink's ledger
    /// is keyed on, so a release knows what the symbol still has.
    held: HashMap<u64, (String, String)>,
}

impl std::fmt::Debug for StratFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StratFeed").field("client", &self.client).finish_non_exhaustive()
    }
}

impl StratFeed {
    /// A feed for `venue` delivering into `sink`. **Never blocks and never dials on the caller's thread**
    /// (the engine's reader dials), so a datahub that is down at construction is a link that is down, not
    /// a failed constructor. The only `Err` is the OS refusing a thread.
    pub fn new(opener: StratOpener, venue: &str, sink: Arc<dyn LiveDataSink>) -> io::Result<Self> {
        let sink = Arc::new(LinkSink::new(sink));
        let client =
            RemoteDataClient::new(opener, venue, Arc::clone(&sink) as Arc<dyn LiveDataSink>)?;
        let probe = client.link_probe();
        // The engine's reader may already be running; a status it delivers before this line is set is
        // not a link loss (nothing was ever up), and `is_down` reads `false` until then.
        let _ = sink.down.set(Box::new(move || !probe.is_up()));
        Ok(Self { client, sink, held: HashMap::new() })
    }

    /// The venue this feed serves.
    pub fn venue(&self) -> &str {
        self.client.venue()
    }

    /// Whether a session is open and acknowledged right now.
    pub fn is_up(&self) -> bool {
        self.client.is_up()
    }

    /// How many symbols have a strat-link gap disclosed and not yet closed.
    pub fn open_link_gaps(&self) -> usize {
        self.sink.open_gaps()
    }

    /// **Does `symbol`'s book lane wait for an anchor?** `true` from the moment a hole in the lane was
    /// disclosed (a `GapStart` under the book word that is not this link's: a delta may be missing, and the
    /// tube has no resync frame) until a snapshot of it arrives. While it is `true` every DELTA of the
    /// symbol is withheld ([`StratFeed::book_deltas_withheld`]), so the consumer's book is never advanced
    /// past the hole: it is stale (its own last anchor plus the deltas before the hole), not corrupt. The
    /// sender anchors at once when it can see the loss (the gatewayhub's session queue) and otherwise on
    /// its next periodic anchor, at most `ANCHOR_EVERY` (2 s) after the book next changes.
    pub fn book_needs_anchor(&self, symbol: &str) -> bool {
        self.sink.book_stale(symbol)
    }

    /// How many book deltas this feed withheld while a lane waited for its anchor, ever.
    pub fn book_deltas_withheld(&self) -> u64 {
        self.sink.book_deltas_withheld()
    }

    /// **The CEX tick pump for `symbol`, sized to `tick_size`** (the tube's `SubOp::Ticks`): not a
    /// `DataClient` verb, so inherent here as it is on the engine. A `tick_size` the tube refuses is an
    /// `Err` before anything is framed.
    pub fn subscribe_ticks(
        &mut self,
        symbol: &str,
        tick_size: f64,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.sink.hold(symbol);
        let r = self.client.subscribe_ticks(symbol, tick_size);
        self.track(r, symbol, LINK_STREAM_WORD)
    }

    fn track(
        &mut self,
        r: Result<SubscriptionId, LiveDataError>,
        symbol: &str,
        word: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        match &r {
            Ok(id) => {
                self.held.insert(id.0, (symbol.to_string(), word.to_string()));
            }
            Err(_) => self.sink.hold_failed(symbol),
        }
        r
    }
}

impl Drop for StratFeed {
    /// A feed dropped without `shutdown` closes its open gaps too (idempotent after one that did).
    fn drop(&mut self) {
        self.sink.close_all(self.client.venue());
    }
}

impl DataClient for StratFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.sink.hold(symbol);
        let r = self.client.subscribe_bars(symbol, interval);
        self.track(r, symbol, interval)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sink.hold(symbol);
        let r = self.client.subscribe_quotes(symbol);
        self.track(r, symbol, StreamLabel::Quotes.as_str())
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sink.hold(symbol);
        let r = self.client.subscribe_trades(symbol);
        self.track(r, symbol, StreamLabel::Trades.as_str())
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.sink.hold(symbol);
        let r = self.client.subscribe_book(symbol);
        self.track(r, symbol, StreamLabel::Book.as_str())
    }

    /// A depth lane is the conflating snapshot lane, and a strategy that asked for depth SNAPSHOTS on a
    /// trading link cannot take a superseded one: the lane's class is raised to `Lossless` BEFORE the
    /// subscribe, so the floor rides the `Sub` (and every replay of it). Only this verb raises anything.
    /// A daemon that does not advertise the delivery class cannot honour the floor: that is said at
    /// `warn` and the subscription runs conflated, as the feed broker does for any client.
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if let Err(e) = self.client.set_delivery(symbol, StreamLabel::Depth, Delivery::Lossless) {
            tracing::warn!(
                venue = %self.client.venue(),
                symbol,
                error = %e,
                "datahub strategy lane: depth snapshots cannot be made lossless on this link; \
                 the lane runs conflated"
            );
        }
        self.sink.hold(symbol);
        let r = self.client.subscribe_depth(symbol);
        self.track(r, symbol, StreamLabel::Depth.as_str())
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.client.unsubscribe(id);
        if let Some((symbol, word)) = self.held.remove(&id.0) {
            let others_on_pair = self.held.values().any(|(s, w)| *s == symbol && *w == word);
            // ⚠ A lane the engine ended on its own (`lane_died`: a callback panicked) is still in `held`
            // and the engine cannot be asked about it (`is_dead` is false for an id it removed). That is
            // harmless to the gap: the sink closes a symbol's gap when NOTHING is owed (every pair it
            // withheld a `Live` for is re-acknowledged or given up), and the ended lane was never owed.
            let others_on_symbol = self.held.values().any(|(s, _)| *s == symbol);
            self.sink.released(
                self.client.venue(),
                &symbol,
                &word,
                others_on_pair,
                others_on_symbol,
            );
        }
    }

    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.client.set_delivery(symbol, lane, class)
    }

    fn is_dead(&self, id: SubscriptionId) -> bool {
        self.client.is_dead(id)
    }

    fn begin_shutdown(&mut self) {
        self.client.begin_shutdown();
    }

    /// Stops the engine, then closes every strat-link gap still open: a feed being torn down (a hot
    /// reload, a disarm) has no lane left to be in doubt about, and the dead-man's window for it would
    /// otherwise stay open until it tripped on a feed nobody reads.
    fn shutdown(&mut self) {
        self.client.shutdown();
        self.sink.close_all(self.client.venue());
    }
}

#[cfg(test)]
#[path = "strat_tests.rs"]
mod strat_tests;
