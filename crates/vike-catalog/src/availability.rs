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
//! none. Neither can call `mode()` on a provider that is not in its binary.
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

use crate::CatalogRoute;

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
    PublicBulk,
    /// A bulk list exists but is reachable only by authenticating as the operator.
    ///
    /// ⚠ **A server must not fetch these on an untrusted client's request** (0062 decision 3): a
    /// rate limit bounds spending a public budget, and nothing bounds spending an identity.
    Credentialed,
    /// A bulk list exists behind the operator's identity, and a server reads it ONLY from a session
    /// the gatewayhub ALREADY holds for a running account of the venue: it never logs in for a list,
    /// so it spends no identity the account was not already spending (0062's decision 3, amended
    /// 2026-11-13). With no such session the listing is a routine refusal
    /// ([`crate::CatalogProvider::no_session`]), never an error and never an empty list.
    HeldSession,
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

    /// Whether a server's provider table may hold this venue: a public list, or one read from a
    /// session the gatewayhub already holds ([`Self::HeldSession`]). The predicate the datahub's
    /// `CatalogTable` and the gatewayhub's `CatalogProviders` are built from.
    pub fn is_served(&self) -> bool {
        matches!(self, Self::PublicBulk | Self::HeldSession)
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
        // `crates/bridges/binance/src/catalog.rs`'s `CatalogProviderBinance::from_urls` — three `exchangeInfo`
        // GETs: spot, USDⓈ-M and COIN-M (the last about 35 KB, request weight 1).
        "binance" => CatalogAvailability::PublicBulk,
        // `crates/bridges/bybit/src/catalog.rs`'s `listing_url` (spot, linear, inverse) — three paged instruments-info listings.
        "bybit" => CatalogAvailability::PublicBulk,
        // `crates/bridges/okx/src/catalog.rs`'s PATH_INSTRUMENTS + PATH_UNDERLYING — four GETs plus
        // ONE PER OPTION FAMILY, which makes this the heaviest request COUNT in the public group
        // (tens).
        "okx" => CatalogAvailability::PublicBulk,
        // `crates/bridges/aster/src/catalog.rs`'s spot_url + perp_url — two `exchangeInfo` GETs.
        "aster" => CatalogAvailability::PublicBulk,
        // `crates/bridges/hyperliquid/src/catalog.rs` — two unsigned `info` POSTs (`meta`,
        // `spotMeta`). With `flags.hyperliquid_hip3` on, which the gatewayhub's catalog reads (the
        // datahub's did until decision 0164; the flag is decision 0095's), add one for `perpDexs`
        // and one `meta` per builder dex it lists
        // (`crates/bridges/hyperliquid/src/instrument/properties.rs`'s `fold_perp_dexs`): 2 + 1 + N.
        "hyperliquid" => CatalogAvailability::PublicBulk,
        // `crates/bridges/polymarket/src/catalog/mod.rs`'s GAMMA_MAX_PAGES x GAMMA_PAGE_LIMIT — up to
        // 40 pages of 500. The heaviest entry anywhere in this table, and the first of 0062
        // decision 4's three reasons the lane is armed at all.
        "polymarket" => CatalogAvailability::PublicBulk,
        // `crates/bridges/deribit/src/catalog.rs`'s `request_urls` — three UNSIGNED GETs of the
        // KEYLESS public `/api/v2/public/get_instruments`, `currency=any` for `kind=future`, `spot` and
        // `option` (the options alone 4.1 MB, MEASURED 2026-11-13). No credentials, no signer. The
        // desktop links no venue bridge, so it lists this one through the datahub like every other;
        // the datahub keeps it (DER-2), like every venue's, only on the operator's refresh.
        "deribit" => CatalogAvailability::PublicBulk,
        // `crates/bridges/dukascopy/src/catalog.rs`'s `hold` — the account's list the JForex sidecar's
        // LOGIN received (`IClient.getAvailableInstruments()`), pushed once per login into
        // `crate::held_list`: ZERO requests to the venue, and no login of its own. No account's
        // sidecar has logged in: a routine refusal.
        "dukascopy" => CatalogAvailability::HeldSession,
        // `crates/bridges/fxcm/src/catalog.rs`'s `CatalogProviderFxcm` — the Offers table of the
        // session a RUNNING fxcm account already holds (its reconcile session): ONE table refresh
        // on that session, never a login of its own. No running account: a routine refusal.
        "fxcm" => CatalogAvailability::HeldSession,

        // Credentialed: a bulk list EXISTS, and reaching it spends the operator's identity.
        // `crates/bridges/alpaca/src/catalog.rs` — `GET /v1/assets?status=active` "sits behind the
        // same OAuth2 Bearer every other Alpaca REST call needs".
        "alpaca" => CatalogAvailability::Credentialed,
        // `crates/bridges/oanda/src/catalog.rs` — `GET /v3/accounts/{account_id}/instruments`
        // behind the same Bearer token, and the path names an ACCOUNT. HELD (2026-11-13, the owner's
        // "yes"): read over a RUNNING account's keys in the gatewayhub, once at its start and anew on each
        // operator refresh (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s
        // `hold_oanda_listing`: ONE `GET` a read, never on a timer), and pushed again by its symbol facts
        // (`hold_listing`); no account started in this process = no session.
        "oanda" => CatalogAvailability::HeldSession,
        // `crates/bridges/ctrader/src/catalog.rs` — "NO REST symbol-list endpoint: the symbol
        // universe is enumerated over the Open API protobuf SESSION", i.e. an authed socket. HELD like
        // oanda's: the listing a running account's login already received (that file's `hold`), zero requests.
        "ctrader" => CatalogAvailability::HeldSession,

        // `crates/bridges/ig/src/market_list.rs`'s `CatalogProviderIgList` — `GET /categories` and every page of
        // `GET /categories/{id}/instruments` over the facts session a RUNNING demo account holds in the gatewayhub
        // (kept from its start; never a second login: one not logged in yet makes its one login for the refresh):
        // ~17 reads (MEASURED on the demo 2026-11-16: 9 categories, EQUITIES 8
        // pages of 1000), paced to a quarter of that session's read cap. HELD (2026-11-16, the owner's option A); it
        // was `NoBulkList` while IG's navigation tree was the only way to enumerate it. Listed only on the
        // operator's command, like every venue since 2026-11-16 (no timer refreshes a kept list).
        "ig" => CatalogAvailability::HeldSession,

        // No bulk list at any price.
        // IBKR ships NO `CatalogProvider` at all — the `crates/bridges/vike-ibkr` crate has no
        // `catalog` module, which is the IBKR-scale case `CatalogMode::QueryBacked`'s own doc names.
        "ibkr" => CatalogAvailability::NoBulkList {
            why: "the IBKR bridge ships no catalog provider — its universe is contract-search \
                  shaped, which is the case `CatalogMode::QueryBacked` was introduced for",
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): classify this venue's catalog and CITE the adapter code the
        // vike:new-venue:row // value was read from, the way every row above does. The scaffolded value is the
        // vike:new-venue:row // FAIL-CLOSED one: a venue is assumed un-enumerable until somebody has read its
        // vike:new-venue:row // bridge's `catalog` module. Do NOT scaffold `PublicBulk` — that would let a server
        // vike:new-venue:row // fetch on an untrusted client's request on the strength of a placeholder.
        // vike:new-venue:row "{venue}" => CatalogAvailability::NoBulkList {
        // vike:new-venue:row     why: "TODO(new-venue: {venue}): unclassified — read this bridge's catalog module",
        // vike:new-venue:row },
        _ => CatalogAvailability::UnknownVenue,
    }
}

