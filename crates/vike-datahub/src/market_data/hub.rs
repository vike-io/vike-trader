//! [`MarketDataHub`] — the registry that makes the data daemon the SINGLE subscriber (ruling 2): one venue
//! socket and one folded book per `(venue, symbol, lane)`, however many desktops, windows or
//! sessions want it.
//!
//! # Two tiers (§5.1)
//!
//! **Tier R — RESIDENT.** A set the daemon itself declares (`VIKE_DATAHUB_LIVE_RESIDENT`),
//! subscribed at startup and PINNED — a refcount FLOOR of 1, never released. It buys three things: a
//! hot symbol's ladder paints on the first DOM open instead of waiting on a REST seed (which is not
//! free — `DEPTH_RESEED_INTERVAL` is 300 s and neither CEX publishes a book checksum); an operator
//! can guarantee the daemon's traded pair stays observable with no desktop attached; and the
//! steady-state venue subscription count is a DECLARED number rather than a function of how many
//! windows somebody left open.
//!
//! **Tier D — ON-DEMAND.** Client-driven, ref-counted per [`MarketDataKey`] **across every session**. Five
//! Trade windows on one symbol across three desktops is ONE venue socket and ONE folded book.
//!
//! # ⚠ THE ONE STRUCTURAL DEPARTURE FROM THE SIGNED-OFF §5, AND WHY
//!
//! §5.2 step 4 has the CONNECTION thread call `hub.acquire(key)`, which on refcount 0 builds the
//! venue's `DataClient` and calls `subscribe_*` there; §5.3 then adds a SEPARATE `market-data-janitor` thread
//! described as "the sole caller of every blocking `DataClient` method". Those two sentences cannot
//! both be true, and the pair puts venue I/O on a request path. Here there is **one owner**:
//!
//! ```text
//! acquire(key)  -> bump the refcount, mark WANTED, poke a Condvar. No venue call. Never blocks.
//! release(key)  -> drop the refcount; at zero, stamp `zero_since`. No venue call.
//! reconcile()   -> the ONLY holder of any DataClient. Computes
//!                  desired = {resident} ∪ {refcount>0} ∪ {refcount==0 AND inside MARKET_DATA_LINGER}
//!                  and drives each venue client to exactly that set.
//! ```
//!
//! Four arguments, in the tree's own terms:
//!
//! 1. `crates/vike-recorder/src/session.rs`'s `SubscriptionSet::reconcile` **already is** this loop,
//!    with the two rules §5.2 step 3 quotes verbatim — stop what left, start what arrived, **leave
//!    survivors strictly alone** (a re-subscribe drops venue book state and punches a gap into the
//!    exact series somebody just asked for). It has been running on the CI box.
//! 2. It removes venue I/O from the connection thread ENTIRELY, which matters because not every
//!    venue's `subscribe_*` is spawn-only: binance's is a bare `spawn_with`, while
//!    `crates/bridges/polymarket/src/feed/mod.rs` shards and reseats seats and
//!    `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS` table records a per-token REST
//!    warmup plus a 10 s dial on that path. With a reconciler it does not matter what any of them
//!    do: **nothing a venue does can stall a `MarketDataSubscribe` reply.**
//! 3. The client vocabulary already covers the latency this introduces. A wanted-but-not-yet-served
//!    key attaches as `Status(GapStart)` and becomes `Live` when it lands — §9's asynchronous-refusal
//!    rule, so "accepted, arriving shortly" needs no new type.
//! 4. §8 items 4 and 9 COLLAPSE, and §5.3's "there is no release channel" argument gets stronger
//!    rather than weaker: there is no channel AND no second thread.
//!
//! It also removes a real bug — see this module's parent doc on `DataClientRegistry::raise_stops`.
//!
//! # ⚠ Depth is resolved per KEY, not per subscriber
//!
//! §6.1 requires ONE serialization per dirty key per tick ("N subscribers of one symbol cost one
//! encode"), so a key's effective depth is the **max** over its live subscribers, clamped to
//! `vike_datahub_client::market::MARKET_DATA_DEPTH_LEVELS_CEILING`. The consequence, stated because it is
//! real: one client asking for 200 levels inflates the frame for EVERY subscriber of that key. The
//! alternative — per-subscriber depth — costs the one-encode property and with it the whole §12.4
//! mailbox derivation. [`crate::market_data::MARKET_DATA_MAILBOX_BYTES`] is what absorbs the consequence, and its own
//! doc carries that argument.
//!
//! ⚠ **"over its LIVE subscribers" means the fold comes back DOWN, and for a while it did not.**
//! [`MarketDataStreamEntry::depth`] was a `fetch_max` and nothing else, so the deep client's request outlived
//! the deep client — permanently on a resident key, which is never reaped — and every OTHER
//! subscriber of that key paid ~105 KB/s where §12.6 priced 27.3. [`SessionState`] therefore records
//! the depth each session asked for, `acquire` raises, and [`MarketDataHub::release_key`] REFOLDS from what
//! remains.
//!
//! # ⚠ Every lock recovers from poisoning
//!
//! `unwrap_or_else(PoisonError::into_inner)` everywhere, §6.1's stated divergence from
//! `crates/vike-tradehub/src/publish.rs`'s `PublisherMailbox` (`.expect("mailbox poisoned")` on every lock).
//! One panicking connection must not convert into a server-wide outage — and it is a PRECONDITION of
//! the suite's panic-path test being able to fail honestly rather than reading as a broken test.
//!
//! # Layout — one `impl MarketDataHub` in four child files
//!
//! This file keeps the types (`MarketDataKey`, `MarketDataStreamEntry`, `MarketDataHub` and its fields, `MarketDataSessionGuard`), the
//! constructor, the helpers every half shares (`entry`, `lookup`, `poke`, `snapshot`, `spawn`) and
//! the free functions the sink and the server reach. The methods are split by what they do:
//!
//! * `subscribe.rs` — what a session or the operator ASKS for: `add_resident`, `open_session`,
//!   `acquire`, `update`.
//! * `release.rs` — the end of a hold: `close_session`, `release_key`.
//! * `publish.rs` — what the hub hands out: `attach_frames`, `publish_tick`.
//! * `reconcile.rs` — the venue side: `reconcile`, `wait_for_poke`.
//!
//! ⚠ A comment in any of them that says "this file" about the lock order means the HUB — all five
//! files — and the order is the same in every one: `sessions` first, then `keys`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, Weak};
use vike_data::DataClientBuilder;

