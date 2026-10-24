//! The CEX TICK PUMP, hosted at the gatewayhub (wave 2, W10): `spawn_binance_market_data` and its
//! bybit, okx and aster twins, which the tradehub used to run in its own process.
//!
//! A pump does not speak `LiveDataSink`. It pushes `Ingest::Quote/Trade/Book/StreamStatus` onto a
//! [`vike_exec::TickSender`], blocking when the channel is full (`TickSender`'s own contract: a
//! lossless lane that back-pressures its feed). The gatewayhub hosts one against a channel it DRAINS,
//! and the drain ([`Translator`]) turns each message into the sink verb of the same name, so the
//! session's wire sink (`vike_market_stream::sink::WireSink`) does the rest: a `seq` per verb, the
//! bounded queue, a `Gap` before a survivor, a lapse budget. Nothing here blocks on a consumer: a sink
//! call is lock-and-enqueue, so a stalled tradehub stalls its own session and never the pump.
//!
//! # What crosses, message by message
//!
//! | pump message | sink verb | notes |
//! |---|---|---|
//! | `Quote` | `quote` | the tick as the pump built it |
//! | `Trade` | `trade` | the print as the pump built it |
//! | `Book` | `book_update` | an ANCHOR (`Snapshot`) or a DELTA, see below |
//! | `StreamStatus` | `stream_status` on the `"ticks"` link word | `FeedStatus` mapped back to a `StreamStatus` |
//! | `DataDelay` | `data_delay` | |
//! | anything else | dropped, debug-logged | a pump sends nothing else today |
//!
//! Every frame is stamped with the symbol the SUBSCRIPTION named, not the one the pump labelled the
//! tick with: the wire sink binds a key per symbol, and a pump that spelled it differently (a case, a
//! `.P`) would otherwise bind a second, unsolicited key the subscriber never asked for.
//!
//! # The book: an anchor, then deltas
//!
//! The pump keeps ONE standing [`L2Book`] and sends the whole folded state behind an `Arc` on every
//! change. Across a hop that is up to a thousand levels a side, ten times a second. [`BookFeed`] sends
//! the top [`PUMP_BOOK_LEVELS`] levels a side once as a `Snapshot` anchor and then only the levels that
//! CHANGED as `Delta` updates (a removed level is `qty == 0`), so the subscriber rebuilds the book with
//! `L2Book::apply_snapshot` / `apply_delta`. A fresh anchor is sent at the start, after a link loss,
//! every [`ANCHOR_EVERY_UPDATES`] deltas or [`ANCHOR_EVERY`], whichever comes first, when the tick grid
//! changes and when a book's `seq` goes backwards; so a subscriber that lost a delta (a disclosed
//! `Gap`) waits at most one period for state it can trust again. The diff is by CONTENT, so it needs
//! nothing from the pump but the book.
//!
//! A delta is sent only when it is SMALLER than the anchor (fewer levels), so a delta is never bigger than
//! the biggest frame an anchor can be and always fits the datahub's frame ceiling when an anchor does: a
//! full reshuffle of a deep book is sent as the anchor it amounts to.
//!
//! **A lost frame is repaired at once, not after a period.** The session's sink counts the book frames it
//! lost (`WireSink::book_losses`, handed in as a [`BookLosses`]); the hub compares the figure after every
//! pump message, and when it moved the subscriber's book is no longer the one this feed last encoded, so
//! the feed is reset and the standing book goes out as an anchor immediately (a quiet book included). A
//! loss at the NEXT hop (the datahub's queue towards the tradehub) is not seen from here: the tradehub's
//! `StratFeed` withholds deltas until the next anchor and says so (`crates/vike-datahub-client/CLAUDE.md`).
//!
//! # One pump, many sessions ([`PumpHub`])
//!
//! A pump bypasses the datahub's feed broker (it is not a `DataClient` verb), so two sessions naming the
//! same `(venue, symbol, tick_size)` would each start a venue connection. The hub shares: the first
//! subscriber starts the pump and its drain thread, later ones join the fan-out (and are told the
//! current book and a standing disconnect at once), and the last one's departure stops it. Two
//! different tick sizes are two pumps: the book grid differs. At most [`MAX_PUMPS`] pumps run at once.
//!
//! ⚠ **A lease drop never blocks.** The session reader drops a [`PumpLease`] on an `Unsub`, and a pump
//! thread can take seconds to join (a dial in flight), so the last holder hands the join to a short-lived
//! reaper thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::LiveDataSink;
use vike_exec::{Ingest, TickSender, event_channel};
use vike_market_stream::market::CAP_MARK;
use vike_market_stream::wire::LINK_STREAM_WORD;
use vike_marketdata::{BookLevel, BookUpdate, BookUpdateKind, L2Book};
use vike_model::{AccountTier, FeedStatus, StreamStatus, now_ms};

