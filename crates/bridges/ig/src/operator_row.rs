//! **One instrument the OPERATOR adds by hand, looked up on IG** — the venue half of the datahub's
//! `Request::OperatorInstruments` (`vike_secrets::operator_instrument`). An operator who wants EURUSD among the kept
//! rows types `EURUSD` (or `EURUSD.CFD`, its standard lot) and IG is asked about that ONE pair: one market search
//! (`GET /markets?searchTerm=EURUSD`, the pair alone: [`search_term`]) and the chosen epic's market details
//! (`GET /markets/{epic}`), both over the session this gatewayhub ALREADY keeps for a running demo account's facts
//! ([`hold_source`]), never a second login beside it. Nothing here is a list: the pairs are what the operator types, at
//! run time.
//!
//! *Corrected 2026-11-16: this doc said IG publishes no list. It does (`GET /categories` and its paged instruments,
//! `crate::market_list`), read over the same held session ([`held_source`]) on the operator's command; the operator's
//! rows stay their own (the datahub folds them in where the list has no row of that epic and never overwrites one).*
//!
//! # Which epic, when IG lists several forms of one pair
//!
//! [`pick_fx_epic`] keeps the CURRENCIES rows whose epic names the pair (`CS.D.EURUSD.*`, the witness
//! `crate::instrument`'s admission reads) and that are a rolling CFD by `crate::catalog`'s own product rule (a
//! `DAILY`/`TODAY` spread bet and a dated forward are passed over: the facts refuse a spread bet for a CFD login, and
//! a dated contract is not the pair). Of those, the form the typed name names (decision 0290 item 6, the owner's
//! ruling of 2026-11-17) is chosen: for `EURUSD` the FX Mini (`crate::account::quote_rate`'s `fx_mini_epic`, the
//! family this bridge trades and rates conversions with), and with no Mini the one remaining form; for `EURUSD.CFD`
//! the standard lot `CS.D.EURUSD.CFD.IP` and nothing else. With none, or with several and none the one named, nothing
//! is chosen and the refusal names what IG listed.
//!
//! # What the row says ([`describe`])
//!
//! Out of the market details, the facts the admission reads too: the pip (`onePipMeans`, the tick), the pair (base
//! and quote, cross-checked against the epic), `contractSize` (what one unit of size is, base units) and
//! `dealingRules.minDealSize` (the size grid's minimum and step), the market's name and IG's type word. A field that
//! is absent or unreadable refuses the row: a row with missing facts is never stored. Its naming is IG's own
//! (`crate::catalog`'s `naming_of`): the Mini is `EURUSD`, the standard lot `EURUSD.CFD` (ten times the Mini's unit:
//! `contractSize` 100,000 against 10,000, measured on the demo gateway on 2026-11-17).

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use vike_bridge_core::json::{get_f64, json_number};
use vike_model::{
    Contract, ContractCfdExpiry, ContractCfdProduct, ContractFx, Instrument, SymbolProperties,
};

use crate::account::quote_rate::fx_mini_epic;
use crate::instrument::{IgPropertiesSource, pip_and_pair};
use crate::refusals::IgSymbolRefusal;

/// IG's type word of a currency pair.
pub(crate) const CURRENCIES: &str = "CURRENCIES";

/// Whether the account a held source belongs to still runs (the gatewayhub's fresh read of the account rows); `Err`
/// says why not, in words.
pub type StillRuns = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The facts source of the demo account last started or looked up in this process (`hold_source`), the session an
/// operator's lookup and the market list ride, and the check that its account still runs. The last one wins: IG's
/// search, market details and list are the same for every demo account.
static HELD: Mutex<Option<(Arc<IgPropertiesSource>, StillRuns)>> = Mutex::new(None);

/// The source [`hold_source`] last kept while its account still runs; else why not, in words
/// ([`IgSymbolRefusal::NoHeldSession`] when no account has handed this process one; the gatewayhub's own words when
/// the account no longer runs).
pub(crate) fn held_source() -> Result<Arc<IgPropertiesSource>, vike_utility::errors::CarriedRefusal>
{
    let held = HELD.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let (source, still_runs) = held.ok_or_else(|| IgSymbolRefusal::NoHeldSession.carried())?;
    still_runs()?;
    Ok(source)
}

/// **Keep `source` as the session an operator's lookup and the list refresh ride, while `still_runs`** — called where
/// the gatewayhub builds or reuses a demo account's facts source, at the account's start and at each facts ask
/// (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `ig_account_source`). Keeping it asks IG nothing:
/// the source logs in at its first read (an order look, a lookup or a refresh), and that ONE session is the account's.
pub fn hold_source(source: &Arc<IgPropertiesSource>, still_runs: StillRuns) {
    *HELD.lock().unwrap_or_else(PoisonError::into_inner) = Some((Arc::clone(source), still_runs));
}

/// **The operator's lookup of `symbol`** over the held session: the chosen epic's row with its market details
/// FIRST, then the pair's other forms the search listed, as `crate::catalog::parse_markets` reads them (what was
/// passed over). The gatewayhub answers a `FetchCredentialed { CatalogSearch }` for ig with exactly this.
///
/// # Errors
/// [`IgSymbolRefusal::NoHeldSession`], an account that no longer runs, or the source's words: a read that failed,
/// the read cap spent, or a refusal of the pick or of the row.
pub fn operator_lookup(symbol: &str) -> Result<Vec<Instrument>, String> {
    let source = held_source().map_err(|why| why.to_string())?;
    source.operator_row(symbol).map_err(|e| e.text())
}

