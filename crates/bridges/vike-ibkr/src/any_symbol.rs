//! **One US stock, admitted by NAME** (any-symbol, decisions 0173/0174): what the gatewayhub answers
//! when the node asks for the facts of `AAPL.SMART.USD`, an instrument the account's engine was not
//! mounted on.
//!
//! IBKR publishes NO instrument list (no bulk universe; `docs/ops/any-symbol-remaining-venues-plan.md`,
//! "ibkr"), so nothing here is a catalog. A name is resolved by ASKING the logged-in gateway, in three
//! requests on one connection (the seam is [`ContractLookup`], a scripted double in every test here):
//!
//! 1. `reqMatchingSymbols(ticker)` - which contracts carry the ticker (at most [`MAX_MATCHES`], paced by
//!    the gateway), narrowed to the stocks of the ticker whose currency is the one the NAME states;
//! 2. `reqContractDetails(conId)` - that one contract's minimum tick, size rules, multiplier and market
//!    rules;
//! 3. `reqMarketRule(id)` once per distinct rule - the price bands behind the minimum tick.
//!
//! # What is admitted, and every refusal by name
//!
//! Only `TICKER.SMART.CCY` (the three-field engine name, `crate::contract::parse_simplified`'s cash equity)
//! where `CCY` is the currency of the account's money ([`stock_facts`]'s `account_currency`). The REFUSALS:
//!
//! * any other spelling (forex, a future, an option, a combo, an explicit exchange): refused before any
//!   request, naming the form;
//! * a ticker that names no stock, or only other kinds of contract (a future, an index): refused with the
//!   kinds it does name;
//! * a ticker whose only stocks are in OTHER currencies (the name states the currency, so those are other
//!   contracts: a fence on the ledger's money, not a conversion);
//! * a name that matches MORE THAN ONE stock in its currency (two primary listings): refused naming each
//!   conId and exchange, never picked from - the engine's order would draw TWS error 200 anyway;
//! * contract details that disagree with the match (another conId, kind or currency), carry a multiplier
//!   other than 1, or give no usable minimum tick, size increment or market rule.
//!
//! # The grid
//!
//! `SymbolGrid` carries ONE tick, but a US stock's tick depends on its price (`minTick` is only "the smallest
//! of the different minimum tick sizes", IB's own words). The grid tick is the COARSEST increment of the
//! contract's market rules, and only when every other increment divides it: a price on the coarse grid is a
//! valid price in every band. Rules that do not nest, or that cannot be read, are refused.
//!
//! ⚠ DOUBT (kept, not decided here): US stocks under one dollar tick at 0.0001 and the rest at 0.01, so the
//! coarsest-band rule gives a sub-dollar stock a 0.01 grid and the engine rounds its prices to the cent, a
//! coarse step for a penny stock; admitting it on its finest band instead would make every price above a
//! dollar fail the venue's own tick. Whether a sub-dollar stock should be admitted at all is the owner's call.
//!
//! The size grid: `sizeIncrement` is the step and `minSize` the minimum; when TWS reports no size rules at
//! all (neither field usable) the stock is refused by name (`size rules unavailable`), and a missing
//! `minSize` beside a usable `sizeIncrement` leaves the minimum at 0 (the engine floors a quantity at one
//! step anyway).
//!
//! # ⚠ Not proven against a gateway
//!
//! Nothing here has run against a gateway. The shape of a real contract-details reply (the multiplier of a
//! share, the market-rule id list, the size rules), the market-rule reply, the paced matching request and the
//! market-data permissions of the paper account are IBKR's documentation, not a capture.

use vike_model::{AssetClass, SymbolProperties};

/// The router the name states. A share traded on a named exchange is another name this adapter refuses.
pub const ROUTE: &str = "SMART";

/// How many contracts IB answers a matching request with, at most.
pub const MAX_MATCHES: usize = 16;

/// How many DISTINCT market rules one contract may name before the lookup refuses it (a request each).
pub const MAX_MARKET_RULES: usize = 4;

/// The longest ticker admitted.
pub const MAX_TICKER_LEN: usize = 12;

/// One row of `reqMatchingSymbols`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRow {
    /// IB's contract id.
    pub conid: i32,
    /// The row's symbol.
    pub symbol: String,
    /// IB's secType word (`STK`, `FUT`, `IND`, ...).
    pub sec_type: String,
    /// The primary exchange, `""` when IB sent none.
    pub primary_exchange: String,
    /// The contract's currency.
    pub currency: String,
}

