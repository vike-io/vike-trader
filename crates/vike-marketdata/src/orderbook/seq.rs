//! The one delta-apply law: the seq contiguity policies and the decision they drive.

use super::L2Book;

/// The contiguity a consumer expects of an incoming delta's seq — see the module doc's
/// "one delta-apply law" section for which stream uses which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqPolicy {
    /// seq increments by exactly 1 per applied event (bybit `u`; the recorded `BookUpdate`
    /// chain). Anything but `last_seq + 1` / `last_seq` is a gap.
    Strict,
    /// seq only needs to increase (binance-grammar `u` spans) — jumps are normal, never a gap.
    Monotonic,
}

/// What [`L2Book::delta_decision`] says to do with an incoming delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaDecision {
    /// In sequence — fold it ([`L2Book::apply_delta`] will accept it).
    Apply,
    /// Already reflected (duplicate/replayed) — drop it, the book stays trustworthy.
    Stale,
    /// Frames were dropped (forward jump) or the venue restarted its counter (regression) —
    /// the book can no longer be trusted; the consumer must resync from a fresh snapshot.
    /// Only the [`SeqPolicy::Strict`] policy can return this.
    Gap,
}

impl L2Book {
    /// The one delta-apply decision (see the module doc): given an incoming delta's `seq` and
    /// the stream's contiguity policy, say whether it folds, is stale, or means dropped frames.
    /// `seq == 0` is the "venue supplied no seq" sentinel: it always applies under
    /// [`SeqPolicy::Monotonic`] (matching [`L2Book::apply_delta`]'s long-standing convention);
    /// under [`SeqPolicy::Strict`] it is judged as a plain number (a strict stream that stops
    /// carrying its seq has, by definition, lost its integrity chain).
    pub fn delta_decision(&self, seq: u64, policy: SeqPolicy) -> DeltaDecision {
        match policy {
            SeqPolicy::Monotonic => {
                if seq != 0 && seq <= self.last_seq {
                    DeltaDecision::Stale
                } else {
                    DeltaDecision::Apply
                }
            }
            SeqPolicy::Strict => {
                // wrapping_add: last_seq == u64::MAX must not panic in debug builds. At that
                // edge the wrap target is 0, so an incoming seq == 0 would actually read as
                // Apply here (folding via this sentinel and resetting last_seq to 0), not
                // Gap/Stale — a real divergence from the "no true next seq" intent. Accepted:
                // no real venue seq counter reaches u64::MAX, so the case is unreachable in
                // practice; wrapping_add exists purely to keep debug builds panic-free.
                if seq == self.last_seq.wrapping_add(1) {
                    DeltaDecision::Apply
                } else if seq == self.last_seq {
                    DeltaDecision::Stale
                } else {
                    DeltaDecision::Gap
                }
            }
        }
    }
}

#[path = "seq_tests.rs"]
#[cfg(test)]
mod seq_tests;
