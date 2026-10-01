//! `SubscribeAck` — the subscription-ack watchdog for market-data subscribe handshakes
//! (net-hardening audit **br7**). The data-feed twin of the exec user-data pump's
//! subscribe→ack-with-timeout→attributable-error handshake ([`crate::user_data`]): a market-data
//! `subscribe` frame is fire-and-forget on the wire, so a feed must positively confirm the venue
//! actually accepted it. This trips ONCE if neither a subscribe ACK nor first DATA arrives within a
//! bounded window of the subscribe — turning a silently-rejected or dead subscribe into an
//! attributable error at subscribe time, instead of only an eventual `Stale`/idle disclosure far
//! later (which the crypto BAR feeds don't even have — only the depth feeds run [`StreamHealth`]).
//!
//! Deliberately clock-free (the caller passes `now_ms` in, so every transition is unit-tested with
//! zero sleeps — the same style as [`StreamHealth`] and the depth [`crate::depth`] freshness
//! watchdog) and deliberately `vike-data`-free: this crate must not depend on `vike-data`. The venue
//! feed reads its own wall clock (a venue WS thread, NOT the vike-core hot fold) once at subscribe
//! and once per read-timeout poll tick, and surfaces the trip as a returned error carrying the
//! venue's message.
//!
//! [`StreamHealth`]: crate::stream_health::StreamHealth

/// Per-subscribe ack watchdog: arm at subscribe (`now_ms`), then [`Self::confirm`] on the venue's
/// subscribe ACK **or** the first data frame; [`Self::overdue`] trips once the arm window elapses
/// with neither. Clock-free — the caller passes `now_ms`/the timeout in.
#[derive(Debug)]
pub struct SubscribeAck {
    /// wall-clock ms by which an ack (or first data) must have arrived — `sent_at + timeout`.
    deadline_ms: i64,
    /// set once an ack OR first data confirms the handshake; after which [`Self::overdue`] is inert.
    confirmed: bool,
}

impl SubscribeAck {
    /// Arm the watchdog the moment the subscribe frame is sent. `now_ms` is the wall clock at
    /// subscribe; `timeout_ms` is how long the venue has to ack-or-deliver before the subscribe is
    /// declared dead. Saturating add keeps a pathological `timeout_ms` from overflowing the epoch-ms
    /// deadline.
    pub fn new(now_ms: i64, timeout_ms: i64) -> Self {
        SubscribeAck { deadline_ms: now_ms.saturating_add(timeout_ms), confirmed: false }
    }

    /// The subscribe handshake is confirmed — call on the venue's subscribe ACK frame **or** on the
    /// first DATA frame (either proves the venue accepted the subscription). Idempotent; once
    /// confirmed, [`Self::overdue`] never trips.
    pub fn confirm(&mut self) {
        self.confirmed = true;
    }

    /// Whether the handshake has been confirmed (an ack or first data was seen).
    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    /// Evaluate at `now_ms` (call on the read-timeout poll tick): `true` exactly when the arm window
    /// has elapsed with no confirmation, so the caller yields an attributable error and reconnects.
    /// Strict `>` boundary (like the freshness watchdog): at exactly the deadline it is not yet
    /// overdue. Always `false` once [`Self::confirm`]ed.
    pub fn overdue(&self, now_ms: i64) -> bool {
        !self.confirmed && now_ms > self.deadline_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core br7 case: a subscribe that is never acked and never delivers data trips `overdue`
    /// once the window elapses — the attributable "no ack/data" error the feed returns. Scripted
    /// clock (injected `now_ms`), zero sleeps.
    #[test]
    fn an_unacked_subscribe_goes_overdue_after_the_window() {
        let ack = SubscribeAck::new(1_000, 10_000); // armed at t=1s, 10s window → deadline 11s
        assert!(!ack.overdue(1_000), "just armed — not overdue");
        assert!(!ack.overdue(5_000), "within the window — not overdue");
        assert!(ack.overdue(11_001), "past the 11s deadline with no ack/data → overdue");
    }

    /// The boundary is strict `>`: at exactly the deadline it is NOT yet overdue; one ms past it is.
    #[test]
    fn the_deadline_boundary_is_strict() {
        let ack = SubscribeAck::new(0, 10_000); // deadline 10_000
        assert!(!ack.overdue(10_000), "exactly at the deadline is still fresh (strict >)");
        assert!(ack.overdue(10_001), "one ms past the deadline → overdue");
    }

    /// A subscribe ACK arriving inside the window disarms the watchdog: it never trips afterward,
    /// however far the clock advances.
    #[test]
    fn an_ack_within_the_window_prevents_the_trip() {
        let mut ack = SubscribeAck::new(0, 10_000);
        assert!(!ack.overdue(5_000), "still within the window before the ack");
        ack.confirm(); // the venue's subscribe ACK landed
        assert!(ack.is_confirmed());
        assert!(!ack.overdue(1_000_000), "a confirmed handshake never goes overdue");
    }

    /// First DATA confirms the handshake exactly like an ack — a venue that streams data before (or
    /// instead of) a distinct ack frame must not be falsely declared dead.
    #[test]
    fn first_data_confirms_like_an_ack() {
        let mut ack = SubscribeAck::new(0, 10_000);
        ack.confirm(); // caller invokes this identically on the first data frame
        assert!(!ack.overdue(50_000), "first data confirmed the subscribe — never overdue");
    }

    /// Confirming after the deadline (a late ack that races the poll tick) still disarms — `overdue`
    /// reads `false` once confirmed regardless of the clock, so a caller that confirms before its
    /// next tick won't spuriously error.
    #[test]
    fn a_late_confirm_still_disarms() {
        let mut ack = SubscribeAck::new(0, 10_000);
        assert!(ack.overdue(20_000), "past the deadline, unconfirmed → overdue");
        ack.confirm();
        assert!(!ack.overdue(20_000), "confirm() disarms even past the deadline");
    }
}
