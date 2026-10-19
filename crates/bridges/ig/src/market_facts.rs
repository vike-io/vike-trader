//! IG's DEALING FACTS of one epic, for the control door's any-symbol admission (decision 0174).
//!
//! The engine can judge an order on an epic it was not started on only if it is told the epic's tick, its
//! minimum deal size and what ONE unit of `size` is worth. IG publishes none of it in a list: it is
//! `GET /markets/{epic}` on an authenticated session, and the account's currency is `GET /accounts`.
//! [`IgFactsSource`] reads both over ONE logged-in session it keeps (a 401 re-logs it in,
//! `crate::rest`'s ladder) and [`market_facts`] turns the pair into facts, or into a REFUSAL by name.
//!
//! # What is admitted, and why only that
//!
//! **Only a CURRENCIES epic quoted in the account's currency, on an account whose currency is USD.** What
//! `size` MEANS differs per epic class (a contract count at a contract size, a stake per index point, a
//! share count priced in pence) and none of it has been measured against the venue: only `minDealSize`
//! 0.1 on EUR/USD mini was ever read live. So every other class is refused by name
//! ([`class_refusal`]), and the CURRENCIES case is admitted only when the payload agrees with itself:
//!
//! * the **contract multiplier** is `contractSize`, and it is accepted only if `contractSize x pip` equals
//!   `valueOfOnePip` (two independent fields of the one payload; in the account's currency because the
//!   quote currency IS the account's): a market whose size semantics are not "contracts of `contractSize`
//!   units" fails this and is refused ([`ig_size_semantics_refusal`] words it);
//! * the **price grid** is the PIP (`onePipMeans`), never a finer step: a level on the pip grid is on
//!   every finer grid IG may use, and it is what [`crate::exec`]'s five-decimal `fmt_level` writes
//!   exactly. A pip that is not a whole multiple of 0.00001 is refused;
//! * the **size grid** is `minDealSize` as BOTH minimum and step (a multiple of the minimum is valid
//!   whatever the finer step is; the venue publishes no step);
//! * the **currency**: this adapter writes `currencyCode: "USD"` and `expiry: "-"` on every order
//!   ([`WIRE_CURRENCY`], [`WIRE_EXPIRY`]), so an epic dealt in another currency or dated, a spread bet
//!   (`DFB`) included, is refused, and so is a non-USD account. The engine's ledger is one currency and
//!   nothing converts a quote currency (decision 0170), so an epic not quoted in the account's currency is
//!   refused too: its PnL would be mis-valued, and overstated PnL feeds the margin-call sweep.
//!
//! Not carried, and owed a demo proof: `marginFactor` (the engine margins at its own configured rate),
//! a maximum deal size, a minimum notional.
//!
//! ⚠ The market-details shape here is the DOCUMENTED one, hand-written in the tests: no capture of
//! `GET /markets/{epic}` is checked in. A field that is absent or unreadable is a refusal, never a default.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use vike_bridge_core::json::get_f64;
use vike_model::{ContractValue, SymbolProperties};

use crate::config::IgConfig;
use crate::rest::IgSession;

/// The `currencyCode` [`crate::exec::build_request`] writes on every order, whatever the epic or the
/// account. The facts admit only an epic this value is right for.
pub(crate) const WIRE_CURRENCY: &str = "USD";

/// The `expiry` every order body carries (cash/daily only: this bridge wires no dated epic).
pub(crate) const WIRE_EXPIRY: &str = "-";

/// The finest price step `fmt_level` can write (it writes five decimals), so the finest an admitted epic's
/// pip may be.
pub(crate) const WIRE_PRICE_STEP: f64 = 0.00001;

/// The facts of one admitted epic: the grid in the engine's units (size in IG contracts) and what one unit
/// of size is worth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IgMarketFacts {
    /// Tick = the pip, step = minimum = `minDealSize`; no maximum, no minimum notional, no contract size
    /// (the multiplier rides [`Self::contract_value`], so it is not scaled twice).
    pub properties: SymbolProperties,
    /// Linear, `contractSize` units of the base currency per unit of size.
    pub contract_value: ContractValue,
}