use super::{BookLosses, PumpLease, PumpSink};
use crate::registry::{Lane, roster};

/// How many pump messages may wait for the drain. The drain only enqueues, so this is slack for a
/// scheduling pause, not a buffer a consumer can fill.
pub const PUMP_CHANNEL_CAPACITY: usize = 4096;

/// The most pumps (distinct `(venue, symbol, tick_size)`) the daemon runs at once: each is a venue
/// websocket and two threads. A START value. A pump being STOPPED still counts until its reaper has
/// joined it ([`PumpHub::reaping`]).
///
/// **16 pumps against the datahub lane's 128 keys (`SL_MAX_KEYS_TOTAL`) is deliberate, not a mismatch to
/// reconcile:** only the four CEX venues have a pump, and a tradehub mounts ONE symbol per venue (the
/// engine accepts exactly its mounted symbol, `wired_symbol`) on one grid, so a strategy set needs about
/// four of them, and the sessions of one grid share one. The lane's 128 counts every key of every lane
/// (bars, quotes, trades, book, ticks) of every venue. The 17th distinct `(venue, symbol, grid)` is a
/// transient refusal the engine retries; there is no per-key quota here (a Read-scope key can still open
/// sessions and fill the slots: the loopback-only exposure is the review's finding 6, listed under the
/// doubts, not guarded).
pub const MAX_PUMPS: usize = 16;

/// The levels a side of an anchor or delta carries: the same figure as the datahub's
/// `SL_BOOK_LEVELS_CEILING`, so a frame is bounded the way the strategy lane's frames are
/// (`vike_datahub_client::proto`; this crate does not name that one).
pub const PUMP_BOOK_LEVELS: usize = 1000;

/// A fresh anchor after this many deltas. A START value.
pub const ANCHOR_EVERY_UPDATES: u32 = 64;

/// A fresh anchor after this long since the last one, when the book changed. A START value.
pub const ANCHOR_EVERY: Duration = Duration::from_secs(2);

/// How often a pump's anchor timer looks at its members ([`Shared::anchor_quiet`]): a due anchor goes out at most
/// this much after it is due, with no pump message needed. An eighth of the period, so a quiet book is within
/// 2.25 s of its last anchor at the latest.
pub const ANCHOR_TICK: Duration = Duration::from_millis(250);

/// How old the pump's standing book may be and still be re-sent by the anchor timer as a fresh anchor: ten
/// anchor periods. The timer exists to repair a quiet book whose subscriber lost a frame; it must not make a pump
/// that went SILENT (a half-open socket that never said `Disconnected`, or a pump that said `Stale`) look like a
/// healthy quiet book, because every anchor reaches the tradehub's strategy as a fresh `on_order_book` and counts
/// as liveness for the opt-in silence dead-man. Past this age the timer sends nothing on the clock alone; a
/// subscriber whose session LOST a book frame is still anchored (that repair is the point of the timer, and a quiet
/// healthy book is exactly where it is needed), and a pump that says `Stale` is never anchored from.
pub const STANDING_BOOK_MAX_AGE: Duration = Duration::from_secs(20);

/// The tier the aster pump is built on: `Live`, the mainnet futures hosts. The tradehub pinned the same
/// (`spawn_cex_tick_pump`): a demo or paper order must rest against the real book, and the tier picks
/// hosts only.
const ASTER_PUMP_TIER: AccountTier = AccountTier::Live;

/// A running venue pump, as the hub sees it: a stop flag and an owner whose drop joins the thread.
pub struct PumpHandle {
    stop: Box<dyn Fn() + Send + Sync>,
    _guard: Box<dyn Send + Sync>,
}

impl PumpHandle {
    /// Wrap a bridge's stop handle (`vike_bridge_core::poller::StopHandle`, whose type this crate does
    /// not name): `stop` raises its flag without waiting, dropping `handle` joins the thread.
    pub fn new<H: Send + Sync + 'static>(handle: H, stop: fn(&H)) -> Self {
        let handle = Arc::new(handle);
        let for_stop = Arc::clone(&handle);
        Self { stop: Box::new(move || stop(&for_stop)), _guard: Box::new(handle) }
    }

    fn signal(&self) {
        (self.stop)();
    }
}

/// How the hub starts the venue pump of `(venue, symbol, tick_size)` on `ticks`. A loopback test passes
/// a scripted one.
pub type PumpSpawner =
    Box<dyn Fn(&str, &str, f64, TickSender) -> Result<PumpHandle, String> + Send + Sync>;

