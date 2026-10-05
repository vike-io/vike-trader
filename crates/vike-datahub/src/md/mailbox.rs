//! The per-subscriber BOUNDED frame queue, and the three-lane overflow policy that makes a
//! market-data laggard's losses honest (§6.2).
//!
//! # Why this is re-implemented rather than imported
//!
//! `crates/vike-tradehub/src/publish.rs`'s `Mailbox` is the shape, and it cannot be reused: both
//! crates declare `layer = 65` and `crates/vike-ops/tests/layer_gate.rs` fails on `to >= from`, so
//! the edge does not exist and may not. Re-homing that type below layer 30 is a bigger diff than the
//! ~200 lines here. §6.2 makes the same call.
//!
//! # ⚠ It DIVERGES from the precedent in policy, and the divergence is the point
//!
//! The account plane has ONE rule — drop-oldest — because every frame there is a full snapshot of
//! the same thing. Here three lanes have three different truths:
//!
//! | lane | on overflow | why |
//! |---|---|---|
//! | `Depth` / `Book` | **SUPERSEDE the unsent frame for that key IN PLACE** | Latest-wins is strictly better than drop-oldest for a conflating ladder: a laggard is served the NEWEST book, never a stale one from four ticks ago. A superseded book is a superseded book, not a loss. |
//! | `Trades` | **DISCARD, COUNT, and OWE a `TapeGap`** | A silently dropped print permanently corrupts CVD, delta and footprint volume in `crates/vike-orderflow/src/bar_agg.rs`'s `OrderflowAgg`, which has no per-trade dedup and no error channel. Loss here must be impossible to hide. |
//! | `Status` / `Bye` | **RESERVED LANE, never evicted; if even it cannot be filled the connection is CLOSED** | The status lane is the only disclosure of a VENUE-side gap. A slow consumer that loses the notice that its tape has a hole is worse off than one that loses the connection. |
//!
//! ⚠ **Copying the precedent verbatim is the single likeliest implementation error in this design**,
//! because §6.2 tells you to model on a file whose rule is drop-oldest. Under drop-oldest a laggard
//! is served a book from `MD_MAILBOX_CAP` ticks ago and then jumps — a stale ladder that reads as
//! live, which is §7.3 rule 1's hazard wearing a different hat.
//!
//! # ⚠ Supersede by SLOT, not by queue position
//!
//! A book key occupies **at most one** mailbox entry no matter how far behind the writer is:
//! [`Mailbox`] keeps `book_slots: HashMap<MdKey, _>` plus a FIFO `order` of pending keys, so
//! superseding is O(1) and per-key FAIRNESS is preserved. That is strictly tighter than what §12.4's
//! derivation assumed (it sized occupancy as *keys × ticks-behind*): the ticks-behind term VANISHES
//! for the conflating lanes, so the book share of a mailbox is bounded by
//! `MD_MAX_SPECS_PER_SESSION × frame` independent of lag — which is exactly what
//! [`crate::md::MD_MAILBOX_BYTES`]'s compile-time assertion checks.
//!
//! # ⚠ `TapeGap` is NOT a control frame, and §6.2's own ordering line is why
//!
//! §6.2 says an owed `TapeGap` is written BEFORE the next `Trades` batch for that key, never after.
//! If it rode the CTRL lane it would jump ahead of *all* queued trades — including batches that
//! arrived BEFORE the gap — placing the marker too EARLY and making a client reset an aggregator
//! that then re-ingests older queued prints. So:
//!
//! > **A `TapeGap` is never queued. It is SYNTHESIZED BY THE WRITER THREAD, immediately before the
//! > next POST-HOLE `Trades` frame it dequeues for that key, from the range this mailbox recorded.**
//!
//! ⚠ **"POST-HOLE" is load-bearing and was missing.** Attaching the marker to the first frame of ANY
//! seq reproduces the CTRL-lane defect this paragraph rejects, one lane over: with seqs 1,2,3 queued
//! and 4 dropped, the client is written `TapeGap` and then `Trades(1)`, `Trades(2)`, `Trades(3)` —
//! the marker BEFORE three batches that chronologically precede the hole. [`Owed`] therefore records
//! `resume_at` rather than a wire range, and [`Mailbox::take`] hands the gap to the first dequeued
//! frame at or beyond it.
//!
//! Four properties fall out that a queued gap does not have: the ordering contract is STRUCTURAL
//! rather than a rule; a hub-side tape eviction (which the publisher knows) and a mailbox-side drop
//! (which only this file knows) funnel into ONE disclosure and cannot double-count; the ranges MERGE
//! exactly, because §7.2's pre-drop `seq` makes them arithmetic; and the per-subscriber gap frame is
//! serialized on the WRITER thread, which holds no shared lock — preserving §6.1's "serialization
//! never happens under a lock" without adding a second serialize to the publisher.
//!
//! # ⚠ Poison recovery, deliberately unlike the precedent
//!
//! Every lock here recovers with `unwrap_or_else(PoisonError::into_inner)`, where
//! `crates/vike-tradehub/src/publish.rs`'s `Mailbox` uses `.expect("mailbox poisoned")` on every
//! one. §6.1 requires the divergence and it is worth the noise: one panicking connection thread must
//! not convert into a server-wide outage on a daemon that also serves the store.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use std::sync::Arc;

