//! **Which roster venues can be enumerated at all, and at whose expense** — the per-venue
//! capability map behind
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`.
//!
//! # Why this is a table and not a property of the provider
//!
//! [`crate::CatalogProvider`] already answers a neighbouring question — [`crate::CatalogMode`] says
//! whether a venue is `Enumerable` or `QueryBacked` — but it answers it only where the provider is
//! LINKED, and the whole point of the venue-catalog verb is a process asking about venues whose
//! bridge crates it does not link. The data daemon links six of the fourteen; the desktop links
//! one. Neither can call `mode()` on a provider that is not in its binary.
//!
//! So the classification is DATA, in the bridge-free crate every consumer already depends on, and
//! [`catalog_availability`] is the registry — the STEP-1 shape the root `CLAUDE.md`'s per-venue
//! capability-map playbook prescribes: one row per venue citing the adapter code it was read from,
//! plus a completeness test iterating the canonical roster so a new venue reddens until it is
//! classified.
//!
//! # ⚠ The distinction this exists to preserve: an EMPTY list is a LIE for five venues
//!
//! Five roster providers return `Ok(vec![])` rather than an error when they cannot enumerate —
//! `crates/bridges/alpaca/src/catalog.rs`, `crates/bridges/oanda/src/catalog.rs`,
//! `crates/bridges/ctrader/src/catalog.rs` and `crates/bridges/ig/src/catalog.rs` each say so in
//! their own module docs ("returns `Ok(vec![])` (never an error) when credentials are absent"),
//! and the trait's own `list_instruments` default returns an empty `Vec` for every `QueryBacked`
//! venue. That is correct for THEM — it is the bridge-wide "absent credentials is the live gate"
//! rule — and it is exactly why a caller must not read an empty vector as "this venue has no
//! instruments". It is the same distinction the credential store draws between a store that is
//! ABSENT and one that is present and unreadable, and 0062's decision 5 is where it is ruled.
//!
//! This table is what lets a caller tell the three apart BEFORE it asks.

use crate::CatalogSource;

/// How a venue's instrument universe can be obtained — and, where it cannot, why not.
///
/// ⚠ The split that matters is **at whose expense**, not "does an endpoint exist":
/// [`Self::Credentialed`] venues DO have a bulk list, and fetching it spends the operator's own
/// venue identity. 0062's decision 3 is that a server will not spend that on a client's request, so
/// the two are different answers rather than degrees of the same one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatalogAvailability {
    /// A PUBLIC bulk list: any process can fetch it with no credentials at all. The only cost is
    /// rate budget, which a token bucket bounds.
    ///
    /// ⚠ Includes the two venues whose "fetch" costs ZERO requests — dukascopy and fxcm ship
    /// BUNDLED STATIC tables — because from a caller's side the question is "can I get a list
    /// without keys", and for those two the answer is yes and instantly.
    PublicBulk,
    /// A bulk list exists but is reachable only by authenticating as the operator.
    ///
    /// ⚠ **A server must not fetch these on an untrusted client's request** (0062 decision 3): a
    /// rate limit bounds spending a public budget, and nothing bounds spending an identity.
    Credentialed,
    /// No bulk list exists at any price. `why` is operator-facing and names the mechanism.
    NoBulkList { why: &'static str },
    /// Not a venue on `vike_model::VENUES` at all — fail-closed, and DISTINCT from every classified
    /// answer so a typo can never be reported as a venue property.
    UnknownVenue,
}

impl CatalogAvailability {
    /// Whether a keyless process may enumerate this venue. The one predicate a server's provider
    /// table is allowed to be built from — see 0062's decision 3.
    pub fn is_public(&self) -> bool {
        matches!(self, Self::PublicBulk)
    }
}

/// The registry: one arm per `vike_model::VENUES` entry, each citing the adapter code its value was
/// read from. An unclassified venue answers [`CatalogAvailability::UnknownVenue`] (fail-closed).
///
/// ⚠ Every roster venue is NAMED even where its value equals a neighbour's — the named row is what
/// proves it was classified rather than forgotten, which is the capability-map playbook's rule.
pub fn catalog_availability(venue: &str) -> CatalogAvailability {
    match venue {
        // Keyless public bulk endpoints. Request counts MEASURED per bridge, carried because they
        // are what a server's rate budget is priced against rather than decoration.
        // `crates/bridges/binance/src/catalog.rs`'s SPOT_URL + PERP_URL — two `exchangeInfo` GETs.
        "binance" => CatalogAvailability::PublicBulk,
        // `crates/bridges/bybit/src/catalog.rs`'s SPOT_URL + PERP_URL — two instruments-info GETs.
        "bybit" => CatalogAvailability::PublicBulk,
        // `crates/bridges/okx/src/catalog.rs`'s BASE + UNDERLYING — four GETs plus ONE PER OPTION
        // FAMILY, which makes this the heaviest request COUNT in the public group (tens).
        "okx" => CatalogAvailability::PublicBulk,
        // `crates/bridges/aster/src/catalog.rs`'s spot_url + perp_url — two `exchangeInfo` GETs.
        "aster" => CatalogAvailability::PublicBulk,
        // `crates/bridges/hyperliquid/src/catalog.rs` — two unsigned `info` POSTs.
        "hyperliquid" => CatalogAvailability::PublicBulk,
        // `crates/bridges/polymarket/src/catalog.rs`'s GAMMA_MAX_PAGES x GAMMA_PAGE_LIMIT — up to
        // 40 pages of 500. The heaviest entry anywhere in this table, and the first of 0062
        // decision 4's three reasons the lane is armed at all.
        "polymarket" => CatalogAvailability::PublicBulk,
        // `crates/bridges/deribit/src/catalog.rs` — "the KEYLESS public
        // `/api/v2/public/get_instruments` (one UNSIGNED GET per currency — no credentials, no
        // signer)". The one venue the DESKTOP still enumerates directly, because the Options tool's
        // chain provider already links that bridge.
        "deribit" => CatalogAvailability::PublicBulk,
        // `crates/bridges/dukascopy/src/catalog.rs`'s FX_TABLE — a BUNDLED STATIC set: "there is no
        // instrument-list endpoint to fetch from". ZERO requests, and therefore the cheapest
        // possible entry in any server's provider table.
        "dukascopy" => CatalogAvailability::PublicBulk,
        // `crates/bridges/fxcm/src/catalog.rs`'s FX_TABLE — the same bundled-static shape, and
        // declared UNCONDITIONALLY rather than behind that crate's `fxcm` feature, so it needs no
        // SDK and costs zero requests.
        "fxcm" => CatalogAvailability::PublicBulk,

        // Credentialed: a bulk list EXISTS, and reaching it spends the operator's identity.
        // `crates/bridges/alpaca/src/catalog.rs` — `GET /v1/assets?status=active` "sits behind the
        // same OAuth2 Bearer every other Alpaca REST call needs".
        "alpaca" => CatalogAvailability::Credentialed,
        // `crates/bridges/oanda/src/catalog.rs` — `GET /v3/accounts/{account_id}/instruments`
        // behind the same Bearer token, and the path names an ACCOUNT.
        "oanda" => CatalogAvailability::Credentialed,
        // `crates/bridges/ctrader/src/catalog.rs` — "NO REST symbol-list endpoint: the symbol
        // universe is enumerated over the Open API protobuf SESSION", i.e. an authed socket.
        "ctrader" => CatalogAvailability::Credentialed,

        // No bulk list at any price.
        // `crates/bridges/ig/src/catalog.rs` — "the FIRST `QueryBacked` `CatalogProvider` […] there
        // is no bulk 'list every market' endpoint worth caching".
        "ig" => CatalogAvailability::NoBulkList {
            why: "IG lists thousands of epics and publishes no bulk market list, so its provider \
                  searches per query (`CatalogMode::QueryBacked`) instead of enumerating",
        },
        // IBKR ships NO `CatalogProvider` at all — the `crates/bridges/vike-ibkr` crate has no
        // `catalog` module, which is the IBKR-scale case `CatalogMode::QueryBacked`'s own doc names.
        "ibkr" => CatalogAvailability::NoBulkList {
            why: "the IBKR bridge ships no catalog provider — its universe is contract-search \
                  shaped, which is the case `CatalogMode::QueryBacked` was introduced for",
        },
        // vike:new-venue:row         // TODO(new-venue: {venue}): classify this venue's catalog and CITE the adapter code the
        // vike:new-venue:row         // value was read from, the way every row above does. The scaffolded value is the
        // vike:new-venue:row         // FAIL-CLOSED one: a venue is assumed un-enumerable until somebody has read its
        // vike:new-venue:row         // bridge's `catalog` module. Do NOT scaffold `PublicBulk` — that would let a server
        // vike:new-venue:row         // fetch on an untrusted client's request on the strength of a placeholder.
        // vike:new-venue:row         "{venue}" => CatalogAvailability::NoBulkList {
        // vike:new-venue:row             why: "TODO(new-venue: {venue}): unclassified — read this bridge's catalog module",
        // vike:new-venue:row         },
        _ => CatalogAvailability::UnknownVenue,
    }
}

/// **Where a CLIENT should get one venue's catalog** — the first consumer
/// [`crate::CatalogSource`] has ever had.
///
/// That type has carried the `Direct` / `ServerBacked` axis since it was written and was referenced
/// by no code at all: the `ServerBacked` half had no wire to sit on until
/// `vike_datahub_client::proto::Request::VenueCatalog` existed. This function is the decision it
/// was declared for.
///
/// `direct` is the set of venue slugs THIS BINARY links a `CatalogProvider` for — a property of the
/// caller's own dependency graph, which no table can know. The desktop's is one venue (`deribit`,
/// linked because the Options tool's chain provider already brings that bridge in); a binary that
/// links none passes an empty slice.
///
/// - `Some(Direct)` — the caller has the provider; no server is needed and none should be asked.
/// - `Some(ServerBacked)` — the caller does not link it, but it is publicly enumerable, so a
///   datahub carrying the provider can list it. **Whether a PARTICULAR server will is a separate
///   question** the handshake answers (`FEATURE_VENUE_CATALOG`) and the response refines
///   (`NotServed`); this says only that the route EXISTS.
/// - `None` — no route exists for anybody: the venue is credentialed (a server will not spend the
///   operator's identity on a client's request) or has no bulk list at all. [`catalog_availability`]
///   carries which, and it is what a UI should render instead of a refresh control.
///
/// ⚠ **A credentialed venue is `None` rather than `Direct` even when the caller links its bridge**,
/// and that is deliberate rather than an oversight: this function answers "can a catalog be
/// REFRESHED from here", and for alpaca/oanda/ctrader the honest answer depends on credentials this
/// function cannot see. A caller holding those credentials reaches the provider directly and does
/// not need routing advice; a caller that does not would render a control that silently returns an
/// empty list, which is the failure `docs/decisions/0062`'s decision 5 exists to prevent.
///
/// ⚠ **SOMETHING SEES THEM NOW, and this function is still not it.** That ⚠ used to end there, and
/// `docs/decisions/0066`'s decision 9 armed a locally-credentialed refresh: on the box that holds a
/// venue's keys, the operator lists it themselves with their own credentials
/// (`vike-backend catalog refresh <venue>`), writing `vike_catalog::LOCAL_FILE`. **That does not change
/// this answer for any caller of this function** — the desktop links none of those three bridges
/// (`crates/vike-desktop/Cargo.toml`'s tombstone deleted them precisely because a catalog fetch is
/// a venue REST call), and the route the record REFUSES is the one where a server does the fetch. So
/// `None` still means "no route from HERE"; what changed is that a `None` is no longer the end of
/// the operator's options, which is why
/// `crates/vike-datahub-client/src/catalog.rs`'s `CatalogRefusal::NeedsCredentials` now names the
/// act. A caller rendering a row wants
/// `crate::catalog_answer` beside this, not a fourth `CatalogSource` variant.
pub fn catalog_source_for(venue: &str, direct: &[&str]) -> Option<CatalogSource> {
    if !catalog_availability(venue).is_public() {
        return None;
    }
    if direct.contains(&venue) {
        return Some(CatalogSource::Direct);
    }
    Some(CatalogSource::ServerBacked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::VENUES;

    #[test]
    fn every_roster_venue_is_classified() {
        // The completeness test the capability-map playbook requires: a new venue fails HERE until
        // its row exists, rather than silently answering `UnknownVenue` to a server that would then
        // report it as a typo.
        for v in VENUES {
            assert_ne!(
                catalog_availability(v),
                CatalogAvailability::UnknownVenue,
                "roster venue {v} has no row in `catalog_availability`"
            );
        }
    }

    #[test]
    fn an_unknown_venue_fails_closed_and_is_not_a_venue_property() {
        // `UnknownVenue` must be its OWN answer: reporting a typo as `NoBulkList` would tell an
        // operator a false fact about a venue that does not exist.
        for bad in ["binanc", "BINANCE", "", "kraken"] {
            assert_eq!(catalog_availability(bad), CatalogAvailability::UnknownVenue, "{bad}");
        }
        assert!(!CatalogAvailability::UnknownVenue.is_public(), "fail-closed");
    }

    #[test]
    fn a_credentialed_venue_is_never_public() {
        // 0062's decision 3, held as a property rather than trusted to the server's table
        // construction: these three are what `is_public` must keep out of any provider table.
        for v in ["alpaca", "oanda", "ctrader"] {
            assert_eq!(catalog_availability(v), CatalogAvailability::Credentialed, "{v}");
            assert!(!catalog_availability(v).is_public(), "{v} must never be fetched keylessly");
        }
    }

    #[test]
    fn the_two_un_enumerable_venues_carry_a_reason_an_operator_can_read() {
        for v in ["ig", "ibkr"] {
            match catalog_availability(v) {
                CatalogAvailability::NoBulkList { why } => {
                    assert!(why.len() > 40, "{v}'s reason is too thin to render: {why}");
                    assert!(!why.contains("TODO"), "{v} is still a scaffold row: {why}");
                }
                other => panic!("{v} must be NoBulkList, got {other:?}"),
            }
            assert!(!catalog_availability(v).is_public(), "{v}");
        }
    }

    #[test]
    fn the_desktops_own_routing_is_one_direct_venue_and_eight_server_backed() {
        // The question the Data Manager's refresh control actually asks, answered over the REAL
        // roster for the REAL desktop (which links `deribit` alone since rulings 1 and 2 took the
        // venue bridges out of its graph — `spawn_catalog_fetcher`'s tombstone).
        const DESKTOP_LINKS: &[&str] = &["deribit"];
        assert_eq!(
            catalog_source_for("deribit", DESKTOP_LINKS),
            Some(CatalogSource::Direct),
            "the one bridge the desktop links must not be routed through a server"
        );
        for v in
            ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "dukascopy", "fxcm"]
        {
            assert_eq!(
                catalog_source_for(v, DESKTOP_LINKS),
                Some(CatalogSource::ServerBacked),
                "{v} is publicly enumerable and unlinked, so it routes to the server"
            );
        }
        // ...and the five with no route at all, which is what a UI renders a SENTENCE for rather
        // than a button.
        for v in ["alpaca", "oanda", "ctrader", "ig", "ibkr"] {
            assert_eq!(catalog_source_for(v, DESKTOP_LINKS), None, "{v} has no refresh route");
        }
        // Every roster venue is decided one way or the other.
        let routed = VENUES.iter().filter(|v| catalog_source_for(v, DESKTOP_LINKS).is_some());
        assert_eq!(routed.count(), 9, "nine of fourteen are refreshable from a desktop");
    }

    #[test]
    fn a_credentialed_venue_has_no_route_even_for_a_caller_that_links_it() {
        // The ⚠ on `catalog_source_for`, held rather than left as prose: linking alpaca's bridge
        // does not make its catalog refreshable, because the credentials are the gate and this
        // function cannot see them.
        assert_eq!(catalog_source_for("alpaca", &["alpaca", "oanda"]), None);
        assert_eq!(catalog_source_for("oanda", &["alpaca", "oanda"]), None);
    }

    #[test]
    fn an_unknown_venue_has_no_route() {
        assert_eq!(catalog_source_for("kraken", &["kraken"]), None, "fail-closed");
    }

    #[test]
    fn nine_roster_venues_are_publicly_enumerable_and_the_count_is_derived() {
        // Not a hand-copied number: it is COUNTED off the table, so the assertion is that the
        // public set is exactly the complement of the five that are not — which is the property a
        // server's table is built from. The named list is what makes a silent reclassification
        // (say, a venue quietly becoming `PublicBulk`) fail here rather than widen a server's
        // venue egress unnoticed.
        let public: Vec<&&str> =
            VENUES.iter().filter(|v| catalog_availability(v).is_public()).collect();
        let not_public: Vec<&&str> =
            VENUES.iter().filter(|v| !catalog_availability(v).is_public()).collect();
        assert_eq!(public.len() + not_public.len(), VENUES.len());
        assert_eq!(not_public.len(), 5, "not-public set changed: {not_public:?}");
        for v in ["alpaca", "oanda", "ctrader", "ig", "ibkr"] {
            assert!(not_public.iter().any(|x| **x == v), "{v} must not be publicly enumerable");
        }
    }
}
