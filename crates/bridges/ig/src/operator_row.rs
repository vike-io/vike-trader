//! **One instrument the OPERATOR adds by hand, looked up on IG** — the venue half of the datahub's
//! `Request::OperatorInstruments` (`vike_secrets::operator_instrument`). IG publishes no list (`crate::catalog`), so an
//! operator who wants EURUSD among the kept rows types `EURUSD` and IG is asked about that ONE pair: one market search
//! (`GET /markets?searchTerm=EURUSD`) and the chosen epic's market details (`GET /markets/{epic}`), both over the
//! session this gatewayhub ALREADY keeps for a running demo account's facts ([`hold_source`]), never a login of their
//! own. Nothing here is a list: the pairs are what the operator types, at run time.
//!
//! # Which epic, when IG lists several forms of one pair
//!
//! [`pick_fx_epic`] keeps the CURRENCIES rows whose epic names the pair (`CS.D.EURUSD.*`, the witness
//! `crate::instrument`'s admission reads) and that are a rolling CFD by `crate::catalog`'s own product rule (a
//! `DAILY`/`TODAY` spread bet and a dated forward are passed over: the facts refuse a spread bet for a CFD login, and
//! a dated contract is not the pair). Of those, the FX Mini (`crate::account::quote_rate`'s `fx_mini_epic`, the family
//! this bridge trades and rates conversions with) is chosen; with no Mini, the one remaining form; with none, or with
//! several and no Mini, nothing is chosen and the refusal names what IG listed.
//!
//! # What the row says ([`describe`])
//!
//! Out of the market details, the facts the admission reads too: the pip (`onePipMeans`, the tick), the pair (base
//! and quote, cross-checked against the epic), `contractSize` (what one unit of size is, base units) and
//! `dealingRules.minDealSize` (the size grid's minimum and step), the market's name and IG's type word. A field that
//! is absent or unreadable refuses the row: a row with missing facts is never stored.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use vike_bridge_core::json::{get_f64, json_number};
use vike_model::{
    Contract, ContractCfdExpiry, ContractCfdProduct, ContractFx, Instrument, SymbolProperties,
};

use crate::account::quote_rate::fx_mini_epic;
use crate::instrument::{IgPropertiesSource, pip_and_pair};

/// IG's type word of a currency pair.
const CURRENCIES: &str = "CURRENCIES";

/// The facts source a running demo account's lookups last built in this process (`hold_source`), the session an
/// operator's lookup rides. The last one wins: IG's search and market details are the same for every demo account.
static HELD: Mutex<Option<Arc<IgPropertiesSource>>> = Mutex::new(None);

/// **Keep `source` as the session an operator's lookup rides** — called where the gatewayhub builds or reuses a demo
/// account's facts source (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`'s `ig_account_source`).
pub fn hold_source(source: &Arc<IgPropertiesSource>) {
    *HELD.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(source));
}

/// The refusal when no account has handed this process a session.
pub const NO_HELD_SESSION: &str = "ig: no running ig demo account has opened its facts session in this \
     gatewayhub since it started, so there is no session to ask IG with (nothing logs in for this); start the \
     account, let it look up its epic, then ask again";

/// **The operator's lookup of `symbol`** over the held session: the chosen epic's row with its market details
/// FIRST, then the pair's other forms the search listed, as `crate::catalog::parse_markets` reads them (what was
/// passed over). The gatewayhub answers a `FetchCredentialed { CatalogSearch }` for ig with exactly this.
///
/// # Errors
/// [`NO_HELD_SESSION`], or the source's words: a read that failed, the read cap spent, or a refusal of the pick or
/// of the row.
pub fn operator_lookup(symbol: &str) -> Result<Vec<Instrument>, String> {
    let source = HELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| NO_HELD_SESSION.to_string())?;
    source.operator_row(symbol).map_err(|e| e.text().to_string())
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

/// **The epic of `symbol` (a pair, `EURUSD`) out of a `/markets?searchTerm=` body** — the module doc's rule.
pub(crate) fn pick_fx_epic(symbol: &str, search: &Value) -> Result<Pick, String> {
    let pair = symbol.trim().to_ascii_uppercase();
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
    let mini = fx_mini_epic(&pair);
    let chosen = match rolling.iter().position(|(epic, _)| *epic == mini) {
        Some(at) => at,
        None if rolling.len() == 1 => 0,
        None => {
            let listed: Vec<&str> = rolling
                .iter()
                .map(|(e, _)| e.as_str())
                .chain(other.iter().map(String::as_str))
                .collect();
            return Err(if rolling.is_empty() {
                format!(
                    "ig: IG's search for `{pair}` lists no rolling CFD currency pair named {pair} (its forms of the \
                     pair: [{}]), so nothing was added",
                    listed.join(", ")
                )
            } else {
                format!(
                    "ig: IG's search for `{pair}` lists {} rolling CFD forms of it ([{}]) and none is the FX Mini \
                     ({mini}) this bridge trades, so which one is meant is not certain; nothing was added",
                    rolling.len(),
                    listed.join(", ")
                )
            });
        }
    };
    let (epic, name) = rolling.remove(chosen);
    let passed_over = rolling.into_iter().map(|(e, _)| e).chain(other).collect();
    Ok(Pick { epic, name, passed_over })
}

/// **The row of `pick` out of its `GET /markets/{epic}` body** — the module doc's fields; a missing one refuses.
pub(crate) fn describe(pick: &Pick, market: &Value) -> Result<Instrument, String> {
    let epic = pick.epic.as_str();
    let refuse =
        |what: &str| format!("ig: the market details of {epic} {what}, so nothing was added");
    let instrument = market
        .get("instrument")
        .filter(|i| i.is_object())
        .ok_or_else(|| refuse("carry no `instrument` block"))?;
    let text = |key: &str| instrument.get(key).and_then(Value::as_str).unwrap_or("");
    if text("epic") != epic {
        return Err(refuse(&format!("answer for `{}`", text("epic"))));
    }
    if text("type") != CURRENCIES {
        return Err(refuse(&format!("type it `{}`, not {CURRENCIES}", text("type"))));
    }
    let (pip, quote, base) = pip_and_pair(text("onePipMeans")).ok_or_else(|| {
        refuse(&format!(
            "state `onePipMeans` `{}`, not '<pip> <QUOTE>/<BASE>'",
            text("onePipMeans")
        ))
    })?;
    if epic.split('.').nth(2) != Some(format!("{base}{quote}").as_str()) {
        return Err(refuse(&format!("name the pair {base}/{quote}, which the epic does not")));
    }
    let contract_size = get_f64(instrument, "contractSize");
    if !(contract_size.is_finite() && contract_size > 0.0) {
        return Err(refuse("state no contractSize"));
    }
    let min_deal = market
        .pointer("/dealingRules/minDealSize/value")
        .and_then(json_number)
        .filter(|m| m.is_finite() && *m > 0.0)
        .ok_or_else(|| refuse("state no minimum deal size"))?;
    let name = Some(text("name")).filter(|n| !n.is_empty()).unwrap_or(pick.name.as_str());
    let contract = Contract::Fx(ContractFx {
        pip_size: Some(pip),
        lot_size: Some(contract_size),
        swap_long: None,
        swap_short: None,
    });
    Ok(Instrument::new("ig", epic, base, quote, contract)
        .with_description(name)
        .with_venue_word(CURRENCIES)
        .with_properties(SymbolProperties {
            tick_size: pip,
            step_size: min_deal,
            min_qty: min_deal,
            contract_size,
            ..SymbolProperties::default()
        }))
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
