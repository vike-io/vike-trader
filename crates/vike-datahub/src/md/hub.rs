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
//! DOM windows on one symbol across three desktops is ONE venue socket and ONE folded book.
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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, Weak};

use vike_data::live::{DataClient, LiveDataSink, SubscriptionId};
use vike_datahub_client::market::{
    BookSnapshot, MD_DEPTH_LEVELS_CEILING, MdFrame, MdLane, MdRefusal, MdSessionId, MdSpec,
    WireStreamStatus, validate_md_symbol,
};
use vike_datahub_client::proto::{Response, write_frame};
use vike_model::TradeTick;

use super::mailbox::{LaneClass, Mailbox, Outgoing, PushOutcome};
use super::{
    MD_FRAME_CEILING_BYTES, MD_LINGER, MD_MAX_KEYS_PER_VENUE, MD_MAX_KEYS_RESIDENT,
    MD_MAX_KEYS_TOTAL, MD_MAX_SPECS_PER_SESSION, MD_MAX_STREAM_CONNS, MD_TAPE_CAP,
};

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
/// [`super::venues::real_market_venue_table`] (behind `live-feeds`) names a bridge.
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
    pub(crate) bids: Vec<vike_model::Level>,
    pub(crate) asks: Vec<vike_model::Level>,
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

    /// PIN a key as tier R — a refcount floor of 1, never released. Idempotent.
    ///
    /// # ⚠ It RUNS [`MdHub::acquire`]'s CHECKS, and it did not
    ///
    /// This went straight to the registry: no `vike_model::VENUES` membership, no `served` check, no
    /// [`vike_data::require_live_verb`], and no cap of any kind. Three things followed, none of them
    /// visible to an operator:
    ///
    /// 1. A typo'd `VIKE_DATAHUB_LIVE_RESIDENT` row (`notavenue:X:depth`, or a real venue on a lane
    ///    it does not serve) PARSED — [`super::parse_resident_set`] validates only the three-field
    ///    shape and the lane word — and then produced an endless retry: a resident entry is
    ///    permanently `wanted`, so [`MdHub::reconcile`] phase 1 attempts it every pass and
    ///    [`MdHub::spawn`]'s loop logs the failure every [`super::MD_REAP_INTERVAL`], forever.
    /// 2. [`MD_MAX_KEYS_PER_VENUE`]'s own doc says "RESIDENT INCLUDED", and it was read only inside
    ///    `acquire`'s on-demand admission — so residents were counted BY it and never bounded by it.
    /// 3. With [`MD_MAX_KEYS_TOTAL`] deliberately exempting residents (§5.4), nothing bounded the
    ///    tier-R set at all: N rows armed N venue subscriptions, which is memory (§12.6's hub term)
    ///    and venue budget (200 resident binance depth keys is ~2,000 weight/min of steady-state
    ///    re-seed against a 2,400/min IP budget shared with the order-signing daemon).
    ///
    /// # The refusal is a `String`, not an `MdRefusal`, deliberately
    ///
    /// This one never crosses the wire — a resident row is the DAEMON's own declaration, and the
    /// only reader of the refusal is the operator's log. A wire variant would widen a type whose
    /// every arm is a client-facing classification (`MdRefusal::is_permanent`) with a case no client
    /// can ever see. The caller (`crate::datahub_cli`) logs it beside the unparseable-row warning:
    /// degrade and NAME the row, never refuse startup — a venue subscription is a CAPABILITY
    /// (`docs/decisions/0013-degrade-vs-refuse.md`).
    pub fn add_resident(&self, spec: &MdSpec) -> Result<(), String> {
        // The same order `acquire` refuses in — cheapest and most permanent first, which since
        // 2026-09-11 starts with the SYMBOL. Sharing the validator is what stops the daemon
        // admitting through `VIKE_DATAHUB_LIVE_RESIDENT` what it refuses on the wire; it also
        // removes an asymmetry that ran the other way, because `super::parse_resident_set` has
        // always refused an empty symbol in the operator's own declaration while `acquire` accepted
        // one from a network client.
        validate_md_symbol(&spec.symbol)?;
        let key = MdKey::of(spec);
        let depth = spec.resolved_depth();
        if !vike_model::VENUES.contains(&key.venue.as_str()) {
            return Err(format!("`{}` is not a venue in vike_model::VENUES", key.venue));
        }
        if !self.served.iter().any(|v| v == &key.venue) {
            return Err(format!(
                "this build links no market-data client for `{}` — it serves [{}]",
                key.venue,
                self.served.join(", ")
            ));
        }
        if let Err(e) = vike_data::require_live_verb(&key.venue, key.lane.live_verb()) {
            return Err(e.to_string());
        }

        let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
        let already = g
            .get(&key.venue)
            .and_then(|s| s.get(&key.symbol))
            .and_then(|slots| slots[lane_index(key.lane)].as_ref())
            .is_some();
        if !already {
            let venue_keys: u32 = g
                .get(&key.venue)
                .map(|s| s.values().flatten().flatten().count() as u32)
                .unwrap_or(0);
            if venue_keys >= MD_MAX_KEYS_PER_VENUE {
                return Err(format!(
                    "`{}` already holds {venue_keys} keys on venue `{}` (MD_MAX_KEYS_PER_VENUE = \
                     {MD_MAX_KEYS_PER_VENUE}, the cap that leaves the order-signing daemon its share \
                     of this box's per-IP venue budget)",
                    key.symbol, key.venue
                ));
            }
            let residents: u32 = g
                .values()
                .flat_map(|s| s.values())
                .flatten()
                .flatten()
                .filter(|e| e.is_resident())
                .count() as u32;
            if residents >= MD_MAX_KEYS_RESIDENT {
                return Err(format!(
                    "this daemon already holds {residents} resident keys (MD_MAX_KEYS_RESIDENT = \
                     {MD_MAX_KEYS_RESIDENT}, the term §12.6's hub-memory arithmetic — and with it \
                     the 32 MB co-hosting claim — is computed over)"
                ));
            }
        }
        let slots = g.entry(key.venue.clone()).or_default().entry(key.symbol.clone()).or_default();
        let idx = lane_index(key.lane);
        match &slots[idx] {
            // A key already admitted on demand becomes resident too; the declared depth is a FLOOR
            // that outlives every subscriber, so it is recorded separately from the live max.
            Some(e) => {
                e.pin_resident(depth);
            }
            None => {
                slots[idx] = Some(Arc::new(StreamEntry::new(key, true, depth)));
            }
        }
        drop(g);
        self.poke();
        Ok(())
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

    // -------------------------------------------------------------------------------------------
    // Sessions
    // -------------------------------------------------------------------------------------------

    /// Open a market-data session, refusing past [`MD_MAX_STREAM_CONNS`].
    ///
    /// The refusal is a whole-REQUEST condition, so it is a `Result<_, String>` the caller answers
    /// with `Response::Error` — never an all-refused `MdSubscribed`, which would mode-switch a
    /// connection into a writer with nothing to write. See
    /// `vike_datahub_client::market::MdRefusal`'s doc for the invariant.
    pub fn open_session(self: &Arc<Self>) -> Result<SessionGuard, String> {
        let n = self.stream_conns.fetch_add(1, Ordering::AcqRel);
        if n as usize >= MD_MAX_STREAM_CONNS {
            self.stream_conns.fetch_sub(1, Ordering::AcqRel);
            return Err(format!(
                "this datahub already serves {MD_MAX_STREAM_CONNS} market-data stream connections \
                 (the cap that bounds this plane's mailbox memory); nothing was subscribed. Close a \
                 stream and retry."
            ));
        }
        let id = MdSessionId::fresh();
        let mailbox = Mailbox::new();
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, SessionState { keys: BTreeMap::new(), mailbox: Arc::clone(&mailbox) });
        Ok(SessionGuard { hub: Arc::clone(self), id, mailbox, released: false })
    }

    /// This session's mailbox, or `None` if it has ended.
    pub fn mailbox_of(&self, id: MdSessionId) -> Option<Arc<Mailbox>> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|s| Arc::clone(&s.mailbox))
    }

    /// Whether this session is still open — what an `MdUpdate` naming an unknown or expired session
    /// is refused on.
    pub fn has_session(&self, id: MdSessionId) -> bool {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner).contains_key(&id)
    }

    /// The keys one session holds, in key order.
    pub fn session_keys(&self, id: MdSessionId) -> Vec<MdKey> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|s| s.keys.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// End a session: release every key it held (stamping the [`MD_LINGER`] deadline at `now_ms`),
    /// close its mailbox, free its connection slot, and poke the reconciler.
    pub fn close_session(&self, id: MdSessionId, now_ms: i64) {
        let held = {
            let mut g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            match g.remove(&id) {
                Some(s) => {
                    s.mailbox.close();
                    s.keys
                }
                None => return,
            }
        };
        self.stream_conns.fetch_sub(1, Ordering::AcqRel);
        for key in held.into_keys() {
            self.release_key(&key, now_ms);
        }
        self.poke();
    }

    // -------------------------------------------------------------------------------------------
    // acquire / release
    // -------------------------------------------------------------------------------------------

    /// Take one spec for `session`. **Never blocks, never calls a venue, never fails on I/O.**
    ///
    /// Returns the AUTHORITATIVE spec — the request with its depth clamped, which §4.3 says is an
    /// acceptance with a smaller number rather than a refusal.
    ///
    /// The refusal ORDER is cheapest-and-most-permanent first, each naming its own number:
    /// [`MdRefusal::SymbolRejected`] → [`MdRefusal::UnknownVenue`] → [`MdRefusal::VenueNotServed`]
    /// → [`MdRefusal::LaneUnsupported`]
    /// (from [`vike_data::require_live_verb`], so §7.5's gate is a re-read of the declared matrix
    /// rather than a hand list) → [`MdRefusal::SpecCapSession`] → [`MdRefusal::KeyCapVenue`] →
    /// [`MdRefusal::KeyCapTotal`].
    ///
    /// # ⚠ The SYMBOL rung is new, and this function used to validate everything but it
    ///
    /// Of a spec's five fields, `venue` faced `vike_model::VENUES`, `lane` is a fieldless enum serde
    /// refuses to decode as anything else, `depth_levels` is clamped by
    /// `vike_datahub_client::market::MdSpec::resolved_depth`, and `session` is a `u128`. `symbol`
    /// was bounded by nothing but the post-auth 64 MiB frame ceiling. A subscribe naming a
    /// 10,000-character symbol was ACCEPTED: two `String` clones into [`MdKey`], both locks, an
    /// `Arc<StreamEntry>`, two more clones into the registry, and a `poke()` that woke the
    /// reconciler into calling `subscribe_depth(venue, <10 KB>)` on the REAL venue. Then
    /// [`MdHub::attach_frames`] emitted a `Status` carrying that string verbatim — it does so for
    /// every accepted key, whether or not any venue ever streams for it — far over
    /// [`super::MD_CTRL_FRAME_CEILING_BYTES`], which is a TERM in `MD_MAILBOX_BYTES`' compile-time
    /// assertion and therefore in the 32 MB claim
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on. That record's
    /// decision 2 — *"A subscription's cost is bounded by server constants and by typed refusals,
    /// never by the request"* — is what the bound RESTORES rather than adds.
    ///
    /// It goes FIRST because it is both the cheapest check (a `len()` and a byte scan, no lock, no
    /// roster walk) and the most permanent, which is the order this list already declares — and
    /// because "refused before anything is allocated or spawned" is only true if it precedes
    /// [`MdKey::of`].
    pub fn acquire(&self, id: MdSessionId, spec: &MdSpec) -> Result<MdSpec, MdRefusal> {
        self.acquire_changed(id, spec).map(|(s, _)| s)
    }

    /// [`MdHub::acquire`] plus the one fact [`MdHub::update`] needs and the wire does not: whether
    /// this session's HOLDING of the key changed — it is new to the session, or the depth it asks
    /// for is a different number from the one it asked for before.
    ///
    /// # ⚠ Why an acceptance is not the same as a change, and why the difference is a BOUND
    ///
    /// [`MdHub::acquire`] returns `Ok` for a key the session already holds — `already_held_by_session`
    /// suppresses the refcount bump and nothing else — so a caller reading only the `Result` cannot
    /// tell a duplicate from a new key. [`MdHub::update`] attaches one `Status` (and, when the key is
    /// `Live`, one book) per key it pushes, and `crate::server`'s `refuse_an_oversized_spec_list`
    /// bounds ONE request at [`MD_MAX_SPECS_PER_SESSION`] = 64 against a
    /// [`super::MD_MAILBOX_CTRL`] of 128. Attaching per ACCEPTED spec would therefore make the ctrl
    /// cost a function of what the client SENT rather than of what changed: three requests re-naming
    /// the same 64 already-held specs — no stall, no slow link, no venue event — push 192 frames onto
    /// a 128-deep lane and the peer is answered with `MdBye::ControlLaneOverflow`. That is #1753's
    /// failure reached through a new producer, and `docs/decisions/0052`'s decision 2 — *"a
    /// subscription's cost is bounded by server constants … never by the request"* — false again on
    /// the one term that record had already had to restore.
    ///
    /// Attaching per CHANGE costs the shipped desktop nothing:
    /// `crates/vike-app-core/src/md_session.rs`'s `MdSession::diff` computes `add` by
    /// `MdSpec::key()` against the server's authoritative `served` set, so it never re-names a key it
    /// is already served. The gate bites only a client re-sending its own set.
    ///
    /// The DEPTH half of the predicate is what closes the depth-change case with no `mark_dirty`
    /// anywhere: see [`StreamEntry::settle_depth`].
    ///
    /// ⚠ **This gate alone does NOT give [`MdHub::update`] the bound above, and the second half is
    /// in that function.** The depth term is what makes a re-send at a NEW depth a change, which is
    /// correct per spec and wrong per KEY: `MdKey::of` ignores `depth_levels`, so one key named 64
    /// times at 64 different depths answers `true` 64 times. `update` collects into a `BTreeSet`
    /// for exactly that reason, and the bound it actually delivers is *distinct keys changed*.
    fn acquire_changed(&self, id: MdSessionId, spec: &MdSpec) -> Result<(MdSpec, bool), MdRefusal> {
        if let Err(why) = validate_md_symbol(&spec.symbol) {
            return Err(MdRefusal::SymbolRejected(why));
        }
        if !vike_model::VENUES.contains(&spec.venue.as_str()) {
            return Err(MdRefusal::UnknownVenue);
        }
        if !self.served.iter().any(|v| v == &spec.venue) {
            return Err(MdRefusal::VenueNotServed(self.served.join(", ")));
        }
        if let Err(e) = vike_data::require_live_verb(&spec.venue, spec.lane.live_verb()) {
            // The venue's own `&'static str`, forwarded verbatim — the wire's refusal set cannot
            // drift from `crates/vike-model/src/venue_caps.rs`'s declared rows.
            return Err(MdRefusal::LaneUnsupported(e.to_string()));
        }

        let key = MdKey::of(spec);
        let depth = spec.resolved_depth();

        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = sessions.get_mut(&id) else {
            // A caller that lost its session between the check and here; treat as a total-cap
            // refusal rather than inventing a variant nothing else can produce.
            return Err(MdRefusal::KeyCapTotal { held: 0, cap: MD_MAX_KEYS_TOTAL });
        };
        let already_held_by_session = state.keys.contains_key(&key);
        if !already_held_by_session && state.keys.len() as u32 >= MD_MAX_SPECS_PER_SESSION {
            return Err(MdRefusal::SpecCapSession {
                held: state.keys.len() as u32,
                cap: MD_MAX_SPECS_PER_SESSION,
            });
        }

        let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
        let existing = g
            .get(&key.venue)
            .and_then(|s| s.get(&key.symbol))
            .and_then(|slots| slots[lane_index(key.lane)].clone());

        if existing.is_none() {
            // ⚠ RESIDENT keys count against the per-VENUE cap and NOT against the total (§5.4): an
            // operator's declared set must never be squeezed out by ad-hoc charts, and
            // `MD_MAX_KEYS_TOTAL = 0` must still serve tier R.
            let venue_keys: u32 = g
                .get(&key.venue)
                .map(|s| s.values().flatten().flatten().count() as u32)
                .unwrap_or(0);
            if venue_keys >= MD_MAX_KEYS_PER_VENUE {
                return Err(MdRefusal::KeyCapVenue {
                    venue: key.venue.clone(),
                    held: venue_keys,
                    cap: MD_MAX_KEYS_PER_VENUE,
                });
            }
            let total_on_demand: u32 = g
                .values()
                .flat_map(|s| s.values())
                .flatten()
                .flatten()
                .filter(|e| !e.is_resident())
                .count() as u32;
            if total_on_demand >= MD_MAX_KEYS_TOTAL {
                return Err(MdRefusal::KeyCapTotal {
                    held: total_on_demand,
                    cap: MD_MAX_KEYS_TOTAL,
                });
            }
        }

        let entry = match existing {
            Some(e) => {
                e.raise_depth(depth);
                e
            }
            None => {
                let e = Arc::new(StreamEntry::new(key.clone(), false, depth));
                g.entry(key.venue.clone()).or_default().entry(key.symbol.clone()).or_default()
                    [lane_index(key.lane)] = Some(Arc::clone(&e));
                e
            }
        };

        // ⚠ **BOTH OF THESE ARE PUBLISHED UNDER `keys.write()`, AND THAT IS WHAT MAKES
        // `reconcile` PHASE 3's RE-CHECK MEAN ANYTHING.** They used to run after `drop(g)`, and the
        // two lines were the whole remaining refcount race: the reaper re-evaluates `wanted` from
        // the slot's current `Arc` while it holds this same lock, so an admission that had already
        // released it — but had not yet bumped the count or cleared the deadline — was still read as
        // unwanted and its slot cleared under a session that now holds the key. Narrowing that
        // window to a few atomics is not closing it, and the comment in phase 3 claimed closed.
        // Neither line needs the lock to be released: both are plain atomics on the entry, and the
        // one thing that genuinely cannot move up (`settle_depth`, which folds over `sessions`) is
        // still below.
        if !already_held_by_session {
            entry.subscribers.fetch_add(1, Ordering::AcqRel);
        }
        // ⚠ CLEAR the linger deadline. Without this a key released and re-acquired inside the linger
        // keeps its ABSOLUTE deadline and is reaped 60 s later anyway — a DOM window toggled off and
        // on then loses its book, in the exact series somebody just asked for.
        entry.zero_since.store(-1, Ordering::Release);
        drop(g);

        // ⚠ RECORDED PER SESSION, and re-recorded on a re-`acquire` of a key this session already
        // holds — an `MdUpdate` may LOWER what this subscriber wants, and the entry's own field is a
        // max that cannot be recomputed from anything else. See `StreamEntry::depth`.
        //
        // The previous value is also the CHANGE answer this function is here to give: `None` is a
        // key new to the session, a different depth is a new request for one it already held, and
        // the SAME depth is a re-send that asks for nothing the first mention did not.
        let changed = state.keys.insert(key.clone(), depth) != Some(depth);
        // The `&mut` borrow of one session ends above, so the fold over ALL of them is legal here
        // and still inside the ONE `sessions` lock this function took — no second acquisition, no
        // window in which a concurrent release could fold against a set this one is not in yet.
        let want = max_requested_depth(&sessions, &key);
        entry.settle_depth(want);
        drop(sessions);
        self.poke();

        Ok((MdSpec { depth_levels: Some(depth), ..spec.clone() }, changed))
    }

    /// Drop one reference to a key. **No venue call.** At zero, stamp the [`MD_LINGER`] deadline.
    ///
    /// ⚠ **Callers must have removed the key from the session's own set FIRST** (both do:
    /// [`MdHub::close_session`] removes the whole session before it releases, and [`MdHub::update`]
    /// removes the entry before it calls here) — the depth refold below folds over what REMAINS, and
    /// a session still holding the key it is releasing would fold its own departing request back in.
    fn release_key(&self, key: &MdKey, now_ms: i64) {
        // ⚠ SESSIONS FIRST, THEN KEYS — the order every path in this file takes them in. The fold is
        // computed and the guard DROPPED before `entry` touches `keys.read`, so this holds one lock
        // at a time and cannot invert against `acquire`.
        let want = {
            let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            max_requested_depth(&sessions, key)
        };
        let Some(entry) = self.entry(&key.venue, &key.symbol, key.lane) else { return };
        // SATURATING, never a bare `fetch_sub`: a decrement below zero would WRAP to `u32::MAX` and
        // pin the key live forever, which is the leak this whole RAII path exists to prevent.
        let prev = entry
            .subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| Some(n.saturating_sub(1)))
            .unwrap_or(0);
        if prev <= 1 && !entry.is_resident() {
            entry.zero_since.store(now_ms, Ordering::Release);
        }
        entry.settle_depth(want);
    }

    /// The `MdUpdate` verb's registry mutation: add, remove, **attach**, report.
    ///
    /// `remove` matches on the KEY and ignores `depth_levels` (`MdSpec::key`), and a `remove` naming
    /// a spec this session never held is silently absent from `released` rather than an error.
    ///
    /// # ⚠ THIS IS THE SECOND DOOR INTO THE REGISTRY, AND FOR A WHILE ONLY THE FIRST ONE ATTACHED
    ///
    /// `MdSubscribe` reaches the registry through `crate::server`'s `run_market_writer`, which
    /// pushes [`MdHub::attach_frames`] per accepted key before it enters its drain loop. This verb
    /// is the OTHER door, and it is the one **every DOM window after the first** takes:
    /// `crates/vike-app-core/src/md_session.rs`'s `push_update` opens a fresh short-lived connection
    /// and sends `MdUpdate`, because the stream socket's server side has left its read loop for
    /// good. It used to bump the refcount, fold the depth and push NOTHING.
    ///
    /// Nothing else covered for it. [`MdHub::publish_tick`] skips any entry whose `dirty` bit is
    /// clear, and the three writers of that bit are all SINK-side (`store_book`, `store_status`,
    /// `push_trade`) — `acquire` sets it nowhere. So the adding session received no status and no
    /// snapshot **until that venue's next update**: seconds to minutes on a quiet polymarket
    /// instrument, and the whole of §12.5's reconnect state on binance depth.
    ///
    /// The client cannot paper over it and is not allowed to try — §10 rules out a snapshot-request
    /// verb deliberately — and the symptom is worse than a blank ladder. `MdSession::gapped` is
    /// populated only by an arriving `Status` frame, so a key that receives NOTHING is not gapped
    /// and `refresh_statuses` counts it among the LIVE streams: an empty DOM with the Connections
    /// tool reporting the venue is fine, which is the exact failure that field's own doc says it
    /// exists to prevent, reached through a door that doc did not know about.
    ///
    /// # Why the attach and not `acquire().mark_dirty()`
    ///
    /// Marking the entry dirty was the cheaper-looking fix and it is a correctness regression, in
    /// four ways that no amount of care in `publish_tick` would fix without turning it into this
    /// function: it emits a `Status` only when `status_dirty` is set, so the status-first rule §5.2
    /// step 5 states would be bypassed on the one path where the ordering is new information; its
    /// data arm has NO `Live` gate, so a key sitting in `GapStart`/`Stale` or lingering with a
    /// two-minute-old book would be shipped to a client that stamps a FRESH local receipt (§7.4) and
    /// renders it live — the hazard [`MdHub::attach_frames`]' structure exists to enforce against;
    /// it emits nothing at all when `book_slot` is `None`, which is the quiet-instrument case this
    /// whole fix is about; and it serves the `Trades` lane not at all. It would also BROADCAST:
    /// one dirty key enqueues to every subscriber of it, so S−1 sessions that already hold the book
    /// are republished to on behalf of a session that did not ask — work on the box that also signs
    /// orders, scaling with the number of open DOM windows on a hot symbol.
    ///
    /// So the push reuses `attach_frames` **verbatim** — status first, book only when `Live`, tape
    /// never replayed are properties of THAT function's structure, and a second copy of them here is
    /// where the next divergence would land — and it goes to the ONE session that asked.
    /// [`MdHub::publish_tick`] is untouched by this fix, so §6.1's "one serialization per dirty key
    /// per tick" stays literally true: the attach path is the documented per-SUBSCRIBER lane
    /// ([`push_attach_frame`]), it runs on the short-lived `MdUpdate` dispatch thread, and it holds
    /// no lock the publisher wants.
    ///
    /// ⚠ It attaches once per KEY CHANGED, not per spec ACCEPTED — two gates, not one, because a
    /// spec carries a depth and a key does not. [`MdHub::acquire_changed`] is the first and carries
    /// why the ctrl lane needs it; the `BTreeSet` in the add loop below is the second.
    ///
    /// ⚠ It runs AFTER the add loop, not inside it: `acquire` has by then raised and settled
    /// [`StreamEntry::depth`] for every spec in the batch, so the snapshot is cut at the depth the
    /// whole request asked for rather than at the depth the batch had reached mid-loop.
    ///
    /// **This defect was found by the wire CLIENT while §9's half was being built (#1758) and
    /// reported against the server rather than fixed there.**
    #[allow(clippy::type_complexity)]
    pub fn update(
        &self,
        id: MdSessionId,
        add: &[MdSpec],
        remove: &[MdSpec],
        now_ms: i64,
    ) -> (Vec<MdSpec>, Vec<(MdSpec, MdRefusal)>, Vec<MdSpec>) {
        // ONE lookup for both halves below. `None` is a session that has already ended, and both
        // halves are then correctly skipped — `Mailbox::push` on a closed box discards anyway.
        let mailbox = self.mailbox_of(id);
        let mut released = Vec::new();
        for spec in remove {
            let key = MdKey::of(spec);
            let dropped = {
                let mut g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
                match g.get_mut(&id) {
                    Some(s) => s.keys.remove(&key).is_some(),
                    None => false,
                }
            };
            if dropped {
                self.release_key(&key, now_ms);
                // ⚠ AND THE KEY'S QUEUED BOOK GOES WITH IT. The BOOK lane is the one lane nothing
                // evicts — `Mailbox::enforce_bounds` drains only the tape, and says why: the byte
                // bound's compile-time assertion *"is what guarantees a full session of
                // ceiling-depth books fits"*. That assertion is
                // `MD_MAX_SPECS_PER_SESSION * MD_FRAME_CEILING_BYTES + …`, so it rests entirely on
                // `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` — and this loop broke it: a removed
                // key's already-queued slot was never dropped, so one `remove 64 + add 64` against a
                // writer that is a tick behind leaves 128 unevictable slots, 1.86× the bound, with
                // nothing able to reclaim them. Attaching on update (below) makes that reachable
                // INSIDE one request rather than one tick later, which is why it is closed here.
                //
                // It is also right on its own terms: `MdSession::apply_frame` routes a book into
                // `BookStore` by `(venue, symbol)` and never consults the served set, so a late
                // frame for a removed key repopulates a store entry the window that wanted it has
                // just dropped.
                //
                // The TAPE is deliberately left alone: it is bounded AND evictable, so it cannot
                // falsify the assertion, and a mailbox's owed gaps are a disclosure that must
                // survive a key leaving and coming back.
                //
                // ⚠ THIS IS ONE THIRD OF THE CLOSE AND NOT THE WHOLE OF IT. Reclaiming the slot
                // here is worth nothing on its own while something can enqueue a NEW one for the
                // same key a moment later, and there are two somethings. `MdHub::publish_tick`
                // reads the session table once per TICK, so a list taken at its top says this
                // session still holds the key for the whole remainder of that tick — closed by
                // `MdHub::fanout`, which re-reads that table under its lock at push time, the same
                // lock the `s.keys.remove` two lines above took. And the attach push at the end of
                // this function cannot test membership where it pushes (it serializes, so it may
                // not hold that lock) — closed by the sweep that follows it. All three, or the
                // bound is luck.
                //
                // The declared residual: `crate::server`'s `run_market_writer` attaches the same
                // way after `MdSubscribe`, and it has no sweep. A client that fires an `MdUpdate`
                // removing a key it has only just subscribed to — it has the session id, the
                // `MdSubscribed` reply precedes that attach loop — can leave one slot per raced key
                // behind. That door predates this function and is not widened by it; its own
                // ctrl-lane `Status` reaches `MD_MAILBOX_CTRL` and closes the connection long
                // before the accumulation becomes interesting.
                if let Some(mailbox) = &mailbox {
                    mailbox.drop_book(&key);
                }
                released.push(spec.clone());
            }
        }
        let mut accepted = Vec::new();
        let mut refused = Vec::new();
        // ⚠ A SET, not a `Vec`, and that is the OTHER half of the ctrl-lane bound the `changed` gate
        // starts. `MdKey::of` ignores `depth_levels` while `MdSpec::resolved_depth` maps every
        // `Some(n)` in 1..=MD_DEPTH_LEVELS_CEILING to its own number, so ONE key named 64 times at
        // 64 different depths is 64 accepted specs, 64 `changed == true` answers — each `insert`
        // returns the PREVIOUS spec's depth — and, from a `Vec`, 64 attach pushes for one key.
        // `crate::server`'s `refuse_an_oversized_spec_list` caps the LENGTH at
        // `MD_MAX_SPECS_PER_SESSION` and leaves dedup to the client ("drop any duplicate spec" is
        // advice in the refusal text, not enforcement), so three such requests against a wedged
        // writer reach 192 on a 128-deep `MD_MAILBOX_CTRL` — the exact bound `acquire_changed`'s doc
        // claims the `changed` gate restores, breached through the term it does not cover. With the
        // set the push count is bounded by DISTINCT keys touched, which is what that doc says.
        //
        // Deduping by key is also what the snapshot WANTS: `attach_frames` cuts at
        // `effective_depth()`, which `acquire` has already folded to the deepest of the request's
        // mentions by the time this loop ends, so one push per key carries the answer all 64 asked
        // for and 63 of them would have carried a cut nobody is owed twice.
        let mut attach = BTreeSet::new();
        for spec in add {
            match self.acquire_changed(id, spec) {
                Ok((s, changed)) => {
                    if changed {
                        attach.insert(MdKey::of(&s));
                    }
                    accepted.push(s);
                }
                Err(r) => refused.push((spec.clone(), r)),
            }
        }
        if let Some(mailbox) = &mailbox {
            for key in &attach {
                for frame in self.attach_frames(key, now_ms) {
                    push_attach_frame(mailbox, key, frame);
                }
            }
            // ⚠ THE ATTACH PUSH IS THE ONE BOOK PRODUCER THAT CANNOT TEST MEMBERSHIP WHERE IT
            // PUSHES, so it tests it AFTERWARDS. `push_attach_frame` SERIALIZES (`frame_bytes`), and
            // serializing up to `MD_MAX_SPECS_PER_SESSION` ceiling-depth snapshots while holding the
            // session table would block the publisher and every `acquire` for the duration — §6.1's
            // rule, and the reason `MdHub::fanout` can afford the lock and this loop cannot.
            //
            // The hole it leaves is the same one `fanout` closes for the publisher, reached from a
            // second `MdUpdate` on THIS session (each arrives on its own short-lived connection and
            // is dispatched on its own thread, so a client may race its own two requests): that one
            // removes the key and reclaims the slot with `Mailbox::drop_book` while this one is
            // mid-attach, and our push then re-creates it for a key the session no longer holds —
            // unevictable, against the bound `MD_MAILBOX_BYTES`' assertion rests on.
            //
            // A sweep AFTER the pushes closes every interleaving of the two, which a re-check
            // BEFORE them would not. Write P for our push, S for this sweep, R for their
            // `keys.remove` and D for their `drop_book`; program order is P<S and R<D, and the six
            // orders are P S R D | P R S D | P R D S | R P S D | R P D S | R D P S. D reclaims in
            // four of them and S in the other two, so no order ends holding the slot. A key that a
            // third request legitimately RE-ADDS before S reads the table is held, so S leaves it —
            // which is right: it has a subscriber again.
            let unheld: Vec<MdKey> = {
                let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
                match g.get(&id) {
                    Some(s) => {
                        attach.iter().filter(|k| !s.keys.contains_key(*k)).cloned().collect()
                    }
                    // The session ended under us; its mailbox is closed and nothing will drain it.
                    None => attach.iter().cloned().collect(),
                }
            };
            for key in &unheld {
                mailbox.drop_book(key);
            }
        }
        self.poke();
        (accepted, refused, released)
    }

    // -------------------------------------------------------------------------------------------
    // attach
    // -------------------------------------------------------------------------------------------

    /// What a NEW subscriber is handed for one key, in this order: the key's current `Status`, then
    /// — **only if that status is `Live`** — its current book snapshot (§5.2 step 5).
    ///
    /// ⚠ **The status-first rule is not decoration, and the ORDER is enforced by this function's
    /// STRUCTURE rather than by a comment.** Without it, a slot that has been stale for two minutes
    /// (venue in `GapStart`, or a quiet resident stream) is written into `BookStore` with a FRESH
    /// LOCAL receipt stamp and reads LIVE at the desktop for a whole `DOM_STALE_MS` window — a
    /// populated, fresh-looking ladder for a market that stopped ticking two minutes ago, which is
    /// indistinguishable from a quiet market. All three source proposals had this hole.
    ///
    /// A key with no status yet emits `GapStart{now_ms}` — the honest reading of "wanted, not yet
    /// live" — and no book, so the client shows "waiting for first book" rather than an empty ladder
    /// that reads as a real, thin market.
    ///
    /// ⚠ **The TAPE is deliberately NOT replayed, and §8 left that open.** §7.3 rule 4 is the
    /// answer: never resume a tape across a socket. The server keeps no replay buffer, so a
    /// reconnect must be a HOLE, never a duplicate — handing a new subscriber prints from before it
    /// existed, with no gap marker, double-counts every reconnect into `OrderflowAgg`, which has no
    /// per-trade dedup.
    ///
    /// It assigns NO `seq`: the publisher owns the counter, so an attach reads the entry's CURRENT
    /// seq and the first pushed frame for that key is `seq + 1`, contiguous by construction.
    pub fn attach_frames(&self, key: &MdKey, now_ms: i64) -> Vec<MdFrame> {
        let Some(entry) = self.entry(&key.venue, &key.symbol, key.lane) else { return Vec::new() };
        let status = entry
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unwrap_or(WireStreamStatus::GapStart { at_ts_ms: now_ms });
        let mut out = vec![MdFrame::Status {
            venue: key.venue.clone(),
            symbol: key.symbol.clone(),
            lane: key.lane,
            status,
        }];
        if !matches!(status, WireStreamStatus::Live { .. }) {
            return out;
        }
        if matches!(key.lane, MdLane::Depth | MdLane::Book) {
            let book = entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(b) = book {
                let seq = entry.seq.load(Ordering::Acquire);
                let snap = snapshot_of(&entry.key, &b, entry.effective_depth(), seq);
                out.push(match key.lane {
                    MdLane::Book => MdFrame::Book(snap),
                    _ => MdFrame::Depth(snap),
                });
            }
        }
        out
    }

    // -------------------------------------------------------------------------------------------
    // publish
    // -------------------------------------------------------------------------------------------

    /// Enqueue one ALREADY-SERIALIZED frame to every session that holds `key` **at this instant**,
    /// building the per-subscriber envelope once per target.
    ///
    /// # ⚠ The membership test and the push are under ONE lock, and that is the whole point
    ///
    /// [`MdHub::publish_tick`] reads the session table once per tick and then serializes its way
    /// down the dirty set, so a target list taken at the top is stale by the time most keys are
    /// pushed — a window a whole tick wide, opening at every DOM-window close. Inside it,
    /// [`MdHub::update`]'s remove path can drop the key from the session, `release_key` it and
    /// reclaim its queued frame with `Mailbox::drop_book`, and a push against the stale list then
    /// RE-CREATES the slot. Nothing evicts a book slot — `Mailbox::enforce_bounds` drains the tape
    /// only, because [`super::MD_MAILBOX_BYTES`]' assertion is what guarantees the books fit — so
    /// the resurrected slot survives until the writer drains, which under a wedged writer is never,
    /// and `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` stops being an invariant. The client half
    /// is the same frame's other cost: `crates/vike-app-core/src/md_session.rs`'s `apply_frame`
    /// routes a book into `BookStore` by `(venue, symbol)` without consulting the served set, so it
    /// repopulates a store entry the window that wanted it has just dropped.
    ///
    /// Taking `sessions` HERE closes both, because the remove path holds the same lock while it
    /// takes the key out of the session and calls `drop_book` after it: a push either precedes the
    /// removal — and `drop_book` then reclaims what it queued — or reads a table the key is already
    /// gone from and enqueues nothing. There is no third ordering.
    ///
    /// ⚠ It closes the PUBLISHER and only the publisher. The other book producer is the attach
    /// push, which serializes and therefore may not hold this lock at all; [`MdHub::update`] answers
    /// that one with a sweep after its pushes instead, and declares the one door that still has
    /// neither.
    ///
    /// ⚠ **It costs one uncontended `sessions` acquisition per FRAME, not per subscriber, and no
    /// serialization happens inside it** — `bytes` is an `Arc` the caller built before calling, so
    /// §6.1's "serialize once, never under a lock" is untouched and only the envelope is per-target.
    /// The lock order is the file's own, `sessions` then the mailbox's: nothing in
    /// [`super::mailbox::Mailbox`] can reach back for the session table, and `close_session` already
    /// nests the two this way.
    fn fanout(&self, key: &MdKey, make: impl Fn() -> Outgoing) {
        let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        for s in g.values() {
            if s.keys.contains_key(key) {
                s.mailbox.push(make());
            }
        }
    }

    /// Fold every DIRTY key into at most one status frame and one data frame, serialize each ONCE,
    /// and enqueue to every subscriber of that key.
    ///
    /// ⚠ **The wire `seq` is assigned HERE, before any drop decision** (§7.2). That is what gives
    /// `TapeGap` a backstop the dropper cannot lie about: a contiguity break at the client means
    /// exactly one thing — this connection did not receive frames the server produced. An
    /// implementation that assigned it AFTER the drop decision would still look self-consistent
    /// (the dropper reports its own count, the surviving frames are contiguous) and would have no
    /// pre-drop numbers to report, so every disclosed range would collapse to `from + 1`.
    ///
    /// It never writes a socket and never calls a `DataClient` —
    /// `crates/vike-tradehub/src/publish.rs`'s `broadcast` discipline.
    pub fn publish_tick(&self) -> TickReport {
        let mut report = TickReport::default();
        // ⚠ SESSIONS FIRST, THEN KEYS — every path in this file takes the two in that order
        // (`acquire` takes `sessions` then `keys.write`), and taking them the other way round here
        // would be the classic inversion: one publish tick against one concurrent subscribe is all
        // it needs. (This line said "ONE pass over the session table per tick, not per key" and it
        // is no longer the whole truth: this pass is the pre-test, and `MdHub::fanout` takes the
        // table again per FRAME PRODUCED — uncontended, O(sessions ≤ MD_MAX_STREAM_CONNS), with
        // nothing serialized inside it. What the sentence was protecting — no per-SUBSCRIBER walk
        // of the table, no serialization under a lock — still holds.)
        //
        // ⚠ **THIS IS A PRE-TEST AND NOT THE TARGET LIST**, and the difference is a bound. Its only
        // job is to answer *is anyone holding this key at all* before the tick pays for a
        // serialization — §6.1's rule that a key nobody subscribes to costs nothing. The frames are
        // handed out by [`MdHub::fanout`], which re-reads the session table UNDER ITS LOCK at push
        // time. A target list snapshotted HERE is stale for the whole remainder of the tick, and a
        // concurrent `MdUpdate` removing a key inside that window re-created the book slot
        // [`MdHub::update`] had just reclaimed with `Mailbox::drop_book` — a slot nothing evicts
        // (`Mailbox::enforce_bounds` drains the tape only), so the invariant
        // `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` that [`super::MD_MAILBOX_BYTES`]'
        // assertion rests on was breachable by ordinary DOM-window churn against a writer a tick
        // behind. Being stale in the OTHER direction is harmless and deliberately not chased: a
        // session that joins a key mid-tick is handed `attach_frames` by whichever door it came
        // through, so the worst case is one book delivered twice, superseded in its own slot.
        let subscribed: BTreeSet<MdKey> = {
            let g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            g.values().flat_map(|s| s.keys.keys().cloned()).collect()
        };
        let entries = self.snapshot();

        for entry in entries {
            if !entry.dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            report.keys_walked += 1;
            let key = &entry.key;
            let listening = subscribed.contains(key);

            // ⚠ **THE PER-TICK DELTAS ARE CONSUMED WHETHER OR NOT ANYONE IS LISTENING, and that is
            // what makes §7.3 rule 4 STRUCTURAL.** A tape held while a key has no subscriber hands
            // the NEXT subscriber prints from before it existed — with no gap marker, because from
            // the hub's side nothing was dropped — and `OrderflowAgg` has no per-trade dedup to
            // notice. The server keeps NO replay buffer, so a reconnect is a HOLE, never a
            // duplicate, and the cheapest way to guarantee that is never to accumulate one. The BOOK
            // slot is deliberately NOT consumed: it is latest-wins state, and `attach_frames` is
            // what decides whether a new subscriber may see it (only when the status is `Live`).
            let status_changed = entry.status_dirty.swap(false, Ordering::AcqRel);
            let drained = if matches!(key.lane, MdLane::Trades) {
                let mut g = entry.tape.lock().unwrap_or_else(PoisonError::into_inner);
                let batch: Vec<TradeTick> = g.drain(..).collect();
                drop(g);
                Some((batch, entry.dropped.swap(0, Ordering::AcqRel)))
            } else {
                None
            };
            if !listening {
                continue;
            }

            // 1. STATUS, if it changed. Ctrl lane, so it is drained ahead of any book backlog.
            if status_changed {
                let status = *entry.status.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(status) = status {
                    let frame = MdFrame::Status {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        lane: key.lane,
                        status,
                    };
                    if let Some(bytes) = frame_bytes(frame) {
                        report.frames_produced += 1;
                        self.fanout(key, || Outgoing {
                            key: key.clone(),
                            lane: LaneClass::Ctrl,
                            bytes: Arc::clone(&bytes),
                            seq: 0,
                            ticks: 0,
                            hub_dropped: 0,
                        });
                    }
                }
            }

            // 2. THE DATA FRAME. One per key per tick — the property §12.4's whole mailbox
            //    derivation rests on, and the reason no `MD_TAPE_BATCH_MAX` is introduced.
            match key.lane {
                MdLane::Depth | MdLane::Book => {
                    let book =
                        entry.book_slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    let Some(b) = book else { continue };
                    // ⚠ seq BEFORE any drop decision.
                    let seq = entry.seq.fetch_add(1, Ordering::AcqRel) + 1;
                    let snap = snapshot_of(key, &b, entry.effective_depth(), seq);
                    let frame = match key.lane {
                        MdLane::Book => MdFrame::Book(snap),
                        _ => MdFrame::Depth(snap),
                    };
                    let Some(bytes) = frame_bytes(frame) else { continue };
                    entry.note_frame_size(bytes.len());
                    report.frames_produced += 1;
                    self.fanout(key, || Outgoing {
                        key: key.clone(),
                        lane: LaneClass::Book,
                        bytes: Arc::clone(&bytes),
                        seq,
                        ticks: 0,
                        hub_dropped: 0,
                    });
                }
                MdLane::Trades => {
                    let (batch, hub_dropped) = drained.expect("the tape lane always drains above");
                    if batch.is_empty() && hub_dropped == 0 {
                        continue;
                    }
                    let seq = entry.seq.fetch_add(1, Ordering::AcqRel) + 1;
                    let ticks = batch.len() as u64;
                    let frame = MdFrame::Trades {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        ticks: batch,
                        seq,
                    };
                    let Some(bytes) = frame_bytes(frame) else { continue };
                    report.frames_produced += 1;
                    self.fanout(key, || Outgoing {
                        key: key.clone(),
                        lane: LaneClass::Tape,
                        bytes: Arc::clone(&bytes),
                        seq,
                        ticks,
                        hub_dropped,
                    });
                }
            }
        }
        report
    }

    // -------------------------------------------------------------------------------------------
    // reconcile
    // -------------------------------------------------------------------------------------------

    /// Drive every venue client to exactly the DESIRED set — the ONLY place a `DataClient` method is
    /// called. See the module doc for why this replaces §5.2 step 4 and §5.3's janitor.
    pub fn reconcile(&self, now_ms: i64) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let entries = self.snapshot();

        let wanted = |e: &StreamEntry| -> bool {
            if e.is_resident() || e.held() > 0 {
                return true;
            }
            let z = e.zero_since.load(Ordering::Acquire);
            z >= 0 && now_ms < z.saturating_add(MD_LINGER.as_millis() as i64)
        };

        let mut clients = self.venues.lock().unwrap_or_else(PoisonError::into_inner);

        // -- phase 1: START what arrived -----------------------------------------------------
        for entry in &entries {
            if !wanted(entry.as_ref()) {
                continue;
            }
            if entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                continue; // a SURVIVOR: leave it strictly alone.
            }
            let venue = entry.key.venue.clone();
            if !clients.contains_key(&venue) {
                match (self.builder)(&venue, self.sink()) {
                    Ok(c) => {
                        clients.insert(venue.clone(), c);
                    }
                    Err(e) => {
                        report.failed.push(format!("{venue}: {e}"));
                        continue;
                    }
                }
            }
            let client = clients.get_mut(&venue).expect("just inserted");
            let started = match entry.key.lane {
                MdLane::Depth => client.subscribe_depth(&entry.key.symbol),
                MdLane::Book => client.subscribe_book(&entry.key.symbol),
                MdLane::Trades => client.subscribe_trades(&entry.key.symbol),
            };
            match started {
                Ok(id) => {
                    *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner) = Some(id);
                    report.started += 1;
                }
                Err(e) => {
                    // ⚠ NO phantom refcount and NO phantom sub_id: the key stays WANTED with
                    // `sub_id = None`, which is the `GapStart` state a client already understands,
                    // and the next pass retries it. A failure here must never look like a live
                    // subscription — that is §6.1's "connects, reports healthy, delivers nothing".
                    report.failed.push(format!("{}/{}: {e}", entry.key.venue, entry.key.symbol));
                }
            }
        }

        // -- phase 2: STOP what left ---------------------------------------------------------
        let mut reap: HashMap<String, Vec<(Arc<StreamEntry>, SubscriptionId)>> = HashMap::new();
        let mut live_per_venue: HashMap<String, usize> = HashMap::new();
        for entry in &entries {
            if entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                *live_per_venue.entry(entry.key.venue.clone()).or_default() += 1;
            }
        }
        for entry in &entries {
            if wanted(entry.as_ref()) {
                continue;
            }
            let id = *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(id) = id {
                reap.entry(entry.key.venue.clone()).or_default().push((Arc::clone(entry), id));
            }
        }
        for (venue, victims) in reap {
            let whole = live_per_venue.get(&venue).copied().unwrap_or(0) == victims.len();
            if let Some(client) = clients.get_mut(&venue) {
                if whole {
                    // ⚠ The two-phase idiom is legitimate ONLY here: `begin_shutdown` is
                    // `FeedRegistry::raise_stops`, which raises EVERY flag this client owns, so it
                    // is correct exactly when the reap set IS the client's whole live set. This is
                    // also the only place the raise-all-then-join win is available.
                    client.begin_shutdown();
                    client.shutdown();
                } else {
                    // A partial reap: per-key `unsubscribe` only. One socket read timeout per key,
                    // on THIS thread — off every serving path, and invisible to every client.
                    for (_, id) in &victims {
                        client.unsubscribe(*id);
                    }
                }
            }
            if whole {
                clients.remove(&venue);
                report.clients_dropped += 1;
            }
            for (entry, _) in &victims {
                *entry.sub_id.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
            report.stopped += victims.len();
        }
        drop(clients);

        // -- phase 3: forget the reaped entries -----------------------------------------------
        //
        // ⚠ **THE DECISION IS RE-TAKEN UNDER THE WRITE LOCK, AND THE SLOT IS IDENTITY-CHECKED.**
        // Deciding outside it and deleting by KEY inside it is a real race, not a theoretical one:
        // `acquire` publishes its refcount AFTER it releases `keys.write` (it must — `settle_depth`
        // and the session bookkeeping run under `sessions`, not `keys`), so a filter that saw a key
        // unwanted, then blocked on this lock while `acquire` took it, cleared a slot a session now
        // holds. Every consequence was SILENT: `attach_frames` returns an empty vec for a missing
        // entry so the client is not even sent its `GapStart`; the sink's `lookup` finds nothing;
        // `publish_tick` never walks it; `release_key` returns early so the accounting never
        // unwinds; and because `state.keys` still names the key, a later `acquire` takes the
        // already-held branch, skips the `fetch_add`, and leaves the session holding a key against
        // an entry it holds no reference on.
        //
        // Re-evaluating `wanted` from the SLOT's current `Arc` closes the refcount race, and
        // `Arc::ptr_eq` closes the second one — a slot REPLACED between the two steps must not be
        // deleted on the strength of a verdict about the entry it replaced.
        //
        // ⚠ **The re-check is only half of the closure, and the other half is in `acquire`.** This
        // comment claimed the race closed while `acquire` still published its refcount and cleared
        // its linger deadline AFTER releasing this lock — so a re-check taken inside that window
        // reads the same stale `held() == 0` the filter did, and re-deciding changes nothing. Both
        // atomics now happen under `keys.write()` there, which is what makes "the decision is
        // re-taken under the write lock" a closure rather than a narrower window.
        let doomed: Vec<MdKey> =
            entries.iter().filter(|e| !wanted(e.as_ref())).map(|e| e.key.clone()).collect();
        if !doomed.is_empty() {
            let judged: HashMap<&MdKey, &Arc<StreamEntry>> =
                entries.iter().map(|e| (&e.key, e)).collect();
            let mut g = self.keys.write().unwrap_or_else(PoisonError::into_inner);
            for key in &doomed {
                if let Some(symbols) = g.get_mut(&key.venue) {
                    if let Some(slots) = symbols.get_mut(&key.symbol) {
                        let idx = lane_index(key.lane);
                        let still_doomed = match (&slots[idx], judged.get(key)) {
                            // `judged` holds BORROWED entries, so the pattern takes the inner
                            // reference out of the `&&Arc` `HashMap::get` hands back.
                            (Some(current), Some(&at_filter_time)) => {
                                Arc::ptr_eq(current, at_filter_time) && !wanted(current.as_ref())
                            }
                            _ => false,
                        };
                        if still_doomed {
                            slots[idx] = None;
                        }
                        if slots.iter().all(Option::is_none) {
                            symbols.remove(&key.symbol);
                        }
                    }
                    if symbols.is_empty() {
                        g.remove(&key.venue);
                    }
                }
            }
        }
        report
    }

    /// Wait up to `timeout` for an `acquire`/`release` poke. The reconcile thread's whole loop body
    /// besides [`MdHub::reconcile`].
    ///
    /// ⚠ **It CONSULTS the flag, and for a while it did not** — it went straight into
    /// `wait_timeout` and then cleared `*g`, so the boolean was written by [`MdHub::poke`] and read
    /// by nobody. A `notify_all` delivered while no thread was parked was simply discarded: the
    /// classic missed notification the flag exists to defeat.
    ///
    /// The lost window is not microseconds. [`MdHub::reconcile`] is by design the thread that
    /// performs every blocking venue call — a whole-venue reap JOINS feed threads, a partial one
    /// costs a socket read timeout per key — so an `MdSubscribe` arriving during a pass had all of
    /// its pokes swallowed and then waited out the pass PLUS a full [`super::MD_REAP_INTERVAL`]
    /// before its venue subscription was even attempted. Since `run_market_writer` acquires each
    /// spec separately, a multi-spec subscribe straddling that boundary could leave some keys
    /// started and the rest stalled in `GapStart`.
    ///
    /// The same predicate loop is what makes a SPURIOUS wakeup harmless: it re-parks instead of
    /// spending a reconcile pass on nothing.
    pub fn wait_for_poke(&self, timeout: std::time::Duration) {
        let (m, cv) = &self.wake;
        let mut g = m.lock().unwrap_or_else(PoisonError::into_inner);
        if !*g {
            let (g2, _) = cv
                .wait_timeout_while(g, timeout, |woken| !*woken)
                .unwrap_or_else(PoisonError::into_inner);
            g = g2;
        }
        *g = false;
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
/// ⚠ **The levels are already BEST-FIRST** — `crates/vike-model/src/orderbook.rs`'s `L2Book::top_n`
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