/// Why an epic of `class` (`instrument.type`) is refused, or `None` for the one admitted class.
fn class_refusal(epic: &str, class: &str) -> Option<String> {
    let what = match class {
        "CURRENCIES" => return None,
        "INDICES" => {
            "an index's size is a stake per index point, or contracts at an exchange point value, in the index's own currency"
        }
        "SHARES" => {
            "a share's size is a share count priced in the share's own currency (pence, GBX, for a UK line)"
        }
        "COMMODITIES" => "a commodity's size is contracts of a market-specific contract size",
        "CRYPTOCURRENCY" => {
            "a crypto market's size is a coin amount at a market-specific contract size"
        }
        "BONDS" => "a bond's size is contracts at a market-specific contract size",
        c if c.starts_with("OPT_") => {
            "an option's size, premium and expiry are not a linear position"
        }
        _ => "its size semantics are not known",
    };
    Some(format!(
        "ig: {epic} is a {class} market and {what}; what `size` means per epic class is UNMEASURED, \
         so only CURRENCIES epics are admitted"
    ))
}

/// The sentence for a CURRENCIES market whose own fields disagree about what one unit of size is worth.
fn ig_size_semantics_refusal(epic: &str, contract: f64, pip: f64, per_pip: f64) -> String {
    format!(
        "ig: {epic}: contractSize {contract} x pip {pip} = {} but valueOfOnePip says {per_pip}, so the \
         market's size is not 'contracts of contractSize units' (UNMEASURED); it is not admitted",
        contract * pip
    )
}

/// `onePipMeans`, `"0.0001 USD/EUR"`: the pip in quote units, then `QUOTE/BASE`. `None` for any other shape.
fn pip_and_pair(text: &str) -> Option<(f64, String, String)> {
    let mut words = text.split_whitespace();
    let pip: f64 = words.next()?.parse().ok()?;
    let pair = words.next()?;
    if words.next().is_some() || !pip.is_finite() || pip <= 0.0 {
        return None;
    }
    let (quote, base) = pair.split_once('/')?;
    let code = |c: &str| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic());
    (code(quote) && code(base))
        .then(|| (pip, quote.to_ascii_uppercase(), base.to_ascii_uppercase()))
}

