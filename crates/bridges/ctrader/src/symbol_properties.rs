//! **What the gatewayhub asks of cTrader to admit a symbol the engine was not started on** (decision 0174, decision
//! 0183): the account-listed venue's spec ([`SymbolPropertiesAccountSpecCtrader`]: the row fences of a plain and a
//! converting node over the account's own symbol list, and how that list is read: its `SymbolPropertiesListSpec`), the spec of the gatewayhub's guard in front of a rate ask
//! ([`QuoteRateAskSpecCtrader`]), and the ONE place the rate read (a `vike_model::QuoteRate` with the session's deposit
//! currency beside it, [`crate::CatalogProviderCtrader::quote_rate`]) becomes the rate a facts answer carries
//! ([`quoted_rate`]), the twin of oanda's. The listing, its bounds, the guard and the wording of a refusal are the
//! gatewayhub's. The orientation is decided in [`crate::account`] (the mid of the conversion pair, turned the
//! right way round, with USDJPY and GBPUSD goldens); [`quoted_rate`] only reads it out and refuses what cannot be
//! priced.
//!
//! The rate is **account-currency units per ONE unit of the instrument's QUOTE currency** (the engine's
//! orientation, `SymbolAdmission::quote_rate`): a pound is worth more than a dollar (`1.35`), a yen a fraction
//! of one (`1/150`, never `150`; the flipped rate is a 22,500x error).
//!
//! **Fail closed**: a row that names no quote currency, a read that failed and a currency the account lists no
//! single conversion pair for are each a refusal. A read that failed or returned an unusable quote is a failure of
//! the RATE ([`SymbolPropertiesError::RateUnavailable`]), so the node does not remember it as a verdict on the
//! symbol; "this account lists no forex pair for the currency" IS a verdict about the symbol and is worded as one
//! (the node remembers it, so a repeated order does not cost a venue session each time). There is no path that
//! answers `1.0` for "I do not know": the only `None` is the quote being the deposit currency, which needs no
//! conversion. A rate priced by a session for ANOTHER account currency than the listing's is a failure of the
//! rate too.

use std::time::Duration;

use vike_bridge_core::quote_rate::{QuoteRateAskSpec, QuoteRateMemoryConfig};
use vike_bridge_core::symbol_properties::{
    SymbolPropertiesAccountSpec, SymbolPropertiesError, SymbolPropertiesListSpec,
};
use vike_model::{Instrument, QuoteRate, QuoteRateError, SymbolProperties};

use crate::catalog::{any_symbol_refusal, quoted_symbol_refusal};

/// **cTrader's account-listed facts**: the account's symbols are a protobuf session's answer for ONE account (ids are
/// per server, the deposit currency per account), so the gatewayhub keeps one listing per account, and one read of it
/// is a whole authenticated session (a failure is remembered for minutes). The plain fence is
/// [`any_symbol_refusal`] (a forex pair or a CFD of a measured class quoted in the deposit currency), the converting
/// one [`quoted_symbol_refusal`] (every refusal but the foreign quote), both run BEFORE the grid checks so a refusal
/// names the fundamental reason; the broker's grid needs no fence of its own here; a rate is asked for the row's
/// QUOTE currency (a broker's symbol name does not say its quote).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesAccountSpecCtrader;

impl SymbolPropertiesAccountSpec for SymbolPropertiesAccountSpecCtrader {
    const VENUE: &'static str = "ctrader";
    const LISTING_IS_A_SESSION: bool = true;

    fn plain_refusal(_symbol: &str, row: &Instrument, currency: &str) -> Option<String> {
        any_symbol_refusal(row, currency)
    }

    fn quoted_refusal(_symbol: &str, row: &Instrument, currency: &str) -> Option<String> {
        quoted_symbol_refusal(row, currency)
    }

    fn grid_refusal(
        _symbol: &str,
        _row: &Instrument,
        _properties: &SymbolProperties,
    ) -> Option<String> {
        None
    }

    fn rate_subject(_symbol: &str, row: &Instrument) -> String {
        row.quote.trim().to_ascii_uppercase()
    }
}

/// **How the account's list is read** (the gatewayhub's `SymbolFacts`, generic over this spec): the broker's name
/// EXACTLY as the account's symbol list spells it (`EURUSD`; names are case-sensitive: `Wheat_H6`), looked up in that
/// account's WHOLE listing. Only an account's own listing answers: a plain (public) source never does, because its rows
/// would skip the account's currency fence above.
impl SymbolPropertiesListSpec for SymbolPropertiesAccountSpecCtrader {
    fn venue(&self) -> &'static str {
        Self::VENUE
    }

    fn covers(&self, _list_key: &str, account_listed: bool) -> Option<&'static str> {
        account_listed.then_some("the account's own symbol list")
    }

    fn case_kept(&self, _symbol: &str) -> bool {
        true
    }
}

/// How long a rate that was read is handed out AGAIN to the next ask of the same account and currency. A
/// foreign-quoted order asks twice within a second (the LOOK, then the COMMIT that tells the reconcile client,
/// whose reply carries a rate nobody reads), and each ask is a whole authenticated cTrader session. The rate
/// keeps the moment it was ASKED (`QuoteRate::as_of_ms`), so reusing it hides no age: the node judges that
/// itself, and keeps what it is handed for its own `RATE_FRESH_FOR` (30 s), so a rate is at most this plus that
/// old when it is last used, well inside the engine's 120 s window.
pub const RATE_REUSE_FOR: Duration = Duration::from_secs(15);