/// The production table: one arm per venue of `crate::registry`'s pump roster (the CEX `spawn_*_market_data` feeds), each the constructor the tradehub called
/// (`crates/vike-tradehub/src/feeds/cex.rs`'s `spawn_cex_tick_pump`), MOVED.
pub fn real_pump_table() -> PumpSpawner {
    Box::new(|venue, symbol, tick_size, ticks| match venue {
        "binance" => Ok(PumpHandle::new(
            vike_binance::market_data::spawn_binance_market_data(ticks, symbol, tick_size),
            |h| h.stop(),
        )),
        "bybit" => Ok(PumpHandle::new(
            vike_bybit::market_data::spawn_bybit_market_data(ticks, symbol, tick_size),
            |h| h.stop(),
        )),
        "okx" => Ok(PumpHandle::new(
            vike_okx::market_data::spawn_okx_market_data(ticks, symbol, tick_size),
            |h| h.stop(),
        )),
        // The `.P` symbol goes in whole: the pump splits it to the exchange's spelling for the wire and
        // labels every emitted tick with the caller's own (`vike_aster::market_data`'s `pump_wire`).
        "aster" => Ok(PumpHandle::new(
            vike_aster::market_data::spawn_aster_market_data(
                ASTER_PUMP_TIER,
                ticks,
                symbol,
                tick_size,
            ),
            |h| h.stop(),
        )),
        other => Err(format!(
            "gatewayhub: venue `{other}` has no tick pump. The pump is the CEX feed of [{}]",
            roster(Lane::Pump).join(", ")
        )),
    })
}

// ---- The book: anchor plus deltas ------------------------------------------------------------------

/// What [`BookFeed`] last sent a subscriber: the state its book holds now.
#[derive(Debug, Clone)]
struct Sent {
    tick_size: f64,
    seq: u64,
    bids: Vec<BookLevel>,
    asks: Vec<BookLevel>,
}

/// The per-subscriber book encoder. See the module doc.
#[derive(Debug, Default)]
pub struct BookFeed {
    sent: Option<Sent>,
    since_anchor: u32,
    anchored: Option<Instant>,
}

impl BookFeed {
    /// A feed that has sent nothing: its first update is an anchor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget what was sent, so the next book goes out as an anchor (a link loss: the subscriber's book
    /// is no longer the pump's).
    pub fn reset(&mut self) {
        self.sent = None;
    }

    /// Whether the periodic anchor is due although the book did not change: something has been sent, and the
    /// last ANCHOR is [`ANCHOR_EVERY`] old at `now`. [`BookFeed::next`] anchors on that timer only when the
    /// top levels changed; a QUIET book needs this, because a subscriber that lost a delta at the next hop
    /// (invisible from here) holds a book that nothing but an anchor corrects.
    pub fn anchor_due(&self, now: Instant) -> bool {
        self.sent.is_some() && self.anchored.is_some_and(|t| now.duration_since(t) >= ANCHOR_EVERY)
    }

    /// The update that takes a subscriber from what it holds to `book`, or `None` when nothing it can
    /// see changed. `ts` stamps the update (the pump's book carries no stamp).
    pub fn next(
        &mut self,
        book: &L2Book,
        now: Instant,
        ts: i64,
        symbol: &str,
    ) -> Option<BookUpdate> {
        let (bids, asks) = book.top_n(PUMP_BOOK_LEVELS);
        let seq = book.last_seq;
        let mut kind = BookUpdateKind::Snapshot;
        let (mut out_bids, mut out_asks) = (None, None);
        if let Some(sent) = &self.sent
            && sent.tick_size == book.tick_size
        {
            let db = diff_side(&sent.bids, &bids, true);
            let da = diff_side(&sent.asks, &asks, false);
            if db.is_empty() && da.is_empty() {
                return None;
            }
            // `seq == 0` is "the venue supplied no sequence": `apply_delta` takes it always.
            let in_order = seq == 0 || seq > sent.seq;
            let timer_due = self.anchored.is_none_or(|t| now.duration_since(t) >= ANCHOR_EVERY);
            // A delta is only worth sending when it is SMALLER than the anchor it replaces: a full
            // reshuffle of a deep book would otherwise be a frame bigger than any anchor, over the
            // datahub's frame ceiling, dropped as a hole nobody repairs.
            let smaller = db.len() + da.len() < bids.len() + asks.len();
            if in_order && smaller && self.since_anchor < ANCHOR_EVERY_UPDATES && !timer_due {
                kind = BookUpdateKind::Delta;
                (out_bids, out_asks) = (Some(db), Some(da));
            }
        }
        let (bids_out, asks_out) = match (out_bids, out_asks) {
            (Some(b), Some(a)) => (b, a),
            _ => (bids.clone(), asks.clone()),
        };
        if kind == BookUpdateKind::Snapshot {
            self.since_anchor = 0;
            self.anchored = Some(now);
        } else {
            self.since_anchor += 1;
        }
        self.sent = Some(Sent { tick_size: book.tick_size, seq, bids, asks });
        Some(BookUpdate {
            ts,
            local_ts: ts,
            seq,
            kind,
            tick_size: book.tick_size,
            bids: bids_out,
            asks: asks_out,
            symbol: symbol.to_string(),
        })
    }
}

