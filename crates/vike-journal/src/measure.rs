//! What the `runtime_latency` harness reads of
//! the asynchronous journal it cannot see from outside a core it does not own — how long the order
//! path waited on the write thread (`WriteFence::wait`), the write thread's longest hand-off, and what a
//! cadence `Snapshot` cost each side since its encode left the core (decision 0279, amended 2026-11-19): the
//! write thread's encode, and the core's settle (the copy into its stage, and a wait when the next record came
//! while the encode ran). Process-wide.
//!
//! # Why it stays (the owner asked: remove or argue)
//!
//! It is the instrument for the claim the design rests on: an order never waits behind a sync, and
//! the write thread is never held up by one. A core-hop percentile cannot show either (the fence waits
//! inside the hop, the write thread is another thread). What it costs, and where:
//! * the CORE thread: nothing on its fast path. It writes here only when a fence actually WAITED
//!   (one `Instant` pair and three relaxed atomics), never per message and never per order that finds
//!   the write thread caught up, and once per cadence `Snapshot` (its step and its settle: an `Instant`
//!   pair and a few relaxed atomics each, the owner's A/B of 2026-11-19);
//! * the WRITE thread: one `Instant` pair and one relaxed `fetch_max` per hand-off;
//! * nothing else reads or writes it.
//!
//! `#[doc(hidden)]`, and not a public contract: a merge would either keep it as this crate's
//! measurement seam, behind a feature the latency harness turns on, or drop it with the harness test
//! that reads it (`journal_fence_stats`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Log2 buckets of the order path's waits, in ns: bucket `k` holds waits in `[2^k, 2^(k+1))`.
static FENCE_BUCKETS: [AtomicU64; 40] = [const { AtomicU64::new(0) }; 40];
/// Calls of `WriteFence::wait` that found the write thread behind and waited.
static FENCE_WAITED: AtomicU64 = AtomicU64::new(0);
static FENCE_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// The write thread's longest single hand-off (a `pwrite` span, or a header stamp).
static WRITE_MAX_NS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn note_fence_wait(waited: Duration) {
    let ns = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX).max(1);
    FENCE_WAITED.fetch_add(1, Ordering::Relaxed);
    FENCE_MAX_NS.fetch_max(ns, Ordering::Relaxed);
    let k = (63 - ns.leading_zeros()) as usize;
    FENCE_BUCKETS[k.min(FENCE_BUCKETS.len() - 1)].fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn note_write(took: Duration) {
    WRITE_MAX_NS.fetch_max(u64::try_from(took.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// Encodes the write thread ran, and their total and longest time.
static SNAPSHOT_ENCODES: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_ENCODE_NS: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_ENCODE_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// Settles on the core thread, how many of them waited for a running encode, and their total and longest time.
static SNAPSHOT_SETTLES: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_SETTLE_WAITED: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_SETTLE_NS: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_SETTLE_MAX_NS: AtomicU64 = AtomicU64::new(0);

/// The core's whole cadence-`Snapshot` step (`vike_core`'s `CoreThread::write_snap`: the engines' clones, the books
/// and the journal's part), and its total and longest time.
static SNAPSHOT_STEPS: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_STEP_NS: AtomicU64 = AtomicU64::new(0);
static SNAPSHOT_STEP_MAX_NS: AtomicU64 = AtomicU64::new(0);

/// Times the core's cadence-`Snapshot` step from here to its drop: one `Instant` pair per `Snapshot`, never per
/// message, read by nothing the fold decides (the fence's wait is timed the same way, `WriteFence::wait`).
pub struct SnapshotStep(std::time::Instant);

/// Start timing one cadence-`Snapshot` step ([`SnapshotStep`]).
pub fn snapshot_step() -> SnapshotStep {
    SnapshotStep(std::time::Instant::now())
}

impl Drop for SnapshotStep {
    fn drop(&mut self) {
        let ns = u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        SNAPSHOT_STEPS.fetch_add(1, Ordering::Relaxed);
        SNAPSHOT_STEP_NS.fetch_add(ns, Ordering::Relaxed);
        SNAPSHOT_STEP_MAX_NS.fetch_max(ns, Ordering::Relaxed);
    }
}

/// Once per cadence `Snapshot`, on the write thread.
pub(crate) fn note_snapshot_encode(took: Duration) {
    let ns = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
    SNAPSHOT_ENCODES.fetch_add(1, Ordering::Relaxed);
    SNAPSHOT_ENCODE_NS.fetch_add(ns, Ordering::Relaxed);
    SNAPSHOT_ENCODE_MAX_NS.fetch_max(ns, Ordering::Relaxed);
}

/// Once per cadence `Snapshot`, on the core thread (never per message).
pub(crate) fn note_snapshot_settle(took: Duration, waited: bool) {
    let ns = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
    SNAPSHOT_SETTLES.fetch_add(1, Ordering::Relaxed);
    SNAPSHOT_SETTLE_WAITED.fetch_add(u64::from(waited), Ordering::Relaxed);
    SNAPSHOT_SETTLE_NS.fetch_add(ns, Ordering::Relaxed);
    SNAPSHOT_SETTLE_MAX_NS.fetch_max(ns, Ordering::Relaxed);
}

/// What the order path and the write thread did since the last [`reset`].
#[derive(Clone, Debug)]
pub struct Stats {
    pub fence_waited: u64,
    pub fence_max_ns: u64,
    /// `(upper bound in ns, count)` per non-empty log2 bucket of the waits.
    pub fence_buckets: Vec<(u64, u64)>,
    pub write_max_ns: u64,
    /// The core's cadence-`Snapshot` steps ([`SnapshotStep`]), and their total and longest time on the core thread.
    pub snapshot_steps: u64,
    pub snapshot_step_ns: u64,
    pub snapshot_step_max_ns: u64,
    /// Cadence `Snapshot`s the write thread encoded, and their total and longest encode.
    pub snapshot_encodes: u64,
    pub snapshot_encode_ns: u64,
    pub snapshot_encode_max_ns: u64,
    /// Cadence `Snapshot`s the core settled (the write thread's encode, or its own when it got there first), how
    /// many of them waited for a running encode, and their total and longest settle on the core thread.
    pub snapshot_settles: u64,
    pub snapshot_settle_waited: u64,
    pub snapshot_settle_ns: u64,
    pub snapshot_settle_max_ns: u64,
}

pub fn stats() -> Stats {
    let fence_buckets = FENCE_BUCKETS
        .iter()
        .enumerate()
        .filter_map(|(k, b)| {
            let n = b.load(Ordering::Relaxed);
            (n > 0).then_some((1u64 << (k + 1), n))
        })
        .collect();
    Stats {
        fence_waited: FENCE_WAITED.load(Ordering::Relaxed),
        fence_max_ns: FENCE_MAX_NS.load(Ordering::Relaxed),
        fence_buckets,
        write_max_ns: WRITE_MAX_NS.load(Ordering::Relaxed),
        snapshot_steps: SNAPSHOT_STEPS.load(Ordering::Relaxed),
        snapshot_step_ns: SNAPSHOT_STEP_NS.load(Ordering::Relaxed),
        snapshot_step_max_ns: SNAPSHOT_STEP_MAX_NS.load(Ordering::Relaxed),
        snapshot_encodes: SNAPSHOT_ENCODES.load(Ordering::Relaxed),
        snapshot_encode_ns: SNAPSHOT_ENCODE_NS.load(Ordering::Relaxed),
        snapshot_encode_max_ns: SNAPSHOT_ENCODE_MAX_NS.load(Ordering::Relaxed),
        snapshot_settles: SNAPSHOT_SETTLES.load(Ordering::Relaxed),
        snapshot_settle_waited: SNAPSHOT_SETTLE_WAITED.load(Ordering::Relaxed),
        snapshot_settle_ns: SNAPSHOT_SETTLE_NS.load(Ordering::Relaxed),
        snapshot_settle_max_ns: SNAPSHOT_SETTLE_MAX_NS.load(Ordering::Relaxed),
    }
}

pub fn reset() {
    for b in &FENCE_BUCKETS {
        b.store(0, Ordering::Relaxed);
    }
    for c in [
        &FENCE_WAITED,
        &FENCE_MAX_NS,
        &WRITE_MAX_NS,
        &SNAPSHOT_STEPS,
        &SNAPSHOT_STEP_NS,
        &SNAPSHOT_STEP_MAX_NS,
        &SNAPSHOT_ENCODES,
        &SNAPSHOT_ENCODE_NS,
        &SNAPSHOT_ENCODE_MAX_NS,
        &SNAPSHOT_SETTLES,
        &SNAPSHOT_SETTLE_WAITED,
        &SNAPSHOT_SETTLE_NS,
        &SNAPSHOT_SETTLE_MAX_NS,
    ] {
        c.store(0, Ordering::Relaxed);
    }
}
