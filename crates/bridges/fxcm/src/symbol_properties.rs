//! **What the gatewayhub asks of FXCM for a node that converts a quote currency** (decision 0183, phase 1): the ONE place
//! FXCM's conversion facts become an answer, so the orientation and the "assumed" flag each have one home and one test
//! (`symbol_properties_tests.rs`). The gatewayhub calls [`facts_for`] into its wire answer and words a refusal
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`); this module names no wire.
//!
//! FXCM is the venue with no REST and no extra login to spend: the account's currency, the instrument's
//! quote currency and the conversion pair's quote all come from the account's RUNNING reconcile session
//! ([`crate::account::reads`]), so this lane opens nothing. What it answers:
//!
//! * the facts of a symbol for a node that converts: the grid, and the quote currency, its rate and its stamp for a
//!   symbol quoted in another currency than the account's. The rate is **account-currency units per ONE
//!   quote-currency unit** (`vike_model::AccountPerQuote`, orientation in its type): USD/JPY on a USD account is about
//!   `1/150`, never `150`;
//! * the account's currency at a start, with whether it is ASSUMED. The venue's API carries it in the system
//!   property `BASE_CRNCY`; when it cannot be read the owner's ruling (2026-10-19, "check the currency by API;
//!   if you can't, USD") fills in USD, which is NEVER silent: the flag travels on the wire
//!   (`AdapterReply::Running::account_currency_assumed`) and [`crate::account::reads`] says it at `warn`.
//!
//! **Fail closed**: a rate that could not be had is a failure of the RATE
//! ([`SymbolPropertiesError::RateUnavailable`]), so the node does not remember it as a verdict on the symbol; a verdict on
//! the symbol (not listed, not a regular forex instrument, no lot) is worded as itself.

use vike_bridge_core::quote_rate::SymbolPropertiesQuote;
use vike_bridge_core::symbol_properties::{SymbolPropertiesAnswer, SymbolPropertiesError};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};

use crate::account::quote_rate::{AccountCurrency, FxcmPropertiesError};

/// The kind of "no" a failed answer is: the RATE's failures are "not now" (the node forgets them), a verdict on the
/// symbol is its own words.
fn refusal(error: &FxcmPropertiesError) -> SymbolPropertiesError {
    if error.is_rate() {
        SymbolPropertiesError::RateUnavailable(error.message().to_string())
    } else {
        SymbolPropertiesError::Refused(error.message().to_string())
    }
}

/// The facts of `symbol` for account `account` (`""` = the default), read through `read`: production passes
/// [`crate::account::reads::quoted_facts`]. The ledger currency the answer is expressed in is stated either way; a metal,
/// index or CFD whose contract multiplier is not 1 carries it (`None` for a forex pair and for 1, the wire's default),
/// and the symbol's own margin rate when the venue states one.
///
/// # Errors
/// The rate's failure (not now) or a verdict on the symbol.
pub fn quoted_facts<A: SymbolPropertiesAnswer>(
    account: &str,
    symbol: &str,
    read: impl FnOnce(&str, &str) -> Result<SymbolPropertiesQuote, FxcmPropertiesError>,
) -> Result<A, SymbolPropertiesError> {
    read(account, symbol)
        .map(|facts| {
            A::of(
                facts.properties,
                facts.contract_value,
                facts.margin_rate,
                facts.quote,
                Some(facts.account_currency),
            )
        })
        .map_err(|e| refusal(&e))
}

/// The account's currency for the start's reply, as `(currency, assumed)`: the venue's `BASE_CRNCY` as stated
/// (`assumed == false`), or the ruling's USD (`assumed == true`) when the API cannot give it. `told` is
/// production's [`crate::account::reads::told_account_currency`]. Only a malformed statement is `(None, false)`: the
/// engine then keeps the label it has and the node refuses a symbol quoted in another currency, never a guess
/// over a wrong answer.
pub fn account_currency(
    account: &str,
    told: impl FnOnce(&str) -> Result<AccountCurrency, String>,
) -> (Option<String>, bool) {
    match told(account) {
        Ok(currency) => {
            let assumed = currency.is_assumed();
            (Some(currency.code), assumed)
        }
        Err(why) => {
            let shown = if account.is_empty() { "default" } else { account };
            tracing::warn!(
                venue = "fxcm",
                account = shown,
                "fxcm: the account's currency could not be settled ({why}); the engine keeps the ledger \
                 label it has and the node refuses a symbol quoted in another currency"
            );
            (None, false)
        }
    }
}

/// The registry label of the account a request names, as this crate registered its session under it
/// (`""` = the default account). A label the wire spelling does not admit names no account.
fn registry_key(account: Option<&str>) -> Result<String, String> {
    let label = match account {
        None => AccountLabel::Default,
        Some(text) => parse_wire_account(text).map_err(|_| {
            format!("fxcm: `{text}` is not an account label, so there is no session to ask")
        })?,
    };
    Ok(label.text().unwrap_or_default().to_string())
}

/// What the gatewayhub answers a converting node for fxcm: the converted facts of `symbol` on the account's running
/// reconcile session. Only a `demo` account has one (the adapter dials the demo login alone).
///
/// # Errors
/// Any other tier, a label that names no account, the rate's failure or a verdict on the symbol.
pub fn facts_for<A: SymbolPropertiesAnswer>(
    tier: AccountTier,
    account: Option<&str>,
    symbol: &str,
) -> Result<A, SymbolPropertiesError> {
    if tier != AccountTier::Demo {
        return Err(SymbolPropertiesError::Refused(
            "fxcm: this adapter trades its demo account only (a `demo` account), so no \
             instrument is looked up for any other tier"
                .to_string(),
        ));
    }
    let key = registry_key(account).map_err(SymbolPropertiesError::Refused)?;
    quoted_facts(&key, symbol, crate::account::reads::quoted_facts)
}

/// What the gatewayhub answers for fxcm's account currency at a start. Only a `demo` account runs; any other tier
/// states nothing, so its engine keeps the label it has.
///
/// ⚠ **A currency the API STATES is told as stated, whatever the engine is mounted on** (second review, HIGH-2): the
/// lane used to decline a stated EUR when the mount was the USD-quoted `EURUSD` and answer `(None, false)`, and the
/// node turned "told nothing" into "USD, assumed" for an account the venue had just said is EUR. Whether the ledger
/// may be anchored to it is the node's mount fence's decision (`unanchorable_mount`: one rule for every venue, in
/// its own words), exactly as for oanda, ctrader, dukascopy and ig. Only a malformed statement is still
/// `(None, false)`.
#[must_use]
pub fn currency_for(tier: AccountTier, account: Option<&str>) -> (Option<String>, bool) {
    if tier != AccountTier::Demo {
        return (None, false);
    }
    match registry_key(account) {
        Ok(key) => account_currency(&key, crate::account::reads::told_account_currency),
        Err(why) => {
            tracing::warn!(venue = "fxcm", "{why}");
            (None, false)
        }
    }
}

/// **Whether the account's running reconcile session decided its currency by ASSUMPTION** (the owner's USD, because
/// the shim cannot state `BASE_CRNCY`): then FXCM's balance, a home-currency figure with no currency beside it
/// ([`crate::account::quote_rate`]'s module doc: the Accounts table has no currency column), is not handed to the
/// node (the gatewayhub's `TradeLane::balance_withheld`), whose ledger is labelled with the assumed currency.
/// `false` for an account with no session (it has no reconcile client either) and for a currency the venue stated.
#[must_use]
pub fn currency_is_assumed(account: Option<&str>) -> bool {
    let Ok(key) = registry_key(account) else { return false };
    crate::account::reads::account_reads(&key)
        .is_some_and(|reads| reads.account_currency().is_ok_and(|currency| currency.is_assumed()))
}

/// **The currency the account's running reconcile session decided by the VENUE's own word** (`BASE_CRNCY`), or
/// `None` when it assumed USD, was never asked, could not decide, or the account has no session. The proof a rate
/// stamp of fxcm lacks on its own (the wire's `account_currency_stated`): `None` is "not proven", never "stated
/// nothing".
#[must_use]
pub fn currency_stated(account: Option<&str>) -> Option<String> {
    let key = registry_key(account).ok()?;
    let currency = crate::account::reads::account_reads(&key)?.account_currency().ok()?;
    (!currency.is_assumed()).then_some(currency.code)
}

#[path = "symbol_properties_tests.rs"]
#[cfg(test)]
mod symbol_properties_tests;