/// The levels of `new` that differ from `old`, both sorted best first (`descending`: bids), with a
/// removed level as `qty == 0`. Prices are compared exactly: both sides come off one tick grid.
pub fn diff_side(old: &[BookLevel], new: &[BookLevel], descending: bool) -> Vec<BookLevel> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        let (o, n) = (old[i], new[j]);
        let mut order = o.price.partial_cmp(&n.price).unwrap_or(Equal);
        if descending {
            order = order.reverse();
        }
        match order {
            Equal => {
                if o.qty != n.qty {
                    out.push(n);
                }
                i += 1;
                j += 1;
            }
            // `old` is behind `new` in the book's order: that level is gone.
            Less => {
                out.push(BookLevel::new(o.price, 0.0));
                i += 1;
            }
            Greater => {
                out.push(n);
                j += 1;
            }
        }
    }
    out.extend(old[i..].iter().map(|o| BookLevel::new(o.price, 0.0)));
    out.extend_from_slice(&new[j..]);
    out
}

// ---- The drain: pump messages to sink verbs --------------------------------------------------------

/// The `StreamStatus` a pump's `FeedStatus` disclosure is on the wire. `FeedStatus` is the lossy half
/// of the pair (`StreamStatus::feed_status`), so the stamps are the drain's: a `Disconnected` opens a gap
/// now, the `Live` after it echoes that stamp, and a `Stale` states the last stamp seen.
fn stream_status_of(feed: FeedStatus, gap: &mut Option<i64>, now: i64) -> StreamStatus {
    match feed {
        // The FIRST disconnect of an outage opens the gap and a repeat restates that stamp: moving it (a
        // crash disclosed after the bridge's own `Disconnected`) would move the stamp the `Live` echoes.
        FeedStatus::Disconnected => {
            let at = *gap.get_or_insert(now);
            StreamStatus::GapStart { at_ts_ms: at }
        }
        FeedStatus::Stale => {
            StreamStatus::Stale { newest_data_ts_ms: gap.unwrap_or(now), now_ms: now }
        }
        FeedStatus::Live => StreamStatus::Live { gap_started_ts_ms: gap.take() },
    }
}

/// One subscriber's translation of the pump's messages: its own book encoder and link stamp.
pub struct Translator {
    venue: String,
    symbol: String,
    book: BookFeed,
    gap: Option<i64>,
}

impl Translator {
    /// A translator for `venue`'s `symbol` (the subscription's spelling).
    pub fn new(venue: &str, symbol: &str) -> Self {
        Self {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            book: BookFeed::new(),
            gap: None,
        }
    }

    /// Deliver one pump message into `sink`.
    pub fn deliver(&mut self, sink: &dyn LiveDataSink, message: &Ingest, now: Instant) {
        let Self { venue, symbol, book, gap } = self;
        match message {
            Ingest::Quote(q) => sink.quote(venue, symbol, q.quote.clone()),
            Ingest::Trade(t) => sink.trade(venue, symbol, t.trade.clone()),
            Ingest::Book(b) => send_book(book, sink, venue, symbol, &b.book, now),
            Ingest::StreamStatus(s) => {
                let status = stream_status_of(s.status, gap, now_ms());
                if s.status == FeedStatus::Disconnected {
                    book.reset();
                }
                sink.stream_status(venue, symbol, LINK_STREAM_WORD, status);
            }
            Ingest::DataDelay(d) => sink.data_delay(venue, symbol, &d.stream, d.delay),
            other => tracing::debug!(
                message = ?std::mem::discriminant(other),
                "vike-gatewayhub pump: a message a pump does not send; dropped"
            ),
        }
    }

    /// Deliver the pump's standing `book` (also how a late joiner is told the current state).
    pub fn deliver_book(&mut self, sink: &dyn LiveDataSink, book: &L2Book, now: Instant) {
        let Self { venue, symbol, book: feed, .. } = self;
        send_book(feed, sink, venue, symbol, book, now);
    }

    /// Forget what the subscriber was sent, so the next book goes out as an anchor.
    pub fn reset_book(&mut self) {
        self.book.reset();
    }

    /// Whether the subscriber is owed its periodic anchor at `now` ([`BookFeed::anchor_due`]).
    pub fn anchor_due(&self, now: Instant) -> bool {
        self.book.anchor_due(now)
    }

    /// Tell `sink` the link is UP (a late joiner onto a pump that has spoken and is not disconnected):
    /// `Live` with no gap to close. The joiner may be a session that replaced one whose last word was this
    /// pump's `GapStart`, and nothing else would ever close the window that word opened.
    pub fn disclose_up(&mut self, sink: &dyn LiveDataSink) {
        let status = stream_status_of(FeedStatus::Live, &mut self.gap, now_ms());
        sink.stream_status(&self.venue, &self.symbol, LINK_STREAM_WORD, status);
    }

