//! [`MdHub`] — the registry that makes the data daemon the SINGLE subscriber (ruling 2): one venue
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
//! **Tier D — ON-DEMAND.** Client-driven, ref-counted per [`MdKey`] **across every session**. Five
//! Trade windows on one symbol across three desktops is ONE venue socket and ONE folded book.
//!
//! # ⚠ THE ONE STRUCTURAL DEPARTURE FROM THE SIGNED-OFF §5, AND WHY
//!
//! §5.2 step 4 has the CONNECTION thread call `hub.acquire(key)`, which on refcount 0 builds the
//! venue's `DataClient` and calls `subscribe_*` there; §5.3 then adds a SEPARATE `md-janitor` thread
//! described as "the sole caller of every blocking `DataClient` method". Those two sentences cannot
//! both be true, and the pair puts venue I/O on a request path. Here there is **one owner**:
//!
//! ```text
//! acquire(key)  -> bump the refcount, mark WANTED, poke a Condvar. No venue call. Never blocks.
//! release(key)  -> drop the refcount; at zero, stamp `zero_since`. No venue call.
//! reconcile()   -> the ONLY holder of any DataClient. Computes
//!                  desired = {resident} ∪ {refcount>0} ∪ {refcount==0 AND inside MD_LINGER}
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
//!    `crates/bridges/polymarket/src/market_feed.rs` shards and reseats seats and
//!    `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS` table records a per-token REST
//!    warmup plus a 10 s dial on that path. With a reconciler it does not matter what any of them
//!    do: **nothing a venue does can stall an `MdSubscribe` reply.**
//! 3. The client vocabulary already covers the latency this introduces. A wanted-but-not-yet-served
//!    key attaches as `Status(GapStart)` and becomes `Live` when it lands — §9's asynchronous-refusal
//!    rule, so "accepted, arriving shortly" needs no new type.
//! 4. §8 items 4 and 9 COLLAPSE, and §5.3's "there is no release channel" argument gets stronger
//!    rather than weaker: there is no channel AND no second thread.
//!
//! It also removes a real bug — see this module's parent doc on `FeedRegistry::raise_stops`.
//!
//! # ⚠ Depth is resolved per KEY, not per subscriber
//!
//! §6.1 requires ONE serialization per dirty key per tick ("N subscribers of one symbol cost one
//! encode"), so a key's effective depth is the **max** over its live subscribers, clamped to
//! `vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING`. The consequence, stated because it is
//! real: one client asking for 200 levels inflates the frame for EVERY subscriber of that key. The
//! alternative — per-subscriber depth — costs the one-encode property and with it the whole §12.4
//! mailbox derivation. [`crate::md::MD_MAILBOX_BYTES`] is what absorbs the consequence, and its own
//! doc carries that argument.
//!
//! ⚠ **"over its LIVE subscribers" means the fold comes back DOWN, and for a while it did not.**
//! [`StreamEntry::depth`] was a `fetch_max` and nothing else, so the deep client's request outlived
//! the deep client — permanently on a resident key, which is never reaped — and every OTHER
//! subscriber of that key paid ~105 KB/s where §12.6 priced 27.3. [`SessionState`] therefore records
//! the depth each session asked for, `acquire` raises, and [`MdHub::release_key`] REFOLDS from what
//! remains.
//!
//! # ⚠ Every lock recovers from poisoning
//!
//! `unwrap_or_else(PoisonError::into_inner)` everywhere, §6.1's stated divergence from
//! `crates/vike-tradehub/src/publish.rs`'s `Mailbox` (`.expect("mailbox poisoned")` on every lock).
//! One panicking connection must not convert into a server-wide outage — and it is a PRECONDITION of
//! the suite's panic-path test being able to fail honestly rather than reading as a broken test.
//!
//! # Layout — one `impl MdHub` in four child files
//!
//! This file keeps the types (`MdKey`, `StreamEntry`, `MdHub` and its fields, `SessionGuard`), the
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

use vike_data::{DataClient, LiveDataSink, SubscriptionId};
use vike_datahub_client::BookSnapshot;
use vike_datahub_client::market::{
    MD_DEPTH_LEVELS_CEILING, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec, WireStreamStatus,
    validate_md_symbol,
};
use vike_datahub_client::proto::{Response, write_frame};
use vike_model::TradeTick;

