//! `fxcm_rate` — the ONE place FXCM's conversion facts become what the wire carries (decision 0183, phase 1),
//! so the orientation and the "assumed" flag each have one home and one test (`fxcm_rate_tests.rs`).
//!
//! FXCM is the venue with no REST and no extra login to spend: the account's currency, the instrument's
//! quote currency and the conversion pair's quote all come from the account's RUNNING reconcile session
//! (`vike_fxcm::reads`), so this lane opens nothing. What it carries:
//!
//! * the facts of a symbol for a node that converts: the grid, and `quote_ccy` / `quote_rate` /
//!   `rate_as_of_ms` for a symbol quoted in another currency than the account's. The rate is **account-currency
//!   units per ONE quote-currency unit** (`vike_fxcm::home_conversion::AccountPerQuote`, orientation in its
//!   type): USD/JPY on a USD account is about `1/150`, never `150`;
//! * the account's currency at a start, with whether it is ASSUMED. The venue's API carries it in the system
//!   property `BASE_CRNCY`; when it cannot be read the owner's ruling (2026-10-19, "check the currency by API;
//!   if you can't, USD") fills in USD, which is NEVER silent: the flag travels on the wire
//!   (`AdapterReply::Live::account_ccy_assumed`) and `vike_fxcm::reads` says it at `warn`.
//!
//! **Fail closed**: a rate that could not be had is worded as a failure of the RATE
//! ([`FACTS_RATE_UNAVAILABLE`]), so the node does not remember it as a verdict on the symbol; a verdict on the
//! symbol (not listed, not a regular forex instrument, no lot) is worded as itself.

use vike_fxcm::home_conversion::{ConvertedFacts, FactsError};
use vike_gatewayhub_client::proto::{FACTS_RATE_UNAVAILABLE, WireSymbolFacts};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};

/// The wire facts of a converted answer: the grid, and the quote currency with its rate and its stamp when the
/// symbol is quoted in another currency than the account's (absent at par, exactly as an older daemon answers).
pub(crate) fn wire_facts(facts: ConvertedFacts) -> WireSymbolFacts {
    let mut wire = WireSymbolFacts { properties: facts.properties, ..WireSymbolFacts::default() };
    if let Some(quote) = facts.quote {
        wire.quote_ccy = Some(quote.quote_ccy);
        wire.quote_rate = Some(quote.account_per_quote.get());
        wire.rate_as_of_ms = Some(quote.as_of_ms);
    }
    wire
}

/// The refusal text of a failed answer: the RATE's failures carry [`FACTS_RATE_UNAVAILABLE`] (the node
/// forgets them), a verdict on the symbol is its own words.
pub(crate) fn refusal(error: &FactsError) -> String {
    if error.is_rate() {
        format!("fxcm: {FACTS_RATE_UNAVAILABLE}: {}", error.message())
    } else {
        error.message().to_string()
    }
}

/// The facts of `symbol` for account `account` (`""` = the default), read through `read`: production passes
/// `vike_fxcm::reads::quoted_facts`.
pub(crate) fn quoted_facts(
    account: &str,
    symbol: &str,
    read: impl FnOnce(&str, &str) -> Result<ConvertedFacts, FactsError>,
) -> Result<WireSymbolFacts, String> {
    read(account, symbol).map(wire_facts).map_err(|e| refusal(&e))
}