use super::hub::MdKey;
use super::{MD_MAILBOX_BYTES, MD_MAILBOX_CAP, MD_MAILBOX_CTRL};

/// Which overflow policy a queued frame is governed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneClass {
    /// `Depth` / `Book` — SUPERSEDE in place.
    Book,
    /// `Trades` — DISCARD, COUNT, OWE.
    Tape,
    /// `Status` / `Bye` — reserved, never evicted.
    Ctrl,
}

/// One pre-framed frame on its way to ONE subscriber.
///
/// The `Arc<Vec<u8>>` is serialized ONCE by the publisher and shared across every subscriber of the
/// key; only this small envelope is per-subscriber, so §6.1's serialize-once win is preserved
/// exactly.
#[derive(Debug, Clone)]
pub struct Outgoing {
    /// Which key this frame belongs to — needed because the policy is per-lane per-key.
    pub key: MdKey,
    /// Which policy governs it.
    pub lane: LaneClass,
    /// The framed bytes (length prefix included).
    pub bytes: Arc<Vec<u8>>,
    /// The wire `seq` this frame carries, for [`LaneClass::Tape`] and [`LaneClass::Book`]; `0` on
    /// [`LaneClass::Ctrl`], which carries none.
    pub seq: u64,
    /// How many prints ride in this frame ([`LaneClass::Tape`] only). It is what a mailbox-side
    /// DISCARD adds to the owed gap — the mailbox cannot count prints it never decoded.
    pub ticks: u64,
    /// How many prints the HUB evicted from its own tape before this frame was built. Merged into
    /// the owed gap at push time so a hub-side eviction and a mailbox-side drop become ONE
    /// disclosure.
    pub hub_dropped: u64,
}

/// What a [`Mailbox::push`] did.
///
/// ⚠ **It is not a `Result` and it contains no `Condvar::wait`, and both are load-bearing.** "The
/// publisher is never blocked" is asserted as a TYPE property rather than proved by a wall-clock
/// test: a timing test would flake on a loaded the CI box runner, and if `push` ever DID block a
/// single-threaded test would HANG rather than fail — a CI timeout reads as infra flake, not as a
/// red gate. What the suite proves instead is the POLICY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Queued.
    Enqueued,
    /// A book frame replaced this key's unsent one. Not a loss.
    Superseded,
    /// A tape frame was discarded and a `TapeGap` is now owed for its key.
    Dropped,
    /// The CTRL lane overflowed. The writer must send `Bye` and CLOSE — see the module doc.
    CloseConnection,
    /// The mailbox is closed; the frame was discarded silently.
    Closed,
}

/// What the writer thread got.
#[derive(Debug)]
pub enum Recv {
    /// One framed payload to write, plus — when the key it belongs to owes one — the gap to
    /// SYNTHESIZE AND WRITE FIRST. See the module doc for why the writer builds it rather than the
    /// mailbox queueing it.
    Frame {
        /// The framed bytes.
        bytes: Arc<Vec<u8>>,
        /// `(key, dropped, from_seq, to_seq)` — write a `TapeGap` for this key BEFORE `bytes`.
        owed_gap: Option<(MdKey, u64, u64, u64)>,
    },
    /// The wait elapsed with nothing queued (loop, and consider a heartbeat).
    Timeout,
    /// Closed and drained — stop.
    Closed,
}