/// How long a FAILED ask is remembered, per account and currency, before the next ask dials again. A dead or
/// mute host costs one deadline's wait, not one per order or per timer tick. NOT longer than the node's own
/// retry of a failed rate (`RATE_RETRY_AFTER`, 10 s; owner ruling F-8): it was 30 s, so after a blip the node's
/// retries at 10 s and 20 s were answered from memory and an admission stayed refused (ig's `NO_RATE_KEEP` and
/// ibkr's `RATE_FAIL_KEEP` say the same).
pub const RATE_FAILED_FOR: Duration = Duration::from_secs(10);

/// The most cTrader rate asks running at once in the whole hub. The hub allows four symbol-facts lookups in
/// flight for EVERY venue (the gatewayhub's `MAX_SYMBOL_FACTS_INFLIGHT`, not changed here), and a rate ask is the
/// longest of them (a whole authenticated session), so cTrader may hold at most half of them: the rest answer
/// "busy" and the other venues' lookups still find a slot.
pub const HUB_RATE_ASKS: usize = 2;

/// **cTrader's guard in front of a rate ask**: each ask is a whole authenticated session, so an answer is handed out
/// again for [`RATE_REUSE_FOR`] (a failure, a `NoPair` verdict included, for [`RATE_FAILED_FOR`]), one ask per account
/// runs at a time, and at most [`HUB_RATE_ASKS`] in the hub.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuoteRateAskSpecCtrader;

impl QuoteRateAskSpec for QuoteRateAskSpecCtrader {
    const VENUE: &'static str = "ctrader";
    const VENUE_NAME: &'static str = "cTrader";
    const MEMORY: QuoteRateMemoryConfig = QuoteRateMemoryConfig {
        reuse: RATE_REUSE_FOR,
        failed: RATE_FAILED_FOR,
        remember_no_pair: true,
    };
    const HUB_ASKS: usize = HUB_RATE_ASKS;
}

/// The rate for an instrument quoted in `quote` on an account whose deposit currency is `deposit`:
/// `Ok(None)` when it is quoted in `deposit`, `Ok(Some(rate))` when it is quoted in another currency and the
/// venue priced it, `Err` otherwise. `read` is the venue read for the quote currency (production:
/// [`crate::CatalogProviderCtrader::quote_rate`] behind the gatewayhub's guard: the rate and the deposit currency of the
/// session that priced it), called only when a conversion is needed.
///
/// # Errors
/// A verdict when the row names no quote currency or the account lists no pair for it; a failure of the rate
/// otherwise.
pub fn quoted_rate(
    quote: &str,
    deposit: &str,
    read: impl FnOnce(&str) -> Result<(QuoteRate, String), QuoteRateError>,
) -> Result<Option<QuoteRate>, SymbolPropertiesError> {
    let quote = quote.trim().to_ascii_uppercase();
    if quote.is_empty() {
        return Err(SymbolPropertiesError::Refused(format!(
            "ctrader: the instrument names no quote currency, so it cannot be converted into the \
             account's currency ({deposit})"
        )));
    }
    if quote.eq_ignore_ascii_case(deposit) {
        return Ok(None);
    }
    let unavailable = |why: &dyn std::fmt::Display| {
        SymbolPropertiesError::RateUnavailable(format!("no rate for {quote} in {deposit} ({why})"))
    };
    let (rate, account_ccy) = match read(&quote) {
        Ok(read) => read,
        // A verdict about the symbol, not a failed moment: this account lists no (single) pair for it.
        Err(no_pair @ QuoteRateError::NoPair(_)) => {
            return Err(SymbolPropertiesError::Refused(format!(
                "{no_pair}; the symbol is NOT admitted (no rate, no admission: it is never valued at par)"
            )));
        }
        Err(e) => return Err(unavailable(&e)),
    };
    // The read must answer for the currency asked: a rate for another one is the flipped-rate class of bug.
    if !rate.quote_currency.eq_ignore_ascii_case(&quote) {
        return Err(unavailable(&format!("the read answered for {}", rate.quote_currency)));
    }
    // ... and for the ACCOUNT the listing was read for: the listing (kept minutes) states a deposit currency, the
    // session that priced this stated its own. Credentials swapped under one label, a listing not yet replaced,
    // would otherwise book one account's rate on another's ledger (a EUR rate of par on a USD engine).
    if !account_ccy.eq_ignore_ascii_case(deposit) {
        return Err(unavailable(&format!(
            "the session that priced it is for an account in {account_ccy}, and this account's listing says \
             {deposit}"
        )));
    }
    Ok(Some(rate))
}

/// **The deposit currency the broker's asset list names for the account**, or why there is none, in this venue's
/// words: the lenient listing ([`crate::CatalogProviderCtrader::account_listing_lenient`]) hands back the deposit it NAMES,
/// or none, and the gatewayhub fills an unnamed one in with the owner's USD, ASSUMED (flagged and said once).
///
/// # Errors
/// The words for a deposit the asset list does not name.
pub fn deposit_named(deposit: Option<String>) -> Result<String, String> {
    deposit.ok_or_else(|| {
        "ctrader: the broker's asset list does not name this account's deposit currency".to_string()
    })
}

#[path = "symbol_properties_tests.rs"]
#[cfg(test)]
mod symbol_properties_tests;
