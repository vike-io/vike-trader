//! `oanda_rate` — the ONE place OANDA's `homeConversions` becomes the rate a facts answer carries (decision
//! 0183, phase 1), so the orientation has one home and one test (`oanda_rate_tests.rs`: USDJPY, EURGBP and a
//! USD-quoted pair, over the docs-shaped pricing body of `crates/bridges/oanda/src/home_conversion.rs`).
//!
//! The rate is **account-currency units per ONE unit of the instrument's QUOTE currency**, the engine's
//! orientation (`SymbolAdmission::quote_rate`): a pound is worth more than a dollar (`1.35`), a yen a fraction
//! of one (`1/150`, never `150`; the flipped rate is a 22,500x error). `vike_oanda::AccountPerQuote` carries
//! that direction in its type and this function only reads it out.
//!
//! **Fail closed**: an instrument that names no quote currency, a read that failed, a home currency the pricing
//! read does not agree with, and a currency OANDA sent no usable factor for are each a refusal worded as a
//! failure of the RATE ([`FACTS_RATE_UNAVAILABLE`]), so the node does not remember it as a verdict on the symbol.
//! There is no path that answers `1.0` for "I do not know": the only `None` is the instrument quoted in the
//! account's own currency, which needs no conversion.

use vike_gatewayhub_client::proto::FACTS_RATE_UNAVAILABLE;
use vike_oanda::{HomeConversions, RateError, quote_currency_of};

use super::symbol_facts::QuotedRate;

/// The rate for `instrument` (`EUR_GBP` or the engine's `EURGBP`) on an account whose currency is `home`:
/// `Ok(None)` when it is quoted in `home`, `Ok(Some(rate))` when it is quoted in another currency and OANDA
/// stated a factor for it, `Err` otherwise. `read` is the pricing read for the instrument (production:
/// `OandaCatalog::home_conversions`), called only when a conversion is needed.
pub(crate) fn quoted_rate(
    instrument: &str,
    home: &str,
    read: impl FnOnce(&str) -> Result<HomeConversions, RateError>,
) -> Result<Option<QuotedRate>, String> {
    let Some(quote) = quote_currency_of(instrument) else {
        return Err(format!(
            "oanda: {instrument} names no quote currency, so it cannot be converted into the \
             account's currency ({home})"
        ));
    };
    if quote.eq_ignore_ascii_case(home) {
        return Ok(None);
    }
    let unavailable = |why: &dyn std::fmt::Display| {
        format!("oanda: {FACTS_RATE_UNAVAILABLE}: no rate for {quote} in {home} ({why})")
    };
    let rates = read(instrument).map_err(|e| unavailable(&e))?;
    // The listing's home currency and the one pricing converts into must be ONE: a mismatch makes every rate
    // wrong at once (`HomeConversions::from_pricing_body`'s identity check guards the same fact from the other
    // side).
    if !rates.home().eq_ignore_ascii_case(home) {
        return Err(unavailable(&format!(
            "the pricing read converts into {}, not the account's {home}",
            rates.home()
        )));
    }
    let rate = rates.quote_rate(&quote).map_err(|e| unavailable(&e))?;
    Ok(Some(QuotedRate {
        quote_ccy: rate.quote_ccy,
        account_per_quote: rate.account_per_quote.get(),
        as_of_ms: rate.as_of_ms,
    }))
}

#[path = "oanda_rate_tests.rs"]
#[cfg(test)]
mod oanda_rate_tests;
