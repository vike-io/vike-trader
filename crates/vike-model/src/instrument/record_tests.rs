use super::*;
use crate::{
    Delivery, ExerciseStyle, FutureFamily, FutureSpec, Margining, OptionKind, OptionSpec, PerpSpec,
};

/// A bare spot instrument: the two fields a test varies, everything else empty.
fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument::new(venue, sym, "BTC", "USDT", Contract::CryptoSpot)
}

fn linear_perp(settle: &str) -> Contract {
    Contract::Perp(PerpSpec {
        margining: Margining::Linear,
        settle: settle.into(),
        value_ccy: None,
        funding_interval_ms: None,
        settling_ms: None,
    })
}

#[test]
fn id_is_symbol_dot_upper_venue() {
    assert_eq!(inst("binance", "BTCUSDT").id(), "BTCUSDT.BINANCE");
    assert_eq!(inst("alpaca", "AAPL").id(), "AAPL.ALPACA");
}

#[test]
fn id_disambiguates_perp_from_spot_via_distinct_raw_symbol() {
    // Distinctness lives in `raw_symbol` (the `.P` suffix set by the venue's catalog provider),
    // not a class suffix on `id()`: id() == "{raw}.{VENUE}" whatever the contract.
    let spot = inst("binance", "BTCUSDT");
    let perp = Instrument::new("binance", "BTCUSDT.P", "BTC", "USDT", linear_perp("USDT"));
    assert_eq!(spot.id(), "BTCUSDT.BINANCE");
    assert_eq!(perp.id(), "BTCUSDT.P.BINANCE");
    assert_ne!(spot.id(), perp.id());

    // OKX derivatives are natively distinct (no `.P` suffix needed).
    let swap = Instrument::new("okx", "BTC-USDT-SWAP", "BTC", "USDT", linear_perp("USDT"));
    assert_eq!(swap.id(), "BTC-USDT-SWAP.OKX");
    assert_ne!(swap.id(), inst("okx", "BTC-USDT").id());
}

#[test]
fn new_fills_the_defining_fields_and_defaults_the_rest() {
    let i = Instrument::new("okx", "BTC-USDT", "BTC", "USDT", Contract::CryptoSpot);
    assert_eq!(i.venue, "okx");
    assert_eq!(i.raw_symbol, "BTC-USDT");
    assert_eq!(i.base, "BTC");
    assert_eq!(i.quote, "USDT");
    assert_eq!(i.contract, Contract::CryptoSpot);
    assert_eq!(i.description, "");
    assert_eq!(i.wire, WireIds::default());
    assert_eq!(i.properties, SymbolProperties::default());
    assert_eq!(i.contract_type, None);
}

#[test]
fn builders_set_their_field_and_nothing_else() {
    let props = SymbolProperties { tick_size: 0.5, ..Default::default() };
    let wire = WireIds { asset_id: Some(3), coin: Some("BTC".into()), group: None };
    let i = Instrument::new("hyperliquid", "BTC", "BTC", "USDC", linear_perp("USDC"))
        .with_description("Bitcoin perp")
        .with_properties(props)
        .with_wire(wire.clone())
        .with_venue_word("perp");
    assert_eq!(i.description, "Bitcoin perp");
    assert_eq!(i.properties, props);
    assert_eq!(i.wire, wire);
    assert_eq!(i.contract_type.as_deref(), Some("perp"));
    assert_eq!(i.contract, linear_perp("USDC"));
}

#[test]
fn asset_class_and_settle_asset_read_the_contract() {
    let spot = inst("binance", "BTCUSDT");
    assert_eq!(spot.asset_class(), AssetClass::CryptoSpot);
    assert_eq!(spot.settle_asset(), None);

    let perp = Instrument::new("bybit", "BTCUSD.P", "BTC", "USD", linear_perp("BTC"));
    assert_eq!(perp.asset_class(), AssetClass::CryptoPerp);
    assert_eq!(perp.settle_asset(), Some("BTC"));

    let fut = Instrument::new(
        "ibkr",
        "ESZ6",
        "ES",
        "USD",
        Contract::Future(FutureSpec {
            margining: Margining::Linear,
            settle: "USD".into(),
            value_ccy: Some("USD".into()),
            expiry_ms: 1_797_000_000_000,
            tenor: None,
            delivery: Delivery::Cash,
            family: FutureFamily::Traditional,
        }),
    );
    assert_eq!(fut.asset_class(), AssetClass::Future);
    assert_eq!(fut.settle_asset(), Some("USD"));

    let opt = Instrument::new(
        "deribit",
        "BTC-30OCT26-60000-P",
        "BTC",
        "USD",
        Contract::Option(OptionSpec {
            right: OptionKind::Put,
            strike: 60_000.0,
            expiry_ms: 1_793_000_000_000,
            style: ExerciseStyle::European,
            margining: Margining::Inverse,
            settle: "BTC".into(),
            value_ccy: None,
            underlying: None,
        }),
    );
    assert_eq!(opt.asset_class(), AssetClass::Option);
    assert_eq!(opt.settle_asset(), Some("BTC"));
}

#[test]
fn roundtrips_through_serde_json() {
    let plain = inst("okx", "BTC-USDT");
    let full = Instrument::new("hyperliquid", "BTC", "BTC", "USDC", linear_perp("USDC"))
        .with_wire(WireIds { asset_id: Some(0), coin: Some("BTC".into()), group: Some("x".into()) })
        .with_venue_word("perp");
    for i in [plain, full] {
        let s = serde_json::to_string(&i).unwrap();
        let back: Instrument = serde_json::from_str(&s).unwrap();
        assert_eq!(i, back, "{s}");
    }
}

/// `wire` and `contract_type` are optional on the wire: a row without them reads back with none.
#[test]
fn absent_wire_and_venue_word_deserialize_as_none() {
    let json = serde_json::json!({
        "venue": "binance",
        "raw_symbol": "BTCUSDT",
        "base": "BTC",
        "quote": "USDT",
        "description": "",
        "contract": "crypto_spot",
        "properties": SymbolProperties::default(),
    });
    let i: Instrument = serde_json::from_value(json).unwrap();
    assert_eq!(i, inst("binance", "BTCUSDT"));
}

/// A row that names no contract — the shape every cache written before decision 0126 has — is
/// REFUSED rather than guessed at; the catalog rebuilds it from the venue fetch.
#[test]
fn a_row_without_a_contract_is_refused() {
    let old = serde_json::json!({
        "venue": "binance",
        "raw_symbol": "BTCUSDT",
        "asset_class": "CryptoSpot",
        "base": "BTC",
        "quote": "USDT",
        "description": "",
        "properties": SymbolProperties::default(),
    });
    assert!(serde_json::from_value::<Instrument>(old).is_err());
}
