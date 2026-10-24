//! `ibkr_rate` — the ONE place IBKR's live forex rate becomes the rate a facts answer carries (decision 0183,
//! phase 1), so the orientation has one home (`vike_ibkr::fx_rate`, USDJPY / EURUSD / EURGBP goldens) and the
//! assembly of an answer has one test (`ibkr_rate_tests.rs`).
//!
//! The rate is **ledger-currency units per ONE unit of the stock's QUOTE currency**, the engine's orientation
//! (`SymbolAdmission::quote_rate`): a euro is worth more than a dollar (`1.10`), a yen a fraction of one
//! (`1/150`, never `150`). `vike_ibkr::fx_rate::AccountPerQuote` carries that direction in its type and this
//! module only reads it out.
//!
//! An answer is assembled from two reads of different ages, and they are kept apart on purpose:
//!
//! * the stock's GRID (tick, lot, minimums), kept by [`IbkrFacts`] for ten minutes like every IBKR answer;
//! * the RATE, asked of the gateway source on EVERY ask and never kept here (the node reuses it for its own 30 s
//!   and re-reads it for a held position about once a minute): a rate served from the facts' keep time would be
//!   ten minutes old on arrival, and the engine calls a rate stale at 120 s. The source reuses one forex read per
//!   CURRENCY for `vike_ibkr::any_symbol_socket::RATE_KEEP` (30 s), so the ten symbols a refresh pass walks in one
//!   currency cost one read, not ten.
//!
//! The grid and the rate of one ask ride ONE gateway connection (`vike_ibkr::any_symbol_socket::FactsScope`): the
//! closures handed to [`quoted_facts`] share it, so a new foreign stock costs the data lane one dial, not two.
//!
//! **Fail closed**: a name that is no stock name, a grid that could not be had, a base currency the account
//! summary does not state, a currency with no forex pair, a minor-unit currency, a FOREIGN stock whose contract
//! price magnifier is not 1 and a quote that is not a rate (delayed, one-sided, crossed, wide) are each a
//! refusal in words. A rate the gateway could not give is worded
//! as a failure of the RATE ([`FACTS_RATE_UNAVAILABLE`]), so the node does not remember it as a verdict on the
//! symbol. There is no path that answers `1.0` for "I do not know": the only answer without a rate is a stock
//! quoted in the account's own base currency, which needs no conversion.

use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{FACTS_RATE_UNAVAILABLE, WireSymbolFacts};
use vike_ibkr::any_symbol::{FactsError, StockFacts, parse_stock_name, price_magnifier_refusal};
use vike_ibkr::any_symbol_socket::RateAnswer;
use vike_ibkr::fx_rate::minor_unit_refusal;

use super::ibkr_facts::IbkrFacts;

/// The whole of what an adapter's start spends on reading the account's base currency, dial and retries
/// together. The start holds the account's adapter lock while it waits and the node waits for the start's reply
/// (`gatewayhub_connect_wait_ms`, a minute by default): a read that took longer would turn a slow gateway into
/// a start that failed. What the bound cuts off is not lost: the account is anchored to the currency this
/// process read before (`IbkrFactsSource::remembered_base`) or, on a restart, to the one its journal carries.
pub(crate) const BASE_BOUND: Duration = Duration::from_secs(8);

/// How long to wait between two asks inside [`BASE_BOUND`]. The data lane of the gateway's gate refuses a dial
/// at once while another dial holds its turn (another account of the same gateway starting) or the previous one
/// ended less than the connect gap ago, so a first refusal is the ordinary case and not an outage.
pub(crate) const BASE_PAUSE: Duration = Duration::from_secs(3);