/// A gap this subscriber owes disclosure of, accumulated across all three eviction paths.
///
/// ⚠ **It records `resume_at` — the seq of the first frame that comes AFTER the hole — and NOT the
/// wire's `from_seq`/`to_seq`.** Both of those are properties of DELIVERY and neither can be known
/// when the hole opens:
///
/// * `from_seq` is *"the wire seq of the last frame DELIVERED before the hole"*, and frames queued
///   ahead of the hole have not been delivered yet when it opens. Deriving it from the last seq
///   ENQUEUED — which is what this type did — is right only for a drop-NEWEST hole and is exactly
///   backwards for [`Mailbox::enforce_bounds`]'s drop-OLDEST eviction, where it put `from_seq`
///   ABOVE `to_seq` and an INVERTED range on the wire.
/// * `to_seq` is *"the wire seq of the first frame DELIVERED after it"*, which is the frame this gap
///   ends up riding — a frame that may not exist yet, and is never the DROPPED frame's own seq.
///
/// So both are computed in [`Mailbox::take`], from `resume_at` and from the per-key last-DELIVERED
/// seq, at the moment the gap is actually handed to the writer.
#[derive(Debug, Clone, Copy)]
struct Owed {
    dropped: u64,
    /// The lowest seq that is on the FAR side of this hole. A dequeued tape frame carries the gap
    /// exactly when its own seq is `>= resume_at`; anything below it was queued BEFORE the hole and
    /// must go out untouched.
    resume_at: u64,
}

#[derive(Default)]
struct Inner {
    /// LATEST-WINS, one entry per book key.
    book_slots: HashMap<MdKey, Arc<Vec<u8>>>,
    /// Which book keys are pending, FIFO, each at most once.
    order: VecDeque<MdKey>,
    /// The tape FIFO.
    tape: VecDeque<Outgoing>,
    /// The reserved control FIFO, drained FIRST.
    ctrl: VecDeque<Arc<Vec<u8>>>,
    /// Per-key owed gaps, taken by the writer when it dequeues that key's first POST-HOLE tape
    /// frame.
    owed: HashMap<MdKey, Owed>,
    /// The last tape `seq` this subscriber actually RECEIVED, per key — the wire's `from_seq`.
    /// ⚠ Deliberately last-DELIVERED and not last-ENQUEUED: a frame sitting in the queue has not
    /// been delivered, so an enqueued seq answers a question nobody asked and is wrong in both
    /// directions (too high for a drop-oldest eviction, and one frame early for a drop-newest one).
    last_delivered: HashMap<MdKey, u64>,
    bytes: usize,
    closed: bool,
    must_close: bool,
}

/// A per-connection bounded queue with the three-lane policy of §6.2.
pub struct Mailbox {
    inner: Mutex<Inner>,
    cv: Condvar,
    frame_cap: usize,
    byte_cap: usize,
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox")
            .field("frame_cap", &self.frame_cap)
            .field("byte_cap", &self.byte_cap)
            .finish_non_exhaustive()
    }
}

impl Inner {
    fn frames(&self) -> usize {
        self.book_slots.len() + self.tape.len() + self.ctrl.len()
    }

