//! The asset-class taxonomy every `Instrument` is tagged with, plus the `Tab` grouping the
//! Symbol picker filters by (TradingView-style tabs). Ports no Python file — vike-native.

use serde::{Deserialize, Serialize};

/// One asset class per instrument. Drives picker tabs, symbology expectations, and market-hours.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub enum AssetClass {
    Equity,
    Etf,
    CryptoSpot,
    CryptoPerp,
    CryptoFuture,
    Option,
    Fx,
    Future,
    Index,
    PredictionMarket,
    Cfd,
}

/// The picker's asset-class tabs. `All` is represented as `Option::None` in a filter, not here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Stocks,
    Forex,
    Crypto,
    Perps,
    Options,
    Futures,
    Indices,
    Prediction,
}

impl AssetClass {
    /// The picker tab this class lives under.
    pub fn tab(self) -> Tab {
        match self {
            AssetClass::Equity | AssetClass::Etf => Tab::Stocks,
            AssetClass::Fx | AssetClass::Cfd => Tab::Forex,
            AssetClass::CryptoSpot => Tab::Crypto,
            AssetClass::CryptoPerp | AssetClass::CryptoFuture => Tab::Perps,
            AssetClass::Option => Tab::Options,
            AssetClass::Future => Tab::Futures,
            AssetClass::Index => Tab::Indices,
            AssetClass::PredictionMarket => Tab::Prediction,
        }
    }
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Stocks => "Stocks",
            Tab::Forex => "Forex",
            Tab::Crypto => "Crypto",
            Tab::Perps => "Perps",
            Tab::Options => "Options",
            Tab::Futures => "Futures",
            Tab::Indices => "Indices",
            Tab::Prediction => "Prediction",
        }
    }

    /// True if `class` belongs under this tab.
    pub fn matches(self, class: AssetClass) -> bool {
        class.tab() == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stocks_tab_groups_equity_and_etf() {
        assert_eq!(AssetClass::Equity.tab(), Tab::Stocks);
        assert_eq!(AssetClass::Etf.tab(), Tab::Stocks);
        assert!(Tab::Stocks.matches(AssetClass::Equity));
        assert!(!Tab::Stocks.matches(AssetClass::CryptoSpot));
    }

    #[test]
    fn perps_tab_groups_perp_and_crypto_future() {
        assert_eq!(AssetClass::CryptoPerp.tab(), Tab::Perps);
        assert_eq!(AssetClass::CryptoFuture.tab(), Tab::Perps);
    }

    #[test]
    fn forex_tab_groups_fx_and_cfd() {
        assert_eq!(AssetClass::Fx.tab(), Tab::Forex);
        assert_eq!(AssetClass::Cfd.tab(), Tab::Forex);
    }
}