/// The facts of the stock `name` (`TICKER.SMART.CCY`, upper case) for an account that converts a quote
/// currency: its grid from `facts`' keep (fetched by `stock` when stale) and its rate read just now by
/// `rate_for` (given the name's currency). `Err` is the operator's sentence.
pub(crate) fn quoted_facts(
    facts: &IbkrFacts,
    name: &str,
    stock: impl FnOnce() -> Result<StockFacts, FactsError>,
    rate_for: impl FnOnce(&str) -> Result<RateAnswer, FactsError>,
) -> Result<WireSymbolFacts, String> {
    // Before the budget is spent and before any dial: a name that is not a stock name is refused in the
    // words the plain path uses.
    let (_, currency) = parse_stock_name(name)?;
    // A name in a currency this adapter refuses by name (pence, agorot, cents: `minor_unit_refusal`) is refused
    // BEFORE its grid is fetched, in the words the rate read gives: the grid costs a data dial of a scarce hourly
    // budget, and it was spent on every such name every ten minutes for an answer that was always the same.
    if let Some(why) = minor_unit_refusal(&currency) {
        return Err(format!("ibkr: {why}"));
    }
    let mut wire = facts.facts(name, stock)?;
    let answer = rate_for(&currency).map_err(|why| match why {
        FactsError::Refused(why) => why,
        FactsError::Unavailable(why) => {
            format!("ibkr: {FACTS_RATE_UNAVAILABLE}: no rate for {currency} ({why})")
        }
    })?;
    if let Some(rate) = answer.rate {
        // A FOREIGN stock (a rate exists exactly when its currency is not the base): the unit its prices are
        // reported in must be known. The contract's `priceMagnifier` is that statement, `1` for whole units; a `0`
        // the gateway left unset is not one. A stock in the base currency (no rate) and the plain path never get
        // here, so an account that converts nothing admits what it always did. A magnifier that was not recorded
        // (it always is, by `facts`) refuses too: no price in an unknown unit is booked.
        let refusal = match facts.price_magnifier(name) {
            Some(magnifier) => price_magnifier_refusal(name, &currency, magnifier),
            None => Some(format!(
                "{name}: the contract's price magnifier was not read, so the unit its prices are reported in \
                 is not known and a stock of another currency than the account's is not admitted on it"
            )),
        };
        if let Some(why) = refusal {
            return Err(format!("ibkr: {why}"));
        }
        wire.quote_ccy = Some(rate.quote_ccy);
        wire.quote_rate = Some(rate.account_per_quote.get());
        wire.rate_as_of_ms = Some(rate.as_of_ms);
        // The ledger currency the rate is expressed in, as THIS read found it: the node refuses a rate that is
        // not in the currency its engine was anchored to.
        wire.account_ccy = Some(answer.base);
    }
    Ok(wire)
}

/// The account's base currency, asked until `total` is spent with `pause` between asks: `Some` the moment the
/// venue states it, `None` when it does not within `total` (the caller then falls back to the currency this
/// process read before, if any; else the account is left unanchored and a stock of another currency stays
/// refused). `read` is handed the time left, so no ask outlives the bound. A refusal (a cpapi account, a summary
/// that names no single currency) is final and is not retried; an `Unavailable` answer is said once, at the end.
pub(crate) fn read_base_currency(
    mut read: impl FnMut(Duration) -> Result<String, FactsError>,
    total: Duration,
    pause: Duration,
) -> Option<String> {
    let end = Instant::now() + total;
    let mut tries = 0_u32;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        tries += 1;
        match read(left) {
            Ok(base) => return Some(base),
            Err(FactsError::Refused(why)) => {
                tracing::debug!("ibkr: the account's base currency is not read: {why}");
                return None;
            }
            Err(FactsError::Unavailable(why)) => {
                // Another ask only fits when the pause and something after it do.
                if end.saturating_duration_since(Instant::now()) <= pause {
                    tracing::warn!(
                        "ibkr: the account's base currency could not be read in {} s ({tries} tries: \
                         {why})",
                        total.as_secs()
                    );
                    return None;
                }
                std::thread::sleep(pause);
            }
        }
    }
}

#[path = "ibkr_rate_tests.rs"]
#[cfg(test)]
mod ibkr_rate_tests;
