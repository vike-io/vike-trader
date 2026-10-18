//! Test support shared by this crate's unit tests (compiled under `cfg(test)` only).

use vike_marketdata::OptionKind;
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, ComboContract, ComboKind, Contract,
    Delivery, EquitySpec, ExerciseStyle, FutureFamily, FutureSpec, FxSpec, Instrument, Margining,
    OptionSpec, PerpSpec, PredictionSpec,
};

/// A bare instrument for tests: the five fields a test varies, everything else empty. The contract
/// is [`contract_for`]'s representative of `class`.
pub(crate) fn test_row(
    venue: &str,
    raw_symbol: &str,
    base: &str,
    quote: &str,
    class: AssetClass,
) -> Instrument {
    Instrument::new(venue, raw_symbol, base, quote, contract_for(class, quote))
}

/// A representative contract projecting to `class`, its unknown fields `None` and its required
/// ones filled with plain test values (settling in `quote`). TEST-ONLY: a production parser fills a
/// contract from the venue payload and never fabricates one from a class.
pub(crate) fn contract_for(class: AssetClass, quote: &str) -> Contract {
    let contract = match class {
        AssetClass::Equity => Contract::Equity(EquitySpec::default()),
        AssetClass::Etf => Contract::Etf(EquitySpec::default()),
        AssetClass::CryptoSpot => Contract::CryptoSpot,
        AssetClass::CryptoPerp => Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: quote.into(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms: None,
        }),
        AssetClass::CryptoFuture | AssetClass::Future => Contract::Future(FutureSpec {
            margining: Margining::Linear,
            settle: quote.into(),
            value_ccy: None,
            expiry_ms: 0,
            tenor: None,
            delivery: Delivery::Unknown,
            family: if class == AssetClass::CryptoFuture {
                FutureFamily::Crypto
            } else {
                FutureFamily::Traditional
            },
        }),
        AssetClass::Option => Contract::Option(OptionSpec {
            right: OptionKind::Call,
            strike: 1.0,
            expiry_ms: 0,
            style: ExerciseStyle::Unknown,
            margining: Margining::Linear,
            settle: quote.into(),
            value_ccy: None,
            underlying: None,
        }),
        AssetClass::Fx => Contract::Fx(FxSpec::default()),
        AssetClass::Index => Contract::Index,
        AssetClass::PredictionMarket => Contract::PredictionOutcome(PredictionSpec {
            condition_id: String::new(),
            outcome: String::new(),
            token_id: String::new(),
            resolution_ms: None,
            neg_risk: None,
        }),
        AssetClass::Cfd => Contract::Cfd(CfdSpec {
            underlying_kind: CfdUnderlying::Other,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Unknown,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
        AssetClass::Bond => Contract::Bond,
        AssetClass::Fund => Contract::Fund,
        AssetClass::Warrant => Contract::Warrant,
        AssetClass::Commodity => Contract::Commodity,
        AssetClass::Combo => Contract::Combo(ComboContract {
            kind: ComboKind::OptionCombo,
            legs: Vec::new(),
            expiry_ms: None,
            settle: None,
        }),
    };
    debug_assert_eq!(contract.class(), class);
    contract
}

#[test]
fn contract_for_projects_every_class() {
    for class in AssetClass::ALL {
        assert_eq!(contract_for(*class, "USD").class(), *class);
    }
}
