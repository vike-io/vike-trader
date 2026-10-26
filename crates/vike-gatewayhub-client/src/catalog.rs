//! `RemoteCatalog` — a `vike_catalog::CatalogProvider` answered by the gatewayhub: the venue catalog
//! lane's providers without the bridges that implement them.
//!
//! * **`asset_classes()` and `mode()` are synchronous and cheap**, so they cannot be per-call RPCs:
//!   [`RemoteCatalog::open`] asks `CatalogDescribe` ONCE and caches the answer for the life of the
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

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, SearchFilter};
use vike_hub_proto::auth::Scope;
use vike_model::{AssetClass, Instrument};

use crate::client::{FetchError, GatewayClient, GatewayLink, gateway_text, unexpected};
use crate::proto::{FEATURE_GW_CATALOG, FetchWhat, Request, Response, WireTab};

/// The venues the catalog lane serves, in the order the datahub's `real_catalog_table` lists them:
/// binance, bybit, okx, aster, hyperliquid, polymarket, dukascopy, fxcm. Deribit is absent on
/// purpose (a separate finding, decision-record-sized, that wave 1 must not touch) and so are the
/// credentialed venues (decision 0062's rule 3). ORDER IS OBSERVABLE: it is what the `NotServed`
/// refusal prints. The drift register's R10 test holds this equal to the daemon's registry and to the
/// public set of `vike_catalog::catalog_availability`.
pub const CATALOG_VENUES: [&str; 8] =
    ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "dukascopy", "fxcm"];

/// One venue's catalog provider, answered by the gatewayhub.
#[derive(Debug, Clone)]
pub struct RemoteCatalog {
    link: GatewayLink,
    venue: String,
    asset_classes: Vec<AssetClass>,
    mode: CatalogMode,
}

fn error(link: &GatewayLink, e: &dyn std::fmt::Display) -> CatalogError {
    CatalogError(gateway_text(link.addr(), e))
}

impl RemoteCatalog {
    /// Ask the gatewayhub for `venue`'s catalog traits and cache them. `Err` when the daemon cannot
    /// be reached, does not serve the lane, or refuses the venue — the daemon's text unchanged for the
    /// last.
    pub fn open(link: GatewayLink, venue: &str) -> Result<Self, CatalogError> {
        let mut client =
            GatewayClient::connect(&link, Scope::Read).map_err(|e| error(&link, &e))?;
        client.require_feature(FEATURE_GW_CATALOG).map_err(|e| error(&link, &e))?;
        match client
            .exchange(&Request::CatalogDescribe { venue: venue.to_string() })
            .map_err(|e| error(&link, &e))?
        {
            Response::CatalogDescribed { asset_classes, mode } => {
                Ok(Self { link, venue: venue.to_string(), asset_classes, mode: mode.mode() })
            }
            Response::Error(text) => Err(CatalogError(text)),
            other => Err(error(&link, &unexpected(client.addr(), "CatalogDescribed", &other))),
        }
    }

    /// One paged instrument fetch, reassembled.
    fn instruments(&self, what: &FetchWhat) -> Result<Vec<Instrument>, CatalogError> {
        let mut client =
            GatewayClient::connect(&self.link, Scope::Read).map_err(|e| error(&self.link, &e))?;
        let reply =
            client.fetch(what, false, None).map_err(|e| CatalogError(FetchError::into_text(e)))?;
        reply
            .assembler
            .finish_instruments(reply.pages, reply.rows)
            .map_err(|why| error(&self.link, &why))
    }
}

impl CatalogProvider for RemoteCatalog {
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
        self.instruments(&FetchWhat::CatalogList { venue: self.venue.clone() })
    }

    fn search_remote(
        &self,
        query: &str,
        filter: &SearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        self.instruments(&FetchWhat::CatalogSearch {
            venue: self.venue.clone(),
            query: query.to_string(),
            tab: filter.tab.map(WireTab::from_tab),
            filter_venue: filter.venue.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_catalog::{CatalogAvailability, catalog_availability};

    /// The roster is exactly the venues `vike_catalog::catalog_availability` calls PUBLIC bulk, minus
    /// the one finding wave 1 leaves alone (deribit is public and absent from the datahub's table).
    #[test]
    fn the_roster_is_the_public_set_without_deribit() {
        for &v in vike_model::VENUES {
            let public = catalog_availability(v) == CatalogAvailability::PublicBulk;
            let listed = CATALOG_VENUES.contains(&v);
            if v == "deribit" {
                assert!(public && !listed, "deribit is public and deliberately not served");
            } else {
                assert_eq!(
                    public, listed,
                    "`{v}`: the catalog roster and the availability table disagree"
                );
            }
        }
    }
}