    /// Record a hole for `key`: MERGE with any existing owed range rather than replacing it, so a
    /// hub-side eviction and a later mailbox-side drop become one disclosure with the right total.
    ///
    /// `resume_at` is the seq of the first frame that is on the FAR side of this hole, and each of
    /// the three callers knows a different one — which is the whole reason it is a PARAMETER rather
    /// than something this function derives:
    ///
    /// * a HUB-side tape eviction happened before the frame `seq` was built, so the hole is
    ///   immediately before it: `resume_at = seq`;
    /// * a push-time DISCARD of the frame `seq` itself puts the hole at `seq`: `resume_at = seq + 1`;
    /// * an `enforce_bounds` eviction of the queued frame `seq` does the same: `resume_at = seq + 1`.
    ///
    /// # ⚠ Merging takes the MINIMUM, which is the counter-intuitive half
    ///
    /// One mailbox can owe ONE gap per key, so two holes at different positions have to collapse
    /// into one disclosure, and the only question is WHERE it is delivered. The maximum — resume
    /// after the LATER hole — is the arithmetically tidy answer and it is the wrong one: with a
    /// hub-side hole before seq 1 and a discard of seq 4 while 1..3 are queued, it delivers
    /// `Trades(1..3)` and only THEN the marker, so the client folds three batches believing them
    /// sound while seven prints are missing from in front of them. That is the corrupted-CVD failure
    /// `vike_datahub_client::market::MdFrame::TapeGap` exists to prevent, arriving one batch late.
    ///
    /// The minimum discloses at the EARLIEST frame any loss is known to precede, so no post-loss
    /// batch is ever folded before the marker. The price is stated on the wire type: `dropped` is
    /// the TOTAL while the range is the earliest window, so a later hole inside the same disclosure
    /// shows up as a §7.2 `seq` jump rather than a second marker — which is exactly what that
    /// backstop is for, and it is strictly better than a marker that is late.
    ///
    /// (The two mailbox-side paths never disagree with each other: `push` drops the NEWEST and
    /// `enforce_bounds` the OLDEST, and in both the surviving frames sit on one side of the hole.
    /// It is the HUB-side path — a hole in front of a frame that is then queued behind a backlog —
    /// that makes the two orders differ at all.)
    fn owe(&mut self, key: &MdKey, dropped: u64, resume_at: u64) {
        if dropped == 0 {
            return;
        }
        let e = self.owed.entry(key.clone()).or_insert(Owed { dropped: 0, resume_at });
        e.dropped = e.dropped.saturating_add(dropped);
        e.resume_at = e.resume_at.min(resume_at);
    }
}

impl Mailbox {
    /// A mailbox at the shipped bounds.
    pub fn new() -> Arc<Self> {
        Self::with_bounds(MD_MAILBOX_CAP, MD_MAILBOX_BYTES)
    }

