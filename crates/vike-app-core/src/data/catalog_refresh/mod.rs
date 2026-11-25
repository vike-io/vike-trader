//! **The explicit, per-venue instrument-catalog refresh, and the desktop's copy of the lists.**
//!
//! # Where the lists live (the owner's ruling of 2026-11-12)
//!
//! The DATAHUB keeps every venue's fetched list, one file per venue in `<project>/instruments` on the
//! server, refreshes one only when the operator asks (2026-11-16: no timer) and is their only writer
//! (`crates/vike-datahub/src/instrument_lists.rs`; `docs/decisions/0062`'s amendment). This module holds an
//! in-memory [`vike_catalog::CatalogCache`] of what the datahub answered (`Request::InstrumentLists`,
//! read when a dial appears) and asks it for a refresh on a press (`Request::RefreshInstrumentLists`).
//! It writes no file: the desktop's old `settings/state/catalog.json` is read and written by nothing,
//! and a leftover copy is ignored.
//!
//! # ⚠ What a refresh can reach — and the answer changed, because the route did
//!
//! A `CatalogProvider` lives in its venue's BRIDGE crate, and rulings 1 and 2 of the 2026-09-09
//! rename design took the venue bridges out of the desktop's dependency graph — the tombstone in
//! `crates/vike-desktop/Cargo.toml` names the six catalog edges that went, *"all of them for the
//! Symbol picker alone"*. **This module's own doc used to conclude from that "a binary can only
//! refresh the venues it LINKS", and that stopped being true** the day
//! `vike_datahub_client::proto::Request::VenueCatalog` shipped: an `Observe` verb by which a
//! process asks the backend's datahub to list a venue whose bridge it does not carry
//! (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`).
//!
//! So there are TWO routes and [`vike_catalog::catalog_source_for`] is the one decision site:
//!
//! * `Some(CatalogRoute::Direct)` — this binary links the provider. Fetch it here, ask no server.
//! * `Some(CatalogRoute::ServerBacked)` — this binary does not link it, and it is publicly
//!   enumerable, so the datahub can. [`crate::data::catalog_refresh::server_route`] is that leg.
//! * `None` — **no route exists for anybody**: the venue is credentialed (a server will not spend
//!   the operator's identity on a client's request) or it publishes no bulk list at all. The row
//!   renders the REASON and no control.
//!
//! ⚠ **That last arm is why [`DatahubCatalogRefreshAvailability`] grew from three variants to six.** It had
//! `NotInThisBuild` as the whole answer for thirteen venues, and that sentence — *"this venue's
//! catalog lives in its bridge crate, which this binary does not link"* — is now true only in the
//! narrow local-provider sense, misleading for the eight that route to a server, and the WRONG KIND
//! of fact for the five that route nowhere. [`vike_catalog::catalog_availability`] is what tells
//! those five apart, and the sentence each renders is the SERVER-side type's
//! (`vike_datahub_client::catalog::CatalogRefusal::describe`), so a credentialed venue reads the
//! same words whether it was refused here or at a daemon's door.
//!
//! [`CatalogRefreshVenueRow::source`] is where the decision lands. It was written by `rows()` and read by
//! NOBODY before this, and it was also wrong for five venues (an `is_some()` test on the local
//! provider called alpaca/oanda/ctrader/ig/ibkr `ServerBacked`, where the routing table answers
//! "no route for anybody").
//!
//! # The failure rule this module exists to enforce
//!
//! **A refresh that fails, times out or returns nothing must never replace the list it already
//! had.** The picker reads one universe; silently swapping a venue's symbols for an empty list
//! makes every one of that venue's instruments un-pickable, with nothing on screen to say why.
//! [`merge_refresh`] returns BEFORE touching the cache on both of those paths, and
//! `crates/vike-app-core/tests/catalog_refresh.rs` mutation-proves it.
//!
//! ⚠ **A REFUSAL is a third thing and must not be folded into either.** `Failed` calls a venue
//! property an error; `Empty` renders *"the venue answered with nothing"* — the exact lie 0062's
//! decision 5 exists to prevent, and a lie for five roster venues rather than two, since
//! alpaca/oanda/ctrader/ig all answer `Ok(vec![])` when credentials are absent. Hence
//! [`DatahubCatalogRefreshOutcome::Refused`].
//!
//! # The rate budget, and why it is the same number on both routes
//!
//! One press is a venue REST call, and the heavier providers are bounded crawls rather than a
//! single GET (Polymarket pages the Gamma browse; OKX fetches per `instType` and then one listing
//! per option family). So a press is refused while one is in flight, and again inside
//! [`REFRESH_COOLDOWN_MS`] of the last completed one — which makes a held-down button a no-op
//! instead of a crawl repeated as fast as the network answers.
//!
//! ⚠ On the `Direct` route that call leaves THIS box; on the `ServerBacked` route it leaves the
//! DATAHUB's, against its budget. ⚠ That used to read "which is the whole cost argument behind
//! `VIKE_DATAHUB_VENUE_CATALOG` being off by default", and `docs/decisions/0066` retired both
//! halves of that clause: the lane is ON by default now, and its decision 2 found that the switch
//! never bounded the cost at all — the per-venue buckets, the TTL and each bridge's own pager do,
//! and none of them moved. [`REFRESH_COOLDOWN_MS`] IS that lane's refill
//! ([`CATALOG_VENUE_REFILL`], 60 s, in milliseconds), derived rather than copied — so a human
//! pressing as fast as this button allows tracks the refill rate and can never drain its two-token
//! burst. The one wire path that spends a token at all is a provider FAILURE (every refusal is decided before the
//! bucket); [`crate::data::catalog_refresh::server_route`]'s module doc carries the whole gate order.
//!
//! ⚠ **[`DatahubCatalogRefresh::spawn_initial`] bypasses [`refresh_block`] and stays LOCAL-ONLY.** It is
//! the one path that fetches without a press, and routing its cold venues to a server would be a
//! startup that opens eight concurrent authenticated dials and spends eight of somebody else's
//! tokens — a poll wearing a first run. A `ServerBacked` venue is fetched only when a human asks.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use vike_catalog::VenueInstrumentList;
use vike_catalog::{
    Catalog, CatalogAnswer, CatalogAvailability, CatalogCache, CatalogCacheVenueStamp, CatalogMode,
    CatalogProvider, CatalogRoute, InstrumentList, InstrumentLists, catalog_answer,
    catalog_availability, catalog_source_for, merge_sources,
};
use vike_datahub_client::catalog::{CATALOG_VENUE_REFILL, CatalogRefusal};
use vike_model::Instrument;

