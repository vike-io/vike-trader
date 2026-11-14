//! `CatalogProviderGateway` — a `vike_catalog::CatalogProvider` answered by the gatewayhub: the venue catalog
//! lane's providers without the bridges that implement them.
//!
//! * **`asset_classes()` and `mode()` are synchronous and cheap**, so they cannot be per-call RPCs:
//!   [`CatalogProviderGateway::open`] asks `CatalogDescribe` ONCE and caches the answer for the life of the
//!   value. The open is the only call that can fail before a listing; build one per refresh, as the
//!   datahub's provider table does.
//! * **`list_instruments()` and `search_remote()` go over the wire as paged fetches**
//!   (`FetchWhat::CatalogList` / `CatalogSearch`), so a listing larger than the frame ceiling
//!   reassembles into the same `Vec` the in-process provider returns. The CAP
//!   (`CATALOG_MAX_INSTRUMENTS`) and the TTL memo are the DATAHUB's and stay there: the daemon sends
//!   the provider's full answer and the caller truncates, exactly as `list_via` does today.
//! * **A transport failure is an `Err`, never an empty listing** (drift register R12): an empty list
//!   is a legitimate venue fact the picker would store. The text is `gatewayhub <addr>: ...`; a
//!   daemon-side `CatalogError` keeps its inner text unchanged.

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, CatalogSearchFilter};
use vike_hub_protocol::auth::Scope;
use vike_model::{AssetClass, Instrument};

use crate::client::{FetchError, GatewayClient, GatewayLink, gateway_text, unexpected};
use crate::proto::{FEATURE_GATEWAY_CATALOG, FetchWhat, Request, Response};

/// The venues the catalog lane serves, in the order the datahub's `real_catalog_table` lists them:
/// binance, bybit, okx, aster, hyperliquid, polymarket, dukascopy, fxcm, and the two held lists, oanda and
/// ctrader. Deribit is absent on purpose (a separate finding, decision-record-sized, that wave 1 must not
/// touch) and so is the credentialed venue (decision 0062's rule 3). ORDER IS OBSERVABLE: it is what the
/// `NotServed` refusal prints. The daemon's registry names this const as its catalog row, and the drift
/// register's R10 test holds it equal to the served set of `vike_catalog::catalog_availability`.
pub const CATALOG_VENUES: [&str; 10] = [
    "binance",
    "bybit",
    "okx",
    "aster",
    "hyperliquid",
    "polymarket",
    "dukascopy",
    "fxcm",
    "oanda",
    "ctrader",
];

/// One venue's catalog provider, answered by the gatewayhub.
#[derive(Debug, Clone)]
pub struct CatalogProviderGateway {
    link: GatewayLink,
    venue: String,
    asset_classes: Vec<AssetClass>,
    mode: CatalogMode,
}

fn error(link: &GatewayLink, e: &dyn std::fmt::Display) -> CatalogError {
    CatalogError(gateway_text(link.addr(), e))
}

impl CatalogProviderGateway {
    /// Ask the gatewayhub for `venue`'s catalog traits and cache them. `Err` when the daemon cannot
    /// be reached, does not serve the lane, or refuses the venue — the daemon's text unchanged for the
    /// last.
    pub fn open(link: GatewayLink, venue: &str) -> Result<Self, CatalogError> {
        let mut client =
            GatewayClient::connect(&link, Scope::Read).map_err(|e| error(&link, &e))?;
        client.require_feature(FEATURE_GATEWAY_CATALOG).map_err(|e| error(&link, &e))?;
        match client
            .exchange(&Request::CatalogDescribe { venue: venue.to_string() })
            .map_err(|e| error(&link, &e))?
        {
            Response::CatalogDescribed { asset_classes, mode } => {
                Ok(Self { link, venue: venue.to_string(), asset_classes, mode })
            }
            Response::Error(text) => Err(CatalogError(text)),
            other => Err(error(&link, &unexpected(client.addr(), "CatalogDescribed", &other))),
        }
    }

