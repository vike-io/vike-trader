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
/// The ledger currency the answer is expressed in is stated either way (`account_ccy`), so the node can refuse
/// an answer in a currency its engine was not anchored to.
pub(crate) fn wire_facts(facts: ConvertedFacts) -> WireSymbolFacts {
    let mut wire = WireSymbolFacts {
        properties: facts.properties,
        account_ccy: Some(facts.account_ccy),
        // A metal, index or CFD whose contract multiplier is not 1 (`None` for a forex pair and for 1, which is the
        // wire's default), and the symbol's own margin rate when the venue states one.
        contract_value: facts.contract_value,
        margin_rate: facts.margin_rate,
        ..WireSymbolFacts::default()
    };
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

/// What `TradeLane::account_currency_stated` answers for fxcm. Only a `demo` account runs; any other tier
/// states nothing, so its engine keeps the label it has.
///
/// ⚠ **A currency the API STATES is told as stated, whatever the engine is mounted on** (second review, HIGH-2): the
/// lane used to decline a stated EUR when the mount was the USD-quoted `EURUSD` and answer `(None, false)`, and the
/// node turned "told nothing" into "USD, assumed" for an account the venue had just said is EUR. Whether the ledger
/// may be anchored to it is the node's mount fence's decision (`unanchorable_mount`: one rule for every venue, in
/// its own words), exactly as for oanda, ctrader, dukascopy and ig. Only a malformed statement is still
/// `(None, false)`.
pub(crate) fn currency_for(tier: AccountTier, account: Option<&str>) -> (Option<String>, bool) {
    if tier != AccountTier::Demo {
        return (None, false);
    }
    match registry_key(account) {
        Ok(key) => account_currency(&key, vike_fxcm::reads::told_account_currency),
        Err(why) => {
            tracing::warn!(venue = "fxcm", "{why}");
            (None, false)
        }
    }
}

/// **Whether the account's running reconcile session decided its currency by ASSUMPTION** (the owner's USD, because
/// the shim cannot state `BASE_CRNCY`): then FXCM's balance, a home-currency figure with no currency beside it
/// (`vike_fxcm::home_conversion`'s module doc: the Accounts table has no currency column), is not handed to the
/// node ([`crate::trade::lane::TradeLane::balance_withheld`]), whose ledger is labelled with the assumed currency.
/// `false` for an account with no session (it has no reconcile client either) and for a currency the venue stated.
pub(crate) fn currency_is_assumed(account: Option<&str>) -> bool {
    let Ok(key) = registry_key(account) else { return false };
    vike_fxcm::reads::account_reads(&key)
        .is_some_and(|reads| reads.account_currency().is_ok_and(|ccy| ccy.is_assumed()))
}

/// **The currency the account's running reconcile session decided by the VENUE's own word** (`BASE_CRNCY`), or
/// `None` when it assumed USD, was never asked, could not decide, or the account has no session. The proof a rate
/// stamp of fxcm lacks on its own (`WireSymbolFacts::account_ccy_stated`): `None` is "not proven", never "stated
/// nothing".
pub(crate) fn currency_stated(account: Option<&str>) -> Option<String> {
    let key = registry_key(account).ok()?;
    let ccy = vike_fxcm::reads::account_reads(&key)?.account_currency().ok()?;
    (!ccy.is_assumed()).then_some(ccy.code)
}

#[path = "fxcm_rate_tests.rs"]
#[cfg(test)]
mod fxcm_rate_tests;
