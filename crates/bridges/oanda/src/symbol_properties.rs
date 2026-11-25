//! **What the gatewayhub asks of OANDA to admit an instrument the engine was not started on** (decision 0174, decision
//! 0183): the account-listed venue's spec ([`SymbolPropertiesAccountSpecOanda`]: the plain answer's currency fence and
//! the adapter's grid fence, over the account's own instrument list, and how that list is read: its
//! `SymbolPropertiesListSpec`), and the ONE place OANDA's `homeConversions`
//! becomes the rate a facts answer carries ([`quoted_rate`]), so the orientation has one home and one test
//! (`symbol_properties_tests.rs`: USDJPY, EURGBP and a USD-quoted pair, over the docs-shaped pricing body of
//! [`crate::account::quote_rate`]). The listing, its bounds and the wording of a refusal are the gatewayhub's.
//!
//! The rate is **account-currency units per ONE unit of the instrument's QUOTE currency**, the engine's
//! orientation (`SymbolAdmission::quote_rate`): a pound is worth more than a dollar (`1.35`), a yen a fraction
//! of one (`1/150`, never `150`; the flipped rate is a 22,500x error). `vike_model::AccountPerQuote` carries
//! that direction in its type and [`quoted_rate`] only reads it out.
//!
//! **Fail closed**: an instrument that names no quote currency is a refusal; a read that failed, a home currency the
//! pricing read does not agree with, and a currency OANDA sent no usable factor for are each a failure of the RATE
//! ([`SymbolPropertiesError::RateUnavailable`]), so the node does not remember it as a verdict on the symbol. There is
//! no path that answers `1.0` for "I do not know": the only `None` is the instrument quoted in the account's own
//! currency, which needs no conversion.

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesAccountSpec, SymbolPropertiesError, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::{Instrument, QuoteRate, QuoteRateError, SymbolProperties};
use vike_utility::errors::CarriedRefusal;

use crate::account::quote_rate::{HomeConversions, quote_currency_of};
use crate::instrument::grid_refusal;
use crate::refusals::OandaPropertiesRefusal;

/// **OANDA's account-listed facts** (the gatewayhub's per-account listing: `GET /summary` beside `GET /instruments`,
/// two plain REST reads, so a failure is remembered for seconds). The instrument list and the home currency are
/// both the ACCOUNT's.
///
/// **The fence of the plain answer: an instrument is admitted only when its QUOTE currency is the account's
/// currency.** The engine's ledger is one currency, and only a node that asks to convert (decision 0183) gets a
/// conversion rate into it (`docs/ops/any-symbol-remaining-venues-plan.md`, blocker 1): `USDJPY` on a USD account
/// would book 100k units at 150 as about 15 million "USD" of notional and a 0.10 yen move as 10,000 "USD" of PnL
/// against about 66 real, and overstated PnL feeds the margin-call sweep and the drawdown latch. So it is refused,
/// by name, as `deribit`'s other money model is. With the quote equal to the home currency, `units x price` IS the
/// real exposure whatever the instrument (FX or CFD), which is what the engine computes. It is run BEFORE the grid
/// checks (a JPY pair would otherwise be refused for its three-decimal price grid, which is true and not the point);
/// the adapter's own grid fence follows the row's grid for either node: a currency pair on its OWN price precision (a
/// JPY pair is three decimals, admitted on an account whose currency is the yen), whole units only, and any other
/// class on the five decimals the adapter was proven on ([`grid_refusal`], the rule the exec client's own grid
/// shares).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesAccountSpecOanda;

/// Every fence below names the instrument by the row's wire name (`GBP_USD`), the venue's own word for it.
impl SymbolPropertiesAccountSpec for SymbolPropertiesAccountSpecOanda {
    const VENUE: &'static str = "oanda";
    const LISTING_IS_A_SESSION: bool = false;

    fn plain_refusal(_symbol: &str, row: &Instrument, currency: &str) -> Option<CarriedRefusal> {
        quote_currency_refusal(&vike_catalog::wire_name_of_row(row), currency)
    }

    /// A node that converts is answered for any quote currency, with its rate; only the grid fence stands.
    fn quoted_refusal(_symbol: &str, _row: &Instrument, _currency: &str) -> Option<CarriedRefusal> {
        None
    }

    /// ⚠ The row's class (its OANDA `type`) reaches this fence through the row, not through the answer (it carries
    /// the grid only).
    fn grid_refusal(
        _symbol: &str,
        row: &Instrument,
        properties: &SymbolProperties,
    ) -> Option<CarriedRefusal> {
        grid_refusal(&vike_catalog::wire_name_of_row(row), row.contract_type.as_deref(), properties)
    }

    fn rate_subject(_symbol: &str, row: &Instrument) -> String {
        vike_catalog::wire_name_of_row(row)
    }
}

