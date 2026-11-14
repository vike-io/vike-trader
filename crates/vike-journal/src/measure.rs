//! What the `runtime_latency` harness reads of
//! the asynchronous journal it cannot see from outside a core it does not own — how long the order
//! path waited on the write thread (`WriteFence::wait`), and the write thread's longest hand-off.
//! Process-wide.
//!
//! # Why it stays (the owner asked: remove or argue)
//!
//! It is the instrument for the claim the design rests on: an order never waits behind a sync, and
//! the write thread is never held up by one. A core-hop percentile cannot show either (the fence waits
//! inside the hop, the write thread is another thread). What it costs, and where:
//! * the CORE thread: nothing on its fast path. It writes here only when a fence actually WAITED
//!   (one `Instant` pair and three relaxed atomics), never per message and never per order that finds
//!   the write thread caught up;
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

/// What the order path and the write thread did since the last [`reset`].
#[derive(Clone, Debug)]
pub struct Stats {
    pub fence_waited: u64,
    pub fence_max_ns: u64,
    /// `(upper bound in ns, count)` per non-empty log2 bucket of the waits.
    pub fence_buckets: Vec<(u64, u64)>,
    pub write_max_ns: u64,
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
    }
}

pub fn reset() {
    for b in &FENCE_BUCKETS {
        b.store(0, Ordering::Relaxed);
    }
    for c in [&FENCE_WAITED, &FENCE_MAX_NS, &WRITE_MAX_NS] {
        c.store(0, Ordering::Relaxed);
    }
}