    /// A mailbox at CHOSEN bounds — the seam the suite scales down so an overflow property is
    /// provable in a handful of frames instead of a thousand. Production has exactly one caller and
    /// it is [`Mailbox::new`].
    pub fn with_bounds(frame_cap: usize, byte_cap: usize) -> Arc<Self> {
        Arc::new(Mailbox {
            inner: Mutex::new(Inner::default()),
            cv: Condvar::new(),
            frame_cap,
            byte_cap,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // ⚠ Deliberately NOT `.expect("mailbox poisoned")` — see the module doc.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enqueue one frame under its lane's policy. **Never blocks and never fails.**
    pub fn push(&self, out: Outgoing) -> PushOutcome {
        let mut g = self.lock();
        if g.closed {
            return PushOutcome::Closed;
        }
        let len = out.bytes.len();
        let outcome = match out.lane {
            LaneClass::Ctrl => {
                if g.ctrl.len() >= MD_MAILBOX_CTRL {
                    // ⚠ The one place a mailbox ENDS a connection rather than losing something.
                    g.must_close = true;
                    drop(g);
                    self.cv.notify_one();
                    return PushOutcome::CloseConnection;
                }
                g.bytes += len;
                g.ctrl.push_back(out.bytes);
                PushOutcome::Enqueued
            }
            LaneClass::Book => {
                // SUPERSEDE IN PLACE. The key's queue POSITION is preserved (it stays where it was
                // in `order`), so a key that has been waiting does not lose its turn to one that
                // just arrived — FIFO fairness across keys, latest-wins within a key.
                match g.book_slots.insert(out.key.clone(), out.bytes) {
                    Some(prev) => {
                        g.bytes = g.bytes + len - prev.len();
                        PushOutcome::Superseded
                    }
                    None => {
                        g.bytes += len;
                        g.order.push_back(out.key.clone());
                        PushOutcome::Enqueued
                    }
                }
            }
            LaneClass::Tape => {
                // A hub-side eviction is disclosed even when THIS frame is then enqueued fine. The
                // prints it lost never became a frame of their own, so the hole is immediately
                // BEFORE this one and this frame is the first on its far side.
                g.owe(&out.key, out.hub_dropped, out.seq);
                if g.frames() >= self.frame_cap || g.bytes + len > self.byte_cap {
                    // No room: DISCARD this batch and owe it. Deliberately the NEWEST rather than
                    // the oldest — dropping the oldest would hand the client a hole in the MIDDLE of
                    // a range it is still reading.
                    //
                    // ⚠ **The byte arm of that test has no lower bound on an EMPTY queue, so a
                    // frame bigger than `byte_cap` is refused however empty this mailbox is.** At a
                    // full `MD_TAPE_CAP` drain that is the ordinary recovery batch, not an edge
                    // case: `crate::md::MD_MAILBOX_BYTES`' own doc carries the per-venue arithmetic,
                    // the stall it takes to reach, and the trade a future change would be making.
                    // The loss is DISCLOSED in full by the `owe` below, which is what keeps it a
                    // degradation rather than a correctness bug.
                    let ticks = out.ticks;
                    let seq = out.seq;
                    g.owe(&out.key, ticks, seq.saturating_add(1));
                    drop(g);
                    return PushOutcome::Dropped;
                }
                g.bytes += len;
                g.tape.push_back(out);
                PushOutcome::Enqueued
            }
        };
        // The byte bound applies across lanes: a mailbox full of DEEP books must give way on its own
        // TAPE, which is what "degrades the deep subscriber's OWN queue" means in practice. Books
        // are never evicted here — `MD_MAILBOX_BYTES`' compile-time assertion is what guarantees a
        // full session of ceiling-depth books fits, so an eviction loop over them could only ever
        // fire on a bound somebody had already broken.
        self.enforce_bounds(&mut g);
        drop(g);
        self.cv.notify_one();
        outcome
    }

    /// Evict from the TAPE lane until both bounds hold (or the tape is empty), owing every print.
    ///
    /// ⚠ This one evicts the OLDEST, which is the opposite end from [`Mailbox::push`]'s discard —
    /// so it is the caller of [`Inner::owe`] whose `resume_at` argument matters most. The frames
    /// still queued behind the victim are on the FAR side of the hole it opens, so the gap must ride
    /// the first of THEM (`victim.seq + 1`), never anything already delivered.
    fn enforce_bounds(&self, g: &mut Inner) {
        while (g.frames() > self.frame_cap || g.bytes > self.byte_cap) && !g.tape.is_empty() {
            let victim = g.tape.pop_front().expect("non-empty");
            g.bytes -= victim.bytes.len();
            g.owe(&victim.key, victim.ticks, victim.seq.saturating_add(1));
        }
    }

    /// Consumer side: CTRL first, then the book/tape lanes.
    ///
    /// ⚠ **CTRL drains first, unconditionally.** A `Status` frame's whole value is arriving
    /// promptly, and the failure it exists to prevent — a client rendering a frozen ladder it
    /// believes is live — is caused precisely by that frame waiting behind a book backlog.
    pub fn recv_timeout(&self, timeout: Duration) -> Recv {
        let mut g = self.lock();
        if let Some(r) = Self::take(&mut g) {
            return r;
        }
        if g.closed {
            return Recv::Closed;
        }
        let (mut g2, _) = self.cv.wait_timeout(g, timeout).unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = Self::take(&mut g2) {
            return r;
        }
        if g2.closed {
            return Recv::Closed;
        }
        Recv::Timeout
    }

    fn take(g: &mut Inner) -> Option<Recv> {
        if let Some(bytes) = g.ctrl.pop_front() {
            g.bytes -= bytes.len();
            return Some(Recv::Frame { bytes, owed_gap: None });
        }
        // Book and tape are drained in the order they were queued relative to each other only
        // loosely: books first while any key is pending, because a ladder that is one tick behind is
        // the visible symptom and a tape batch is not time-critical inside one tick.
        while let Some(key) = g.order.pop_front() {
            if let Some(bytes) = g.book_slots.remove(&key) {
                g.bytes -= bytes.len();
                return Some(Recv::Frame { bytes, owed_gap: None });
            }
        }
        if let Some(out) = g.tape.pop_front() {
            g.bytes -= out.bytes.len();
            // ⚠ **THE GAP RIDES THE FIRST POST-HOLE FRAME, NOT THE FIRST FRAME.** Taking it off the
            // front of the FIFO unconditionally — which is what this did — writes the marker AHEAD
            // of batches that arrived BEFORE the hole, which is precisely the mis-ordering the
            // module doc rejects the CTRL lane for: a client resets its aggregator and then
            // re-ingests older queued prints, and the §7.2 contiguity check sees a gap claiming
            // "last delivered = 3" immediately followed by seq 1.
            let from_seq = g.last_delivered.get(&out.key).copied().unwrap_or(0);
            let post_hole = g.owed.get(&out.key).is_some_and(|o| out.seq >= o.resume_at);
            let owed_gap = if post_hole {
                g.owed.remove(&out.key).map(|o| (out.key.clone(), o.dropped, from_seq, out.seq))
            } else {
                None
            };
            debug_assert!(
                owed_gap.as_ref().is_none_or(|(_, _, f, t)| f < t),
                "a disclosed gap must span forwards: {from_seq}..{}",
                out.seq
            );
            g.last_delivered.insert(out.key.clone(), out.seq);
            return Some(Recv::Frame { bytes: out.bytes, owed_gap });
        }
        None
    }

    /// Drop this key's PENDING book slot, if it has one. `true` when something was dropped.
    ///
    /// ⚠ **The BOOK lane is the one lane nothing else can reclaim.** [`Mailbox::enforce_bounds`]
    /// evicts from the tape only, and deliberately: [`crate::md::MD_MAILBOX_BYTES`]' compile-time
    /// assertion is what guarantees a full session of ceiling-depth books fits, so an eviction loop
    /// over them could only ever fire on a bound somebody had already broken. That guarantee rests
    /// on the invariant `book_slots.len() <= MD_MAX_SPECS_PER_SESSION`, which nothing enforced
    /// across a SUBSCRIPTION CHANGE: `crate::md::hub::MdHub::update`'s remove path dropped the key
    /// from the session and left its queued frame here, so `remove 64 + add 64` against a writer one
    /// tick behind held 128 slots — 1.86× the byte bound, unevictable.
    ///
    /// ⚠ **The `order` entry goes with the slot, and the queue POSITION is NOT preserved** — a key
    /// removed and re-added is pushed to the BACK by [`Mailbox::push`]'s `None` arm. That is the
    /// opposite of what the supersede path guarantees a key (*"it stays where it was in `order`"*),
    /// and it is right here for the same reason supersede is right there: this function is reached
    /// only for a key the session has just UNSUBSCRIBED, so there is no turn left to lose. Retaining
    /// the entry would not preserve a position either — the re-add pushes a new one regardless, so
    /// it would leave a DUPLICATE. [`Mailbox::take`] tolerates an `order` entry with no slot, so
    /// leaving one is not a correctness bug; it would simply let a churning session grow that deque
    /// without bound.
    ///
    /// It touches NEITHER the tape nor `owed`: a tape frame is evictable and therefore cannot
    /// falsify the assertion, and an owed gap is a §7.3 disclosure that must outlive the key it
    /// describes.
    ///
    /// ⚠ **On its own it reclaims a slot and closes nothing**, because something can enqueue a new
    /// one for the same key a moment later, and there are two somethings.
    /// `crate::md::hub::MdHub::fanout` answers the publisher: it re-reads the session table under
    /// that table's own lock at push time rather than trusting a per-tick snapshot, so a book for a
    /// key this session no longer holds is never enqueued at all. The ATTACH push cannot do that —
    /// it serializes, and serializing under the session table is the one thing §6.1 forbids — so
    /// `crate::md::hub::MdHub::update` calls this function again in a sweep after its pushes. The
    /// three together are what make `book_slots.len() <= MD_MAX_SPECS_PER_SESSION` an invariant
    /// rather than a hope; `update`'s own comments carry the interleavings and the one door that
    /// still has neither.
    pub fn drop_book(&self, key: &MdKey) -> bool {
        let mut g = self.lock();
        let Some(bytes) = g.book_slots.remove(key) else { return false };
        g.bytes -= bytes.len();
        g.order.retain(|k| k != key);
        true
    }

    /// Whether the CTRL lane overflowed and this connection must be closed.
    pub fn must_close(&self) -> bool {
        self.lock().must_close
    }

    /// Close: no more frames accepted, a waiting consumer wakes to [`Recv::Closed`].
    pub fn close(&self) {
        let mut g = self.lock();
        g.closed = true;
        drop(g);
        self.cv.notify_all();
    }

    /// Frames currently queued across all three lanes — the suite's non-vacuity floor (a mailbox
    /// that enqueued NOTHING satisfies every "holds at most N" assertion vacuously).
    pub fn len(&self) -> usize {
        self.lock().frames()
    }

    /// Whether the mailbox holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes currently queued.
    pub fn bytes(&self) -> usize {
        self.lock().bytes
    }
}

#[path = "mailbox_tests.rs"]
#[cfg(test)]
mod mailbox_tests;
