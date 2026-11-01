//! **The daemon's own view of a venue's price-feed health**, for the reconcile health gate (wave 2, W13;
//! the plan's R31).
//!
//! Before the tradehub moved onto the gatewayhub, `ReconManager` skipped a venue's reconcile leg while
//! that venue's bridge feed read mid-gap (`Degraded`): a reconcile against reports the venue has not
//! caught up on after a reconnect folds a storm of false divergences. The feed object was the
//! tradehub's; it is the gatewayhub's now (the tick pump), so the evidence is HERE and the tradehub
//! learns it as a `ReconNotReady{reason}` answer to a `Recon` request (`crate::trade::lease`), which
//! its manager treats as a skipped pass.
//!
//! The rule is the old one, narrowed to what the old code actually fed the gate: the four CEX venues
//! whose tick pump this daemon hosts (`crate::data::pump`), and a venue is degraded while ANY pump of it
//! last disclosed a disconnect and has not recovered. A venue with no pump running reads healthy
//! (the fail-soft direction: this gate can only SUPPRESS a pass). Nothing here dials or reads a
//! socket; it reads a flag the pump already keeps.

/// What the trade lane asks of the data half: is this venue's price feed mid-gap?
pub trait VenueHealth: Send + Sync {
    /// `Some(reason)` while `venue`'s feed is mid-gap (a reconcile pass should wait), `None` otherwise.
    /// Cheap and non-blocking: a lock and a scan over the running pumps.
    fn degraded(&self, venue: &str) -> Option<String>;
}

/// The words every degraded answer begins with (the client shows them behind its own NOT READY prefix).
pub const DEGRADED_PREFIX: &str = "Degraded: ";

/// The reason text for a venue whose feed is mid-gap.
#[must_use]
pub fn degraded_reason(venue: &str) -> String {
    format!(
        "{DEGRADED_PREFIX}{venue}'s price feed is mid-gap (disconnected, resyncing); a reconcile \
         against reports the venue has not caught up on would fold false divergences, so this pass \
         waits for the next poke"
    )
}
