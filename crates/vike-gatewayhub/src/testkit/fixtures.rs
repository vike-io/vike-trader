//! Small builders the lib's own unit suites and this crate's integration groups both need (`crate::testkit`'s
//! module doc): each used to be written once per side of the crate boundary, or once per group.

use std::sync::{Arc, Mutex, PoisonError};

use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, CatalogSearchFilter, PickerTab};
use vike_model::{AssetClass, Contract, Instrument};

use crate::rest::RestRosters;

/// The searches a [`ScriptedCatalog`] was asked, `(query, tab, venue filter)` in order.
pub type CatalogSearches = Arc<Mutex<Vec<(String, Option<PickerTab>, Option<String>)>>>;

/// A scripted catalog provider (`QueryBacked`): its venue and asset classes, what it lists and answers
/// every search with, and every search it was asked, `(query, tab, venue filter)` in order.
pub struct ScriptedCatalog {
    /// The venue it is the provider of.
    pub venue: String,
    /// The asset classes it states.
    pub classes: &'static [AssetClass],
    /// What it lists, and what every search answers.
    pub listing: Result<Vec<Instrument>, CatalogError>,
    /// The searches it was asked, appended.
    pub searched: CatalogSearches,
}

impl CatalogProvider for ScriptedCatalog {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn asset_classes(&self) -> &[AssetClass] {
        self.classes
    }

    fn mode(&self) -> CatalogMode {
        CatalogMode::QueryBacked
    }

    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        self.listing.clone()
    }

    fn search_remote(
        &self,
        query: &str,
        filter: &CatalogSearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        self.searched.lock().unwrap_or_else(PoisonError::into_inner).push((
            query.to_string(),
            filter.tab,
            filter.venue.clone(),
        ));
        self.listing.clone()
    }
}

/// The full wave-1 facts: klines (binance, bybit), funding (binance), ticks (dukascopy), catalog
/// (binance, dukascopy), credentialed klines (oanda), options, a polymarket family and presence.
pub fn full_facts() -> RestRosters {
    RestRosters {
        klines: vec!["binance".into(), "bybit".into()],
        funding: vec!["binance".into()],
        ticks: vec!["dukascopy".into()],
        catalog: vec!["binance".into(), "dukascopy".into()],
        credentialed: vec!["oanda".into()],
        options: true,
        family: vec!["polymarket".into()],
        presence: true,
    }
}

/// `n` spot instruments of `venue`, `SYM0USDT` up.
pub fn instruments(venue: &str, n: usize) -> Vec<Instrument> {
    (0..n)
        .map(|i| {
            Instrument::new(
                venue,
                format!("SYM{i}USDT"),
                format!("SYM{i}"),
                "USDT",
                Contract::CryptoSpot,
            )
        })
        .collect()
}

/// An active two-outcome (`Up`/`Down`) Gamma market `slug` over `tokens`.
#[cfg(feature = "data")]
pub fn market(slug: &str, tokens: &[&str]) -> vike_polymarket::catalog::gamma::GammaMarket {
    vike_polymarket::catalog::gamma::GammaMarket {
        slug: slug.into(),
        condition_id: format!("0x{slug}"),
        active: true,
        tick_size: 0.01,
        outcomes: vec!["Up".into(), "Down".into()],
        token_ids: tokens.iter().map(|s| (*s).to_string()).collect(),
        ..Default::default()
    }
}

/// A fresh open of the default `binance` account at `tier` by `client_id`, with a key id and no resume.
#[cfg(feature = "trade")]
pub fn open_request(
    client_id: &str,
    tier: vike_model::AccountTier,
) -> crate::trade::session::TradeLeaseOpenRequest {
    crate::trade::session::TradeLeaseOpenRequest {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        account: None,
        tier,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: std::sync::Arc::new(|| {}),
    }
}

/// An ibkr partial fill of `qty` at `px` on `symbol`, for order `order_client_id`, trade `trade_id`.
#[cfg(feature = "trade")]
pub fn partial_fill(
    symbol: &str,
    px: f64,
    order_client_id: &str,
    trade_id: &str,
    qty: f64,
) -> vike_model::events::Event {
    use vike_model::events::{Event, FillEvent, OrderPartiallyFilled, TradeId};
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        order_client_id: order_client_id.to_string(),
        fill: FillEvent {
            trade_id: TradeId::new(trade_id).expect("a trade id"),
            order_client_id: order_client_id.to_string(),
            venue: "ibkr".into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: qty,
            last_price: px,
            commission: 0.0,
            commission_asset: "USD".into(),
            liquidity_side: Default::default(),
            timestamp: 0,
            mark_price: None,
            position_side: Default::default(),
        },
        timestamp: 0,
    })
}
