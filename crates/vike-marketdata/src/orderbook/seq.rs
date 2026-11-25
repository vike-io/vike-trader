//! The delta-apply law: the sequence-contiguity policies and the decision they drive. See
//! [`L2Book::delta_decision`]. And the one fold of a RECORDED [`L2BookUpdate`] chain into a book slot
//! that rule implies ([`apply_book_event`]).

use super::{L2Book, L2BookUpdate, L2BookUpdateKind};

/// The contiguity a consumer expects of an incoming delta's sequence number. Pick the policy that
/// matches the stream: [`L2BookSequencePolicy::Strict`] when every event increments the sequence by exactly 1,
/// [`L2BookSequencePolicy::Monotonic`] when it only ever increases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2BookSequencePolicy {
    /// The sequence increments by exactly 1 per applied event (for example Bybit's `u`, or a
    /// recorded [`L2BookUpdate`](crate::L2BookUpdate) chain). Anything but `last_sequence + 1` (apply) or
    /// `last_sequence` (stale duplicate) is a gap.
    Strict,
    /// The sequence only needs to increase (for example Binance-style `u` spans): jumps are
    /// normal and never reported as a gap.
    Monotonic,
}

/// What [`L2Book::delta_decision`] says to do with an incoming delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2BookDeltaDecision {
    /// In sequence: fold it ([`L2Book::apply_delta`] will accept it).
    Apply,
    /// Already reflected (duplicate or replayed): drop it, the book stays trustworthy.
    Stale,
    /// Frames were dropped (forward jump) or the venue restarted its counter (regression), so the
    /// book can no longer be trusted and the consumer must resync from a fresh snapshot. Only the
    /// [`L2BookSequencePolicy::Strict`] policy can return this.
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
    /// [`L2BookSequencePolicy::Monotonic`]; under [`L2BookSequencePolicy::Strict`] it is judged as a plain number (a
    /// strict stream that stops carrying its sequence has, by definition, lost its integrity
    /// chain).
    pub fn delta_decision(&self, seq: u64, policy: L2BookSequencePolicy) -> L2BookDeltaDecision {
        match policy {
            L2BookSequencePolicy::Monotonic => {
                if seq != 0 && seq <= self.last_sequence {
                    L2BookDeltaDecision::Stale
                } else {
                    L2BookDeltaDecision::Apply
                }
            }
            L2BookSequencePolicy::Strict => {
                // wrapping_add: last_sequence == u64::MAX must not panic in debug builds. At that
                // edge the wrap target is 0, so an incoming seq == 0 would actually read as
                // Apply here (folding via this sentinel and resetting last_sequence to 0), not
                // Gap/Stale — a real divergence from the "no true next seq" intent. Accepted:
                // no real venue seq counter reaches u64::MAX, so the case is unreachable in
                // practice; wrapping_add exists purely to keep debug builds panic-free.
                if seq == self.last_sequence.wrapping_add(1) {
                    L2BookDeltaDecision::Apply
                } else if seq == self.last_sequence {
                    L2BookDeltaDecision::Stale
                } else {
                    L2BookDeltaDecision::Gap
                }
            }
        }
    }
}

/// **Fold one RECORDED book event into a replay slot**: the recorded chain's integrity rule (the
/// gap-sentinel, replayed), shared by the simulator's replay and every research reader of a recorded
/// chain, so the two cannot trust different books. Returns whether a state-changing apply happened
/// (a snapshot, or an in-sequence delta): the simulator's `on_order_book` delivery gate.
///
/// The recorded chain is feed-local contiguous, so a delta applies ONLY when
/// [`L2Book::delta_decision`] says [`L2BookDeltaDecision::Apply`] under [`L2BookSequencePolicy::Strict`]
/// (`seq == last_sequence + 1`, the same decision bybit's live depth fold consults); anything else (a
/// gap, an orphan delta with no book, a `GapStart` or `Stale` marker) empties the slot until the
/// next snapshot re-anchors it: what the live consumer knew at the time. A `LiveResume` changes
/// nothing.
///
/// A snapshot builds its book on `tick_size` (the simulator passes the event's own; a reader that
/// folds on a finer lossless grid passes that) and lands in the slot through `S::from`; a delta
/// reaches the slot's book through `book_mut` (`std::rc::Rc::make_mut` for a shared slot, the
/// identity for an owned one).
pub fn apply_book_event<S: From<L2Book>>(
    slot: &mut Option<S>,
    b: &L2BookUpdate,
    tick_size: f64,
    book_mut: impl FnOnce(&mut S) -> &mut L2Book,
) -> bool {
    match b.kind {
        L2BookUpdateKind::Snapshot => {
            let mut book = L2Book::new(tick_size);
            book.apply_snapshot(b.sequence, &b.bids, &b.asks);
            *slot = Some(S::from(book));
            true
        }
        L2BookUpdateKind::Delta => match slot.as_mut().map(book_mut) {
            Some(book)
                if book.delta_decision(b.sequence, L2BookSequencePolicy::Strict)
                    == L2BookDeltaDecision::Apply =>
            {
                book.apply_delta(b.sequence, &b.bids, &b.asks);
                true
            }
            _ => {
                *slot = None; // discontinuity — untrustworthy until the next anchor
                false
            }
        },
        L2BookUpdateKind::GapStart | L2BookUpdateKind::Stale => {
            *slot = None;
            false
        }
        L2BookUpdateKind::LiveResume => false,
    }
}

#[path = "sequence_tests.rs"]
#[cfg(test)]
mod sequence_tests;