/// The epic [`pick_fx_epic`] chose for a pair, and every other epic of that pair the search listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pick {
    pub(crate) epic: String,
    /// The search row's `instrumentName`.
    pub(crate) name: String,
    /// The pair's other epics, in the search's order.
    pub(crate) passed_over: Vec<String>,
}

/// The pair a typed name asks IG about, upper case: `EURUSD` for `eurusd` and for `EURUSD.CFD`.
pub(crate) fn search_term(symbol: &str) -> String {
    let typed = symbol.trim().to_ascii_uppercase();
    typed.strip_suffix(vike_catalog::CFD_SUFFIX).map_or_else(|| typed.clone(), str::to_string)
}

/// **The epic of `symbol` (a pair, `EURUSD`, or its standard lot, `EURUSD.CFD`) out of a `/markets?searchTerm=`
/// body** — the module doc's rule.
pub(crate) fn pick_fx_epic(symbol: &str, search: &Value) -> Result<Pick, IgSymbolRefusal> {
    let standard_lot = symbol.trim().to_ascii_uppercase().ends_with(vike_catalog::CFD_SUFFIX);
    let pair = search_term(symbol);
    let rows = search.get("markets").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut rolling: Vec<(String, String)> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for row in rows {
        let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("");
        let epic = text("epic");
        if epic.split('.').nth(2) != Some(pair.as_str()) {
            continue;
        }
        let expiry = row.get("expiry").and_then(Value::as_str).filter(|s| !s.is_empty());
        if text("instrumentType") == CURRENCIES
            && crate::catalog::product_and_expiry(epic, expiry)
                == (ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling)
        {
            rolling.push((epic.to_string(), text("instrumentName").to_string()));
        } else {
            other.push(epic.to_string());
        }
    }
    let named = if standard_lot { format!("CS.D.{pair}.CFD.IP") } else { fx_mini_epic(&pair) };
    let chosen = match rolling.iter().position(|(epic, _)| *epic == named) {
        Some(at) => at,
        None if !standard_lot && rolling.len() == 1 => 0,
        None => {
            let listed = rolling
                .iter()
                .map(|(e, _)| e.as_str())
                .chain(other.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(if standard_lot {
                IgSymbolRefusal::NoStandardLot { pair, named, listed }
            } else if rolling.is_empty() {
                IgSymbolRefusal::NoRollingPair { pair, listed }
            } else {
                IgSymbolRefusal::SeveralRollingForms { pair, count: rolling.len(), listed, named }
            });
        }
    };
    let (epic, name) = rolling.remove(chosen);
    let passed_over = rolling.into_iter().map(|(e, _)| e).chain(other).collect();
    Ok(Pick { epic, name, passed_over })
}

/// **The row of `pick` out of its `GET /markets/{epic}` body** — the module doc's fields; a missing one refuses.
pub(crate) fn describe(pick: &Pick, market: &Value) -> Result<Instrument, IgSymbolRefusal> {
    let epic = pick.epic.as_str();
    let epic_text = || epic.to_string();
    let instrument = market
        .get("instrument")
        .filter(|i| i.is_object())
        .ok_or_else(|| IgSymbolRefusal::RowNoInstrumentBlock { epic: epic_text() })?;
    let text = |key: &str| instrument.get(key).and_then(Value::as_str).unwrap_or("");
    if text("epic") != epic {
        return Err(IgSymbolRefusal::RowAnsweredForAnotherEpic {
            epic: epic_text(),
            answered: text("epic").to_string(),
        });
    }
    if text("type") != CURRENCIES {
        return Err(IgSymbolRefusal::RowNotCurrencies {
            epic: epic_text(),
            kind: text("type").to_string(),
        });
    }
    let (pip, quote, base) =
        pip_and_pair(text("onePipMeans")).ok_or_else(|| IgSymbolRefusal::RowPipUnreadable {
            epic: epic_text(),
            one_pip_means: text("onePipMeans").to_string(),
        })?;
    if epic.split('.').nth(2) != Some(format!("{base}{quote}").as_str()) {
        return Err(IgSymbolRefusal::RowPairNotNamed { epic: epic_text(), base, quote });
    }
    let contract_size = get_f64(instrument, "contractSize");
    if !(contract_size.is_finite() && contract_size > 0.0) {
        return Err(IgSymbolRefusal::RowNoContractSize { epic: epic_text() });
    }
    let min_deal = market
        .pointer("/dealingRules/minDealSize/value")
        .and_then(json_number)
        .filter(|m| m.is_finite() && *m > 0.0)
        .ok_or_else(|| IgSymbolRefusal::RowNoMinimumDeal { epic: epic_text() })?;
    let name = Some(text("name")).filter(|n| !n.is_empty()).unwrap_or(pick.name.as_str());
    let contract = Contract::Fx(ContractFx {
        pip_size: Some(pip),
        lot_size: Some(contract_size),
        swap_long: None,
        swap_short: None,
    });
    let mut row = Instrument::new("ig", epic, base, quote, contract)
        .with_description(name)
        .with_venue_word(CURRENCIES)
        .with_properties(SymbolProperties {
            tick_size: pip,
            step_size: min_deal,
            min_qty: min_deal,
            contract_size,
            ..SymbolProperties::default()
        });
    row.naming = crate::catalog::naming_of(epic, CURRENCIES, name);
    Ok(row)
}

/// The forms of the pair the search listed, other than the pick, as `crate::catalog::parse_markets` reads them.
pub(crate) fn passed_over_rows(pick: &Pick, search: &Value) -> Vec<Instrument> {
    crate::catalog::parse_markets(search)
        .into_iter()
        .filter(|row| pick.passed_over.contains(&row.raw_symbol))
        .collect()
}

#[cfg(test)]
#[path = "operator_row_tests.rs"]
mod operator_row_tests;