    /// Tell `sink` the link is down (a late joiner onto a pump that is disconnected, or a pump that
    /// ended).
    pub fn disclose_down(&mut self, sink: &dyn LiveDataSink) {
        let status = stream_status_of(FeedStatus::Disconnected, &mut self.gap, now_ms());
        self.book.reset();
        sink.stream_status(&self.venue, &self.symbol, LINK_STREAM_WORD, status);
    }
}

/// One book message through `feed` into `sink`.
fn send_book(
    feed: &mut BookFeed,
    sink: &dyn LiveDataSink,
    venue: &str,
    symbol: &str,
    book: &L2Book,
    now: Instant,
) {
    if let Some(update) = feed.next(book, now, now_ms(), symbol) {
        sink.book_update(venue, symbol, update);
    }
}

// ---- The hub: one pump per key, shared ---------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PumpKey {
    venue: String,
    symbol: String,
    tick_bits: u64,
}

struct Member {
    id: u64,
    sink: Arc<dyn LiveDataSink>,
    translator: Translator,
    /// How many book frames the member's session has lost (`None`: not tracked), and the figure last seen.
    losses: Option<BookLosses>,
    seen_losses: u64,
}

impl Member {
    /// Whether the session lost a book frame since the figure was last read, reading it again.
    fn losses_moved(&mut self) -> bool {
        let Some(losses) = &self.losses else { return false };
        let total = losses();
        let moved = total != self.seen_losses;
        self.seen_losses = total;
        moved
    }

    /// The periodic anchor of a QUIET book, on the clock: when the member's last anchor is [`ANCHOR_EVERY`] old,
    /// the pump's standing book goes out again as an anchor (the same repair `deliver` makes on a pump message).
    ///
    /// `fresh` is whether the standing book is young enough to stand for the pump's present
    /// ([`STANDING_BOOK_MAX_AGE`]). A book past it is re-sent only to a member whose session LOST a frame since it
    /// was last looked at: the repair this timer exists for, and not a claim that the pump is alive.
    fn anchor_quiet(&mut self, standing: &L2Book, now: Instant, fresh: bool) {
        let lost = self.losses_moved();
        if lost || (fresh && self.translator.anchor_due(now)) {
            self.translator.reset_book();
            self.translator.deliver_book(self.sink.as_ref(), standing, now);
            self.losses_moved();
        }
    }

    /// Deliver one pump message, and repair the subscriber's book when its session lost a frame of it.
    ///
    /// `standing` is the pump's current book (`None` while the link is down: a stale book is not sent).
    /// A loss seen BEFORE the message makes the message's own book (when it is one) the anchor; a loss
    /// this very push caused (the queue evicts the oldest frame of the pushing key) is repaired right
    /// after, from the standing book, so a quiet book does not wait for its next change. The figure is
    /// read again after the anchor, so the older frames the anchor's own push evicts (superseded by it)
    /// do not ask for another.
    fn deliver(&mut self, message: &Ingest, standing: Option<&L2Book>, now: Instant) {
        let mut resync = self.losses_moved();
        if resync {
            self.translator.reset_book();
        }
        self.translator.deliver(self.sink.as_ref(), message, now);
        if matches!(message, Ingest::Book(_)) {
            resync = false;
        }
        resync |= self.losses_moved();
        // The periodic anchor of a QUIET book: its top levels did not change, so `BookFeed::next` sent
        // nothing, and a loss at the datahub's hop (not visible from here) would leave the subscriber with a
        // book that is wrong until the book next moves. Any pump message past the period brings it. Not while
        // there is no standing book to send (the link is down, or the pump says `Stale`): resetting the feed then
        // would stop the timer, which needs something sent, from ever anchoring this member again.
        resync |= standing.is_some() && self.translator.anchor_due(now);
        if resync {
            self.translator.reset_book();
            if let Some(book) = standing {
                self.translator.deliver_book(self.sink.as_ref(), book, now);
            }
            self.losses_moved();
        }
    }
}

#[derive(Default)]
struct Inner {
    members: Vec<Member>,
    /// The pump's standing book as last sent, for a late joiner.
    last_book: Option<Arc<L2Book>>,
    /// Whether the pump last disclosed a disconnect and has not recovered.
    link_down: bool,
    /// When `last_book` was last REFRESHED by the pump (a book message), for the anchor timer's staleness bound
    /// ([`STANDING_BOOK_MAX_AGE`]).
    book_at: Option<Instant>,
    /// Whether the pump last said `Stale` and has not spoken since: the standing book is then a book the pump
    /// itself calls old, and the anchor timer sends nothing.
    stale: bool,
    /// Whether the pump has spoken (a tick, a book or a `Live`) since it was last disconnected: a joiner
    /// onto it is told the link is up. A pump that has not spoken has nothing to restate.
    heard: bool,
}