use vike_data::{DataClient, DataClientSink, DataClientSubscriptionId};
use vike_datahub_client::BookSnapshot;
use vike_datahub_client::market::{
    MARKET_DATA_DEPTH_LEVELS_CEILING, MarketDataFrame, MarketDataFrameQuote, MarketDataRefusal,
    MarketDataSessionId, MarketDataSpec, validate_market_data_symbol,
};
use vike_hub_protocol::frame::{BodyCodec, MAX_FRAME_LEN, encode_reply};
use vike_marketdata::{TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};

use super::mailbox::{
    MarketDataMailbox, MarketDataMailboxLaneClass, MarketDataMailboxOutgoing,
    MarketDataMailboxPushOutcome,
};
use super::{
    MARKET_DATA_FRAME_CEILING_BYTES, MARKET_DATA_LINGER, MARKET_DATA_MAX_KEYS_PER_VENUE,
    MARKET_DATA_MAX_KEYS_RESIDENT, MARKET_DATA_MAX_KEYS_TOTAL, MARKET_DATA_MAX_SPECS_PER_SESSION,
    MARKET_DATA_MAX_STREAM_CONNECTIONS, MARKET_DATA_TAPE_CAP,
};

mod publish;
mod reconcile;
mod release;
mod subscribe;

/// One subscription's identity — `(venue, symbol, lane)`. **Depth is deliberately not part of it**
/// (`vike_datahub_client::market::MarketDataSpec::key`): two clients asking for one key at different depths
/// share ONE venue subscription.
///
/// The derived `Ord` (venue, then symbol, then `StreamLabel`'s own `Quotes < Trades < Book < Depth`)
/// orders [`MarketDataHub`]'s snapshot, a session's held set and the attach pushes of one request. Nothing
/// depends on the order between one symbol's lanes: each lane is its own subscription with its own
/// frames.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MarketDataKey {
    /// The venue slug.
    pub venue: String,
    /// The venue's own symbol spelling.
    pub symbol: String,
    /// Which lane.
    pub lane: StreamLabel,
}

impl MarketDataKey {
    /// The key one spec names.
    pub fn of(spec: &MarketDataSpec) -> Self {
        MarketDataKey { venue: spec.venue.clone(), symbol: spec.symbol.clone(), lane: spec.lane }
    }
}

/// What this hub may do with one key of a venue (decision 0193, verdict 8). The answer is a question to
/// the broker in production (`crate::market_data::venues::attach_gate`) and a closure in a test, for the reason
/// [`DataClientBuilder`] is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketDataAttach {
    /// A keyless venue: a window's subscription may open the key.
    Open,
    /// A credentialed venue, and an operator-owned holder holds this key: a window may join it.
    Held,
    /// A credentialed venue, and no operator-owned holder holds this key: [`MarketDataRefusal::NotHeld`].
    NotHeld,
}

/// The hub's question about one key. It must wait on no venue: it runs on a request path.
pub type MarketDataAttachGate =
    Box<dyn Fn(&str, &str, StreamLabel) -> MarketDataAttach + Send + Sync>;

/// The latest quote of a key, stamped with the hub's own clock so a frame can say how old it is.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MarketDataQuoteState {
    bid: f64,
    ask: f64,
    bid_size: f64,
    ask_size: f64,
    venue_timestamp: i64,
    received_ms: i64,
}

impl MarketDataQuoteState {
    pub(crate) fn of(quote: &TickQuote, received_ms: i64) -> Self {
        MarketDataQuoteState {
            bid: quote.bid,
            ask: quote.ask,
            bid_size: quote.bid_size,
            ask_size: quote.ask_size,
            venue_timestamp: quote.timestamp,
            received_ms,
        }
    }
}

/// The pre-clamp folded book plus its stamps, as the sink left it.
#[derive(Debug, Clone)]
pub(crate) struct MarketDataBookState {
    pub(crate) tick_size: f64,
    pub(crate) bids: Vec<vike_marketdata::L2BookLevel>,
    pub(crate) asks: Vec<vike_marketdata::L2BookLevel>,
    pub(crate) venue_timestamp: i64,
    pub(crate) venue_sequence: u64,
}