/// **How the account's list is read** (the gatewayhub's `SymbolFacts`, generic over this spec): the USER name
/// (`GBPUSD`, decision 0290) looked up as its kept row's wire name (`GBP_USD`, `crate::names`) in that account's WHOLE
/// listing. Only an account's own listing answers: a plain (public) source never does, because its rows would skip
/// the account's currency fence above.
///
/// A name the kept list carries no row of (an old spelling, `GBP_USD`) is refused in the translator's words before
/// anything is read. With NO kept list at all the listing is still read, because that read is what keeps the list
/// (the gatewayhub holds it as the venue's list for the datahub, `hold_listing`): the name then finds no row there
/// ([`SymbolPropertiesListSpec::key`]), said at WARN with the fix ([`SymbolPropertiesListSpec::symbol_refusal`]).
impl SymbolPropertiesListSpec for SymbolPropertiesAccountSpecOanda {
    fn venue(&self) -> &'static str {
        Self::VENUE
    }

    /// MEASURED 2026-10-26: the practice account lists 68 currency pairs and no CFD, metal or index, and OANDA
    /// answers an order on one INSTRUMENT_NOT_TRADEABLE, so "FX and CFD" would promise too much.
    fn covers(&self, _list_key: &str, account_listed: bool) -> Option<&'static str> {
        account_listed.then_some(
            "the instruments this account's own OANDA list carries; a CFD, metal or index is listed \
             only if OANDA offers it to the account",
        )
    }

    fn symbol_refusal(
        &self,
        symbol: &str,
        _words: &SymbolPropertiesWords,
    ) -> Option<CarriedRefusal> {
        if crate::names::no_kept_list() {
            tracing::warn!(
                venue = Self::VENUE,
                symbol,
                "oanda: the account's listing is read to be kept, and `{symbol}` finds no row in it until it is: {}",
                vike_catalog::no_kept_list_refusal(Self::VENUE)
            );
            return None;
        }
        crate::names::wire_name(symbol)
            .err()
            .map(|why| OandaPropertiesRefusal::NotKept { why }.carried())
    }

    /// The kept row's wire name; with no kept list (the one case [`Self::symbol_refusal`] lets through) the empty
    /// key, which names no row of a listing (every row has a name), so the answer is the not-found refusal.
    fn key(&self, symbol: &str) -> String {
        crate::names::wire_name(symbol).unwrap_or_default()
    }
}

/// Why an OANDA `instrument` (`GBP_USD`) is not quoted in `home`, the account's currency, or `None`.
fn quote_currency_refusal(instrument: &str, home: &str) -> Option<CarriedRefusal> {
    let quote = instrument.split_once('_').map(|(_, quote)| quote).filter(|q| !q.is_empty());
    let refusal = match quote {
        Some(quote) if quote.eq_ignore_ascii_case(home) => return None,
        Some(quote) => OandaPropertiesRefusal::ForeignQuote {
            instrument: instrument.to_string(),
            quote: quote.to_string(),
            home: home.to_string(),
        },
        None => OandaPropertiesRefusal::NoQuoteCurrency {
            instrument: instrument.to_string(),
            home: home.to_string(),
        },
    };
    Some(refusal.carried())
}

/// The rate for `instrument` (the kept row's wire name, `EUR_GBP`: the spec's rate subject) on an account whose
/// currency is `home`:
/// `Ok(None)` when it is quoted in `home`, `Ok(Some(rate))` when it is quoted in another currency and OANDA
/// stated a factor for it, `Err` otherwise. `read` is the pricing read for the instrument (production:
/// `CatalogProviderOanda::home_conversions`), called only when a conversion is needed.
///
/// # Errors
/// A refusal when the instrument names no quote currency; a failure of the rate otherwise.
pub fn quoted_rate(
    instrument: &str,
    home: &str,
    read: impl FnOnce(&str) -> Result<HomeConversions, QuoteRateError>,
) -> Result<Option<QuoteRate>, SymbolPropertiesError> {
    let Some(quote) = quote_currency_of(instrument) else {
        let refusal = OandaPropertiesRefusal::NoQuoteToConvert {
            instrument: instrument.to_string(),
            home: home.to_string(),
        };
        return Err(SymbolPropertiesError::Refused(refusal.carried()));
    };
    if quote.eq_ignore_ascii_case(home) {
        return Ok(None);
    }
    let unavailable =
        |refusal: OandaPropertiesRefusal| SymbolPropertiesError::RateUnavailable(refusal.carried());
    let no_rate = |e: QuoteRateError| OandaPropertiesRefusal::NoRate {
        quote: quote.clone(),
        home: home.to_string(),
        why: e.to_string(),
    };
    let rates = read(instrument).map_err(|e| unavailable(no_rate(e)))?;
    // The listing's home currency and the one pricing converts into must be ONE: a mismatch makes every rate
    // wrong at once (`HomeConversions::from_pricing_body`'s identity check guards the same fact from the other
    // side).
    if !rates.home().eq_ignore_ascii_case(home) {
        return Err(unavailable(OandaPropertiesRefusal::RateForAnotherHome {
            quote: quote.clone(),
            home: home.to_string(),
            converts: rates.home().to_string(),
        }));
    }
    rates.quote_rate(&quote).map(Some).map_err(|e| unavailable(no_rate(e)))
}

#[path = "symbol_properties_tests.rs"]
#[cfg(test)]
mod symbol_properties_tests;