/// The facts of `epic` out of its `GET /markets/{epic}` body and the account's currency, or the reason
/// they cannot be had. Pure: the module doc is the authority on every refusal.
pub fn market_facts(
    epic: &str,
    market: &Value,
    account_currency: &str,
) -> Result<IgMarketFacts, String> {
    let Some(instrument) = market.get("instrument").filter(|i| i.is_object()) else {
        return Err(format!("ig: the market details of {epic} carry no `instrument` block"));
    };
    let text = |key: &str| instrument.get(key).and_then(Value::as_str).unwrap_or("");
    // The venue's own spelling must be the one asked (the recon client and the engine compare it
    // exactly): an answer for another epic, or for the same one in another case, is not this epic.
    if text("epic") != epic {
        return Err(format!(
            "ig: GET /markets/{epic} answered for `{}`, so this epic's spelling is not the venue's",
            text("epic")
        ));
    }
    if let Some(why) = class_refusal(epic, text("type")) {
        return Err(why);
    }
    if text("expiry") != WIRE_EXPIRY {
        return Err(format!(
            "ig: {epic} has expiry `{}` and this adapter sends `{WIRE_EXPIRY}` (an undated cash CFD) on \
             every order, so a dated contract or a spread bet is not admitted",
            text("expiry")
        ));
    }
    if text("unit") != "CONTRACTS" || get_f64(instrument, "lotSize") != 1.0 {
        return Err(format!(
            "ig: {epic} is sized in `{}` with a lot of {}, not in single CONTRACTS, so its size is not \
             a contract count",
            text("unit"),
            get_f64(instrument, "lotSize")
        ));
    }
    let Some((pip, quote, base)) = pip_and_pair(text("onePipMeans")) else {
        return Err(format!(
            "ig: {epic}: `onePipMeans` (`{}`) is not '<pip> <QUOTE>/<BASE>', so the pip and the quote \
             currency cannot be read",
            text("onePipMeans")
        ));
    };
    // A second, independent witness of the pair: the epic itself (`CS.D.EURUSD.MINI.IP`).
    if epic.split('.').nth(2) != Some(format!("{base}{quote}").as_str()) {
        return Err(format!(
            "ig: {epic}: the epic does not name the pair {base}/{quote} that `onePipMeans` states, so \
             the quote currency is not certain"
        ));
    }
    if !quote.eq_ignore_ascii_case(account_currency) {
        return Err(format!(
            "ig: {epic} is quoted in {quote} but this account's currency is {account_currency}, and the \
             engine books money in ONE currency (nothing converts a quote currency), so a position in \
             it would be mis-valued; only instruments quoted in {account_currency} are admitted"
        ));
    }
    if !account_currency.eq_ignore_ascii_case(WIRE_CURRENCY) {
        return Err(format!(
            "ig: this account's currency is {account_currency} and this adapter sends currencyCode \
             {WIRE_CURRENCY} on every order, so only a {WIRE_CURRENCY} account is admitted (a per-order \
             currency is an adapter change)"
        ));
    }
    let dealt_in_wire_currency =
        instrument.get("currencies").and_then(Value::as_array).is_some_and(|rows| {
            rows.iter().any(|c| c.get("code").and_then(Value::as_str) == Some(WIRE_CURRENCY))
        });
    if !dealt_in_wire_currency {
        return Err(format!(
            "ig: {epic} lists no {WIRE_CURRENCY} among its dealing currencies, and this adapter sends \
             currencyCode {WIRE_CURRENCY}"
        ));
    }
    let (contract, per_pip) =
        (get_f64(instrument, "contractSize"), get_f64(instrument, "valueOfOnePip"));
    if !(contract.is_finite() && contract > 0.0 && per_pip.is_finite() && per_pip > 0.0) {
        return Err(format!(
            "ig: {epic} carries no contractSize or no valueOfOnePip (missing or zero), so what one unit \
             of size is worth is unknown"
        ));
    }
    if (contract * pip - per_pip).abs() > 1e-6 * per_pip.max(1.0) {
        return Err(ig_size_semantics_refusal(epic, contract, pip, per_pip));
    }
    let min_deal = market
        .pointer("/dealingRules/minDealSize/value")
        .and_then(vike_bridge_core::json::json_num)
        .unwrap_or(0.0);
    if !min_deal.is_finite() || min_deal <= 0.0 {
        return Err(format!("ig: {epic} publishes no minimum deal size (missing or zero)"));
    }
    let steps = pip / WIRE_PRICE_STEP;
    if steps.round() < 1.0 || (steps - steps.round()).abs() > 1e-6 {
        return Err(format!(
            "ig: {epic} has a pip of {pip}, and this adapter writes a level with five decimals, so a \
             pip that is not a whole multiple of {WIRE_PRICE_STEP} is not admitted"
        ));
    }
    Ok(IgMarketFacts {
        properties: SymbolProperties {
            tick_size: pip,
            step_size: min_deal,
            min_qty: min_deal,
            ..SymbolProperties::default()
        },
        contract_value: ContractValue::linear(contract),
    })
}

/// The account's currency out of a `GET /accounts` body: the row of `account_id` and no other (a first-row
/// fallback would name another account's currency), upper-cased. An unknown currency is an ERROR, never a
/// default.
pub(crate) fn parse_account_currency(body: &Value, account_id: &str) -> Result<String, String> {
    let row = body
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find(|a| a.get("accountId").and_then(Value::as_str) == Some(account_id))
        })
        .ok_or_else(|| format!("ig: GET /accounts lists no account `{account_id}`"))?;
    row.get("currency")
        .and_then(Value::as_str)
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .ok_or_else(|| format!("ig: the account `{account_id}` carries no readable currency"))
}

