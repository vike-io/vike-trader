//! [`EventRingCursor`], where an event stood in a gateway's per-account event ring: the ONE type for it.
//!
//! The gatewayhub's lease wire carries it (`TradeOpen.resume`), the tradehub journals it with each venue
//! event (`vike_execution::GatewayEvent::cursor`, the `Snapshot`'s `gateway_cursors`), and the client's duplicate
//! guard holds it. One type on all three, so the wire and the journal cannot drift apart: its serde shape
//! is the plain derive, `{"gateway_epoch":…,"seq":…,"timestamp_ms":…}`, and a field renamed here is a wire AND a
//! journal change at once (the goldens in `fixtures/gatewayhub_wire/` and the journal's compat tests pin it).

/// Where an event stood in a gateway's per-account event ring: the resume point a restarted
/// consumer presents so the gateway can replay what it missed.
///
/// `gateway_epoch` is a RANDOM id the gateway mints per session (it is not ordered), `seq` is the
/// per-account, per-epoch sequence number the gateway assigned when it emitted the event, and
/// `timestamp_ms` the event's own timestamp, epoch ms (the floor a gateway restart's history replay would
/// start from). A cursor is only comparable with another of the SAME epoch; [`Self::after`] is the
/// one rule that folds a newer observation into a held one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EventRingCursor {
    /// The gatewayhub SESSION the ring belongs to (random per session, not per process).
    pub gateway_epoch: u64,
    /// The per-account, per-epoch sequence number.
    pub sequence: u64,
    /// The gatewayhub's clock when the event entered the ring, epoch ms: not the venue's event time. A bound on
    /// "everything at or before this has been seen", which is what the journal cursor uses it for.
    pub timestamp_ms: i64,
}

impl EventRingCursor {
    /// The cursor to HOLD once `seen` has been observed after `self`, in the order the observations
    /// were folded (journal order). Inside one epoch the larger `seq` wins, so a straggler that a
    /// replay re-delivers (a lower `seq`) can never move the resume point backwards; a different
    /// epoch means the gateway restarted and its ring began again, so the later observation wins
    /// whatever its `seq` (an epoch is random: there is no "larger" one).
    ///
    /// This is the ONE rule: the core's live fold, the boot restore's fold over the journal tail,
    /// the `Snapshot` and the gatewayhub client's duplicate guard all use it, so a cursor means the same
    /// thing in each.
    #[inline]
    pub fn after(self, seen: EventRingCursor) -> EventRingCursor {
        if seen.gateway_epoch == self.gateway_epoch && seen.sequence < self.sequence {
            self
        } else {
            seen
        }
    }
}