/// One key's hub-side state.
pub(crate) struct MarketDataStreamEntry {
    pub(crate) key: MarketDataKey,
    /// Tier R: a refcount FLOOR of 1. A resident key is never reaped through client release.
    ///
    /// ATOMIC rather than a plain `bool` because [`MarketDataHub::add_resident`] may find a slot that
    /// already exists and must be able to PIN it in place — replacing the entry would throw away its
    /// folded book, its tape and its live `sub_id`, and leaving it unpinned would make the operator's
    /// declared row silently a no-op.
    resident: AtomicBool,
    /// Tier D refcount, across EVERY session.
    subscribers: AtomicU32,
    /// The depth this key's frames are cut to: the max over its LIVE subscribers, floored by
    /// [`Self::resident_depth`] and clamped at ingest — see the module doc.
    ///
    /// ⚠ **It comes back DOWN.** It was written only by `new` and a `fetch_max`, which made it a
    /// monotonic ratchet: one client's single ceiling-depth request retired
    /// `vike_datahub_client::market::MARKET_DATA_DEPTH_LEVELS_DEFAULT` for that key until the key was reaped
    /// — and forever, on a RESIDENT key, which is never reaped. Since `publish_tick` serializes ONCE
    /// per key at this depth, that inflated the frame for EVERY subscriber of the key: §12.4's
    /// measured 2.74 KB became 10.48 KB, and §12.6's 27.3 KB/s per binance depth key became
    /// ~105 KB/s, permanently. [`MarketDataHub::acquire`] raises it and [`MarketDataHub::release_key`] REFOLDS it
    /// from the sessions that remain.
    depth: AtomicU32,
    /// The tier-R declared depth — a FLOOR that outlives every subscriber, so a resident key keeps
    /// serving the depth the operator declared once the last desktop closes. `0` on tier D.
    resident_depth: AtomicU32,
    /// Epoch-ms when the refcount last reached zero; `-1` = held. The [`MARKET_DATA_LINGER`] deadline.
    zero_since: AtomicI64,
    /// RECONCILER-OWNED. `None` = wanted but not yet live (the honest `GapStart` state).
    pub(crate) sub_id: Mutex<Option<DataClientSubscriptionId>>,
    /// LATEST-WINS. `Arc` so the publisher clones and drops the guard before it serializes.
    pub(crate) book_slot: Mutex<Option<Arc<MarketDataBookState>>>,
    /// LATEST-WINS, the quote lane's slot.
    pub(crate) quote_slot: Mutex<Option<MarketDataQuoteState>>,
    /// The stream's last delay label, ridden by every quote frame of the key.
    pub(crate) delay: Mutex<Option<StreamDelay>>,
    /// The STICKY latest status — read by `attach_frames`, which needs the current one rather than
    /// an unconsumed delta.
    pub(crate) status: Mutex<Option<StreamStatus>>,
    /// Set when `status` changed and the publisher has not yet emitted it.
    status_dirty: AtomicBool,
    /// Bounded, evict-oldest.
    pub(crate) tape: Mutex<VecDeque<TickTrade>>,
    /// HUB-side tape evictions not yet disclosed.
    dropped: AtomicU64,
    /// Set by the sink, cleared by the publisher.
    dirty: AtomicBool,
    /// Set once a produced BOOK frame has exceeded [`MARKET_DATA_FRAME_CEILING_BYTES`], so the disclosure is
    /// one line per key rather than one per publish tick. See [`MarketDataStreamEntry::note_frame_size`].
    oversize_warned: AtomicBool,
    /// The WIRE sequence (§7.2) — assigned by the publisher ONLY, and BEFORE any drop decision.
    sequence: AtomicU64,
}