/// One row of `reqContractDetails`.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailsRow {
    /// IB's contract id.
    pub conid: i32,
    /// The contract's symbol.
    pub symbol: String,
    /// IB's secType word.
    pub sec_type: String,
    /// The contract's currency.
    pub currency: String,
    /// IB's multiplier string: `""` for a share.
    pub multiplier: String,
    /// IB's `minTick`: the smallest of the contract's minimum price variations.
    pub min_tick: f64,
    /// The order size increment, `None` when TWS reported no size rules.
    pub size_increment: Option<f64>,
    /// The minimum order size, `None` when TWS reported no size rules.
    pub min_size: Option<f64>,
    /// The ids of the market rules, one per valid exchange as IB lists them.
    pub market_rule_ids: Vec<String>,
}

/// One band of a market rule: from `low_edge` up, the price moves in steps of `increment`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PriceBand {
    /// The lowest price the band covers.
    pub low_edge: f64,
    /// The minimum price variation inside the band.
    pub increment: f64,
}

/// The three requests a name is resolved with. The production impl runs them on one connection of the
/// logged-in gateway (`crate::any_symbol_socket`); a test scripts them. An `Err` is a request that did not
/// answer (the gateway is not ready), never a verdict about the symbol.
pub trait ContractLookup {
    /// `reqMatchingSymbols(pattern)`.
    fn matching_symbols(&self, pattern: &str) -> Result<Vec<MatchRow>, String>;
    /// `reqContractDetails` of the one contract `contract` names.
    fn contract_details(&self, contract: &MatchRow) -> Result<Vec<DetailsRow>, String>;
    /// `reqMarketRule(id)`.
    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String>;
}

/// Why no facts: a verdict about the name, or a gateway that could not say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactsError {
    /// The name cannot be admitted, in words an operator reads verbatim. A definitive answer.
    Refused(String),
    /// The gateway did not answer (not logged in, a request failed or timed out): ask again later.
    Unavailable(String),
}

/// The facts of one admitted stock.
#[derive(Clone, Debug, PartialEq)]
pub struct StockFacts {
    /// The engine name, `TICKER.SMART.CCY`, upper case.
    pub symbol: String,
    /// IB's contract id of the stock the name resolved to.
    pub conid: i32,
    /// The grid and minimums, in shares and the stock's currency.
    pub properties: SymbolProperties,
}

/// The ticker and currency of an engine name `TICKER.SMART.CCY`, upper-cased, or why it is not one.
///
/// # Errors
/// Words naming the form: any other field count, any other route, a ticker with a character a ticker
/// does not have (a dot cannot be written in this grammar, `crate::contract`), a currency that is not three letters.
pub fn parse_stock_name(name: &str) -> Result<(String, String), String> {
    let hint = "an IBKR stock is admitted as `TICKER.SMART.CCY`, e.g. `AAPL.SMART.USD` (a ticker of \
                letters and digits, routed SMART, with its currency; forex, futures, options and an \
                explicit exchange are not admitted by name)";
    let parts: Vec<&str> = name.trim().split('.').collect();
    let [ticker, route, currency] = parts.as_slice() else {
        return Err(format!("`{name}` is not an IBKR stock name ({hint})"));
    };
    let ticker_ok = !ticker.is_empty()
        && ticker.len() <= MAX_TICKER_LEN
        && ticker.bytes().all(|b| b.is_ascii_alphanumeric());
    let currency_ok = currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_alphabetic());
    if !route.eq_ignore_ascii_case(ROUTE) || !ticker_ok || !currency_ok {
        return Err(format!("`{name}` is not an IBKR stock name ({hint})"));
    }
    Ok((ticker.to_ascii_uppercase(), currency.to_ascii_uppercase()))
}

/// The engine name of `ticker` in `currency`.
#[must_use]
pub fn stock_name(ticker: &str, currency: &str) -> String {
    format!("{ticker}.{ROUTE}.{currency}")
}

/// The ticker and currency of `name`, after the two refusals that cost no request: its form
/// ([`parse_stock_name`]) and its currency against `account_currency` (the ledger is one currency and nothing
/// converts, so a stock of another currency is a fence, not a conversion).
///
/// # Errors
/// [`FactsError::Refused`] naming the form or the two currencies.
pub fn checked_name(name: &str, account_currency: &str) -> Result<(String, String), FactsError> {
    let (ticker, currency) = parse_stock_name(name).map_err(FactsError::Refused)?;
    if !currency.eq_ignore_ascii_case(account_currency) {
        return Err(FactsError::Refused(format!(
            "`{name}` is a {currency} stock and this account's money is {account_currency}: the \
             ledger is one currency and nothing converts, so only a {account_currency} stock is admitted"
        )));
    }
    Ok((ticker, currency))
}