use server_route::refresh_kept_list;
use vike_datahub_client::catalog_fetch::CatalogFetchReport;
use vike_datahub_client::dial::DatahubDial;

mod engine;
mod model;
pub mod server_route;

pub use engine::DatahubCatalogRefresh;
pub(crate) use model::refreshed_label;
pub use model::{
    CatalogRefreshBlock, CatalogRefreshVenueRow, CatalogRefreshVenueState,
    DatahubCatalogRefreshAvailability, DatahubCatalogRefreshOutcome, merge_refresh, refresh_block,
};

/// How long after a COMPLETED refresh the same venue refuses another one.
///
/// A minute, and the argument is the budget rather than the UI: a press spends a venue's public
/// API allowance — from the box that also signs orders on the `Direct` route, and from the
/// DATAHUB's box on the `ServerBacked` one — and the expensive providers are bounded crawls. A
/// user who presses twice because they could not tell whether the first press worked is the
/// failure this pairs with the stamp: the stamp answers the question, the cooldown makes the second
/// press harmless either way. Sixty seconds is short enough that a genuine re-check (a venue just
/// listed something) is never blocked for long.
///
/// ⚠ It IS [`CATALOG_VENUE_REFILL`], the datahub bucket's refill, in this clock's milliseconds:
/// derived, so the two cannot drift, and that is load-bearing rather than decorative: it is what makes
/// a human retrying a failed `ServerBacked` fetch track the server's refill rate instead of draining
/// its burst. See the module doc.
pub const REFRESH_COOLDOWN_MS: i64 = CATALOG_VENUE_REFILL.as_millis() as i64;

/// What the row says for a venue whose catalog is searched live, per query.
pub(crate) const QUERY_BACKED_NOTE: &str =
    "searched live per query — this venue publishes no bulk list, so there is nothing to refresh";

/// What the row says for a venue this binary links no provider for AND
/// [`vike_catalog::catalog_source_for`] can route nowhere — i.e. one it cannot classify at all.
///
/// ⚠ **This used to be the note for thirteen of the fourteen roster venues and is now the
/// fail-closed arm for a venue not on the roster.** Every classified venue reaches one of the five
/// other [`DatahubCatalogRefreshAvailability`] arms.
pub(crate) const NOT_LINKED_NOTE: &str =
    "this venue's catalog cannot be listed here: no source for it is known";

/// What the row says for a venue that routes to the backend's datahub.
pub(crate) const SERVER_BACKED_NOTE: &str = "listed by the connected backend's datahub";

/// **What THIS BOX adds to the shared credential refusal** — whether the keys that refusal names
/// are here, and if not, what would put them here.
///
/// `docs/decisions/0066`'s decision 9 says the refusal must stop reading *"this cannot be
/// refreshed"* and start naming what would arm it. The ACT is named by the shared server-side
/// sentence, which a daemon also prints; this is the half that is a fact about the local box and is
/// therefore allowed to name a local SCREEN.
///
/// ⚠ **Never an empty list and never a bare disabled control**, which is the other half of that
/// ruling: a venue with no keys gets a sentence, a venue with keys gets a different sentence, and
/// neither gets a button — because this binary links none of those three bridges and a control that
/// silently returned `Ok(vec![])` is the failure 0062's decision 5 exists to prevent.
#[must_use]
pub(crate) fn credential_note(venue: &str, own_keys: bool) -> String {
    if own_keys {
        format!(
            "Your `{}` credentials ARE saved on this box, so that command will list it now.",
            venue.to_uppercase()
        )
    } else {
        format!(
            "No `{}` credentials are saved on this box yet — enter, save and verify them in \
             Connections first, and that command arms.",
            venue.to_uppercase()
        )
    }
}

#[path = "catalog_refresh_tests.rs"]
#[cfg(test)]
mod catalog_refresh_tests;