struct Shared {
    key: PumpKey,
    inner: Mutex<Inner>,
    /// Set before the pump is stopped on purpose, so the drain's end is not read as a crash.
    stopping: AtomicBool,
    handle: Mutex<Option<PumpHandle>>,
    drain: Mutex<Option<JoinHandle<()>>>,
}

/// The shared tick pumps of one daemon. See the module doc.
pub struct PumpHub {
    spawner: PumpSpawner,
    max: usize,
    live: Mutex<HashMap<PumpKey, Arc<Shared>>>,
    next_member: AtomicU64,
    /// Pumps that left `live` and are still being stopped and joined (a dial in flight can take
    /// seconds): each is still a venue socket and a thread until the reaper is done, so they count
    /// against the cap beside the live ones.
    reaping: Arc<AtomicUsize>,
}

impl std::fmt::Debug for PumpHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpHub").field("pumps", &lock(&self.live).len()).finish_non_exhaustive()
    }
}

/// The reconcile health gate's evidence (`crate::health`, R31): a venue is degraded while any of its
/// running pumps last disclosed a disconnect and has not recovered (`Inner::link_down`, the flag the
/// pump already keeps for its joiners).
impl crate::health::VenueHealth for PumpHub {
    fn degraded(&self, venue: &str) -> Option<String> {
        let pumps: Vec<Arc<Shared>> = lock(&self.live).values().cloned().collect();
        pumps
            .iter()
            .any(|p| p.key.venue == venue && lock(&p.inner).link_down)
            .then(|| crate::health::degraded_reason(venue))
    }
}

impl PumpHub {
    /// A hub that starts pumps with `spawner`, at most [`MAX_PUMPS`] at once.
    pub fn new(spawner: PumpSpawner) -> Arc<Self> {
        Self::with_max(spawner, MAX_PUMPS)
    }

