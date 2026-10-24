//! The universal instrument record — one row per tradable thing, venue-tagged. Identity is the
//! `{symbol}.{venue}` shape. Carries WHAT the instrument is as a typed [`Contract`]
//! (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`),
//! the venue's own wire identities beside the canonical one ([`WireIds`]), and the per-symbol
//! `SymbolProperties` (may be `Default` until fetched) so the picker/order paths have tick/lot.
//!
//! It is vocabulary (the nouns a venue bridge fills and every plane reads), so it sits beside
//! `AssetClass` and `SymbolProperties`; the catalog that stores and searches rows of it is
//! vike-catalog's.
//!
//! The class is NOT a field: [`Instrument::asset_class`] projects it from the contract, and
//! [`Instrument::settle_asset`] reads the settlement asset off the derivative spec that carries it,
//! so the two can never disagree with the contract. A row is built with [`Instrument::new`] and the
//! `with_*` builders.

use crate::instrument::SymbolProperties;
use crate::instrument::asset_class::AssetClass;
use crate::instrument::contract::Contract;
use serde::{Deserialize, Serialize};

/// The venue's OWN identities for an instrument, beside the canonical `raw_symbol` — so no consumer
/// parses a name to find what the venue calls it. Every field is `None` where the venue has no such
/// identity (most have none beyond the symbol).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WireIds {
    /// A numeric asset id the venue signs or routes on (hyperliquid's asset index).
    pub asset_id: Option<u64>,
    /// The venue's coin name where it differs from the symbol (hyperliquid's `coin`).
    pub coin: Option<String>,
    /// A grouping id the venue keys requests on, e.g. okx's `instFamily` or a hyperliquid HIP-3
    /// dex name.
    pub group: Option<String>,
}

/// One tradable instrument in the catalog. `venue` is the venue key (`"binance"`, `"alpaca"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    pub venue: String,
    pub raw_symbol: String,
    pub base: String,
    pub quote: String,
    pub description: String,
    /// What this instrument IS, with the data that defines its class. Required on the wire: a
    /// cached row written before this field existed fails to load and the catalog is rebuilt from
    /// the venue fetch (no migrations).
    pub contract: Contract,
    /// The venue's own identities beside `raw_symbol`. `#[serde(default)]`: absent = none known.
    #[serde(default)]
    pub wire: WireIds,
    pub properties: SymbolProperties,
    /// **The venue's OWN word for what this contract IS, carried VERBATIM and UNINTERPRETED** —
    /// bybit's `contractType` (`"InversePerpetual"`, `"LinearPerpetual"`), deribit's
    /// `instrument_type` (`"reversed"`, `"linear"`). `None` is the ordinary state: most venues
    /// publish no such field.
    ///
    /// INFORMATIONAL only — for rendering, auditing a fill, answering an operator. The typed,
    /// cross-venue answer is the contract's [`crate::Margining`]; never route on this string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_type: Option<String>,
}

impl Instrument {
    /// A row with the defining fields: empty description, no wire ids, `Default` properties and no
    /// venue word. Add the rest with the `with_*` builders.
    #[must_use]
    pub fn new(
        venue: impl Into<String>,
        raw_symbol: impl Into<String>,
        base: impl Into<String>,
        quote: impl Into<String>,
        contract: Contract,
    ) -> Self {
        Self {
            venue: venue.into(),
            raw_symbol: raw_symbol.into(),
            base: base.into(),
            quote: quote.into(),
            description: String::new(),
            contract,
            wire: WireIds::default(),
            properties: SymbolProperties::default(),
            contract_type: None,
        }
    }

    /// Set the human description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the per-symbol order-placement grid.
    #[must_use]
    pub fn with_properties(mut self, properties: SymbolProperties) -> Self {
        self.properties = properties;
        self
    }

    /// Set the venue's own identities.
    #[must_use]
    pub fn with_wire(mut self, wire: WireIds) -> Self {
        self.wire = wire;
        self
    }

    /// Carry the venue's own contract word verbatim ([`Instrument::contract_type`]).
    #[must_use]
    pub fn with_venue_word(mut self, word: impl Into<String>) -> Self {
        self.contract_type = Some(word.into());
        self
    }

    /// The [`AssetClass`] this instrument projects to — [`Contract::class`] of its contract.
    #[must_use]
    pub fn asset_class(&self) -> AssetClass {
        self.contract.class()
    }

    /// The asset PnL and margin settle in, off the derivative spec that carries it
    /// ([`Contract::settle_asset`]); `None` for a cash class.
    #[must_use]
    pub fn settle_asset(&self) -> Option<&str> {
        self.contract.settle_asset()
    }

    /// Stable display identity: `"{RAW_SYMBOL}.{VENUE}"` with an upper-cased venue, and NOTHING
    /// appended for the class. A perp and its spot twin on one venue stay distinct because their
    /// `raw_symbol`s already differ: OKX's dashed instIds (`BTC-USDT-SWAP`), Deribit's instrument
    /// names, and — for the symbol-reusing venues — the `.P`-suffixed perp symbol their catalog
    /// providers set (TradingView's `BINANCE:BTCUSDT.P` convention).
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}.{}", self.raw_symbol, self.venue.to_uppercase())
    }
}

#[path = "record_tests.rs"]
#[cfg(test)]
mod record_tests;