impl MarketDataStreamEntry {
    fn new(key: MarketDataKey, resident: bool, depth: u16) -> Self {
        MarketDataStreamEntry {
            key,
            resident: AtomicBool::new(resident),
            subscribers: AtomicU32::new(0),
            depth: AtomicU32::new(depth as u32),
            resident_depth: AtomicU32::new(if resident { depth as u32 } else { 0 }),
            zero_since: AtomicI64::new(if resident { -1 } else { 0 }),
            sub_id: Mutex::new(None),
            book_slot: Mutex::new(None),
            quote_slot: Mutex::new(None),
            delay: Mutex::new(None),
            status: Mutex::new(None),
            status_dirty: AtomicBool::new(false),
            tape: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            oversize_warned: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
        }
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub(crate) fn mark_status_dirty(&self) {
        self.status_dirty.store(true, Ordering::Release);
        self.mark_dirty();
    }

    pub(crate) fn note_tape_drop(&self, n: u64) {
        self.dropped.fetch_add(n, Ordering::AcqRel);
    }

    /// The depth this key's frames are cut to — the max over its LIVE subscribers, floored by the
    /// tier-R declaration.
    pub(crate) fn effective_depth(&self) -> usize {
        self.depth.load(Ordering::Acquire) as usize
    }

    /// Whether this key is tier R.
    pub(crate) fn is_resident(&self) -> bool {
        self.resident.load(Ordering::Acquire)
    }

    fn raise_depth(&self, want: u16) {
        self.depth.fetch_max(want as u32, Ordering::AcqRel);
    }

    /// Pin an EXISTING slot as tier R at `depth` — the idempotent half of
    /// [`MarketDataHub::add_resident`], and the reason [`Self::resident`] is atomic.
    fn pin_resident(&self, depth: u16) {
        self.resident.store(true, Ordering::Release);
        self.resident_depth.fetch_max(depth as u32, Ordering::AcqRel);
        self.raise_depth(depth);
        self.zero_since.store(-1, Ordering::Release);
    }

    /// Compare one produced BOOK frame against [`MARKET_DATA_FRAME_CEILING_BYTES`] and say so ONCE per key.
    ///
    /// ⚠ **It exists to make the compile-time memory assertions rest on an OBSERVED fact rather than
    /// on one measurement of one venue.** [`MARKET_DATA_FRAME_CEILING_BYTES`] is §12.4's measured 10,479 B
    /// binance book at 200 levels a side, and `crate::market_data::MARKET_DATA_MAILBOX_BYTES`' assertion — which
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` now leans on for the
    /// §11 Q1 co-hosting claim — multiplies it by [`MARKET_DATA_MAX_SPECS_PER_SESSION`]. Nothing compared a
    /// real frame to it: `snapshot_of` clamps LEVELS, never bytes, so a venue/symbol whose 400
    /// levels serialize wider (a longer slug, more price or size decimals) silently falsifies both
    /// that assertion and the process-wide one above it.
    ///
    /// A `warn!` rather than a refusal, and once per key rather than per frame: the deployment must
    /// keep serving — a book that is 3% over a derived constant is not a reason to drop a
    /// subscriber — and a per-frame line at [`crate::market_data::MARKET_DATA_PUBLISH_INTERVAL`] would be the
    /// per-message logging the hot-path rule forbids. What it buys is that the next person to raise
    /// a bound finds a measurement from THIS deployment instead of one venue's number from §12.
    pub(crate) fn note_frame_size(&self, bytes: usize) {
        if bytes <= MARKET_DATA_FRAME_CEILING_BYTES
            || self.oversize_warned.swap(true, Ordering::AcqRel)
        {
            return;
        }
        tracing::warn!(
            venue = %self.key.venue,
            symbol = %self.key.symbol,
            lane = ?self.key.lane,
            bytes,
            ceiling = MARKET_DATA_FRAME_CEILING_BYTES,
            depth = self.effective_depth(),
            "vike-datahub market data: a book frame is OVER MARKET_DATA_FRAME_CEILING_BYTES — the constant \
             crate::market_data::MARKET_DATA_MAILBOX_BYTES' compile-time assertion (and the 32 MB plane ceiling above \
             it) is computed from. Re-run §12.4's measurement for this venue before trusting either"
        );
    }

    /// REFOLD the depth from the live subscribers' maximum, never below the tier-R floor.
    ///
    /// ⚠ `want == 0` — no session holds this key at all — leaves the current value ALONE rather than
    /// zeroing it. A key with no subscriber has no frame to cut (the publisher skips it on an empty
    /// target list), and a zero would make [`MarketDataHub::attach_frames`] hand the NEXT subscriber an
    /// empty ladder in the window before its own `acquire` raised it again.
    ///
    /// # ⚠ NEITHER THIS NOR [`Self::raise_depth`] MARKS THE ENTRY DIRTY, AND THAT IS A DECISION
    ///
    /// A depth change schedules no republish, so the cut a key's frames carry moves only when the
    /// VENUE next updates it. The half of that which produced a WRONG PICTURE is closed, and it is
    /// closed without a `mark_dirty` anywhere: the session that asked — a new subscriber joining an
    /// existing key deeper, or one re-requesting a key it already holds at a new depth — is handed
    /// [`MarketDataHub::attach_frames`] on the spot by `run_market_writer` or by [`MarketDataHub::update`], and that
    /// snapshot is cut at [`Self::effective_depth`], which `acquire` has already raised.
    ///
    /// **The residual is declared rather than closed.** The key's OTHER already-attached subscribers
    /// keep receiving the previous cut until the next venue update. On a RAISE they are missing only
    /// levels they never asked for — depth is a per-key MAX, so the inflation this field's own doc
    /// calls a cost was always a windfall and a windfall arriving one tick late is not a loss. On a
    /// LOWER (a deep subscriber leaving, the refold here) they keep the deeper frame for one tick,
    /// which is strictly more data and self-corrects. Neither shows a blank ladder, which is the
    /// failure class the attach exists to kill.
    ///
    /// Marking dirty here would republish the key to EVERY subscriber of it on every window open and
    /// every window close — §6.1's one-serialization-per-dirty-key-per-tick model paying a broadcast
    /// for a change nobody asked for, which is precisely the cost [`MarketDataHub::update`]'s attach is
    /// shaped to avoid.
    fn settle_depth(&self, want: u16) {
        let d = (want as u32).max(self.resident_depth.load(Ordering::Acquire));
        if d > 0 {
            self.depth.store(d, Ordering::Release);
        }
    }

    /// Subscribers held, RESIDENT INCLUDED as a floor of one.
    fn held(&self) -> u32 {
        self.subscribers.load(Ordering::Acquire) + u32::from(self.is_resident())
    }
}

/// What one session holds.
///
/// ⚠ **`keys` is a MAP, not a set, and the value is the depth THIS session asked for.** Without it
/// a release cannot refold [`MarketDataStreamEntry::depth`] — there is nowhere else the per-subscriber
/// request survives, and the entry's own field is the max it must be recomputed from. See
/// [`MarketDataStreamEntry::depth`] for what the missing fold cost.
struct SessionState {
    keys: BTreeMap<MarketDataKey, u16>,
    mailbox: Arc<MarketDataMailbox>,
}

/// The deepest request any LIVE session holds for `key`, or `0` when none does.
///
/// A free function rather than a method so both callers can pass the `sessions` guard they already
/// hold — [`MarketDataHub::acquire`] must fold inside the lock it took, and taking it a second time there
/// would open the window this fold exists to close.
fn max_requested_depth(
    sessions: &HashMap<MarketDataSessionId, SessionState>,
    key: &MarketDataKey,
) -> u16 {
    sessions.values().filter_map(|s| s.keys.get(key).copied()).max().unwrap_or(0)
}

/// A per-tick account of what the publisher produced.
///
/// ⚠ `frames_produced` exists as an INDEPENDENT WITNESS rather than for logs: the `TapeGap`
/// arithmetic (`to_sequence - from_sequence == dropped`) is tautological if an implementation computes
/// `to_sequence` as `from_sequence + dropped`, and two paths to the same number is what makes neither one
/// circular.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarketDataTickReport {
    /// Frames serialized this tick (one per dirty key per lane class).
    pub frames_produced: u64,
    /// Dirty keys walked.
    pub keys_walked: usize,
}

/// What one reconcile pass did — the suite's non-vacuity floor, and the operator's log line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarketDataSyncReport {
    /// Venue subscriptions STARTED.
    pub started: usize,
    /// Venue subscriptions STOPPED.
    pub stopped: usize,
    /// Venue clients dropped entirely (their whole live set was reaped).
    pub clients_dropped: usize,
    /// Subscribe attempts that failed and will be retried on the next pass.
    pub failed: Vec<String>,
}

type LaneSlots = [Option<Arc<MarketDataStreamEntry>>; 4];

fn lane_index(lane: StreamLabel) -> usize {
    match lane {
        StreamLabel::Depth => 0,
        StreamLabel::Book => 1,
        StreamLabel::Trades => 2,
        StreamLabel::Quotes => 3,
    }
}

/// The registry. See the module doc.
pub struct MarketDataHub {
    /// `venue -> symbol -> [depth, book, trades, quotes]`.
    ///
    /// ⚠ **Two levels, and that is load-bearing rather than stylistic.** `DataClientSink`'s verbs
    /// arrive as `(&str venue, &str symbol, …)` on a hot feed thread — at polymarket's measured
    /// group p99 of 2,918 updates/s — and a flat `HashMap<MarketDataKey, _>` cannot be looked up from
    /// borrowed parts without ALLOCATING a key per sink call. Both levels here take `get(&str)`
    /// through `Borrow`. It also yields `MARKET_DATA_MAX_KEYS_PER_VENUE` counting for free.
    keys: RwLock<HashMap<String, HashMap<String, LaneSlots>>>,
    /// RECONCILER-ONLY. Never locked by a sink or a writer.
    venues: Mutex<HashMap<String, Box<dyn DataClient + Send>>>,
    sessions: Mutex<HashMap<MarketDataSessionId, SessionState>>,
    /// The ONE sink every venue client is constructed with.
    sink: Arc<super::sink::DataClientSinkMarketDataHub>,
    builder: DataClientBuilder,
    gate: MarketDataAttachGate,
    /// Venues this BUILD links a market-data client for, and the credentialed venues the operator's
    /// row opened to this plane — the `market_data_venue=` advertisement.
    served: Vec<String>,
    /// `acquire`/`release` poke; the reconciler waits on it.
    wake: (Mutex<bool>, Condvar),
    stream_connections: AtomicU32,
}

impl std::fmt::Debug for MarketDataHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketDataHub").field("served", &self.served).finish_non_exhaustive()
    }
}

impl MarketDataHub {
    /// A hub over a venue-client builder. **Spawns NO threads** — see [`MarketDataHub::spawn`].
    ///
    /// ⚠ The publish tick and the reconcile pass are CALLABLE FUNCTIONS
    /// ([`MarketDataHub::publish_tick`], [`MarketDataHub::reconcile`]) and the threads are a dozen lines around
    /// them, deliberately: every property worth testing here is a statement about *what one tick
    /// produced*, and against a free-running 100 ms thread each of them becomes sleep-and-hope —
    /// the flake shape this repo has already paid for twice (`vike-store`'s `live_rec` overflow test,
    /// the tradehub daemon port race). The precedent is in this crate's own graph since ruling 10:
    /// `crates/vike-recorder/src/runtime.rs`'s `RecorderRuntime::tick` returns a report per feed and
    /// the loop lives elsewhere.
    pub fn new(builder: DataClientBuilder, served: Vec<String>) -> Arc<Self> {
        Self::with_gate(builder, served, Box::new(|_, _, _| MarketDataAttach::Open))
    }

    /// [`MarketDataHub::new`] over the question that keeps a credentialed venue's keys attach-only.
    pub fn with_gate(
        builder: DataClientBuilder,
        served: Vec<String>,
        gate: MarketDataAttachGate,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<MarketDataHub>| MarketDataHub {
            keys: RwLock::new(HashMap::new()),
            venues: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            sink: Arc::new(super::sink::DataClientSinkMarketDataHub::new(weak.clone())),
            builder,
            gate,
            served,
            wake: (Mutex::new(false), Condvar::new()),
            stream_connections: AtomicU32::new(0),
        })
    }

    /// The ONE sink every venue client this hub builds is constructed with — legal because every
    /// `DataClientSink` verb carries `venue` and `symbol`.
    pub fn sink(&self) -> Arc<dyn DataClientSink> {
        Arc::clone(&self.sink) as Arc<dyn DataClientSink>
    }

    /// The venues this build serves, for the `market_data_venue=` advertisement.
    pub fn served_venues(&self) -> &[String] {
        &self.served
    }

    /// How many quote-lane entries this hub holds: zero is a quote that costs a feed thread one atomic.
    pub fn quote_keys(&self) -> u32 {
        self.sink.quote_keys()
    }

    fn entry(
        &self,
        venue: &str,
        symbol: &str,
        lane: StreamLabel,
    ) -> Option<Arc<MarketDataStreamEntry>> {
        let g = self.keys.read().unwrap_or_else(PoisonError::into_inner);
        g.get(venue)?.get(symbol)?[lane_index(lane)].clone()
    }

    /// The sink's lookup — read-locked for the duration of a map lookup and DROPPED before any slot
    /// is touched, so the publisher never holds the map lock while it serializes and a feed thread
    /// never waits on a reconcile.
    pub(crate) fn lookup(
        &self,
        venue: &str,
        symbol: &str,
        lane: StreamLabel,
    ) -> Option<Arc<MarketDataStreamEntry>> {
        self.entry(venue, symbol, lane)
    }

    fn poke(&self) {
        let (m, cv) = &self.wake;
        let mut g = m.lock().unwrap_or_else(PoisonError::into_inner);
        *g = true;
        drop(g);
        cv.notify_all();
    }

    /// Every live entry, in key order — the reconciler's and the publisher's snapshot of the map.
    fn snapshot(&self) -> Vec<Arc<MarketDataStreamEntry>> {
        let g = self.keys.read().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for symbols in g.values() {
            for slots in symbols.values() {
                for slot in slots.iter().flatten() {
                    out.push(Arc::clone(slot));
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Start the two owning threads — `dh-data-sync` and `dh-data-publish`.
    ///
    /// ⚠ **The threads are a dozen lines around two CALLABLE functions**, and the split is the
    /// difference between a suite that GATES and one that flakes: every property worth testing here
    /// is a statement about what ONE tick produced, and against a free-running 100 ms thread each of
    /// them becomes sleep-and-hope. This repo has paid for that shape twice already (`vike-store`'s
    /// `live_rec` overflow test, the tradehub daemon port race), and a test that sometimes passes is
    /// not evidence.
    ///
    /// ⚠ **NEITHER THREAD IS JOINED, AND NO SIGNAL HANDLER IS INSTALLED** — see
    /// `crate::datahub_cli`'s market-data block for the argument. The hub holds nothing durable: an
    /// in-memory tape and open venue sockets, both of which a restart rebuilds and neither of which
    /// a venue cares about.
    ///
    /// ⚠ **SO THIS PLANE RELIES ON PROCESS EXIT, AND THERE IS NO HUB-LEVEL TEARDOWN TO CALL.**
    /// There WAS a `MarketDataHub::shutdown` — "tear every venue client down at process stop" — that
    /// nothing anywhere called, and it has been deleted rather than wired. Two spellings of the
    /// lifecycle disagreed and the doc was the false one: the hub cannot drop (this function moves
    /// an `Arc<Self>` into two `loop {}` threads that never exit), there is no `impl Drop`, and
    /// `deploy/vike-datahub.service`'s own stop block argues at length that this daemon must keep
    /// dying at once on SIGTERM — a handler here would raise a flag the accept loop does not poll,
    /// turning `systemctl stop` into a full `TimeoutStopSec=` wait and then a SIGKILL, which is
    /// strictly worse. A SIGKILL costs a subscriber a reconnect, not data. If a future change gives
    /// this plane something durable to lose, the teardown and the unit's budget are ONE change.
    pub fn spawn(self: &Arc<Self>) {
        let reconcile = Arc::clone(self);
        std::thread::Builder::new()
            .name("dh-data-sync".into())
            .spawn(move || {
                loop {
                    let report = reconcile.reconcile(vike_model::now_ms());
                    if report.started > 0 || report.stopped > 0 || !report.failed.is_empty() {
                        // CONNECTION-BOUNDARY logging only, the server's own rule: one line per
                        // subscription change, never per frame.
                        tracing::info!(
                            started = report.started,
                            stopped = report.stopped,
                            clients_dropped = report.clients_dropped,
                            failed = ?report.failed,
                            "vike-datahub market data: venue subscriptions reconciled"
                        );
                    }
                    reconcile.wait_for_poke(super::MARKET_DATA_REAP_INTERVAL);
                }
            })
            .expect("spawn dh-data-sync");

        let publish = Arc::clone(self);
        std::thread::Builder::new()
            .name("dh-data-publish".into())
            .spawn(move || {
                loop {
                    publish.publish_tick();
                    std::thread::sleep(super::MARKET_DATA_PUBLISH_INTERVAL);
                }
            })
            .expect("spawn dh-data-publish");
    }
}

/// Cut a folded book down to `depth` levels a side and stamp it for the wire.
///
/// ⚠ **The levels are already BEST-FIRST** — `crates/vike-marketdata/src/orderbook.rs`'s `L2Book::top_n`
/// returns bids descending and asks ascending, and the sink stores them in that order — so this
/// truncates from the FRONT, never sorts. `BookSnapshot`'s own doc carries the contract.
fn snapshot_of(
    key: &MarketDataKey,
    b: &MarketDataBookState,
    depth: usize,
    seq: u64,
) -> BookSnapshot {
    BookSnapshot {
        venue: key.venue.clone(),
        symbol: key.symbol.clone(),
        tick_size: b.tick_size,
        bids: b.bids.iter().take(depth).copied().collect(),
        asks: b.asks.iter().take(depth).copied().collect(),
        venue_timestamp: b.venue_timestamp,
        venue_sequence: b.venue_sequence,
        sequence: seq,
    }
}

/// Stamp a key's latest quote for the wire, as old as the hub's clock says at `now_ms`, carrying the
/// stream's delay label.
fn quote_frame(
    entry: &MarketDataStreamEntry,
    q: &MarketDataQuoteState,
    now_ms: i64,
    seq: u64,
) -> MarketDataFrame {
    MarketDataFrame::Quote(MarketDataFrameQuote {
        venue: entry.key.venue.clone(),
        symbol: entry.key.symbol.clone(),
        bid: q.bid,
        ask: q.ask,
        bid_size: q.bid_size,
        ask_size: q.ask_size,
        venue_timestamp: q.venue_timestamp,
        age_ms: now_ms.saturating_sub(q.received_ms).max(0).unsigned_abs(),
        delay: *entry.delay.lock().unwrap_or_else(PoisonError::into_inner),
        sequence: seq,
    })
}

/// `Response::MarketData` over a BORROWED frame: serde writes it exactly as that variant (an externally tagged
/// newtype variant of the same name, in JSON and in msgpack alike), so one frame is framed in several forms
/// without a copy.
#[derive(serde::Serialize)]
enum MarketDataReply<'f> {
    MarketData(&'f MarketDataFrame),
}

/// Serialize ONE frame into the wire's own length-prefixed encoding, in the form `codec` its reader reads
/// (`vike_hub_protocol::frame::BodyCodec`, the form of the session's `MarketDataSubscribe`), so the pre-framed bytes go
/// through the same `MAX_FRAME_LEN` check every other frame does and the writer's `write_all` is
/// verbatim. `None` when the frame could not be framed at all (over the ceiling — three orders of
/// magnitude away from anything this wire produces, and a silent skip is the only safe answer since
/// there is no per-frame error channel).
pub(crate) fn frame_bytes(frame: &MarketDataFrame, codec: BodyCodec) -> Option<Arc<Vec<u8>>> {
    // `encode_reply` leaves its four prefix bytes for the writer to fill: these bytes are written verbatim.
    let mut bytes = encode_reply(codec, &MarketDataReply::MarketData(frame)).ok()?;
    let len = u32::try_from(bytes.len() - 4).ok().filter(|len| *len <= MAX_FRAME_LEN)?;
    bytes[..4].copy_from_slice(&len.to_be_bytes());
    Some(Arc::new(bytes))
}

/// The forms ([`BodyCodec`]) one key's subscribers read, as the publisher's pre-test saw them.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MarketDataForms([bool; 3]);

impl MarketDataForms {
    pub(crate) fn add(&mut self, codec: BodyCodec) {
        self.0[form_index(codec)] = true;
    }
}

/// [`MarketDataForms`]' slot of `codec`.
fn form_index(codec: BodyCodec) -> usize {
    match codec {
        BodyCodec::Json => 0,
        BodyCodec::Msgpack => 1,
        BodyCodec::MsgpackLz4 => 2,
    }
}

/// One frame's bytes in each form its subscribers read, every one encoded BEFORE the fan-out, so the
/// publisher's "serialize once, never under a lock" holds once per form ([`frame_bytes`]).
pub(crate) struct MarketDataFrameBytes([Option<Arc<Vec<u8>>>; 3]);

impl MarketDataFrameBytes {
    /// `frame` in each of `forms`; `None` when no form could be framed.
    pub(crate) fn of(frame: &MarketDataFrame, forms: MarketDataForms) -> Option<Self> {
        let codecs = [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4];
        let bytes = codecs
            .map(|codec| forms.0[form_index(codec)].then(|| frame_bytes(frame, codec)).flatten());
        bytes.iter().any(Option::is_some).then_some(Self(bytes))
    }

    /// The bytes in `codec`'s form, when a subscriber in it was seen.
    pub(crate) fn get(&self, codec: BodyCodec) -> Option<Arc<Vec<u8>>> {
        self.0[form_index(codec)].clone()
    }

    /// The longest of its forms: what the frame-size watch compares to its ceiling.
    pub(crate) fn longest(&self) -> usize {
        self.0.iter().flatten().map(|b| b.len()).max().unwrap_or(0)
    }
}

/// An RAII hold on a market-data session.
///
/// ⚠ **`Drop` rather than an explicit call**, mirroring `crates/vike-tradehub/src/publish.rs`'s
/// `PublisherSubscription`: a PANIC in the writer thread must release every key this session held. A release
/// written as the last statement of `run_market_writer` is correct on every ordinary return path and
/// leaks a venue refcount forever on the panic path — and a leaked nonzero refcount is never reaped,
/// so the socket lives forever and `MARKET_DATA_MAX_KEYS_TOTAL` is permanently spent. On a daemon that runs
/// for weeks that ends service with no error line anywhere.
pub struct MarketDataSessionGuard {
    hub: Arc<MarketDataHub>,
    id: MarketDataSessionId,
    mailbox: Arc<MarketDataMailbox>,
    released: bool,
}

impl std::fmt::Debug for MarketDataSessionGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketDataSessionGuard").field("id", &self.id).finish_non_exhaustive()
    }
}

impl MarketDataSessionGuard {
    /// This session's id — what a client presents in a later `MarketDataUpdate`.
    pub fn id(&self) -> MarketDataSessionId {
        self.id
    }

    /// This session's mailbox.
    pub fn mailbox(&self) -> &Arc<MarketDataMailbox> {
        &self.mailbox
    }

    /// Release now, at an explicit clock — the seam the suite uses so a [`MARKET_DATA_LINGER`] property is
    /// expressible without sleeping a minute. `Drop` calls it with the real clock.
    pub fn release_at(&mut self, now_ms: i64) {
        if self.released {
            return;
        }
        self.released = true;
        self.hub.close_session(self.id, now_ms);
    }
}

impl Drop for MarketDataSessionGuard {
    fn drop(&mut self) {
        self.release_at(vike_model::now_ms());
    }
}

/// Push one frame straight into a subscriber's mailbox — the attach path, which bypasses the
/// publisher because its frames are per-SUBSCRIBER rather than per-key.
pub fn push_attach_frame(
    mailbox: &MarketDataMailbox,
    key: &MarketDataKey,
    frame: MarketDataFrame,
) -> MarketDataMailboxPushOutcome {
    let lane = match &frame {
        MarketDataFrame::Status { .. } | MarketDataFrame::Bye(_) | MarketDataFrame::Heartbeat => {
            MarketDataMailboxLaneClass::Control
        }
        MarketDataFrame::Trades { .. } | MarketDataFrame::TapeGap { .. } => {
            MarketDataMailboxLaneClass::Tape
        }
        MarketDataFrame::Depth(_) | MarketDataFrame::Book(_) | MarketDataFrame::Quote(_) => {
            MarketDataMailboxLaneClass::Book
        }
    };
    let Some(bytes) = frame_bytes(&frame, mailbox.body_codec()) else {
        return MarketDataMailboxPushOutcome::Enqueued;
    };
    mailbox.push(MarketDataMailboxOutgoing {
        key: key.clone(),
        lane,
        bytes,
        sequence: 0,
        ticks: 0,
        hub_dropped: 0,
    })
}

/// The tape's bounded push — used by the sink, exposed here so the eviction accounting lives beside
/// the entry that owns it.
pub(crate) fn push_trade(entry: &MarketDataStreamEntry, mut tick: TickTrade) {
    // ⚠ DROP THE SYMBOL. The symbol is CONSTANT per `MarketDataKey` and the `Trades` frame carries it
    // once in the envelope, so a tape entry's own is empty: what that frame has always served. (It was a
    // `String` until 2026-11-20, whose heap this reclaimed: §12.4's second finding, a 78-character polymarket
    // token id per entry; an interned `vike_marketdata::Symbol` has none, and an entry is 48 B.)
    tick.symbol = Default::default();
    let mut evicted = 0u64;
    {
        let mut g = entry.tape.lock().unwrap_or_else(PoisonError::into_inner);
        while g.len() >= MARKET_DATA_TAPE_CAP {
            g.pop_front();
            evicted += 1;
        }
        g.push_back(tick);
    }
    entry.note_tape_drop(evicted);
    entry.mark_dirty();
}

/// Replace a key's latest-wins book slot. Built OUTSIDE the lock, moved in.
pub(crate) fn store_book(entry: &MarketDataStreamEntry, state: MarketDataBookState) {
    // Clamp to the CEILING at ingest, so the hub's own memory is bounded by a server constant rather
    // than by whatever the venue publishes. The per-subscriber cut to `effective_depth` happens at
    // publish; both are needed — this one makes the MEMORY claim true, that one the WIRE claim.
    let cap = MARKET_DATA_DEPTH_LEVELS_CEILING as usize;
    let state = MarketDataBookState {
        bids: state.bids.into_iter().take(cap).collect(),
        asks: state.asks.into_iter().take(cap).collect(),
        ..state
    };
    *entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(state));
    entry.mark_dirty();
}

/// Replace a key's latest-wins status slot.
///
/// ⚠ **A gap or a staleness empties the quote slot, and first**: the publisher emits `Status` and then the key's data
/// frame in one tick, so the price the stream was cut off at would follow its own gap, and the client, which forgets the
/// price on the status and takes any `Quote` as a stream that flows, would show a frozen bid and ask as live. Emptied
/// before the status is visible, so no tick can read the new status and the old price together. A book lane has the same
/// old republish (`crates/vike-datahub/tests/gateway_twin.rs` counts it) and is left alone.
pub(crate) fn store_status(entry: &MarketDataStreamEntry, status: StreamStatus) {
    if matches!(status, StreamStatus::GapStart { .. } | StreamStatus::Stale { .. }) {
        *entry.quote_slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    *entry.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
    entry.mark_status_dirty();
}

/// Replace a key's latest-wins quote slot.
pub(crate) fn store_quote(entry: &MarketDataStreamEntry, quote: MarketDataQuoteState) {
    *entry.quote_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(quote);
    entry.mark_dirty();
}

/// Replace a key's delay label. Dirty, so a price already on screen is re-sent under its new label.
pub(crate) fn store_delay(entry: &MarketDataStreamEntry, delay: StreamDelay) {
    *entry.delay.lock().unwrap_or_else(PoisonError::into_inner) = Some(delay);
    entry.mark_dirty();
}