use super::mailbox::{LaneClass, Mailbox, Outgoing, PushOutcome};
use super::{
    MD_FRAME_CEILING_BYTES, MD_LINGER, MD_MAX_KEYS_PER_VENUE, MD_MAX_KEYS_RESIDENT,
    MD_MAX_KEYS_TOTAL, MD_MAX_SPECS_PER_SESSION, MD_MAX_STREAM_CONNS, MD_TAPE_CAP,
};

mod publish;
mod reconcile;
mod release;
mod subscribe;

/// One subscription's identity — `(venue, symbol, lane)`. **Depth is deliberately not part of it**
/// (`vike_datahub_client::market::MdSpec::key`): two clients asking for one key at different depths
/// share ONE venue subscription.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MdKey {
    /// The venue slug.
    pub venue: String,
    /// The venue's own symbol spelling.
    pub symbol: String,
    /// Which lane.
    pub lane: MdLane,
}

impl MdKey {
    /// The key one spec names.
    pub fn of(spec: &MdSpec) -> Self {
        MdKey { venue: spec.venue.clone(), symbol: spec.symbol.clone(), lane: spec.lane }
    }
}

/// How the hub obtains a venue's market-data client.
///
/// ⚠ **A `Box<dyn Fn>` and therefore FEATURE-FREE**, exactly like `crate::backfill::BackfillTable`
/// and for the identical reason: no venue crate is named in any signature, so the hub — and
/// `crate::server::serve_authed`'s parameter — compile on a DEFAULT build. Only
/// `crates/vike-datahub/src/feeds/venues.rs`'s `build_client` (its arms behind the `venue-<venue>`
/// features) names a bridge; md reaches it through [`super::venues::market_builder`].
///
/// It also IS the test seam: the §8 item 15 suite hands the hub a scripted `DataClient` double and
/// therefore runs on the default build, in the derived roster lane, on every PR.
pub type MarketClientBuilder = Box<
    dyn Fn(&str, Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, String> + Send + Sync,
>;

/// The pre-clamp folded book plus its stamps, as the sink left it.
#[derive(Debug, Clone)]
pub(crate) struct BookState {
    pub(crate) tick_size: f64,
    pub(crate) bids: Vec<vike_model::BookLevel>,
    pub(crate) asks: Vec<vike_model::BookLevel>,
    pub(crate) venue_ts: i64,
    pub(crate) venue_seq: u64,
}

/// One key's hub-side state.
pub(crate) struct StreamEntry {
    pub(crate) key: MdKey,
    /// Tier R: a refcount FLOOR of 1. A resident key is never reaped through client release.
    ///
    /// ATOMIC rather than a plain `bool` because [`MdHub::add_resident`] may find a slot that
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
    /// `vike_datahub_client::market::MD_DEPTH_LEVELS_DEFAULT` for that key until the key was reaped
    /// — and forever, on a RESIDENT key, which is never reaped. Since `publish_tick` serializes ONCE
    /// per key at this depth, that inflated the frame for EVERY subscriber of the key: §12.4's
    /// measured 2.74 KB became 10.48 KB, and §12.6's 27.3 KB/s per binance depth key became
    /// ~105 KB/s, permanently. [`MdHub::acquire`] raises it and [`MdHub::release_key`] REFOLDS it
    /// from the sessions that remain.
    depth: AtomicU32,
    /// The tier-R declared depth — a FLOOR that outlives every subscriber, so a resident key keeps
    /// serving the depth the operator declared once the last desktop closes. `0` on tier D.
    resident_depth: AtomicU32,
    /// Epoch-ms when the refcount last reached zero; `-1` = held. The [`MD_LINGER`] deadline.
    zero_since: AtomicI64,
    /// RECONCILER-OWNED. `None` = wanted but not yet live (the honest `GapStart` state).
    pub(crate) sub_id: Mutex<Option<SubscriptionId>>,
    /// LATEST-WINS. `Arc` so the publisher clones and drops the guard before it serializes.
    pub(crate) book_slot: Mutex<Option<Arc<BookState>>>,
    /// The STICKY latest status — read by `attach_frames`, which needs the current one rather than
    /// an unconsumed delta.
    pub(crate) status: Mutex<Option<WireStreamStatus>>,
    /// Set when `status` changed and the publisher has not yet emitted it.
    status_dirty: AtomicBool,
    /// Bounded, evict-oldest.
    pub(crate) tape: Mutex<VecDeque<TradeTick>>,
    /// HUB-side tape evictions not yet disclosed.
    dropped: AtomicU64,
    /// Set by the sink, cleared by the publisher.
    dirty: AtomicBool,
    /// Set once a produced BOOK frame has exceeded [`MD_FRAME_CEILING_BYTES`], so the disclosure is
    /// one line per key rather than one per publish tick. See [`StreamEntry::note_frame_size`].
    oversize_warned: AtomicBool,
    /// The WIRE sequence (§7.2) — assigned by the publisher ONLY, and BEFORE any drop decision.
    seq: AtomicU64,
}

impl StreamEntry {
    fn new(key: MdKey, resident: bool, depth: u16) -> Self {
        StreamEntry {
            key,
            resident: AtomicBool::new(resident),
            subscribers: AtomicU32::new(0),
            depth: AtomicU32::new(depth as u32),
            resident_depth: AtomicU32::new(if resident { depth as u32 } else { 0 }),
            zero_since: AtomicI64::new(if resident { -1 } else { 0 }),
            sub_id: Mutex::new(None),
            book_slot: Mutex::new(None),
            status: Mutex::new(None),
            status_dirty: AtomicBool::new(false),
            tape: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            oversize_warned: AtomicBool::new(false),
            seq: AtomicU64::new(0),
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
    /// [`MdHub::add_resident`], and the reason [`Self::resident`] is atomic.
    fn pin_resident(&self, depth: u16) {
        self.resident.store(true, Ordering::Release);
        self.resident_depth.fetch_max(depth as u32, Ordering::AcqRel);
        self.raise_depth(depth);
        self.zero_since.store(-1, Ordering::Release);
    }

    /// Compare one produced BOOK frame against [`MD_FRAME_CEILING_BYTES`] and say so ONCE per key.
    ///
    /// ⚠ **It exists to make the compile-time memory assertions rest on an OBSERVED fact rather than
    /// on one measurement of one venue.** [`MD_FRAME_CEILING_BYTES`] is §12.4's measured 10,479 B
    /// binance book at 200 levels a side, and `crate::md::MD_MAILBOX_BYTES`' assertion — which
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` now leans on for the
    /// §11 Q1 co-hosting claim — multiplies it by [`MD_MAX_SPECS_PER_SESSION`]. Nothing compared a
    /// real frame to it: `snapshot_of` clamps LEVELS, never bytes, so a venue/symbol whose 400
    /// levels serialize wider (a longer slug, more price or size decimals) silently falsifies both
    /// that assertion and the process-wide one above it.
    ///
    /// A `warn!` rather than a refusal, and once per key rather than per frame: the deployment must
    /// keep serving — a book that is 3% over a derived constant is not a reason to drop a
    /// subscriber — and a per-frame line at [`crate::md::MD_PUBLISH_INTERVAL`] would be the
    /// per-message logging the hot-path rule forbids. What it buys is that the next person to raise
    /// a bound finds a measurement from THIS deployment instead of one venue's number from §12.
    pub(crate) fn note_frame_size(&self, bytes: usize) {
        if bytes <= MD_FRAME_CEILING_BYTES || self.oversize_warned.swap(true, Ordering::AcqRel) {
            return;
        }
        tracing::warn!(
            venue = %self.key.venue,
            symbol = %self.key.symbol,
            lane = ?self.key.lane,
            bytes,
            ceiling = MD_FRAME_CEILING_BYTES,
            depth = self.effective_depth(),
            "vike-datahub md: a book frame is OVER MD_FRAME_CEILING_BYTES — the constant \
             crate::md::MD_MAILBOX_BYTES' compile-time assertion (and the 32 MB plane ceiling above \
             it) is computed from. Re-run §12.4's measurement for this venue before trusting either"
        );
    }

    /// REFOLD the depth from the live subscribers' maximum, never below the tier-R floor.
    ///
    /// ⚠ `want == 0` — no session holds this key at all — leaves the current value ALONE rather than
    /// zeroing it. A key with no subscriber has no frame to cut (the publisher skips it on an empty
    /// target list), and a zero would make [`MdHub::attach_frames`] hand the NEXT subscriber an
    /// empty ladder in the window before its own `acquire` raised it again.
    ///
    /// # ⚠ NEITHER THIS NOR [`Self::raise_depth`] MARKS THE ENTRY DIRTY, AND THAT IS A DECISION
    ///
    /// A depth change schedules no republish, so the cut a key's frames carry moves only when the
    /// VENUE next updates it. The half of that which produced a WRONG PICTURE is closed, and it is
    /// closed without a `mark_dirty` anywhere: the session that asked — a new subscriber joining an
    /// existing key deeper, or one re-requesting a key it already holds at a new depth — is handed
    /// [`MdHub::attach_frames`] on the spot by `run_market_writer` or by [`MdHub::update`], and that
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
    /// for a change nobody asked for, which is precisely the cost [`MdHub::update`]'s attach is
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
/// a release cannot refold [`StreamEntry::depth`] — there is nowhere else the per-subscriber
/// request survives, and the entry's own field is the max it must be recomputed from. See
/// [`StreamEntry::depth`] for what the missing fold cost.
struct SessionState {
    keys: BTreeMap<MdKey, u16>,
    mailbox: Arc<Mailbox>,
}

/// The deepest request any LIVE session holds for `key`, or `0` when none does.
///
/// A free function rather than a method so both callers can pass the `sessions` guard they already
/// hold — [`MdHub::acquire`] must fold inside the lock it took, and taking it a second time there
/// would open the window this fold exists to close.
fn max_requested_depth(sessions: &HashMap<MdSessionId, SessionState>, key: &MdKey) -> u16 {
    sessions.values().filter_map(|s| s.keys.get(key).copied()).max().unwrap_or(0)
}

/// A per-tick account of what the publisher produced.
///
/// ⚠ `frames_produced` exists as an INDEPENDENT WITNESS rather than for logs: the `TapeGap`
/// arithmetic (`to_seq - from_seq == dropped`) is tautological if an implementation computes
/// `to_seq` as `from_seq + dropped`, and two paths to the same number is what makes neither one
/// circular.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Frames serialized this tick (one per dirty key per lane class).
    pub frames_produced: u64,
    /// Dirty keys walked.
    pub keys_walked: usize,
}

/// What one reconcile pass did — the suite's non-vacuity floor, and the operator's log line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Venue subscriptions STARTED.
    pub started: usize,
    /// Venue subscriptions STOPPED.
    pub stopped: usize,
    /// Venue clients dropped entirely (their whole live set was reaped).
    pub clients_dropped: usize,
    /// Subscribe attempts that failed and will be retried on the next pass.
    pub failed: Vec<String>,
}

type LaneSlots = [Option<Arc<StreamEntry>>; 3];

fn lane_index(lane: MdLane) -> usize {
    match lane {
        MdLane::Depth => 0,
        MdLane::Book => 1,
        MdLane::Trades => 2,
    }
}

/// The registry. See the module doc.
pub struct MdHub {
    /// `venue -> symbol -> [depth, book, trades]`.
    ///
    /// ⚠ **Two levels, and that is load-bearing rather than stylistic.** `LiveDataSink`'s verbs
    /// arrive as `(&str venue, &str symbol, …)` on a hot feed thread — at polymarket's measured
    /// group p99 of 2,918 updates/s — and a flat `HashMap<MdKey, _>` cannot be looked up from
    /// borrowed parts without ALLOCATING a key per sink call. Both levels here take `get(&str)`
    /// through `Borrow`. It also yields `MD_MAX_KEYS_PER_VENUE` counting for free.
    keys: RwLock<HashMap<String, HashMap<String, LaneSlots>>>,
    /// RECONCILER-ONLY. Never locked by a sink or a writer.
    venues: Mutex<HashMap<String, Box<dyn DataClient + Send>>>,
    sessions: Mutex<HashMap<MdSessionId, SessionState>>,
    /// The ONE sink every venue client is constructed with.
    sink: Arc<super::sink::MdHubSink>,
    builder: MarketClientBuilder,
    /// Venues this BUILD links a market-data client for — the `md_venue=` advertisement.
    served: Vec<String>,
    /// `acquire`/`release` poke; the reconciler waits on it.
    wake: (Mutex<bool>, Condvar),
    stream_conns: AtomicU32,
}

impl std::fmt::Debug for MdHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdHub").field("served", &self.served).finish_non_exhaustive()
    }
}

impl MdHub {
    /// A hub over a venue-client builder. **Spawns NO threads** — see [`MdHub::spawn`].
    ///
    /// ⚠ The publish tick and the reconcile pass are CALLABLE FUNCTIONS
    /// ([`MdHub::publish_tick`], [`MdHub::reconcile`]) and the threads are a dozen lines around
    /// them, deliberately: every property worth testing here is a statement about *what one tick
    /// produced*, and against a free-running 100 ms thread each of them becomes sleep-and-hope —
    /// the flake shape this repo has already paid for twice (`vike-data`'s `live_rec` overflow test,
    /// the tradehub daemon port race). The precedent is in this crate's own graph since ruling 10:
    /// `crates/vike-recorder/src/runtime.rs`'s `RecorderRuntime::tick` returns a report per feed and
    /// the loop lives elsewhere.
    pub fn new(builder: MarketClientBuilder, served: Vec<String>) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<MdHub>| MdHub {
            keys: RwLock::new(HashMap::new()),
            venues: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            sink: Arc::new(super::sink::MdHubSink::new(weak.clone())),
            builder,
            served,
            wake: (Mutex::new(false), Condvar::new()),
            stream_conns: AtomicU32::new(0),
        })
    }

    /// The ONE sink every venue client this hub builds is constructed with — legal because every
    /// `LiveDataSink` verb carries `venue` and `symbol`.
    pub fn sink(&self) -> Arc<dyn LiveDataSink> {
        Arc::clone(&self.sink) as Arc<dyn LiveDataSink>
    }

    /// The venues this build serves, for the `md_venue=` advertisement.
    pub fn served_venues(&self) -> &[String] {
        &self.served
    }

    fn entry(&self, venue: &str, symbol: &str, lane: MdLane) -> Option<Arc<StreamEntry>> {
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
        lane: MdLane,
    ) -> Option<Arc<StreamEntry>> {
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
    fn snapshot(&self) -> Vec<Arc<StreamEntry>> {
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

    /// Start the two owning threads — `md-reconcile` and `md-publish`.
    ///
    /// ⚠ **The threads are a dozen lines around two CALLABLE functions**, and the split is the
    /// difference between a suite that GATES and one that flakes: every property worth testing here
    /// is a statement about what ONE tick produced, and against a free-running 100 ms thread each of
    /// them becomes sleep-and-hope. This repo has paid for that shape twice already (`vike-data`'s
    /// `live_rec` overflow test, the tradehub daemon port race), and a test that sometimes passes is
    /// not evidence.
    ///
    /// ⚠ **NEITHER THREAD IS JOINED, AND NO SIGNAL HANDLER IS INSTALLED** — see
    /// `crate::datahub_cli`'s market-data block for the argument. The hub holds nothing durable: an
    /// in-memory tape and open venue sockets, both of which a restart rebuilds and neither of which
    /// a venue cares about.
    ///
    /// ⚠ **SO THIS PLANE RELIES ON PROCESS EXIT, AND THERE IS NO HUB-LEVEL TEARDOWN TO CALL.**
    /// There WAS an `MdHub::shutdown` — "tear every venue client down at process stop" — that
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
            .name("md-reconcile".into())
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
                            "vike-datahub md: venue subscriptions reconciled"
                        );
                    }
                    reconcile.wait_for_poke(super::MD_REAP_INTERVAL);
                }
            })
            .expect("spawn md-reconcile");

        let publish = Arc::clone(self);
        std::thread::Builder::new()
            .name("md-publish".into())
            .spawn(move || {
                loop {
                    publish.publish_tick();
                    std::thread::sleep(super::MD_PUBLISH_INTERVAL);
                }
            })
            .expect("spawn md-publish");
    }
}

/// Cut a folded book down to `depth` levels a side and stamp it for the wire.
///
/// ⚠ **The levels are already BEST-FIRST** — `crates/vike-marketdata/src/orderbook.rs`'s `L2Book::top_n`
/// returns bids descending and asks ascending, and the sink stores them in that order — so this
/// truncates from the FRONT, never sorts. `BookSnapshot`'s own doc carries the contract.
fn snapshot_of(key: &MdKey, b: &BookState, depth: usize, seq: u64) -> BookSnapshot {
    BookSnapshot {
        venue: key.venue.clone(),
        symbol: key.symbol.clone(),
        tick_size: b.tick_size,
        bids: b.bids.iter().take(depth).copied().collect(),
        asks: b.asks.iter().take(depth).copied().collect(),
        venue_ts: b.venue_ts,
        venue_seq: b.venue_seq,
        seq,
    }
}

/// Serialize ONE frame into the wire's own length-prefixed encoding, so the pre-framed bytes go
/// through the same `MAX_FRAME_LEN` check every other frame does and the writer's `write_all` is
/// verbatim. `None` when the frame could not be framed at all (over the ceiling — three orders of
/// magnitude away from anything this wire produces, and a silent skip is the only safe answer since
/// there is no per-frame error channel).
pub(crate) fn frame_bytes(frame: MdFrame) -> Option<Arc<Vec<u8>>> {
    let mut buf = Vec::new();
    let resp = Response::Md(Box::new(frame));
    write_frame(&mut buf, &resp).ok()?;
    Some(Arc::new(buf))
}

/// An RAII hold on a market-data session.
///
/// ⚠ **`Drop` rather than an explicit call**, mirroring `crates/vike-tradehub/src/publish.rs`'s
/// `Subscription`: a PANIC in the writer thread must release every key this session held. A release
/// written as the last statement of `run_market_writer` is correct on every ordinary return path and
/// leaks a venue refcount forever on the panic path — and a leaked nonzero refcount is never reaped,
/// so the socket lives forever and `MD_MAX_KEYS_TOTAL` is permanently spent. On a daemon that runs
/// for weeks that ends service with no error line anywhere.
pub struct SessionGuard {
    hub: Arc<MdHub>,
    id: MdSessionId,
    mailbox: Arc<Mailbox>,
    released: bool,
}

impl std::fmt::Debug for SessionGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGuard").field("id", &self.id).finish_non_exhaustive()
    }
}

impl SessionGuard {
    /// This session's id — what a client presents in a later `MdUpdate`.
    pub fn id(&self) -> MdSessionId {
        self.id
    }

    /// This session's mailbox.
    pub fn mailbox(&self) -> &Arc<Mailbox> {
        &self.mailbox
    }

    /// Release now, at an explicit clock — the seam the suite uses so a [`MD_LINGER`] property is
    /// expressible without sleeping a minute. `Drop` calls it with the real clock.
    pub fn release_at(&mut self, now_ms: i64) {
        if self.released {
            return;
        }
        self.released = true;
        self.hub.close_session(self.id, now_ms);
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.release_at(vike_model::now_ms());
    }
}

/// Push one frame straight into a subscriber's mailbox — the attach path, which bypasses the
/// publisher because its frames are per-SUBSCRIBER rather than per-key.
pub fn push_attach_frame(mailbox: &Mailbox, key: &MdKey, frame: MdFrame) -> PushOutcome {
    let lane = match &frame {
        MdFrame::Status { .. } | MdFrame::Bye(_) | MdFrame::Heartbeat => LaneClass::Ctrl,
        MdFrame::Trades { .. } | MdFrame::TapeGap { .. } => LaneClass::Tape,
        MdFrame::Depth(_) | MdFrame::Book(_) => LaneClass::Book,
    };
    let Some(bytes) = frame_bytes(frame) else { return PushOutcome::Enqueued };
    mailbox.push(Outgoing { key: key.clone(), lane, bytes, seq: 0, ticks: 0, hub_dropped: 0 })
}

/// The tape's bounded push — used by the sink, exposed here so the eviction accounting lives beside
/// the entry that owns it.
pub(crate) fn push_trade(entry: &StreamEntry, mut tick: TradeTick) {
    // ⚠ DROP THE SYMBOL. §12.4's second finding: `TradeTick` is 64 B but carries a `String`, and a
    // polymarket token id is 78 characters — so a real polymarket tape entry costs ~142 B, the
    // per-key tape becomes ~582 KB and 64 keys becomes 37 MB, which on its own breaks §5.5's budget.
    // The symbol is CONSTANT per `MdKey` and the `Trades` frame carries it once in the envelope.
    // ⚠ `String::new()`, NOT `String::clear()` — `clear` retains the heap capacity, which IS the
    // quantity being reclaimed.
    tick.symbol = String::new();
    let mut evicted = 0u64;
    {
        let mut g = entry.tape.lock().unwrap_or_else(PoisonError::into_inner);
        while g.len() >= MD_TAPE_CAP {
            g.pop_front();
            evicted += 1;
        }
        g.push_back(tick);
    }
    entry.note_tape_drop(evicted);
    entry.mark_dirty();
}

/// Replace a key's latest-wins book slot. Built OUTSIDE the lock, moved in.
pub(crate) fn store_book(entry: &StreamEntry, state: BookState) {
    // Clamp to the CEILING at ingest, so the hub's own memory is bounded by a server constant rather
    // than by whatever the venue publishes. The per-subscriber cut to `effective_depth` happens at
    // publish; both are needed — this one makes the MEMORY claim true, that one the WIRE claim.
    let cap = MD_DEPTH_LEVELS_CEILING as usize;
    let state = BookState {
        bids: state.bids.into_iter().take(cap).collect(),
        asks: state.asks.into_iter().take(cap).collect(),
        ..state
    };
    *entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(state));
    entry.mark_dirty();
}

/// Replace a key's latest-wins status slot.
pub(crate) fn store_status(entry: &StreamEntry, status: WireStreamStatus) {
    *entry.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
    entry.mark_status_dirty();
}
