//! The ring position the client has DELIVERED to the core, and the duplicate guard that rides on it.
//!
//! The gatewayhub keeps a per-account ring of events (`seq` rises by one inside a `gw_epoch`); the
//! tradehub journals each event with the cursor it arrived at (`vike_exec::Ingest::GatewayEvent`).
//! This is the in-memory twin of that cursor, used for two things:
//!
//! * **resume on a link flap**: the cursor goes into the next `TradeOpen` so the daemon replays what the
//!   client missed (a process restart resumes from the journal's cursor instead, handed to the builder);
//! * **the duplicate guard**: inside one epoch an event whose `seq` is not above the last delivered one
//!   is a duplicate (a replay overlapping a live frame, a straggler of the old connection) and is not
//!   delivered twice. Delivering twice is not a safe default: `apply_funding` is not idempotent.
//!
//! The advance rule is `vike_exec::GatewayCursor::after`, the ONE rule the core's fold, the boot restore
//! and the `Snap` use, so a cursor means the same thing in each.
//!
//! # The anchor: a client that has heard nothing yet still has a position
//!
//! A client that has delivered no event holds no cursor, and a redial with no cursor opens FRESH: the
//! daemon replays nothing, so the events the account produced between the first open and the flap (the
//! first order's `OrderSubmitted`/`OrderAccepted` of a quiet account) were lost for good and no reconcile was
//! forced (W19-D3). The lease's own `TradeOpened` says where the client stood: `event_ring_floor == Some(0)` is an
//! EMPTY ring (a daemon that does not send the field is `None`: unknown, never an anchor, W19g), so every event the epoch holds later is one this client has not seen, and "the ring start of
//! this epoch" (`seq` 0) is its exact position. [`CursorBook::opened`] records that anchor and
//! [`CursorBook::resume`] sends it until a delivered event supersedes it; the daemon's `plan_resume` serves
//! `seq` 0 as "from the first entry" (or `ReplayUnavailable` when the ring has already evicted it). A ring that
//! was NOT empty at the first open has no position the client can name (the events before it are not its to
//! replay), so a redial in that epoch before any delivery cannot be replayed and is a gap: the caller reconciles
//! ([`CursorBook::opened`] returns `true`).

use vike_exec::GatewayCursor;

/// What the guard says about an incoming event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// A new position: deliver it.
    Deliver,
    /// At or behind the last delivered position of the same epoch: drop it.
    Duplicate,
}

/// Where the client stood when it opened a lease on an epoch, for a client that has delivered nothing there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Anchor {
    /// No lease was ever opened.
    #[default]
    None,
    /// The ring was EMPTY at the open: `seq` 0 of this epoch is the client's exact position.
    At(GatewayCursor),
    /// The ring held events at the open (the epoch it names): the client has no position it can name.
    Blind(u64),
}

/// The last cursor delivered to the core, or the one the client started from.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CursorBook {
    last: Option<GatewayCursor>,
    anchor: Anchor,
}

impl CursorBook {
    /// A book starting from `initial` (the journal's cursor after a restart, `None` for a fresh run).
    pub fn new(initial: Option<GatewayCursor>) -> Self {
        Self { last: initial, anchor: Anchor::None }
    }

    /// The last position DELIVERED to the core (or the one the book started from): never the anchor.
    pub fn last(&self) -> Option<GatewayCursor> {
        self.last
    }

    /// The position a redial resumes from: the delivered cursor, unless the client has delivered nothing in
    /// the anchored epoch, when it is the anchor (the ring start of that epoch). See the module doc.
    pub fn resume(&self) -> Option<GatewayCursor> {
        match (self.last, self.anchor) {
            (None, Anchor::At(anchor)) => Some(anchor),
            (Some(held), Anchor::At(anchor)) if held.gw_epoch != anchor.gw_epoch => Some(anchor),
            (last, _) => last,
        }
    }

    /// A lease was opened on epoch `gw_epoch`, whose ring was empty when `ring_empty` (the daemon SAID so:
    /// `TradeOpened::event_ring_floor == Some(0)`; a daemon that omits the field is not empty, it is unknown,
    /// and an unknown ring is a gap like a full one). Records the anchor
    /// for an epoch the book has not seen. Returns whether events may have been missed that no cursor can
    /// name (a redial inside an epoch whose ring was not empty at the first open, with nothing delivered in
    /// it): the caller reconciles.
    pub fn opened(&mut self, gw_epoch: u64, ring_empty: bool) -> bool {
        if self.last.is_some_and(|held| held.gw_epoch == gw_epoch) {
            return false; // a delivered cursor names the position
        }
        match self.anchor {
            Anchor::At(anchor) if anchor.gw_epoch == gw_epoch => false,
            Anchor::Blind(epoch) if epoch == gw_epoch => true,
            _ => {
                self.anchor = if ring_empty {
                    Anchor::At(GatewayCursor { gw_epoch, seq: 0, ts_ms: 0 })
                } else {
                    Anchor::Blind(gw_epoch)
                };
                false
            }
        }
    }

    /// Whether `incoming` is new. A different epoch is always new: the ring began again, and an epoch
    /// is random (there is no "older" one).
    pub fn admit(&self, incoming: GatewayCursor) -> Admit {
        match self.resume() {
            Some(held) if held.gw_epoch == incoming.gw_epoch && incoming.seq <= held.seq => {
                Admit::Duplicate
            }
            _ => Admit::Deliver,
        }
    }

