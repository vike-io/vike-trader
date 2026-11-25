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
        market_rules: vec!["polymarket".into()],
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

/// **Hold ONE kept bybit list in this test process** (`vike_catalog::hold_kept_list`), what the bybit bridge reads a
/// USER name's book and venue name off (decision 0290): the pair `DOGEUSDT`, the linear perpetuals `DOGEUSDT.P`,
/// `SOLUSDT.P`, `ETHUSDT.P`, `BTCUSDT.P` (the arming golden's, `crate::testkit::arming_fixtures`' `start_symbol`), the
/// inverse `BTCUSD.P`, the dated linear `ETHUSDT-27NOV37` (`ETHUSDT_371127`),
/// `ETHUSDT-01NOV26` (`ETHUSDT_261101`), `ETHUSDT-01JAN20` (`ETHUSDT_200101`) and the dated inverse `BTCUSDZ37`
/// (`BTCUSD_371225`). ⚠ The delivery guard reads a dated contract's day off its user name against the real clock
/// (`crates/vike-venues/src/venue_delivery_guard.rs`'s `delivery_verdict`), so a row a test admits delivers in 2037 and
/// one it refuses has delivered already: no fixture turns red on a calendar day. Every test of this crate that needs a
/// bybit list holds THIS one (the list is process-wide and the
/// tests run as threads), so a second call changes nothing. ⚠ It carries no `ETHUSDT` or `SOLUSDT` pair on purpose:
/// the posted-leverage suites name those bare and expect a figure, which a held pair would turn into a spot answer.
pub fn hold_bybit_kept_rows() {
    use vike_model::{
        ContractFuture, ContractFutureDelivery, ContractFutureFamily, ContractPerp, Margining,
    };
    let perp = |key: &str, base: &str, quote: &str, margining: Margining, settle: &str| {
        let contract = Contract::Perp(ContractPerp {
            margining,
            settle: settle.into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        Instrument::new("bybit", format!("{key}.P"), base, quote, contract)
    };
    // A dated contract delivers at 08:00 UTC of its day (`expiry_ms`).
    let dated = |key: &str, base: &str, quote: &str, margining: Margining, expiry_ms: i64| {
        let settle = if margining == Margining::Inverse { base } else { quote };
        let contract = Contract::Future(ContractFuture {
            margining,
            settle: settle.into(),
            value_currency: None,
            expiry_ms,
            tenor: None,
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        });
        Instrument::new("bybit", key, base, quote, contract)
    };
    let spot =
        |key: &str, base: &str| Instrument::new("bybit", key, base, "USDT", Contract::CryptoSpot);
    vike_catalog::hold_kept_list(
        "bybit",
        vec![
            spot("DOGEUSDT", "DOGE"),
            perp("DOGEUSDT", "DOGE", "USDT", Margining::Linear, "USDT"),
            perp("SOLUSDT", "SOL", "USDT", Margining::Linear, "USDT"),
            perp("ETHUSDT", "ETH", "USDT", Margining::Linear, "USDT"),
            perp("BTCUSDT", "BTC", "USDT", Margining::Linear, "USDT"),
            perp("BTCUSD", "BTC", "USD", Margining::Inverse, "BTC"),
            dated("ETHUSDT-27NOV37", "ETH", "USDT", Margining::Linear, 2_142_921_600_000),
            dated("ETHUSDT-01NOV26", "ETH", "USDT", Margining::Linear, 1_793_520_000_000),
            dated("ETHUSDT-01JAN20", "ETH", "USDT", Margining::Linear, 1_577_865_600_000),
            dated("BTCUSDZ37", "BTC", "USD", Margining::Inverse, 2_145_340_800_000),
        ],
    );
}

/// **Hold ONE kept hyperliquid list in this test process** (`vike_catalog::hold_kept_list`), what the hyperliquid
/// facts spec reads a USER name's listing name off (decision 0290): the USDC perpetuals `BTC`, `XRP`, `LINK`, `kPEPE`,
/// `RLB`, `OLD`, `NOTICK`, `NOPE`, `FUT`, `WEIRD` (`BTCUSDC.P` … `KPEPEUSDC.P` …), the spot pairs `HYPE/USDC`
/// (`HYPEUSDC`) and `ABC/USDH` (`ABCUSDH`), and the builder-dex coins `test:ABC`, `test:ISO`, `test:GONE`
/// (`TEST:ABCUSDC.P` …). `FUT` and `WEIRD` are kept as perpetuals so their names reach a LISTING row of another class
/// (a future, an index), which the facts then refuse. Every test of this crate that needs a hyperliquid list holds
/// THIS one (the list is process-wide and the tests run as threads), so a second call changes nothing.
pub fn hold_hyperliquid_kept_rows() {
    use vike_model::{ContractPerp, Margining};
    let perp = |raw: &str, base: &str| {
        let contract = Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDC".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        Instrument::new("hyperliquid", raw, base, "USDC", contract)
    };
    let spot = |raw: &str, base: &str, quote: &str| {
        Instrument::new("hyperliquid", raw, base, quote, Contract::CryptoSpot)
    };
    let mut rows: Vec<Instrument> =
        ["BTC", "XRP", "LINK", "kPEPE", "RLB", "OLD", "NOTICK", "NOPE", "FUT", "WEIRD"]
            .into_iter()
            .map(|coin| perp(coin, coin))
            .collect();
    rows.extend([
        spot("HYPE/USDC", "HYPE", "USDC"),
        spot("ABC/USDH", "ABC", "USDH"),
        perp("test:ABC", "ABC"),
        perp("test:ISO", "ISO"),
        perp("test:GONE", "GONE"),
    ]);
    vike_catalog::hold_kept_list("hyperliquid", rows);
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
