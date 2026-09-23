//! `datahub_resolve` — the ONE datahub-address adoption rule (split-plane REQ-2).
//!
//! A client can learn where its datahub is from TWO places: its own explicit
//! `config.datahub_addr` (the operator's client-side setting), and the ACTIVE backend's
//! `Welcome.features` advertisement (`datahub=<addr>` — the daemon's `datahub_advertise_addr`,
//! parsed by `vike_tradehub_client::proto::advertised_datahub` and carried on the observe bridge
//! as [`crate::observe_bridge::BridgeHandle::advertised_datahub`]). This module is the ONE place
//! the two are ranked, so the Studio store, the backfill route
//! ([`crate::backfill_route::backfill_route`]) and the Stored-grid mode
//! ([`crate::stored_mode::stored_mode`]) cannot each invent their own answer: the binary resolves
//! ONCE through [`resolve_datahub_addr`] and threads the result into all three.
//!
//! # The adoption rule (the product decision)
//!
//! **An explicit `config.datahub_addr` ALWAYS wins; absent it, the active backend's
//! advertisement fills in; absent both, there is no datahub** (the local-store arm, exactly the
//! pre-REQ-2 behavior). Explicit-wins is deliberate: the operator who wrote an address into
//! their own config file must never have it silently replaced by whatever a daemon claims —
//! an advertisement is a DEFAULT, not an override. One address becomes literal for the common
//! case (configure the backend, get both planes) while the escape hatch (a client-side pin, e.g.
//! a different tunnel mouth than the daemon assumes) keeps working untouched.
//!
//! # Timing honesty — "connect first, then Studio sees it"
//!
//! The advertisement arrives ONLY at a backend handshake, so before any backend connects the
//! resolution is the explicit key alone — a Studio opened first behaves exactly as pre-REQ-2 and
//! (because the store open is deliberately one-shot per process, see vike-app's
//! `open_studio_store`) KEEPS that store for the session even if an advertisement arrives later.
//! Every OTHER consumer re-reads at the moment of use, so a backend switch re-resolves by
//! construction: the advertisement is read off the ACTIVE `BackendConn`'s bridge, and a link
//! drop clears it (a stale daemon's advertisement must not outlive its connection, the same
//! discipline as the identity block).

/// Rank the two sources per the adoption rule: `explicit` (the client's own
/// `config.datahub_addr`) always wins; otherwise the active backend's `advertised` fills in;
/// otherwise `None` (no datahub — the local-store arm). Blank/whitespace values count as absent
/// on BOTH inputs — defense in depth; the config layer already rejects a blank address, and the
/// advertisement parser already trims — so a resolved `Some` is always a usable non-empty
/// address.
pub fn resolve_datahub_addr(explicit: Option<&str>, advertised: Option<&str>) -> Option<String> {
    let non_blank = |s: &&str| !s.trim().is_empty();
    explicit.filter(non_blank).or(advertised.filter(non_blank)).map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adoption table, all four cells: explicit wins / advertised fills / explicit alone
    /// stands / neither = None.
    #[test]
    fn explicit_wins_advertised_fills_neither_is_none() {
        assert_eq!(
            resolve_datahub_addr(Some("pin:1111"), Some("adv:2222")),
            Some("pin:1111".to_string()),
            "an explicit config.datahub_addr always wins"
        );
        assert_eq!(
            resolve_datahub_addr(None, Some("adv:2222")),
            Some("adv:2222".to_string()),
            "absent an explicit address, the active backend's advertisement fills in"
        );
        assert_eq!(
            resolve_datahub_addr(Some("pin:1111"), None),
            Some("pin:1111".to_string()),
            "an explicit address needs no backend at all"
        );
        assert_eq!(resolve_datahub_addr(None, None), None, "neither ⇒ no datahub (local store)");
    }

    /// A blank on either input is ABSENT, not a value — a blank explicit must fall through to
    /// the advertisement rather than masking it, and a blank advertisement must not conjure a
    /// dial address out of whitespace.
    #[test]
    fn blank_counts_as_absent_on_both_inputs() {
        assert_eq!(
            resolve_datahub_addr(Some("   "), Some("adv:2222")),
            Some("adv:2222".to_string())
        );
        assert_eq!(resolve_datahub_addr(None, Some("   ")), None);
        assert_eq!(resolve_datahub_addr(Some(""), Some("")), None);
        assert_eq!(
            resolve_datahub_addr(Some(" pin:1111 "), None),
            Some("pin:1111".to_string()),
            "the resolved value is trimmed — never a padded dial string"
        );
    }
}