    /// Whether `incoming` leaves a hole: the same epoch and a `seq` above the next one. The ring is
    /// contiguous, so a hole is an event the daemon did not (or could not) send; the legitimate jumps
    /// (a resume the ring cannot serve, a slow lease that fell behind) are announced by
    /// `ReplayUnavailable` first, which the caller accounts for.
    pub fn skipped(&self, incoming: GatewayCursor) -> bool {
        self.resume().is_some_and(|held| {
            held.gw_epoch == incoming.gw_epoch && incoming.seq > held.seq.saturating_add(1)
        })
    }

    /// `delivered` reached the core: fold it into the held position by the one rule.
    pub fn delivered(&mut self, delivered: GatewayCursor) {
        self.last = Some(match self.last {
            Some(held) => held.after(delivered),
            None => delivered,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(gw_epoch: u64, seq: u64) -> GatewayCursor {
        GatewayCursor { gw_epoch, seq, ts_ms: i64::try_from(seq).unwrap_or(0) * 10 }
    }

    #[test]
    fn a_fresh_book_admits_anything_and_resumes_from_nothing() {
        let book = CursorBook::new(None);
        assert_eq!(book.last(), None);
        assert_eq!(book.admit(at(1, 1)), Admit::Deliver);
    }

    #[test]
    fn inside_an_epoch_only_a_higher_seq_is_new() {
        let mut book = CursorBook::new(Some(at(7, 5)));
        assert_eq!(book.admit(at(7, 5)), Admit::Duplicate, "the very same event again");
        assert_eq!(book.admit(at(7, 4)), Admit::Duplicate, "a straggler behind the cursor");
        assert_eq!(book.admit(at(7, 6)), Admit::Deliver);
        book.delivered(at(7, 6));
        assert_eq!(book.last(), Some(at(7, 6)));
        book.delivered(at(7, 4));
        assert_eq!(book.last(), Some(at(7, 6)), "a straggler never rewinds the resume point");
    }

    #[test]
    fn a_hole_is_a_jump_above_the_next_seq_inside_one_epoch() {
        let book = CursorBook::new(Some(at(7, 5)));
        assert!(!book.skipped(at(7, 6)), "the next one is contiguous");
        assert!(!book.skipped(at(7, 5)), "a duplicate is not a hole");
        assert!(book.skipped(at(7, 7)), "seq 6 never arrived");
        assert!(!book.skipped(at(8, 100)), "another epoch begins its own count");
        assert!(!CursorBook::new(None).skipped(at(7, 100)), "the first event sets the baseline");
    }

    #[test]
    fn a_new_epoch_is_new_whatever_its_seq_and_becomes_the_resume_point() {
        let mut book = CursorBook::new(Some(at(7, 500)));
        assert_eq!(book.admit(at(8, 1)), Admit::Deliver, "the ring began again");
        book.delivered(at(8, 1));
        assert_eq!(book.last(), Some(at(8, 1)));
    }

    #[test]
    fn a_client_that_heard_nothing_resumes_from_the_ring_start_of_the_epoch_it_opened() {
        let mut book = CursorBook::new(None);
        assert!(!book.opened(7, true), "a first open of an empty ring misses nothing");
        assert_eq!(book.resume(), Some(at(7, 0)), "the ring start, not nothing (W19-D3)");
        assert_eq!(book.last(), None, "but nothing was DELIVERED");
        assert!(!book.opened(7, false), "a redial of the same epoch keeps the first anchor");
        assert_eq!(book.resume(), Some(at(7, 0)));
        assert_eq!(book.admit(at(7, 1)), Admit::Deliver);
        assert!(
            !book.skipped(at(7, 1)),
            "the first entry of the ring is contiguous with the anchor"
        );
        assert!(book.skipped(at(7, 3)), "entries 1 and 2 never arrived");
        book.delivered(at(7, 1));
        assert_eq!(book.resume(), Some(at(7, 1)), "a delivered event supersedes the anchor");
    }

    #[test]
    fn a_new_epoch_moves_the_anchor_and_an_old_cursor_never_outranks_it() {
        let mut book = CursorBook::new(Some(at(7, 40)));
        assert!(!book.opened(7, false), "a delivered cursor of the epoch names the position");
        assert_eq!(book.resume(), Some(at(7, 40)));
        assert!(
            !book.opened(8, true),
            "the daemon restarted: its answer is ReplayUnavailable, not ours"
        );
        assert_eq!(
            book.resume(),
            Some(at(8, 0)),
            "a flap before the new epoch's first event resumes there"
        );
        assert_eq!(book.last(), Some(at(7, 40)));
        assert_eq!(book.admit(at(8, 1)), Admit::Deliver);
        book.delivered(at(8, 1));
        assert_eq!(book.resume(), Some(at(8, 1)));
    }

    #[test]
    fn a_ring_that_was_not_empty_at_the_first_open_cannot_be_replayed_and_asks_for_a_reconcile() {
        let mut book = CursorBook::new(None);
        assert!(
            !book.opened(7, false),
            "the first open owes nothing: the boot reconcile covers it"
        );
        assert_eq!(book.resume(), None, "no position the client can name");
        assert!(
            book.opened(7, false),
            "a redial before any delivery cannot be replayed: reconcile"
        );
        book.delivered(at(7, 12));
        assert!(!book.opened(7, false), "once an event is delivered the cursor names the position");
    }
}
