//! The universal instrument record — one row per tradable thing, venue-tagged. Identity follows
//! NautilusTrader's `InstrumentId` shape (`{symbol}.{venue}`). Carries the per-symbol
//! `SymbolProperties` (may be `Default` until fetched) so the picker/order paths have tick/lot.

use crate::AssetClass;
use serde::{Deserialize, Serialize};
use vike_model::SymbolProperties;

/// One tradable instrument in the catalog. `venue` is the venue key (`"binance"`, `"alpaca"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    pub venue: String,
    pub raw_symbol: String,
    pub asset_class: AssetClass,
    pub base: String,
    pub quote: String,
    pub description: String,
    pub properties: SymbolProperties,
}

impl Instrument {
    /// Stable display identity: `"{RAW_SYMBOL}.{VENUE}"` with an upper-cased venue. Derivative
    /// asset classes append a market suffix (`.P`/`.F`/`.O`) so a spot and a perp/future/option
    /// sharing the same raw symbol on the same venue (e.g. binance `BTCUSDT` spot vs perp) don't
    /// collide — spot/equity/fx/etc. are unchanged for backward compatibility.
    pub fn id(&self) -> String {
        // `{raw_symbol}.{VENUE}`. Every derivative already carries a DISTINCT `raw_symbol` — OKX's
        // dashed instIds (`BTC-USDT-SWAP`), Deribit's instrument names, and (for the symbol-reusing
        // venues) a `.P`-suffixed perp symbol set by their catalog providers (TradingView's
        // `BINANCE:BTCUSDT.P` convention) — so no extra product suffix is needed to keep a perp
        // distinct from its spot twin.
        format!("{}.{}", self.raw_symbol, self.venue.to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AssetClass;

    fn inst(venue: &str, sym: &str) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: AssetClass::CryptoSpot,
            base: "BTC".into(),
            quote: "USDT".into(),
            description: String::new(),
            properties: Default::default(),
        }
    }

    #[test]
    fn id_is_symbol_dot_upper_venue() {
        assert_eq!(inst("binance", "BTCUSDT").id(), "BTCUSDT.BINANCE");
        assert_eq!(inst("alpaca", "AAPL").id(), "AAPL.ALPACA");
    }

    #[test]
    fn id_disambiguates_perp_from_spot_via_distinct_raw_symbol() {
        // Distinctness now lives in `raw_symbol` (the `.P` suffix set by the venue's catalog
        // provider), not an asset-class suffix on `id()`. id() == "{raw}.{VENUE}".
        let spot = inst("binance", "BTCUSDT");
        let mut perp = inst("binance", "BTCUSDT.P");
        perp.asset_class = AssetClass::CryptoPerp;
        assert_eq!(spot.id(), "BTCUSDT.BINANCE");
        assert_eq!(perp.id(), "BTCUSDT.P.BINANCE");
        assert_ne!(spot.id(), perp.id());

        // OKX derivatives are natively distinct (no `.P` suffix needed).
        let mut swap = inst("okx", "BTC-USDT-SWAP");
        swap.asset_class = AssetClass::CryptoPerp;
        assert_eq!(swap.id(), "BTC-USDT-SWAP.OKX");
        assert_ne!(swap.id(), inst("okx", "BTC-USDT").id());
    }

    #[test]
    fn roundtrips_through_serde_json() {
        let i = inst("okx", "BTC-USDT-SWAP");
        let s = serde_json::to_string(&i).unwrap();
        let back: Instrument = serde_json::from_str(&s).unwrap();
        assert_eq!(i, back);
    }
}