fn unavailable(what: &str, why: &str) -> FactsError {
    FactsError::Unavailable(format!("ibkr: {what} did not answer: {why}"))
}

/// **Resolve `name` (`TICKER.SMART.CCY`) to the facts of one stock** through `lookup`. `account_currency`
/// is the currency of the account's money: a name stating another currency is refused before any request
/// (the ledger is one currency and nothing converts).
///
/// At most `2 + MAX_MARKET_RULES` requests are made, and none when the name is refused by its form or its
/// currency ([`checked_name`]).
///
/// # Errors
/// [`FactsError::Refused`] for every verdict in the module doc; [`FactsError::Unavailable`] when a request
/// did not answer.
pub fn stock_facts(
    lookup: &dyn ContractLookup,
    name: &str,
    account_currency: &str,
) -> Result<StockFacts, FactsError> {
    let (ticker, currency) = checked_name(name, account_currency)?;
    let name = stock_name(&ticker, &currency);
    let chosen = choose_stock(lookup, &name, &ticker, &currency)?;
    let rows = lookup
        .contract_details(&chosen)
        .map_err(|why| unavailable(&format!("contract details of {name}"), &why))?;
    let details = checked_details(&rows, &chosen, &name, &currency)?;
    let tick = grid_tick(lookup, details, &name)?;
    // TWS sent NO size rules at all (neither a usable minSize nor sizeIncrement): the order's size grid and
    // minimum are unknown, so the stock is refused by name rather than admitted on a guessed floor.
    let usable = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
    if usable(details.size_increment).is_none() && usable(details.min_size).is_none() {
        return Err(FactsError::Refused(format!(
            "{name}: size rules unavailable (the contract details carry neither a minimum size nor a \
             size increment), so the order size grid and minimum are unknown"
        )));
    }
    let step = details.size_increment.filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| {
        FactsError::Refused(format!(
            "{name}: the contract details carry no size increment (missing or zero), so the order \
             size grid is unknown"
        ))
    })?;
    let min_qty = details.min_size.unwrap_or(0.0);
    if !min_qty.is_finite() || min_qty < 0.0 {
        return Err(FactsError::Refused(format!(
            "{name}: the contract details carry a minimum size of {min_qty}"
        )));
    }
    Ok(StockFacts {
        symbol: name,
        conid: chosen.conid,
        properties: SymbolProperties {
            tick_size: tick,
            step_size: step,
            min_qty,
            asset_class: Some(AssetClass::Equity),
            ..SymbolProperties::default()
        },
    })
}