/// An epic that may stand in a URL path: the characters an IG epic has, nothing else.
fn wire_safe(epic: &str) -> bool {
    !epic.is_empty()
        && epic.len() <= 64
        && epic.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// One account's way to ask IG for an epic's facts: a kept, logged-in [`IgSession`] (one login however many
/// epics are asked; a 401 re-authenticates it in place), the account's currency (read once) and the market
/// read per epic. Cache, rate and failure memory
/// are the CALLER's (`vike-gatewayhub`'s `IgFacts`): this type spends a request per call.
pub struct IgFactsSource {
    config: IgConfig,
    session: Mutex<Option<Arc<IgSession>>>,
    /// The account's currency, read once: it is the SESSION's account's, so it cannot differ per epic, and a
    /// rotated credential replaces this whole source ([`Self::serves`]). A failed read is not kept.
    currency: Mutex<Option<String>>,
}

impl IgFactsSource {
    /// A source over `config`; nothing is dialled until the first ask.
    #[must_use]
    pub fn new(config: IgConfig) -> Self {
        Self { config, session: Mutex::new(None), currency: Mutex::new(None) }
    }

    /// The account's currency: the kept one, or `GET /accounts` (v1) once. The lock is held across the read so
    /// concurrent first askers share ONE request.
    fn currency(&self, session: &IgSession) -> Result<String, IgFactsError> {
        let mut held = self.currency.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(currency) = held.as_ref() {
            return Ok(currency.clone());
        }
        let accounts = session
            .get("/accounts", "1", "")
            .map_err(|e| IgFactsError::Unavailable(format!("ig GET /accounts: {e}")))?;
        let currency = parse_account_currency(&accounts, &session.account_id)
            .map_err(IgFactsError::Unavailable)?;
        *held = Some(currency.clone());
        Ok(currency)
    }

    /// Whether this source logs in with exactly `config` (a rotated key or password needs a new source).
    #[must_use]
    pub fn serves(&self, config: &IgConfig) -> bool {
        self.config == *config
    }

    /// The kept session, logging in on the first ask. The lock is held across the login so concurrent
    /// askers share ONE login.
    fn session(&self) -> Result<Arc<IgSession>, IgFactsError> {
        let mut held = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(session) = held.as_ref() {
            return Ok(Arc::clone(session));
        }
        let session = IgSession::login(&self.config)
            .map_err(|e| IgFactsError::Unavailable(format!("ig login: {e}")))?;
        let session = Arc::new(session);
        *held = Some(Arc::clone(&session));
        Ok(session)
    }

    /// The facts of `epic`: the account's currency (`GET /accounts` (v1), read ONCE per source), `GET
    /// /markets/{epic}` (v3) for the market, then [`market_facts`]. One request per call after the first. IG's own 400 / 404 on the epic is a
    /// [`IgFactsError::Refused`] (the epic is not one); every other failure to read is
    /// [`IgFactsError::Unavailable`] (asking again may work).
    pub fn facts(&self, epic: &str) -> Result<IgMarketFacts, IgFactsError> {
        if !wire_safe(epic) {
            return Err(IgFactsError::Refused(format!(
                "ig: `{epic}` is not an epic (letters, digits, `.`, `_`, `-`)"
            )));
        }
        let session = self.session()?;
        let currency = self.currency(&session)?;
        let market = session.get(&format!("/markets/{epic}"), "3", "").map_err(|e| {
            let text = format!("ig GET /markets/{epic}: {e}");
            if matches!(e.status, 400 | 404) {
                IgFactsError::Refused(text)
            } else {
                IgFactsError::Unavailable(text)
            }
        })?;
        market_facts(epic, &market, &currency).map_err(IgFactsError::Refused)
    }
}

/// Why [`IgFactsSource::facts`] gave no facts: the venue's answer about the epic, or no answer at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgFactsError {
    /// A definitive "no" about THIS epic (not an epic, unknown to IG, or a fence of [`market_facts`]).
    Refused(String),
    /// "Not now": the login, the transport or a read failed, so asking again may work.
    Unavailable(String),
}

impl IgFactsError {
    /// The sentence, whichever kind.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            IgFactsError::Refused(t) | IgFactsError::Unavailable(t) => t,
        }
    }
}

#[cfg(test)]
#[path = "market_facts_tests.rs"]
mod market_facts_tests;