    /// [`PumpHub::new`] with a cap of `max` pumps (a test's).
    pub fn with_max(spawner: PumpSpawner, max: usize) -> Arc<Self> {
        Arc::new(Self {
            spawner,
            max,
            live: Mutex::new(HashMap::new()),
            next_member: AtomicU64::new(1),
            reaping: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// How many pumps run now.
    pub fn running(&self) -> usize {
        lock(&self.live).len()
    }

    /// How many pumps have left the table and are still being stopped (counted by the cap).
    pub fn reaping(&self) -> usize {
        self.reaping.load(Ordering::SeqCst)
    }

    /// Subscribe `sink` to the pump of `(venue, symbol, tick_size)`, starting it when nobody holds it.
    /// The subscriber's book losses are not tracked ([`PumpHub::join_with`]).
    pub fn join(
        self: &Arc<Self>,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<PumpLease, String> {
        self.join_with(venue, symbol, tick_size, PumpSink::untracked(sink))
    }

    /// [`PumpHub::join`] for a subscriber that can say how many book frames its session has lost
    /// ([`PumpSink::tracked`]): the hub repairs the subscriber's book with an anchor when the figure moves.
    pub fn join_with(
        self: &Arc<Self>,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        sink: PumpSink,
    ) -> Result<PumpLease, String> {
        let key = PumpKey {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            tick_bits: tick_size.to_bits(),
        };
        let id = self.next_member.fetch_add(1, Ordering::Relaxed);
        let mut live = lock(&self.live);
        let shared = match live.get(&key) {
            Some(shared) => Arc::clone(shared),
            None => {
                // A pump being stopped is still a socket and two threads: subscribe/unsubscribe churn
                // of distinct symbols must not run past the cap by the pumps its own `Unsub`s are reaping.
                let stopping = self.reaping.load(Ordering::SeqCst);
                if live.len() + stopping >= self.max {
                    // Full of RUNNING pumps: a cap, which persists until an operator raises it or a
                    // subscriber leaves (a retry meets it again, and a caller that restarts itself on a
                    // transient refusal must not). Full only because of pumps being STOPPED: those leave
                    // within seconds, so that one is a transient.
                    let why = if live.len() >= self.max {
                        format!("{CAP_MARK}: raise the cap or drop a subscription")
                    } else {
                        "transient, retried on the caller's next pass".to_string()
                    };
                    return Err(format!(
                        "gatewayhub: already hosting {} tick pumps (the daemon's cap; {stopping} of \
                         them being stopped); {why}",
                        self.max
                    ));
                }
                let shared = self.start(&key, tick_size)?;
                live.insert(key.clone(), Arc::clone(&shared));
                shared
            }
        };
        shared.add(id, sink);
        drop(live);
        Ok(PumpLease::new(Membership { hub: Arc::clone(self), key, id }))
    }

    /// Start the pump and its drain thread.
    fn start(self: &Arc<Self>, key: &PumpKey, tick_size: f64) -> Result<Arc<Shared>, String> {
        let (events, mut rx) = event_channel(PUMP_CHANNEL_CAPACITY);
        let ticks = TickSender { ingest: events.ingest.clone() };
        drop(events);
        let handle = (self.spawner)(&key.venue, &key.symbol, tick_size, ticks)?;
        let shared = Arc::new(Shared {
            key: key.clone(),
            inner: Mutex::new(Inner::default()),
            stopping: AtomicBool::new(false),
            handle: Mutex::new(Some(handle)),
            drain: Mutex::new(None),
        });
        let drain_shared = Arc::clone(&shared);
        let hub = Arc::clone(self);
        let name = format!("gwh-pump-{}", key.venue);
        let drain = std::thread::Builder::new().name(name).spawn(move || {
            while let Some(message) = rx.blocking_recv() {
                drain_shared.deliver(&message);
            }
            hub.drain_ended(&drain_shared);
        });
        match drain {
            Ok(join) => {
                *lock(&shared.drain) = Some(join);
                // The anchor timer. A QUIET book is re-anchored on the clock, not only when a pump message
                // happens to arrive: a symbol with nothing to say (and a hole at the next hop that only an
                // anchor mends) would otherwise stay stale for as long as it stayed quiet. The thread is
                // detached and ends within one tick of the pump stopping.
                let ticker = Arc::downgrade(&shared);
                let spawned = std::thread::Builder::new()
                    .name(format!("gwh-anchor-{}", key.venue))
                    .spawn(move || {
                        // A `Weak`: the thread ends when the pump's state is gone (stopped, reaped, or died).
                        while let Some(shared) = ticker.upgrade() {
                            if shared.stopping.load(Ordering::SeqCst) {
                                return;
                            }
                            shared.anchor_quiet(Instant::now());
                            drop(shared);
                            std::thread::sleep(ANCHOR_TICK);
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!(
                        venue = %key.venue, error = %e,
                        "vike-gatewayhub pump: the anchor timer could not start; a quiet book is \
                         re-anchored by the next pump message only"
                    );
                }
                Ok(shared)
            }
            Err(e) => {
                shared.stopping.store(true, Ordering::SeqCst);
                Err(format!("gatewayhub: could not start the tick pump's drain thread: {e}"))
            }
        }
    }

    /// A member leaves; the last one out stops the pump.
    fn leave(&self, key: &PumpKey, id: u64) {
        let mut live = lock(&self.live);
        let Some(shared) = live.get(key).cloned() else { return };
        if !shared.remove(id) {
            return;
        }
        live.remove(key);
        // Counted as reaping BEFORE the table lock is given back, so a joiner never sees the slot free.
        self.reaping.fetch_add(1, Ordering::SeqCst);
        drop(live);
        shared.stop_and_reap(Arc::clone(&self.reaping));
    }

    /// The drain saw its channel close. A stop on purpose set `stopping`; anything else is a pump that
    /// died, which its subscribers are told as a disconnect, and the entry goes so the next subscriber
    /// starts a fresh pump.
    fn drain_ended(&self, shared: &Arc<Shared>) {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        tracing::error!(
            venue = %shared.key.venue, symbol = %shared.key.symbol,
            "vike-gatewayhub pump: the venue pump ended on its own; its subscribers are told it is \
             disconnected"
        );
        shared.disclose_down();
        let mut live = lock(&self.live);
        if live.get(&shared.key).is_some_and(|cur| Arc::ptr_eq(cur, shared)) {
            live.remove(&shared.key);
        }
    }
}

impl Shared {
    fn add(&self, id: u64, subscriber: PumpSink) {
        let PumpSink { sink, book_losses } = subscriber;
        let mut inner = lock(&self.inner);
        let mut translator = Translator::new(&self.key.venue, &self.key.symbol);
        if inner.link_down {
            translator.disclose_down(sink.as_ref());
        } else {
            // A joiner onto a pump that has spoken is told the link is up BEFORE the book: a session
            // that replaced one whose last word was this pump's `GapStart` (a datahub restart) would
            // otherwise never hear the `Live` that closes the window it opened.
            if inner.heard {
                translator.disclose_up(sink.as_ref());
            }
            if let Some(book) = inner.last_book.clone() {
                translator.deliver_book(sink.as_ref(), &book, Instant::now());
            }
        }
        let seen_losses = book_losses.as_ref().map_or(0, |losses| losses());
        inner.members.push(Member { id, sink, translator, losses: book_losses, seen_losses });
    }

    /// Remove member `id`; whether the pump has no member left.
    fn remove(&self, id: u64) -> bool {
        let mut inner = lock(&self.inner);
        inner.members.retain(|m| m.id != id);
        inner.members.is_empty()
    }

    fn deliver(&self, message: &Ingest) {
        let now = Instant::now();
        let mut inner = lock(&self.inner);
        // A pump that has not spoken since it started (or since its last disconnect) and now delivers data is
        // UP, and its members are told so before the data: a fresh pump has no `Disconnected`/`Live` of its
        // own to say it. Without this a subscriber that outlived a datahub restart (the old session's last
        // word on this pump may have been a `GapStart`, and its `Live` fell into the dead socket) keeps a
        // dead-man window open for a pump the new datahub has up and healthy.
        let announce_up = !inner.heard
            && !inner.link_down
            && matches!(message, Ingest::Book(_) | Ingest::Quote(_) | Ingest::Trade(_));
        match message {
            Ingest::Book(b) => {
                inner.last_book = Some(Arc::clone(&b.book));
                inner.book_at = Some(now);
                inner.stale = false;
                inner.heard = true;
            }
            Ingest::Quote(_) | Ingest::Trade(_) => {
                inner.stale = false;
                inner.heard = true;
            }
            Ingest::StreamStatus(s) => match s.status {
                FeedStatus::Disconnected => {
                    inner.link_down = true;
                    inner.heard = false;
                    inner.stale = false;
                    // The book the pump held is the one the outage ended: a joiner after the recovery
                    // waits for the pump's fresh book and is not handed this one as an anchor.
                    inner.last_book = None;
                    inner.book_at = None;
                }
                FeedStatus::Live => {
                    inner.link_down = false;
                    inner.stale = false;
                    inner.heard = true;
                }
                // The pump's own word that its data is old: the link is not down, but the standing book is
                // not evidence of anything, and the anchor timer must not restate it as a fresh one.
                FeedStatus::Stale => {
                    inner.link_down = false;
                    inner.stale = true;
                }
            },
            _ => {}
        }
        let standing = if inner.link_down || inner.stale { None } else { inner.last_book.clone() };
        if announce_up {
            for member in &mut inner.members {
                member.translator.disclose_up(member.sink.as_ref());
            }
        }
        for member in &mut inner.members {
            member.deliver(message, standing.as_deref(), now);
        }
    }

    /// The anchor timer's step ([`ANCHOR_TICK`]): every member whose periodic anchor is due gets the standing
    /// book, whether or not the pump said anything. Nothing while the link is down or the pump says its data is
    /// `Stale` (a stale book is never sent as a fresh one), and nothing on the clock alone once the standing book
    /// is older than [`STANDING_BOOK_MAX_AGE`] (a pump gone silent without a word): only a member whose session
    /// lost a book frame is still repaired from it.
    fn anchor_quiet(&self, now: Instant) {
        let mut inner = lock(&self.inner);
        if inner.link_down || inner.stale {
            return;
        }
        let Some(book) = inner.last_book.clone() else { return };
        let fresh = inner
            .book_at
            .is_some_and(|at| now.saturating_duration_since(at) <= STANDING_BOOK_MAX_AGE);
        for member in &mut inner.members {
            member.anchor_quiet(&book, now, fresh);
        }
    }

    fn disclose_down(&self) {
        let mut inner = lock(&self.inner);
        inner.link_down = true;
        for member in &mut inner.members {
            member.translator.disclose_down(member.sink.as_ref());
        }
    }

    /// Stop the pump and join its threads on a thread of its own (the caller may be a session reader).
    fn stop_and_reap(self: Arc<Self>, reaping: Arc<AtomicUsize>) {
        self.stopping.store(true, Ordering::SeqCst);
        let handle = lock(&self.handle).take();
        if let Some(h) = &handle {
            h.signal();
        }
        let drain = lock(&self.drain).take();
        let reap = move || {
            let _done = ReapGuard(reaping);
            drop(handle);
            if let Some(d) = drain {
                let _ = d.join();
            }
        };
        let reap = Arc::new(Mutex::new(Some(reap)));
        let for_thread = Arc::clone(&reap);
        let spawned = std::thread::Builder::new().name("gwh-pump-reap".into()).spawn(move || {
            if let Some(f) = lock(&for_thread).take() {
                f();
            }
        });
        if spawned.is_err()
            && let Some(f) = lock(&reap).take()
        {
            f();
        }
    }
}

/// Gives a reaped pump's slot back to the cap when the reaper has finished (or unwound).
struct ReapGuard(Arc<AtomicUsize>);

impl Drop for ReapGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A session's place in a shared pump; its drop leaves it.
struct Membership {
    hub: Arc<PumpHub>,
    key: PumpKey,
    id: u64,
}

impl Drop for Membership {
    fn drop(&mut self) {
        self.hub.leave(&self.key, self.id);
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod pump_tests;