/// The ONE stock of `ticker` in `currency` the matching request names, or the refusal.
fn choose_stock(
    lookup: &dyn ContractLookup,
    name: &str,
    ticker: &str,
    currency: &str,
) -> Result<MatchRow, FactsError> {
    let rows = lookup
        .matching_symbols(ticker)
        .map_err(|why| unavailable(&format!("the symbol search for {ticker}"), &why))?;
    // `reqMatchingSymbols` answers every contract whose symbol STARTS with the pattern.
    let named: Vec<&MatchRow> =
        rows.iter().filter(|r| r.symbol.eq_ignore_ascii_case(ticker)).collect();
    if named.is_empty() {
        return Err(FactsError::Refused(format!(
            "{name}: the gateway's symbol search names no contract with the ticker {ticker}"
        )));
    }
    let stocks: Vec<&MatchRow> = named.iter().copied().filter(|r| r.sec_type == "STK").collect();
    if stocks.is_empty() {
        let mut kinds: Vec<&str> = named.iter().map(|r| r.sec_type.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        return Err(FactsError::Refused(format!(
            "{name}: the ticker {ticker} names only {} contracts here and no stock; this adapter \
             admits a stock by name",
            kinds.join(", ")
        )));
    }
    let mut ours: Vec<&MatchRow> =
        stocks.iter().copied().filter(|r| r.currency.eq_ignore_ascii_case(currency)).collect();
    if ours.is_empty() {
        let mut others: Vec<String> =
            stocks.iter().map(|r| r.currency.to_ascii_uppercase()).collect();
        others.sort_unstable();
        others.dedup();
        return Err(FactsError::Refused(format!(
            "{name}: the stock {ticker} is listed here in {} only, not in {currency}; a stock of \
             another currency is another contract and is not admitted",
            others.join(", ")
        )));
    }
    ours.sort_by_key(|r| r.conid);
    ours.dedup_by_key(|r| r.conid);
    if let [one] = ours.as_slice() {
        return Ok((*one).clone());
    }
    let listed: Vec<String> = ours
        .iter()
        .take(MAX_MATCHES)
        .map(|r| {
            let venue =
                if r.primary_exchange.is_empty() { "?" } else { r.primary_exchange.as_str() };
            format!("conId {} on {venue}", r.conid)
        })
        .collect();
    Err(FactsError::Refused(format!(
        "{name} is ambiguous: the ticker {ticker} names {} stocks in {currency} ({}); an order on it \
         would draw TWS error 200, so it is refused and not picked from",
        ours.len(),
        listed.join("; ")
    )))
}

/// The one details row the reply is, checked against the match: every row names the chosen contract,
/// and the contract is a share of the right currency with a multiplier of 1.
fn checked_details<'a>(
    rows: &'a [DetailsRow],
    chosen: &MatchRow,
    name: &str,
    currency: &str,
) -> Result<&'a DetailsRow, FactsError> {
    let Some(first) = rows.first() else {
        return Err(FactsError::Refused(format!(
            "{name}: the gateway returned no contract details for conId {}",
            chosen.conid
        )));
    };
    for row in rows {
        if row.conid != chosen.conid
            || row.sec_type != "STK"
            || !row.currency.eq_ignore_ascii_case(currency)
            || !row.symbol.eq_ignore_ascii_case(&chosen.symbol)
        {
            return Err(FactsError::Refused(format!(
                "{name}: the contract details name {} {} {} (conId {}) and not the stock the search \
                 chose (conId {}); nothing is admitted on a disagreement",
                row.symbol, row.sec_type, row.currency, row.conid, chosen.conid
            )));
        }
    }
    let multiplier = first.multiplier.trim();
    if !multiplier.is_empty() {
        match multiplier.parse::<f64>() {
            Ok(1.0) => {}
            _ => {
                return Err(FactsError::Refused(format!(
                    "{name}: the contract carries a multiplier of {multiplier}, and the engine books a \
                     share as quantity times price"
                )));
            }
        }
    }
    Ok(first)
}

/// The grid tick: the coarsest increment of the contract's market rules, when every other increment
/// divides it. See the module doc.
fn grid_tick(
    lookup: &dyn ContractLookup,
    details: &DetailsRow,
    name: &str,
) -> Result<f64, FactsError> {
    if !details.min_tick.is_finite() || details.min_tick <= 0.0 {
        return Err(FactsError::Refused(format!(
            "{name}: the contract details carry no minimum tick (missing or zero)"
        )));
    }
    let mut ids: Vec<i32> = Vec::new();
    for word in details.market_rule_ids.iter().flat_map(|w| w.split(',')) {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        let id: i32 = word.parse().map_err(|_| {
            FactsError::Refused(format!("{name}: the market rule id {word:?} is not a number"))
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(FactsError::Refused(format!(
            "{name}: the contract names no market rule, so the price bands behind its minimum tick \
             ({}) cannot be read",
            details.min_tick
        )));
    }
    if ids.len() > MAX_MARKET_RULES {
        return Err(FactsError::Refused(format!(
            "{name}: the contract names {} distinct market rules (more than {MAX_MARKET_RULES}); \
             its price grid is not read",
            ids.len()
        )));
    }
    let mut increments: Vec<f64> = Vec::new();
    for id in ids {
        let bands = lookup
            .market_rule(id)
            .map_err(|why| unavailable(&format!("market rule {id} of {name}"), &why))?;
        if bands.is_empty() {
            return Err(FactsError::Refused(format!("{name}: market rule {id} has no price band")));
        }
        for band in bands {
            if !band.increment.is_finite() || band.increment <= 0.0 {
                return Err(FactsError::Refused(format!(
                    "{name}: market rule {id} has a band with an increment of {}",
                    band.increment
                )));
            }
            increments.push(band.increment);
        }
    }
    let coarsest = increments.iter().copied().fold(0.0_f64, f64::max);
    for inc in &increments {
        let ratio = coarsest / inc;
        if (ratio - ratio.round()).abs() > 1e-6 * ratio {
            return Err(FactsError::Refused(format!(
                "{name}: the price bands of its market rules do not nest ({inc} does not divide \
                 {coarsest}), so no single tick is valid in every band"
            )));
        }
    }
    Ok(coarsest)
}

#[cfg(test)]
#[path = "any_symbol_tests.rs"]
mod any_symbol_tests;
