//! `server_route` — **the refresh button's SECOND route: ask the backend's datahub for a venue
//! this binary links no bridge for**, and when it will not answer, say why in a sentence rather
//! than in an empty list.
//!
//! # Where this sits
//!
//! [`catalog_refresh`](crate::data::catalog_refresh) owns the cache, the per-venue budget and the fold;
//! it had exactly one fetch — a `vike_catalog::CatalogProvider` this binary LINKS — and rulings 1
//! and 2 of the 2026-09-09 rename design left the desktop linking one venue bridge. So that
//! module's own ⚠ ("less than you expect") was the whole truth: thirteen of the fourteen roster
//! rows said *not in this build* and offered nothing.
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` built the
//! other side — `vike_datahub_client::proto::Request::VenueCatalog`, an Observe verb — and
//! `vike_catalog::catalog_source_for` is the routing decision it was declared for. The client of
//! that verb (one blocking call, five refusal shapes, one sentence each, and the module doc on why
//! the refusals are the point and why nothing here retries, polls or batches) is
//! `vike_datahub_client::catalog_fetch`, the one listing the Options poll's index asks through too;
//! this module keeps the refresh's own verbs: the kept-list refresh, the kept-lists read and the
//! LOCAL refusal's sentence.
//!
//! # Why this may use the desktop's OWN key
//!
//! The identical argument [`crate::data::stored::chart_seed`] makes, and it is the reason that module rather
//! than [`crate::data::backfill`] is the template here. `Request::VenueCatalog` is
//! `VerbScope::Read` (0062's decision 1, asserted in
//! `crates/vike-datahub/tests/default_build/auth_roundtrip.rs`), so the datahub OBSERVE pair the desktop already
//! resolves for its store reads reaches it. `Request::Backfill` is `VerbScope::Write`, which is
//! why `run_datahub_backfill` dials unauthenticated and cannot work against a keyed server at all.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::catalog::{CatalogOutcome, CatalogRefusal};
use vike_datahub_client::catalog_fetch::{
    CATALOG_REPLY_TIMEOUT, CatalogFetchReport, describe_outcome,
};
use vike_datahub_client::dial::DatahubDial;
use vike_hub_protocol::auth::Scope;

/// **Ask the datahub to refresh one venue's KEPT list now** — `Request::RefreshInstrumentLists`, the refresh button's
/// route since the owner's ruling of 2026-11-12: the datahub keeps every venue's list (`<project>/instruments`) and is
/// its only writer, so a press asks it to fetch AND keep. A Control verb, dialled under the WRITE scope with this
/// desktop's device key (decision 0288); a keyed datahub whose access list does not admit the device at Write
/// refuses it at the handshake, which lands as [`CatalogFetchReport::ConnectFailed`] naming it. The answer maps onto the same report `vike_datahub_client::catalog_fetch::fetch_venue_catalog`
/// gives, so a row reads it the same way.
pub(crate) fn refresh_kept_list(dial: Option<&DatahubDial>, venue: &str) -> CatalogFetchReport {
    let Some(dial) = dial else { return CatalogFetchReport::NoBackend };
    let mut client =
        match DatahubClient::connect_with_keys(&dial.address, dial.keys.as_ref(), Scope::Write) {
            Ok(mut c) => {
                c.set_reply_timeout(Some(CATALOG_REPLY_TIMEOUT));
                c
            }
            Err(e) => {
                return CatalogFetchReport::ConnectFailed {
                    addr: dial.address.clone(),
                    error: e.to_string(),
                };
            }
        };
    if !client.serves_instrument_lists() {
        return CatalogFetchReport::ServerUnsupported {
            addr: dial.address.clone(),
            features: client.features().to_vec(),
        };
    }
    let failed = |error: String| CatalogFetchReport::Failed { addr: dial.address.clone(), error };
    match client.refresh_instrument_lists(Some(venue)) {
        Ok(rows) => match rows.into_iter().find(|r| r.venue == venue).map(|r| r.outcome) {
            Some(Ok(CatalogOutcome::Listed { instruments, truncated, cached })) => {
                CatalogFetchReport::Listed { instruments, truncated, cached }
            }
            Some(Ok(other)) => CatalogFetchReport::Refused { why: describe_outcome(venue, &other) },
            Some(Err(error)) => failed(error),
            None => failed(format!("the datahub answered no row for `{venue}`")),
        },
        Err(error) => failed(error),
    }
}

/// **Every list the datahub keeps, and the operator's own** — `Request::InstrumentLists`, the desktop's startup read
/// of the symbol universe (it replaced the desktop's own `catalog.json`). An Observe verb.
pub(crate) fn fetch_kept_lists(
    dial: &DatahubDial,
) -> Result<vike_datahub_client::catalog::InstrumentListsReply, String> {
    let mut client =
        DatahubClient::connect_with_keys(&dial.address, dial.keys.as_ref(), Scope::Read)
            .map_err(|e| format!("the datahub at {} could not be reached: {e}", dial.address))?;
    client.set_reply_timeout(Some(CATALOG_REPLY_TIMEOUT));
    client.instrument_lists()
}

/// A LOCAL refusal's sentence, for a venue [`vike_catalog::catalog_source_for`] routes nowhere.
///
/// ⚠ **No frame is sent for these and none ever should be**: the routing table answers `None`
/// because no server may list the venue (credentialed — 0062's decision 3) or because the venue
/// publishes no list at all. The sentence is still the SERVER-side type's —
/// [`CatalogRefusal::describe`] — so a credentialed venue reads the same words whether it was
/// refused here or at a daemon's door.
#[must_use]
pub(crate) fn local_refusal_line(venue: &str, refusal: &CatalogRefusal) -> String {
    refusal.describe(venue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_refusal_reads_the_same_as_the_servers_would() {
        let refusal = CatalogRefusal::NeedsCredentials;
        assert_eq!(local_refusal_line("oanda", &refusal), refusal.describe("oanda"));
    }
}
