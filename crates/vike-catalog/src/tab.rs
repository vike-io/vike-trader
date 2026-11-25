//! The `PickerTab` grouping the Symbol picker filters by (TradingView-style tabs), and the one function
//! that maps a [`vike_model::AssetClass`] onto one. Ports no Python file — vike-native.
//!
//! # ⚠ This is a GUI judgement, which is why it did not move down with the taxonomy
//!
//! [`AssetClass`] itself lives in `vike-model` (`crates/vike-model/src/instrument/asset_class.rs`), one layer
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
//! `match` below is what forces a new variant's author to decide where it belongs instead of
//! inheriting a rendering of its name.

use serde::{Deserialize, Serialize};
use vike_model::AssetClass;

/// The picker's asset-class tabs. `All` is represented as `Option::None` in a filter, not here.
///
/// ⚠ **A variant name is a wire tag**: the gatewayhub's `CatalogSearch` carries this enum as it is
/// (`crates/vike-gatewayhub-client/src/proto.rs`'s `FetchWhat`), so a renamed variant is a protocol
/// break, and a new one ships with a captured frame in `fixtures/gatewayhub_wire/`
/// (`crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `CATALOG_TAB_SOURCE`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PickerTab {
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
pub fn tab_for(class: AssetClass) -> PickerTab {
    match class {
        AssetClass::Equity | AssetClass::Etf => PickerTab::Stocks,
        // Exchange-listed securities beside the shares: no drawer of their own until a venue lists
        // enough of them to want one.
        AssetClass::Bond | AssetClass::Fund | AssetClass::Warrant => PickerTab::Stocks,
        // The CFD venues list their indices, shares, metals and crypto CFDs beside the currency pairs,
        // so the drawer that holds them all is labelled for both (`PickerTab::label`). A drawer of their
        // own would be a new `PickerTab`, which is a new wire variant of the catalog search's tab filter.
        AssetClass::Fx | AssetClass::Cfd => PickerTab::Forex,
        // A spot commodity (a physical metal) sits where the metal CFDs of oanda, fxcm and
        // dukascopy already sit.
        AssetClass::Commodity => PickerTab::Forex,
        AssetClass::CryptoSpot => PickerTab::Crypto,
        AssetClass::CryptoPerp => PickerTab::Perps,
        AssetClass::Option => PickerTab::Options,
        // A venue-listed multi-leg strategy sits with the options; a future spread is a combo too
        // and lands here as well (one stored word, so one drawer).
        AssetClass::Combo => PickerTab::Options,
        // A dated crypto future is a future: it expires, so it is never under "Perps" (it was until
        // 2026-11-12).
        AssetClass::Future | AssetClass::CryptoFuture => PickerTab::Futures,
        AssetClass::Index => PickerTab::Indices,
        AssetClass::PredictionMarket => PickerTab::Prediction,
    }
}

impl PickerTab {
    pub fn label(self) -> &'static str {
        match self {
            PickerTab::Stocks => "Stocks",
            PickerTab::Forex => "FX & CFD",
            PickerTab::Crypto => "Crypto",
            PickerTab::Perps => "Perps",
            PickerTab::Options => "Options",
            PickerTab::Futures => "Futures",
            PickerTab::Indices => "Indices",
            PickerTab::Prediction => "Prediction",
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
        assert_eq!(tab_for(AssetClass::Equity), PickerTab::Stocks);
        assert_eq!(tab_for(AssetClass::Etf), PickerTab::Stocks);
        assert!(PickerTab::Stocks.matches(AssetClass::Equity));
        assert!(!PickerTab::Stocks.matches(AssetClass::CryptoSpot));
    }

    #[test]
    fn perps_tab_holds_the_perps_and_futures_tab_the_dated_futures() {
        assert_eq!(tab_for(AssetClass::CryptoPerp), PickerTab::Perps);
        assert_eq!(tab_for(AssetClass::CryptoFuture), PickerTab::Futures);
        assert_eq!(tab_for(AssetClass::Future), PickerTab::Futures);
    }

    #[test]
    fn forex_tab_groups_fx_and_cfd() {
        assert_eq!(tab_for(AssetClass::Fx), PickerTab::Forex);
        assert_eq!(tab_for(AssetClass::Cfd), PickerTab::Forex);
    }

    /// ⚠ **A tab holds only what its label says.** A dated crypto future is a future, so it sits with
    /// the dated futures and never under "Perps"; and the FX drawer holds the CFDs (index, share,
    /// metal, crypto) beside the currency pairs, so its label names them. The `vike_catalog::SymbolPickerKind`
    /// picker already gives a CFD its own word; the two pickers now agree on what a CFD is called.
    #[test]
    fn a_tab_holds_only_what_its_label_says() {
        assert_eq!(
            tab_for(AssetClass::CryptoFuture),
            PickerTab::Futures,
            "a dated future is no perp"
        );
        assert!(!PickerTab::Perps.matches(AssetClass::CryptoFuture));
        assert_eq!(tab_for(AssetClass::Cfd), PickerTab::Forex);
        assert_eq!(PickerTab::Forex.label(), "FX & CFD", "the drawer names the CFDs it holds");
        assert_eq!(PickerTab::Perps.label(), "Perps");
    }

    /// The decision-0126 classes: listed securities with the shares, a spot commodity with the metal
    /// CFDs, a combo with the options.
    #[test]
    fn the_0126_classes_have_a_drawer() {
        for class in [AssetClass::Bond, AssetClass::Fund, AssetClass::Warrant] {
            assert_eq!(tab_for(class), PickerTab::Stocks, "{class:?}");
        }
        assert_eq!(tab_for(AssetClass::Commodity), PickerTab::Forex);
        assert_eq!(tab_for(AssetClass::Combo), PickerTab::Options);
    }

    /// Every variant of the taxonomy answers — the `match` in [`tab_for`] is exhaustive, so this
    /// cannot fail at run time, but it is what makes the ROSTER sweep explicit: a new variant
    /// stops this crate compiling until its author picks a drawer.
    #[test]
    fn every_class_names_a_tab() {
        for class in AssetClass::ALL {
            let _ = tab_for(*class).label();
        }
    }
}
