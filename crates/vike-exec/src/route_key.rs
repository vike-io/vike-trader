//! [`RouteKey`] — the borrowed "which ENGINE" string, as a type the capability plane cannot accept
//! and which no canonical venue id can become by accident.
//!
//! [`crate::ExecutionEngine`] carries TWO strings: `venue` (the exchange id every per-venue
//! capability table is keyed on — `vike_model::caps_for`, `amend_semantics`, `fee_schedule_for`,
//! `venue_tif`, `venue_margin_support`) and `route_key` (which of this process's engines a
//! venue-tagged payload belongs to). `ExecutionEngine::new` seeds them EQUAL and nothing sets them
//! apart today, which is exactly the condition under which a wrong use is invisible. So
//! `vike_core`'s `CoreThread::engine_idx_for_route_key` takes a `RouteKey`, and one can only be
//! built by naming which claim you are making:
//!
//! * [`RouteKey::declared`] — "this string IS a route key" (an engine's own `route_key` field, a
//!   [`crate::ReconcileReports`]'s carried key, a held alert's stored one).
//! * [`RouteKey::sole_account_of`] — "this is a CANONICAL venue id, and I am asserting it has
//!   exactly one account in this process." That stops being true the moment a second account of
//!   one exchange is mounted, so every call is one site a second account has to revisit: a DERIVED
//!   roster. `CoreThread::engine_idx_for_route_key`'s doc carries the one command that prints it
//!   (`crates/vike-ops/tests/docs/unrun_command_gate.rs` runs it); do not spell a second copy here.
//!
//! Borrowed and [`Copy`]: a `RouteKey<'_>` is a `&str` and nothing else, so a construction is a
//! pointer+len copy. `vike_core::runtime`'s `route_event` and `drain_market` build one PER EVENT
//! on the fold under the µs-p99 gate, where an owned newtype would allocate.

/// Which ENGINE an inbound venue-tagged payload belongs to — unique per venue ACCOUNT rather than
/// per venue. The parameter type of `vike_core`'s `CoreThread::engine_idx_for_route_key`.
///
/// Deliberately NOT constructible from a bare `&str` (`From`/`Deref`/`AsRef` are all absent): the
/// two constructors below are the two different claims a caller can make, and making the caller
/// pick one is the whole point. See the module doc.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RouteKey<'a>(&'a str);

impl<'a> RouteKey<'a> {
    /// `s` IS a route key already — an [`crate::ExecutionEngine`]'s own `route_key` field, a
    /// [`crate::ReconcileReports`]'s carried key, or a route key a held record stored earlier in
    /// this process run. Asserts nothing about the venue.
    #[inline]
    pub fn declared(s: &'a str) -> Self {
        RouteKey(s)
    }

    /// `venue` is a CANONICAL venue id (`"binance"`), and the caller asserts that this venue has
    /// exactly ONE account mounted in this process — so its canonical id doubles as its routing
    /// key.
    ///
    /// True for every mount this workspace builds (`vike_tradehub::wired_markets::WIRED_MARKETS` is
    /// unique per venue, enforced by its own test). With a second account of one exchange mounted, a
    /// payload labelled `"binance"` can no longer say WHICH account it is for, and every call site
    /// spelled this way has to start carrying a real route key on the wire.
    #[inline]
    pub fn sole_account_of(venue: &'a str) -> Self {
        RouteKey(venue)
    }

    /// The underlying string, for comparison against an engine's stored `route_key`.
    #[inline]
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl std::fmt::Display for RouteKey<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// INERTNESS, stated on the type: while a venue has one account, both claims produce the same
    /// key — which is why every call site in this tree can be spelled either way today and why the
    /// conversion of the router's parameter changed no routing decision.
    #[test]
    fn both_claims_agree_while_a_venue_has_one_account() {
        assert_eq!(RouteKey::sole_account_of("binance"), RouteKey::declared("binance"));
        assert_eq!(RouteKey::sole_account_of("binance").as_str(), "binance");
    }

    /// …and they diverge exactly when the second account exists — the case the type is here for.
    #[test]
    fn a_decorated_route_key_is_not_its_venues_sole_account_key() {
        assert_ne!(RouteKey::declared("binance-sub2"), RouteKey::sole_account_of("binance"));
    }
}
