//! **Whether a process serves the venue catalog, and why** — the ONE decision site for
//! `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`.
//!
//! # Why this is here and not in the daemon that uses it
//!
//! The same argument [`crate::availability`] makes for its own table, one level up: a verdict two
//! roots could word differently is a verdict two roots WILL word differently. `vike-datahub` arms
//! the lane and `vike-cli` reports on it; both depend on this crate already, and neither depends
//! on the other. The precedent is `vike_ops::reconcile_config::reconcile_gate`, which lives one
//! crate below both of ITS roots for exactly this reason, and this module copies four of its
//! properties rather than inventing new ones:
//!
//! 1. **The verdict is an ENUM, not a `bool`** — the ANSWER and the REASON are read by different
//!    people. A server operator needs "is it on"; the person reading the startup log needs "and
//!    why", and an [`VenueCatalogGate::ArmedWithNoProviders`] box is serving a verb that will
//!    refuse every venue.
//! 2. **The REFUSAL is checked FIRST.** A written `flags.toml` line must not be overruled by the
//!    shape of the build, so an operator who refused the lane reads a refusal even on a build that
//!    could not have served it anyway.
//! 3. **The disclosure fires whichever way it went** ([`venue_catalog_gate_line`]) — both answers
//!    are news. "Serving" is news because the lane spends this box's venue-API budget on an
//!    Observe-scope client's request; "refused" is news because the operator's symbol picker will
//!    be empty and the reason has to be findable from the log alone.
//! 4. **It takes plain values, not a `Settings`.** This crate has no `vike-config` edge and wants
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
///
/// ⚠ Three arms rather than two, because [`Self::Armed`] and [`Self::ArmedWithNoProviders`] are
/// different facts about the world that an operator acts on differently: the second is a BUILD
/// carrying no `CatalogProvider`, so the verb answers and every venue comes back `NotServed` with
/// an empty supported set. Collapsing them would report a rebuild-shaped problem as a working
/// server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VenueCatalogGate {
    /// `flags.venue_catalog_off` (or `VIKE_DATAHUB_VENUE_CATALOG_OFF=1`) — the operator WROTE the
    /// refusal. No lane is built, the capability is absent from the handshake, and the verb is
    /// still ANSWERED having called no venue.
    RefusedByOperator,
    /// The default. The lane is built over a non-empty provider table.
    Armed,
    /// The default, on a build carrying NO provider — `--features catalog-serve` is missing. The
    /// lane is still built (see [`Self::serves`]) and every venue answers `NotServed`, which is a
    /// sentence an operator can act on; refusing to start would be the mistake
    /// `docs/decisions/0013-degrade-vs-refuse.md` rules against.
    ArmedWithNoProviders,
}

impl VenueCatalogGate {
    /// Whether a lane is built at all — the ONE predicate a root may branch on, so a new arm
    /// cannot silently become served.
    #[must_use]
    pub fn serves(self) -> bool {
        matches!(self, Self::Armed | Self::ArmedWithNoProviders)
    }
}

/// **The verdict.** `refused` is the resolved `flags.venue_catalog_off`; `providers` is how many
/// `CatalogProvider`s this build linked into its table.
///
/// The refusal is checked FIRST — see the module doc's property 2.
#[must_use]
pub fn venue_catalog_gate(refused: bool, providers: usize) -> VenueCatalogGate {
    if refused {
        return VenueCatalogGate::RefusedByOperator;
    }
    if providers == 0 {
        return VenueCatalogGate::ArmedWithNoProviders;
    }
    VenueCatalogGate::Armed
}

