//! The `Tab` grouping the Symbol picker filters by (TradingView-style tabs), and the one function
//! that maps a [`vike_model::AssetClass`] onto one. Ports no Python file — vike-native.
//!
//! # ⚠ This is a GUI judgement, which is why it did not move down with the taxonomy
//!
//! [`AssetClass`] itself lives in `vike-model` (`crates/vike-model/src/asset_class.rs`), one layer
//! below this crate, because the hist store must be able to record whether a series is spot or
//! perp and `vike-data` is this crate's own layer. The tab grouping stayed: *which drawer of the
//! picker a class belongs in* is a claim about the Symbol picker's information architecture, not
//! about the instrument, and a taxonomy that the store and the bridges both name should not carry
//! a window's layout opinion down with it.
//!
//! ⚠ [`tab_for`] is a FREE FUNCTION rather than a method, and not by preference: after the move an
//! inherent impl on [`AssetClass`] is not available to this crate at all. That matches the style
//! every other per-class/per-venue table here already uses — [`crate::catalog_availability`],
//! [`crate::session_calendar_for`], [`crate::addressing_for`].
//!
//! ⚠ It is deliberately NOT macro-generated from the variant list, which is the property the
//! taxonomy's own module doc points back at here for. A tab is a JUDGEMENT, so the exhaustive
//! `match` below is what forces a twelfth variant's author to decide where it belongs instead of
//! inheriting a rendering of its name.

use vike_model::AssetClass;

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

/// The picker tab this class lives under.
#[must_use]
pub fn tab_for(class: AssetClass) -> Tab {
    match class {
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
        tab_for(class) == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stocks_tab_groups_equity_and_etf() {
        assert_eq!(tab_for(AssetClass::Equity), Tab::Stocks);
        assert_eq!(tab_for(AssetClass::Etf), Tab::Stocks);
        assert!(Tab::Stocks.matches(AssetClass::Equity));
        assert!(!Tab::Stocks.matches(AssetClass::CryptoSpot));
    }

    #[test]
    fn perps_tab_groups_perp_and_crypto_future() {
        assert_eq!(tab_for(AssetClass::CryptoPerp), Tab::Perps);
        assert_eq!(tab_for(AssetClass::CryptoFuture), Tab::Perps);
    }

    #[test]
    fn forex_tab_groups_fx_and_cfd() {
        assert_eq!(tab_for(AssetClass::Fx), Tab::Forex);
        assert_eq!(tab_for(AssetClass::Cfd), Tab::Forex);
    }

    /// Every variant of the taxonomy answers — the `match` in [`tab_for`] is exhaustive, so this
    /// cannot fail at run time, but it is what makes the ROSTER sweep explicit: a twelfth variant
    /// stops this crate compiling until its author picks a drawer.
    #[test]
    fn every_class_names_a_tab() {
        for class in AssetClass::ALL {
            let _ = tab_for(*class).label();
        }
    }
}
