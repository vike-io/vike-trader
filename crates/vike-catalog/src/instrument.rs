//! The universal instrument record — one row per tradable thing, venue-tagged. Identity follows
//! NautilusTrader's `InstrumentId` shape (`{symbol}.{venue}`). Carries the per-symbol
//! `SymbolProperties` (may be `Default` until fetched) so the picker/order paths have tick/lot.

use serde::{Deserialize, Serialize};
use vike_model::AssetClass;
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
    /// **The venue's OWN word for what this contract IS, carried VERBATIM and UNINTERPRETED** —
    /// bybit's `contractType` (`"InversePerpetual"`, `"LinearPerpetual"`), deribit's
    /// `instrument_type` (`"reversed"`, `"linear"`). `None` is the ordinary state: most venues
    /// publish no such field, and a parser that reads none leaves this absent.
    ///
    /// # Why this is DATA and not a taxonomy variant
    ///
    /// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 1 keeps [`AssetClass`]'s perp
    /// variant WHOLE — no linear/inverse split — because *"nothing ROUTES on the difference"*, and
    /// what the difference actually governs is economics, which already has a home in
    /// `vike_model::instrument::SymbolProperties`'s `contract_size` (whose own doc names inverse
    /// perps and options as its two cases). This pair is the IDENTIFICATION half of the same
    /// argument: the venue hands us the fact and we were discarding it, leaving linear-vs-inverse to
    /// be reconstructed from the tail of a symbol string — the implicit encoding that record exists
    /// to remove.
    ///
    /// ⚠ **NOTHING IN THIS WORKSPACE ROUTES ON THIS FIELD, AND A ROUTE THAT DID WOULD BE A DECISION
    /// RATHER THAN A REFACTOR.** 0061's reopen list names *"the vocabulary gaining a consumer that
    /// must separate linear from inverse for ROUTING rather than economics"* as the ONE trigger that
    /// splits the perp variant — so a router reading this is that trigger firing, and it goes
    /// through the record rather than arriving as a side effect. Read it for IDENTIFICATION:
    /// rendering what an instrument is, auditing a fill, answering an operator.
    ///
    /// A `String` rather than an enum, deliberately: an enum would be a second vocabulary for the
    /// axis 0061 refuses to encode a fifth time, and the words are not comparable across venues
    /// anyway (bybit's `"InversePerpetual"` and deribit's `"reversed"` name one thing).
    /// [`Instrument::settle_asset`] is the cross-venue-comparable half.
    ///
    /// ⚠ It lives HERE and not on `SymbolProperties`, and the reason is structural rather than
    /// taxonomic: that struct is `Copy` (it is folded through pricing paths by value) and a `String`
    /// cannot live in a `Copy` type. This is also where the persisted shape is friendlier —
    /// [`crate::CatalogCache`] is serde JSON, so `#[serde(default)]` carries an old cache forward,
    /// whereas the `kind=properties` Parquet codec in `vike-data` would have needed a new column
    /// before anything could populate one (see `SymbolProperties::tick_scheme`'s own doc for what
    /// happens to a struct field with no column).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_type: Option<String>,
    /// **The asset this instrument's PnL and margin SETTLE in** — bybit's `settleCoin`, deribit's
    /// `settlement_currency`, okx's `settleCcy`. `None` where the venue publishes none.
    ///
    /// The cross-venue-comparable half of [`Instrument::contract_type`], and the fact that actually
    /// decides linear-vs-inverse without parsing anybody's word for it: a perpetual settling in its
    /// BASE asset is coin-settled (inverse), one settling in its QUOTE asset is linear. That
    /// comparison is deliberately NOT written as a helper here — see the ⚠ on `contract_type` for
    /// why a router reading it is a decision rather than a refactor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_asset: Option<String>,
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
    use vike_model::AssetClass;

    fn inst(venue: &str, sym: &str) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: AssetClass::CryptoSpot,
            base: "BTC".into(),
            quote: "USDT".into(),
            description: String::new(),
            properties: Default::default(),
            contract_type: None,
            settle_asset: None,
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
