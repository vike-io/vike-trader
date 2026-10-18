//! The delta-apply law: the sequence-contiguity policies and the decision they drive. See
//! [`L2Book::delta_decision`].

use super::L2Book;

/// The contiguity a consumer expects of an incoming delta's sequence number. Pick the policy that
/// matches the stream: [`SeqPolicy::Strict`] when every event increments the sequence by exactly 1,
/// [`SeqPolicy::Monotonic`] when it only ever increases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqPolicy {
    /// The sequence increments by exactly 1 per applied event (for example Bybit's `u`, or a
    /// recorded [`BookUpdate`](crate::BookUpdate) chain). Anything but `last_seq + 1` (apply) or
    /// `last_seq` (stale duplicate) is a gap.
    Strict,
    /// The sequence only needs to increase (for example Binance-style `u` spans): jumps are
    /// normal and never reported as a gap.
    Monotonic,
}

/// What [`L2Book::delta_decision`] says to do with an incoming delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaDecision {
    /// In sequence: fold it ([`L2Book::apply_delta`] will accept it).
    Apply,
    /// Already reflected (duplicate or replayed): drop it, the book stays trustworthy.
    Stale,
    /// Frames were dropped (forward jump) or the venue restarted its counter (regression), so the
    /// book can no longer be trusted and the consumer must resync from a fresh snapshot. Only the
    /// [`SeqPolicy::Strict`] policy can return this.
    Gap,
}

impl L2Book {
    // Engineering note: `seq == 0` always applying under `Monotonic` is [`L2Book::apply_delta`]'s
    // long-standing convention.
    /// The delta-apply decision: given an incoming delta's `seq` and the stream's contiguity
    /// policy, say whether it folds, is stale, or means dropped frames. It only reads the book; it
    /// never changes it.
    ///
    /// `seq == 0` is the "venue supplied no sequence" sentinel: it always applies under
    /// [`SeqPolicy::Monotonic`]; under [`SeqPolicy::Strict`] it is judged as a plain number (a
    /// strict stream that stops carrying its sequence has, by definition, lost its integrity
    /// chain).
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