/// The account's currency for the start's reply, as `(currency, assumed)`: the venue's `BASE_CRNCY` as stated
/// (`assumed == false`), or the ruling's USD (`assumed == true`) when the API cannot give it. `told` is
/// production's `vike_fxcm::reads::told_account_currency`. Only a malformed statement is `(None, false)`: the
/// engine then keeps the label it has and the node refuses a symbol quoted in another currency, never a guess
/// over a wrong answer.
pub(crate) fn account_currency(
    account: &str,
    told: impl FnOnce(&str) -> Result<vike_fxcm::home_conversion::AccountCcy, String>,
) -> (Option<String>, bool) {
    match told(account) {
        Ok(ccy) => {
            let assumed = ccy.is_assumed();
            (Some(ccy.code), assumed)
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

/// The registry label of the account a request names, as `vike_fxcm` registered its session under it
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

/// What `BridgeTrade::symbol_facts_quoted` answers for fxcm: the converted facts of `symbol` on the account's
/// running reconcile session. Only a `demo` account has one (the adapter dials the demo login alone).
pub(crate) fn facts_for(
    tier: AccountTier,
    account: Option<&str>,
    symbol: &str,
) -> Result<WireSymbolFacts, String> {
    if tier != AccountTier::Demo {
        return Err("fxcm: this adapter trades its demo account only (a `demo` account), so no \
                    instrument is looked up for any other tier"
            .to_string());
    }
    quoted_facts(&registry_key(account)?, symbol, vike_fxcm::reads::quoted_facts)
}

/// **The currency the engine's ledger may be anchored to, given the money of the engine's MOUNTED symbol.** The
/// mounted symbol is not admitted with a rate (only a symbol the door admits is), so its fills are booked at par
/// in the account's currency: anchoring the ledger to a currency other than the mounted pair's quote currency
/// would book (say) EUR/USD's dollars as euros under a label that says EUR, which is worse than the label it has
/// (the same fence dukascopy's lane has, `ledger_currency`). So a currency that differs from the mount's money,
/// or a mount whose money cannot be told, is not handed to the node: `(None, false)`, said at `warn`, and the
/// engine keeps its label and refuses a symbol quoted in another currency. `None` in, `None` out.
pub(crate) fn anchorable(
    told: (Option<String>, bool),
    mount_money: Option<&str>,
) -> (Option<String>, bool) {
    let (ccy, assumed) = told;
    let Some(ccy) = ccy else { return (None, assumed) };
    match mount_money {
        Some(money) if money.eq_ignore_ascii_case(&ccy) => (Some(ccy), assumed),
        Some(money) => {
            tracing::warn!(
                venue = "fxcm",
                "fxcm: the account's currency is {ccy} but the engine is mounted on a pair that counts its \
                 money in {money}; the ledger is NOT anchored to {ccy} (the mounted symbol's PnL would be \
                 booked as {ccy} at par), so a symbol quoted in another currency stays refused"
            );
            (None, false)
        }
        None => {
            tracing::warn!(
                venue = "fxcm",
                "fxcm: the money of the engine's mounted symbol cannot be told, so the ledger is NOT anchored \
                 to the account's currency ({ccy})"
            );
            (None, false)
        }
    }
}

/// What `TradeLane::account_currency_stated` answers for fxcm. Only a `demo` account runs; any other tier
/// states nothing, so its engine keeps the label it has. The currency comes through [`anchorable`].
///
/// `mount_symbol` is the symbol THIS engine was started on (`AdapterStart::symbol`), never the roster's wired
/// symbol: a labelled account can be mounted on its own row's symbol (`USDJPY` on a USD account), and fencing the
/// wired `EURUSD` instead would anchor USD under yen fills (booked at par, 150 times too large).
pub(crate) fn currency_for(
    tier: AccountTier,
    account: Option<&str>,
    mount_symbol: &str,
) -> (Option<String>, bool) {
    if tier != AccountTier::Demo {
        return (None, false);
    }
    let money = vike_fxcm::home_conversion::quote_of_symbol(mount_symbol);
    match registry_key(account) {
        Ok(key) => anchorable(
            account_currency(&key, vike_fxcm::reads::told_account_currency),
            money.as_deref(),
        ),
        Err(why) => {
            tracing::warn!(venue = "fxcm", "{why}");
            (None, false)
        }
    }
}

#[path = "fxcm_rate_tests.rs"]
#[cfg(test)]
mod fxcm_rate_tests;