/// The startup disclosure, as a LINE rather than a log call — this crate carries no `tracing`
/// dependency, and the root that has one decides the level.
///
/// `supported` is the venue set the table can serve, rendered by the caller (empty for
/// [`VenueCatalogGate::ArmedWithNoProviders`], which is the whole of what that arm means).
#[must_use]
pub fn venue_catalog_gate_line(gate: VenueCatalogGate, supported: &str) -> String {
    match gate {
        VenueCatalogGate::RefusedByOperator => format!(
            "VENUE-CATALOG lane REFUSED by this operator (`venue_catalog_off = true` in \
             <project>/settings/flags.toml, or VIKE_DATAHUB_VENUE_CATALOG_OFF=1). The verb is \
             still answered, having called no venue, and `venue_catalog` is absent from \
             Welcome.features — so a client says so to ITS operator rather than drawing an empty \
             symbol list. This build could have served: [{supported}]. \
             docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md"
        ),
        VenueCatalogGate::Armed => format!(
            "VENUE-CATALOG lane SERVING (the default since docs/decisions/0066) — an OBSERVE-scope \
             client may have this process fetch one venue's PUBLIC instrument list, spending this \
             box's venue-API budget. Supported: [{supported}]. It writes NOTHING to the store, and \
             no credentialed venue is reachable through it at any setting. Refuse it with \
             `venue_catalog_off = true` in <project>/settings/flags.toml"
        ),
        VenueCatalogGate::ArmedWithNoProviders => {
            "VENUE-CATALOG lane SERVING but this build carries NO catalog providers — every venue \
             will answer `NotServed` with an empty supported set. Rebuild with `--features \
             catalog-serve`"
                .to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_serves_and_the_refusal_is_the_only_thing_that_stops_it() {
        assert_eq!(venue_catalog_gate(false, 9), VenueCatalogGate::Armed);
        assert_eq!(venue_catalog_gate(true, 9), VenueCatalogGate::RefusedByOperator);
        assert!(venue_catalog_gate(false, 9).serves());
        assert!(!venue_catalog_gate(true, 9).serves());
    }

    /// Property 2, held rather than left as prose: the build's shape may not overrule the written
    /// refusal, so a refused table-less box reports the REFUSAL and not the rebuild.
    #[test]
    fn the_refusal_is_checked_before_the_build() {
        assert_eq!(venue_catalog_gate(true, 0), VenueCatalogGate::RefusedByOperator);
        assert_eq!(venue_catalog_gate(false, 0), VenueCatalogGate::ArmedWithNoProviders);
    }

    /// A table-less build still SERVES — the degrade, not a refusal to start.
    #[test]
    fn a_provider_less_build_still_answers_and_says_what_is_missing() {
        let gate = venue_catalog_gate(false, 0);
        assert!(gate.serves(), "the verb must still be answered");
        let line = venue_catalog_gate_line(gate, "");
        assert!(line.contains("catalog-serve"), "{line}");
        assert!(line.contains("NotServed"), "{line}");
    }

    /// Property 3: both answers are news, and each names what the operator does next.
    #[test]
    fn every_verdict_discloses_itself_and_names_its_own_switch() {
        let refused = venue_catalog_gate_line(VenueCatalogGate::RefusedByOperator, "binance, okx");
        assert!(refused.contains("venue_catalog_off"), "{refused}");
        assert!(refused.contains("VIKE_DATAHUB_VENUE_CATALOG_OFF=1"), "{refused}");
        assert!(refused.contains("binance, okx"), "it says what it would have served: {refused}");

        let armed = venue_catalog_gate_line(VenueCatalogGate::Armed, "binance, okx");
        assert!(armed.contains("venue-API budget"), "the cost is stated: {armed}");
        assert!(armed.contains("venue_catalog_off"), "the way out is named: {armed}");
        assert!(armed.contains("binance, okx"), "{armed}");

        // The OLD arming must not be suggested anywhere: it configures nothing now, and a line
        // naming it would send an operator to a variable `vike_config::load` only warns about.
        for line in [&refused, &armed] {
            assert!(!line.contains("VIKE_DATAHUB_VENUE_CATALOG=1"), "{line}");
        }
    }
}