    /// One paged instrument fetch, reassembled; the daemon's failure keeps its variant.
    fn fetch_instruments(&self, what: &FetchWhat) -> Result<Vec<Instrument>, FetchError> {
        let link_error =
            |e: &dyn std::fmt::Display| FetchError::Link(gateway_text(self.link.addr(), e));
        let mut client =
            GatewayClient::connect(&self.link, Scope::Read).map_err(|e| link_error(&e))?;
        let reply = client.fetch(what, false, None)?;
        reply.assembler.finish_instruments(reply.pages, reply.rows).map_err(|why| link_error(&why))
    }

    /// One paged instrument fetch, its failure as the provider seam's text.
    fn instruments(&self, what: &FetchWhat) -> Result<Vec<Instrument>, CatalogError> {
        self.fetch_instruments(what).map_err(|e| CatalogError(e.into_text()))
    }

    /// **[`CatalogProvider::list_instruments`] with the daemon's answer kept TYPED**: a routine
    /// refusal (`FetchError::Failed(SourceError::Refused)`: the daemon asked no venue, as for a venue
    /// listed from a session it does not hold now, `vike_catalog::CatalogProvider::no_session`) stays
    /// apart from a failure, which the trait's `CatalogError` text cannot keep.
    pub fn list_answer(&self) -> Result<Vec<Instrument>, FetchError> {
        self.fetch_instruments(&FetchWhat::CatalogList { venue: self.venue.clone() })
    }
}

/// **The operator's lookup of ONE instrument on `venue`** — `FetchCredentialed { CatalogSearch }` on a Write
/// connection: the gatewayhub asks the venue over a session it ALREADY holds (no login of its own) and answers the
/// row it chose FIRST, then the forms of the instrument it passed over (`crates/vike-gatewayhub/src/rest/lane.rs`'s
/// `operator_lookup`; only ig answers one). The datahub's `OperatorInstruments` verb is the caller. A credentialed
/// verb because it spends the operator's own venue session, never a Read client's (decision 0062's rule 3).
///
/// # Errors
/// The daemon could not be reached or does not serve the credentialed lane, or its answer in words (an older daemon
/// refuses a credentialed search as "klines only").
pub fn operator_lookup(
    link: &GatewayLink,
    venue: &str,
    symbol: &str,
) -> Result<Vec<Instrument>, String> {
    let fail = |e: &dyn std::fmt::Display| gateway_text(link.addr(), e);
    let mut client = GatewayClient::connect(link, Scope::Write).map_err(|e| fail(&e))?;
    let what = FetchWhat::CatalogSearch {
        venue: venue.to_string(),
        query: symbol.to_string(),
        tab: None,
        filter_venue: None,
    };
    let reply = client.fetch(&what, true, None).map_err(FetchError::into_text)?;
    reply.assembler.finish_instruments(reply.pages, reply.rows).map_err(|why| fail(&why))
}

impl CatalogProvider for CatalogProviderGateway {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn asset_classes(&self) -> &[AssetClass] {
        &self.asset_classes
    }

    fn mode(&self) -> CatalogMode {
        self.mode
    }

    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        self.list_answer().map_err(|e| CatalogError(e.into_text()))
    }

    fn search_remote(
        &self,
        query: &str,
        filter: &CatalogSearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        self.instruments(&FetchWhat::CatalogSearch {
            venue: self.venue.clone(),
            query: query.to_string(),
            tab: filter.tab,
            filter_venue: filter.venue.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_catalog::catalog_availability;

    /// The roster is exactly the venues `vike_catalog::catalog_availability` lets a server hold (a
    /// PUBLIC bulk list, or one read from a held session: fxcm), minus the one finding wave 1 leaves
    /// alone (deribit is public and absent from the datahub's table).
    #[test]
    fn the_roster_is_the_served_set_without_deribit() {
        for &v in vike_model::VENUES {
            let served = catalog_availability(v).is_served();
            let listed = CATALOG_VENUES.contains(&v);
            if v == "deribit" {
                assert!(served && !listed, "deribit is public and deliberately not served");
            } else {
                assert_eq!(
                    served, listed,
                    "`{v}`: the catalog roster and the availability table disagree"
                );
            }
        }
    }
}
