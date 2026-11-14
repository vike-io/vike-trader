//! **Whether a process serves the venue catalog, and why** — the ONE decision site for
//! `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`.
//!
//! # Why this is here and not in the daemon that uses it
//!
//! The same argument [`crate::availability`] makes for its own table, one level up: a verdict two
//! roots could word differently is a verdict two roots WILL word differently. `vike-datahub` arms
//! the lane and `vike-cli` reports on it; both depend on this crate already, and neither depends
//! on the other. The precedent is `vike_tradehub::reconcile_config::reconcile_gate`, which lives one
//! crate below both of ITS roots for exactly this reason, and this module copies three of its
//! properties rather than inventing new ones:
//!
//! 1. **The verdict is an ENUM, not a `bool`** — the ANSWER and the REASON are read by different
//!    people. A server operator needs "is it on"; the person reading the startup log needs "and
//!    why".
//! 2. **The disclosure fires whichever way it went** ([`venue_catalog_gate_line`]) — both answers
//!    are news. "Serving" is news because the lane spends this box's venue-API budget on an
//!    Observe-scope client's request; "refused" is news because the operator's symbol picker will
//!    be empty and the reason has to be findable from the log alone.
//! 3. **It takes plain values, not a `Settings`.** This crate has no `vike-config` edge and wants
//!    none: the caller resolves `flags.venue_catalog_off` through the one loader
//!    `vike_boot::boot` owns and passes the `bool`.
//!
//! # ⚠ What this gate does NOT do, said out loud
//!
//! It does not bound the COST. `docs/decisions/0066`'s decision 2 takes that apart: a flag cannot
//! make a saturated fetch cheaper, it can only make the feature absent. What protects the
//! order-signing daemon's shared venue budget is the PER-VENUE token buckets
//! (`crates/vike-datahub/src/catalog.rs`'s `CATALOG_VENUE_REFILL`), the memo TTL and each bridge's
//! own compiled pager — none of which moves when this verdict moves. A reader who takes the
//! refusal for a rate limit has the wrong model of what it buys, which is a ZERO for the operator
//! who wants no catalog at all.

/// What a process decided about the venue-catalog lane, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogVenueGate {
    /// `flags.venue_catalog_off` — the operator WROTE the refusal. No lane is built, the capability
    /// is absent from the handshake, and the verb is still ANSWERED having called no venue.
    RefusedByOperator,
    /// The default. The lane is built.
    Armed,
}

impl CatalogVenueGate {
    /// Whether a lane is built at all — the ONE predicate a root may branch on, so a new arm
    /// cannot silently become served.
    #[must_use]
    pub fn serves(self) -> bool {
        matches!(self, Self::Armed)
    }
}

/// **The verdict.** `refused` is the resolved `flags.venue_catalog_off`.
#[must_use]
pub fn venue_catalog_gate(refused: bool) -> CatalogVenueGate {
    if refused { CatalogVenueGate::RefusedByOperator } else { CatalogVenueGate::Armed }
}

/// The startup disclosure, as a LINE rather than a log call — this crate carries no `tracing`
/// dependency, and the root that has one decides the level.
///
/// `supported` is the venue set the table can serve, rendered by the caller.
#[must_use]
pub fn venue_catalog_gate_line(gate: CatalogVenueGate, supported: &str) -> String {
    match gate {
        CatalogVenueGate::RefusedByOperator => format!(
            "VENUE-CATALOG lane REFUSED by this operator (`vike-cli config set \
             flags.venue_catalog_off true`). To serve it, run `vike-cli config set \
             flags.venue_catalog_off false` and restart this process. The verb is \
             still answered, having called no venue, and `venue_catalog` is absent from \
             Welcome.features — so a client says so to ITS operator rather than drawing an empty \
             symbol list. This build could have served: [{supported}]. \
             docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md"
        ),
        CatalogVenueGate::Armed => format!(
            "VENUE-CATALOG lane SERVING (the default since docs/decisions/0066) — an OBSERVE-scope \
             client may have this process fetch one venue's PUBLIC instrument list, spending this \
             box's venue-API budget. Supported: [{supported}]. It writes NOTHING to the store, and \
             no credentialed venue is reachable through it at any setting. Refuse it with \
             `vike-cli config set flags.venue_catalog_off true`"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_serves_and_the_refusal_is_the_only_thing_that_stops_it() {
        assert_eq!(venue_catalog_gate(false), CatalogVenueGate::Armed);
        assert_eq!(venue_catalog_gate(true), CatalogVenueGate::RefusedByOperator);
        assert!(venue_catalog_gate(false).serves());
        assert!(!venue_catalog_gate(true).serves());
    }

    /// Property 2: both answers are news, and each names what the operator does next.
    #[test]
    fn every_verdict_discloses_itself_and_names_its_own_switch() {
        let refused = venue_catalog_gate_line(CatalogVenueGate::RefusedByOperator, "binance, okx");
        assert!(refused.contains("venue_catalog_off"), "{refused}");
        assert!(refused.contains("restart"), "the way back is named: {refused}");
        assert!(refused.contains("binance, okx"), "it says what it would have served: {refused}");

        let armed = venue_catalog_gate_line(CatalogVenueGate::Armed, "binance, okx");
        assert!(armed.contains("venue-API budget"), "the cost is stated: {armed}");
        assert!(armed.contains("venue_catalog_off"), "the way out is named: {armed}");
        assert!(armed.contains("binance, okx"), "{armed}");

        // Neither retired variable may be suggested: setting either one refuses startup (decision
        // 0111). The bare prefix covers both spellings, `_OFF` included.
        for line in [&refused, &armed] {
            assert!(!line.contains("VIKE_DATAHUB_VENUE_CATALOG"), "{line}");
        }
    }
}
