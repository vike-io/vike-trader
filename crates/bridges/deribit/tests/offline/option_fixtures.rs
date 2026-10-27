//! The deribit instrument builders the option-grid members below share (`option_index_payloads`,
//! `option_ticker_fold`, `option_markprice_fold`): copies of
//! `crates/vike-options/src/chain/index.rs`'s `test_option` and `test_delta_one` (test-only there,
//! so an integration test cannot reach them), with `crate::expiry_ms` spelled from outside as
//! `vike_options::expiry_ms`.

use vike_marketdata::OptionKind;
use vike_model::{Contract, Instrument, OptionSpec};

/// The venue the builders stamp, as `vike_options::OptionIndex` indexes only this venue's rows.
const VENUE: &str = "deribit";

/// A deribit option [`Instrument`] for tests, built the way the catalog builds one: the margining
/// follows the settlement (coin-settled = inverse, else linear), the expiry is 08:00 UTC of
/// `expiry_iso`.
pub(crate) fn test_option(
    name: &str,
    underlying: &str,
    expiry_iso: &str,
    strike: f64,
    right: OptionKind,
    settle: &str,
) -> Instrument {
    let margining = if settle == underlying {
        vike_model::Margining::Inverse
    } else {
        vike_model::Margining::Linear
    };
    Instrument::new(
        VENUE,
        name,
        underlying,
        settle,
        Contract::Option(OptionSpec {
            right,
            strike,
            expiry_ms: vike_options::expiry_ms(expiry_iso),
            style: vike_model::ExerciseStyle::European,
            margining,
            settle: settle.to_string(),
            value_ccy: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// A deribit perpetual or dated future [`Instrument`] for tests (`base` is its underlying).
pub(crate) fn test_delta_one(name: &str, base: &str) -> Instrument {
    Instrument::new(
        VENUE,
        name,
        base,
        "USD",
        Contract::Perp(vike_model::PerpSpec {
            margining: vike_model::Margining::Inverse,
            settle: base.to_string(),
            value_ccy: Some("USD".to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
}