/// **Where a CLIENT should get one venue's catalog** — the first consumer
/// [`crate::CatalogRoute`] has ever had.
///
/// That type has carried the `Direct` / `ServerBacked` axis since it was written and was referenced
/// by no code at all: the `ServerBacked` half had no wire to sit on until
/// `vike_datahub_client::proto::Request::VenueCatalog` existed. This function is the decision it
/// was declared for.
///
/// `direct` is the set of venue slugs THIS BINARY links a `CatalogProvider` for — a property of the
/// caller's own dependency graph, which no table can know. The desktop's is EMPTY: it links no venue
/// bridge (its Options tool reads the datahub too), so it passes an empty slice and every publicly
/// enumerable venue routes to the server.
///
/// - `Some(Direct)` — the caller has the provider; no server is needed and none should be asked.
/// - `Some(ServerBacked)` — the caller does not link it, but it is publicly enumerable (or listed
///   from a session the gatewayhub holds, [`CatalogAvailability::HeldSession`]), so a
///   datahub carrying the provider can list it. **Whether a PARTICULAR server will is a separate
///   question** the handshake answers (`FEATURE_VENUE_CATALOG`) and the response refines
///   (`NotServed`); this says only that the route EXISTS.
/// - `None` — no route exists for anybody: the venue is credentialed (a server will not spend the
///   operator's identity on a client's request) or has no bulk list at all. [`catalog_availability`]
///   carries which, and it is what a UI should render instead of a refresh control.
///
/// ⚠ **A credentialed venue is `None` rather than `Direct` even when the caller links its bridge**,
/// and that is deliberate rather than an oversight: this function answers "can a catalog be
/// REFRESHED from here", and for alpaca the honest answer depends on credentials this
/// function cannot see. A caller holding those credentials reaches the provider directly and does
/// not need routing advice; a caller that does not would render a control that silently returns an
/// empty list, which is the failure `docs/decisions/0062`'s decision 5 exists to prevent.
///
/// ⚠ **SOMETHING SEES THEM NOW, and this function is still not it.** That ⚠ used to end there, and
/// `docs/decisions/0066`'s decision 9 armed a locally-credentialed refresh: on the box that holds a
/// venue's keys, the operator lists it themselves with their own credentials
/// (`vike-backend catalog refresh <venue>`), writing the settings database's `operator_instrument_list`
/// row. **That does not change
/// this answer for any caller of this function** — the desktop links no venue bridge at all (a
/// catalog fetch is a venue REST call, and `crates/vike-desktop/Cargo.toml` names none), and the route the record REFUSES is the one where a server does the fetch. So
/// `None` still means "no route from HERE"; what changed is that a `None` is no longer the end of
/// the operator's options, which is why
/// `crates/vike-datahub-client/src/catalog.rs`'s `CatalogRefusal::NeedsCredentials` now names the
/// act. A caller rendering a row wants
/// `crate::catalog_answer` beside this, not a fourth `CatalogRoute` variant.
pub fn catalog_source_for(venue: &str, direct: &[&str]) -> Option<CatalogRoute> {
    if !catalog_availability(venue).is_served() {
        return None;
    }
    if direct.contains(&venue) {
        return Some(CatalogRoute::Direct);
    }
    Some(CatalogRoute::ServerBacked)
}

#[path = "availability_tests.rs"]
#[cfg(test)]
mod availability_tests;
