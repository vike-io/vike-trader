//! **One IBKR FOREX pair and one dated cash-settled FUTURE, admitted by NAME** (decision 0234): the pure half. What the
//! gatewayhub answers when the node asks for the facts of `EUR.USD.IDEALPRO` or `ES.CME.USD.FUT.20261218`, resolved
//! over the [`ClassLookup`] seam (a scripted double in every test here; the socket impl is
//! `crate::any_symbol_socket`'s `SocketLookup`). The stock half is `crate::any_symbol`; this module touches none of it.
//!
//! # What the paper account MEASURED (2026-10-30, `tests/ibkr_forex_futures_probe.rs`; the fixtures of the tests)
//!
//! * **A pair is ONE contract.** `reqContractDetails(CASH EUR/USD IDEALPRO)` answers exactly one row (conId 12087792,
//!   `minTick` 0.00005, market rule 3188, `minSize` and `sizeIncrement` 0.01), so a pair's name needs no conId pin (a
//!   stock's does: a ticker is listed on several exchanges). `USD.JPY` is conId 15016059, tick 0.005, size 1.
//! * **The IdealPro minimum is NOT in the details.** The order's own warning states it: `Your order size is below the EUR
//!   20000 IdealPro minimum` (GBP 20000, USD 25000), and a smaller order is an ODD LOT the venue still accepts and fills
//!   ("not guaranteed at the IdealPro displayed quotes"). [`idealpro_minimum`] reads the figure out of that warning.
//! * **A future's day is `realExpirationDate`**: `lastTradeDateOrContractMonth` of a contract carries a time and a zone
//!   (`20261218 08:30:00 US/Central`), the month is `contractMonth` (`202612`). The multiplier is a numeric string
//!   (ES 50, MES 5, MNQ 2, CL 1000), the tick 0.25 (CL 0.01) and the size grid 1.
//! * **The margin is the venue's, per contract and per SIDE** (a what-if order): ES 23986 to buy and 19779 to sell, MES
//!   2398 and 1978, NQ 43841 and 39856, MNQ 4397 and 3996, CL 19384 both; `initialMarginChange` of a forex buy that the
//!   account's own cash funds is 0 (EUR.USD), of one that borrows a currency 3 percent (USD.JPY 750 on 25000).
//!
//! # The verdicts
//!
//! * a forex pair is admitted as a linear instrument in units of its BASE currency, quoted in its quote currency
//!   (`SymbolProperties::contract_size` unset, the grid the venue's, `min_qty` the IdealPro minimum, never the odd lot);
//!   a margin rate is NOT stated for it (see the record's "Forex": what the venue holds depends on which currency the
//!   account's cash funds);
//! * a future is admitted only on a CASH-SETTLED root ([`CASH_SETTLED_ROOTS`]) and only while at least
//!   [`MIN_DAYS_TO_LAST_TRADE`] days remain to its last trading day; its contract value is the multiplier and its margin
//!   rate is the larger of the two sides' initial margin over the notional at a reference price;
//! * an option is refused by name ([`OPTION_REFUSAL`]).

use vike_model::{AssetClass, ContractValue, SymbolProperties};
use vike_util::calendar::{civil_from_days, days_from_civil};

use crate::any_symbol::{FactsError, PriceBand};
use crate::contract::{IbkrContract, SecType, parse_simplified};

/// IBKR's forex venue.
pub const IDEALPRO: &str = "IDEALPRO";

/// The fewest whole days that must remain to a future's last trading day for it to be ADMITTED. IB's own close-out of a
/// position it will not let expire begins days before the last day; nothing in the engine rolls or closes a position at
/// expiry, so a contract this close is not taken on.
pub const MIN_DAYS_TO_LAST_TRADE: i64 = 5;

/// The roots admitted: the CME equity-index futures (full and micro) that settle in CASH. A future that settles by
/// DELIVERY (CL, GC, ...) is refused by name: IB closes such a position days before the last trading day at the
/// venue's prices, which the engine would carry as a position the venue no longer holds. A root joins this list when its
/// settlement is known and a round trip of it has been read.
pub const CASH_SETTLED_ROOTS: &[&str] = &["ES", "MES", "NQ", "MNQ", "RTY", "M2K", "YM", "MYM"];

/// Why an option is not admitted by name, in the operator's words.
pub const OPTION_REFUSAL: &str = "an IBKR option is not admitted: this engine has no option payoff and no short-option \
    margin model (a long option's premium is its whole cost and a short one's margin is the venue's portfolio figure), \
    so it is refused by name and never booked as a share";

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// The engine name of a forex pair: `BASE.QUOTE.IDEALPRO`, the spelling this tree already writes (`crate::contract`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForexName {
    /// The base currency, upper case: one unit of quantity is one unit of it.
    pub base: String,
    /// The quote currency, upper case: the price is quote units per base unit.
    pub quote: String,
}

impl ForexName {
    /// `BASE.QUOTE.IDEALPRO`.
    #[must_use]
    pub fn engine_name(&self) -> String {
        format!("{}.{}.{IDEALPRO}", self.base, self.quote)
    }
}

/// A dated future named by `ROOT.EXCHANGE.CURRENCY.FUT.EXPIRY[.MULTIPLIER]` (`crate::contract`'s grammar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FutureName {
    /// The root, upper case (`ES`).
    pub root: String,
    /// The exchange, upper case (`CME`).
    pub exchange: String,
    /// The settlement currency, upper case.
    pub currency: String,
    /// `YYYYMMDD` (the last trading day) or `YYYYMM` (the contract month).
    pub expiry: String,
    /// The multiplier the name claims, when it does.
    pub multiplier: Option<String>,
}

/// A name of the two classes this module admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassName {
    /// A forex pair.
    Forex(ForexName),
    /// A dated future.
    Future(FutureName),
}

impl ClassName {
    /// The currency the contract counts its money in: a pair's QUOTE currency (its profit is in it), a future's
    /// settlement currency.
    #[must_use]
    pub fn money(&self) -> &str {
        match self {
            ClassName::Forex(f) => &f.quote,
            ClassName::Future(f) => &f.currency,
        }
    }
}

/// Whether `name` is of one of the classes of this module, and which: `None` for a name that is none of them (a stock
/// name, or a shape that is nobody's), `Some(Err)` for a name of an unadmitted class (an option, an index, a crypto
/// coin) or a malformed forex or future name, in the operator's words.
///
/// # Errors
/// The `Some(Err)` cases above.
#[must_use]
pub fn parse_class_name(name: &str) -> Option<Result<ClassName, String>> {
    let upper = name.trim().to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('.').collect();
    let claim = parts.get(3).copied().unwrap_or("");
    let ours = parts.iter().skip(1).any(|p| *p == IDEALPRO)
        || matches!(claim, "CASH" | "FUT" | "OPT" | "IND" | "CRYPTO");
    if !ours {
        return None;
    }
    let contract = match parse_simplified(&upper) {
        Ok(c) => c,
        Err(why) => return Some(Err(why)),
    };
    Some(match contract.sec_type {
        SecType::Cash => forex_name_of(&upper, &contract),
        SecType::Fut => future_name_of(&upper, &contract),
        SecType::Opt => Err(format!("`{name}`: {OPTION_REFUSAL}")),
        _ => Err(format!(
            "`{name}` is a {} contract and this adapter admits a stock, a forex pair on IDEALPRO or a dated \
             cash-settled future by name",
            contract.sec_type.as_ib_code()
        )),
    })
}

fn is_ccy(c: &str) -> bool {
    c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())
}

fn forex_name_of(name: &str, c: &IbkrContract) -> Result<ClassName, String> {
    if !c.exchange.eq_ignore_ascii_case(IDEALPRO) {
        return Err(format!(
            "`{name}`: a forex pair is admitted on {IDEALPRO} only, not on {}",
            c.exchange
        ));
    }
    if !is_ccy(&c.symbol) || !is_ccy(&c.currency) || c.symbol.eq_ignore_ascii_case(&c.currency) {
        return Err(format!(
            "`{name}` is not a forex pair: a pair is two different three-letter currencies, `EUR.USD.IDEALPRO`"
        ));
    }
    Ok(ClassName::Forex(ForexName {
        base: c.symbol.to_ascii_uppercase(),
        quote: c.currency.to_ascii_uppercase(),
    }))
}

fn future_name_of(name: &str, c: &IbkrContract) -> Result<ClassName, String> {
    let Some(expiry) = c.expiry.clone() else {
        return Err(format!("`{name}`: a future names its expiry, `ES.CME.USD.FUT.20261218`"));
    };
    if c.symbol.is_empty() || !c.symbol.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!("`{name}`: {:?} is not a futures root", c.symbol));
    }
    Ok(ClassName::Future(FutureName {
        root: c.symbol.to_ascii_uppercase(),
        exchange: c.exchange.to_ascii_uppercase(),
        currency: c.currency.to_ascii_uppercase(),
        expiry,
        multiplier: c.multiplier.clone(),
    }))
}

/// Whether the engine symbol `name` is one whose position moves the account's CASH in a way the one-currency balance
/// does not follow: a forex pair (the cash of two currencies moves by the notional) or a future (the venue settles its
/// variation into cash daily). The reconcile client withholds the account's cash figure while the engine holds one
/// (`crate::recon::socket`).
#[must_use]
pub fn moves_cash(name: &str) -> bool {
    parse_simplified(name).is_ok_and(|c| matches!(c.sec_type, SecType::Cash | SecType::Fut))
}

/// One row of `reqContractDetails` for a contract of these classes.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassRow {
    /// IB's contract id.
    pub conid: i32,
    /// The contract's symbol (the BASE currency of a pair, the root of a future).
    pub symbol: String,
    /// IB's secType word (`CASH`, `FUT`).
    pub sec_type: String,
    /// The exchange the row names.
    pub exchange: String,
    /// The contract's currency (the QUOTE currency of a pair, the settlement currency of a future).
    pub currency: String,
    /// IB's multiplier string: `""` for a pair.
    pub multiplier: String,
    /// `realExpiration`: `YYYYMMDD` for a future, `""` for a pair.
    pub real_expiration: String,
    /// IB's `lastTradeDateOrContractMonth` as the row states it (a time and a zone may follow the day).
    pub last_trade: String,
    /// IB's `priceMagnifier`.
    pub price_magnifier: i32,
    /// IB's `minTick`.
    pub min_tick: f64,
    /// The order size increment, `None` when TWS reported no size rules.
    pub size_increment: Option<f64>,
    /// The minimum order size, `None` when TWS reported no size rules.
    pub min_size: Option<f64>,
    /// The ids of the market rules, one per valid exchange as IB lists them.
    pub market_rule_ids: Vec<String>,
}

/// What the venue answers to a WHAT-IF order (nothing reaches the market): the margin and the commission the order
/// would carry, and the venue's warning about it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct WhatIfRow {
    /// `initialMarginChange`; `None` when the venue states none (a size below the IdealPro minimum states none).
    pub initial_margin_change: Option<f64>,
    /// `maintenanceMarginChange`.
    pub maintenance_margin_change: Option<f64>,
    /// The currency the margin is stated in.
    pub margin_currency: String,
    /// `commission`.
    pub commission: Option<f64>,
    /// The currency the commission is stated in.
    pub commission_currency: String,
    /// The venue's `warningText` (empty when it has none).
    pub warning_text: String,
}

/// The requests of these classes' facts, over one connection of the logged-in gateway. An `Err` is a request that did
/// not answer (the gateway is not ready), never a verdict about the symbol.
pub trait ClassLookup {
    /// `reqContractDetails` of the contract `contract` names (symbol, secType, exchange, currency, expiry).
    fn details_of(&self, contract: &IbkrContract) -> Result<Vec<ClassRow>, String>;
    /// `reqMarketRule(id)`.
    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String>;
    /// A WHAT-IF market order of `qty` on the contract of `row`, on the BUY side or the sell side.
    fn what_if(&self, row: &ClassRow, buy: bool, qty: f64) -> Result<WhatIfRow, String>;
    /// A reference price of the contract of `row` (a two-sided or last quote, delayed ones included), `None` when the
    /// venue sends none in the bound.
    fn reference_price(&self, row: &ClassRow) -> Result<Option<f64>, String>;
}

/// The facts of one admitted forex pair.
#[derive(Clone, Debug, PartialEq)]
pub struct ForexFacts {
    /// The engine name, `BASE.QUOTE.IDEALPRO`.
    pub symbol: String,
    /// IB's contract id.
    pub conid: i32,
    /// The base currency.
    pub base: String,
    /// The quote currency.
    pub quote: String,
    /// The grid and minimums, in units of the base currency and prices in the quote currency.
    pub properties: SymbolProperties,
}

/// The facts of one admitted dated future.
#[derive(Clone, Debug, PartialEq)]
pub struct FutureFacts {
    /// The engine name, `ROOT.EXCHANGE.CURRENCY.FUT.YYYYMMDD` with the multiplier when the name claimed one: always
    /// the name that was ASKED (a name in any other spelling is refused with this one).
    pub symbol: String,
    /// IB's contract id.
    pub conid: i32,
    /// The grid and minimums; `contract_size` is the multiplier.
    pub properties: SymbolProperties,
    /// What one contract is worth per price point.
    pub contract_value: ContractValue,
    /// The settlement currency.
    pub currency: String,
    /// The last trading day at 00:00 UTC, epoch ms.
    pub last_trade_ms: i64,
    /// The initial-margin rate (the larger side's initial margin per contract over the notional at the reference
    /// price), when both could be read.
    pub margin_rate: Option<f64>,
}

fn refused(msg: String) -> FactsError {
    FactsError::Refused(msg)
}

fn unavailable(what: &str, why: &str) -> FactsError {
    FactsError::Unavailable(format!("ibkr: {what} did not answer: {why}"))
}

/// The ONE row a details reply must be, or the refusal naming each contract it lists.
fn one_row<'a>(
    rows: &'a [ClassRow],
    name: &str,
    sec_type: &str,
) -> Result<&'a ClassRow, FactsError> {
    let mut ids: Vec<i32> = rows.iter().map(|r| r.conid).collect();
    ids.sort_unstable();
    ids.dedup();
    match (rows.first(), ids.as_slice()) {
        (None, _) => Err(refused(format!("{name}: the gateway returned no contract details"))),
        (Some(row), [_]) if row.sec_type == sec_type => Ok(row),
        (Some(row), [_]) => Err(refused(format!(
            "{name}: the contract details name a {} contract (conId {}) and not a {sec_type}",
            row.sec_type, row.conid
        ))),
        (Some(_), many) => {
            let listed: Vec<String> = rows
                .iter()
                .take(8)
                .map(|r| format!("conId {} ({} {})", r.conid, r.symbol, r.last_trade))
                .collect();
            Err(refused(format!(
                "{name} is ambiguous: it names {} contracts ({}); state the one you mean (a future's multiplier \
                 as the last field of its name)",
                many.len(),
                listed.join("; ")
            )))
        }
    }
}

/// The grid tick from the market rules the contract names: THE one rule of `crate::any_symbol::grid_tick_of` (decision
/// 0220), asked with no price (a forex pair's and the admitted futures' rules are one band, so the finest band is the
/// tick; the call order the tests pin stays details, rule, price, what-if).
fn grid_tick(lookup: &dyn ClassLookup, row: &ClassRow, name: &str) -> Result<f64, FactsError> {
    crate::any_symbol::grid_tick_of(
        &|id| lookup.market_rule(id),
        row.min_tick,
        &row.market_rule_ids,
        name,
        None,
    )
    .map(|(tick, _)| tick)
}

/// The size step and minimum a row states: the step must be usable, the minimum may be absent (then 0).
fn size_rules(row: &ClassRow, name: &str) -> Result<(f64, f64), FactsError> {
    let step = row.size_increment.filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| {
        refused(format!("{name}: the contract details carry no size increment, so the order size grid is unknown"))
    })?;
    let min_qty = row.min_size.unwrap_or(0.0);
    if !min_qty.is_finite() || min_qty < 0.0 {
        return Err(refused(format!(
            "{name}: the contract details carry a minimum size of {min_qty}"
        )));
    }
    Ok((step, min_qty))
}

/// **The IdealPro minimum order size the venue's warning states**, as `(base currency, size)`: the order's own
/// `warningText` for a size below it reads `Your order size is below the EUR 20000 IdealPro minimum.`
#[must_use]
pub fn idealpro_minimum(warning_text: &str) -> Option<(String, f64)> {
    let tail = warning_text.split("below the ").nth(1)?;
    let mut words = tail.split_whitespace();
    let (ccy, size, tag) = (words.next()?, words.next()?, words.next()?);
    if !tag.eq_ignore_ascii_case("IdealPro") || !is_ccy(ccy) {
        return None;
    }
    let size: f64 = size.parse().ok().filter(|s: &f64| s.is_finite() && *s > 0.0)?;
    Some((ccy.to_ascii_uppercase(), size))
}

/// **The facts of the forex pair `name`**: the one CASH contract on IDEALPRO, its grid from its market rule, its size
/// step from the details and its minimum the IdealPro minimum the venue states for the pair's base currency (an
/// order below it is an odd lot, which the engine does not place).
///
/// # Errors
/// [`FactsError::Refused`] for every verdict in the module doc (no contract, several, another kind, a different symbol
/// or currency than the name's, no tick, rules that do not nest, no size grid, an IdealPro minimum stated in another
/// currency than the pair's base); [`FactsError::Unavailable`] when a request did not answer.
pub fn forex_facts(lookup: &dyn ClassLookup, name: &ForexName) -> Result<ForexFacts, FactsError> {
    let engine = name.engine_name();
    let contract = parse_simplified(&engine).map_err(refused)?;
    let rows = lookup
        .details_of(&contract)
        .map_err(|why| unavailable(&format!("contract details of {engine}"), &why))?;
    let row = one_row(&rows, &engine, "CASH")?;
    if !row.symbol.eq_ignore_ascii_case(&name.base)
        || !row.currency.eq_ignore_ascii_case(&name.quote)
    {
        return Err(refused(format!(
            "{engine}: the contract details name {}.{} (conId {}) and not the pair asked for; nothing is admitted on a disagreement",
            row.symbol, row.currency, row.conid
        )));
    }
    if row.price_magnifier > 1 {
        return Err(refused(format!(
            "{engine}: the contract carries a price magnifier of {}, so the unit its prices are reported in is not known",
            row.price_magnifier
        )));
    }
    let tick = grid_tick(lookup, row, &engine)?;
    let (step, details_min) = size_rules(row, &engine)?;
    // A size of one base unit is below every IdealPro minimum: the warning on the answer to such a what-if states it.
    let probe = lookup
        .what_if(row, true, 1.0)
        .map_err(|why| unavailable(&format!("the what-if order on {engine}"), &why))?;
    let min_qty = match idealpro_minimum(&probe.warning_text) {
        Some((ccy, size)) if ccy.eq_ignore_ascii_case(&name.base) => size.max(details_min),
        Some((ccy, size)) => {
            return Err(refused(format!(
                "{engine}: the venue states its IdealPro minimum as {size} {ccy}, which is not the pair's base currency {}; the order size floor is unknown",
                name.base
            )));
        }
        None => details_min,
    };
    Ok(ForexFacts {
        symbol: engine,
        conid: row.conid,
        base: name.base.clone(),
        quote: name.quote.clone(),
        properties: SymbolProperties {
            tick_size: tick,
            step_size: step,
            min_qty,
            asset_class: Some(AssetClass::Fx),
            ..SymbolProperties::default()
        },
    })
}

/// The last trading day of a row as `YYYYMMDD`: `realExpiration` when it is a day, else the first eight digits of the
/// contract's own `lastTradeDateOrContractMonth` when they are.
fn last_trade_day(row: &ClassRow) -> Option<String> {
    let day = |s: &str| -> Option<String> {
        let head: String = s.chars().take(8).collect();
        (head.len() == 8 && head.bytes().all(|b| b.is_ascii_digit())).then_some(head)
    };
    day(&row.real_expiration).or_else(|| day(&row.last_trade))
}

/// A `YYYYMMDD` as that day's 00:00 UTC in epoch ms; `None` for a day the calendar does not have.
fn day_ms(day: &str) -> Option<i64> {
    let (y, m, d): (i64, u32, u32) =
        (day.get(0..4)?.parse().ok()?, day.get(4..6)?.parse().ok()?, day.get(6..8)?.parse().ok()?);
    let days = days_from_civil(y, m, d);
    (civil_from_days(days) == (y, m, d)).then_some(days * DAY_MS)
}

/// **The initial-margin rate of a future**: the larger of the two sides' initial margin per contract over the notional
/// (`price * multiplier`), `None` when any figure is missing or not positive, or the rate is not in `(0, 1]`. A rate the
/// admission cannot state leaves the engine's own (`SymbolAdmission::im_requirement`).
#[must_use]
pub fn future_margin_rate(
    buy: Option<f64>,
    sell: Option<f64>,
    price: f64,
    multiplier: f64,
) -> Option<f64> {
    let side = |m: Option<f64>| m.filter(|m| m.is_finite() && *m > 0.0);
    let margin = match (side(buy), side(sell)) {
        (Some(b), Some(s)) => b.max(s),
        (Some(m), None) | (None, Some(m)) => m,
        (None, None) => return None,
    };
    let notional = price * multiplier;
    (price.is_finite() && price > 0.0 && multiplier > 0.0)
        .then(|| margin / notional)
        .filter(|r| r.is_finite() && *r > 0.0 && *r <= 1.0)
}

/// **The facts of the dated future `name`** at `now_ms` (the node's clock): a cash-settled root, exactly one contract
/// of the exchange, currency and expiry the name states, a multiplier, a last trading day at least
/// [`MIN_DAYS_TO_LAST_TRADE`] days away, the grid of its market rules, and the margin rate the venue's what-if orders
/// state at the reference price (absent when either cannot be had).
///
/// # Errors
/// [`FactsError::Refused`] for every verdict above, naming the contract and the day; [`FactsError::Unavailable`] when a
/// details or rule request did not answer (a what-if or a quote that did not only costs the margin rate).
pub fn future_facts(
    lookup: &dyn ClassLookup,
    name: &FutureName,
    now_ms: i64,
) -> Result<FutureFacts, FactsError> {
    let asked = format!("{}.{}.{}.FUT.{}", name.root, name.exchange, name.currency, name.expiry);
    if !CASH_SETTLED_ROOTS.contains(&name.root.as_str()) {
        return Err(refused(format!(
            "{asked}: the root {} is not admitted: only the cash-settled equity-index futures ({}) are, because \
             IB closes a position in a future that settles by delivery before its last trading day, at prices the \
             engine would carry as a position the venue no longer holds",
            name.root,
            CASH_SETTLED_ROOTS.join(", ")
        )));
    }
    let contract = parse_simplified(&asked).map_err(refused)?;
    let rows = lookup
        .details_of(&contract)
        .map_err(|why| unavailable(&format!("contract details of {asked}"), &why))?;
    let rows: Vec<ClassRow> = rows
        .into_iter()
        .filter(|r| {
            r.symbol.eq_ignore_ascii_case(&name.root)
                && r.currency.eq_ignore_ascii_case(&name.currency)
        })
        .collect();
    let row = one_row(&rows, &asked, "FUT")?;
    let Some(day) = last_trade_day(row) else {
        return Err(refused(format!(
            "{asked}: the contract details state no last trading day (conId {}), so its expiry cannot be guarded",
            row.conid
        )));
    };
    if name.expiry.len() == 8 && name.expiry != day {
        return Err(refused(format!(
            "{asked}: the contract of that name last trades on {day} (conId {}), not on {}",
            row.conid, name.expiry
        )));
    }
    let Some(last_trade_ms) = day_ms(&day) else {
        return Err(refused(format!("{asked}: the last trading day {day} is not a calendar day")));
    };
    let remaining_days = (last_trade_ms - now_ms).div_euclid(DAY_MS);
    if remaining_days < MIN_DAYS_TO_LAST_TRADE {
        return Err(refused(format!(
            "{asked}: the contract last trades on {day}, {remaining_days} day(s) from now, and a future with fewer \
             than {MIN_DAYS_TO_LAST_TRADE} days left is not admitted (nothing in the engine rolls or closes it \
             at expiry); name the next month"
        )));
    }
    let multiplier =
        row.multiplier.trim().parse::<f64>().ok().filter(|m| m.is_finite() && *m > 0.0);
    let Some(multiplier) = multiplier else {
        return Err(refused(format!(
            "{asked}: the contract carries no usable multiplier ({:?}), so what one contract is worth is unknown",
            row.multiplier
        )));
    };
    if let Some(claimed) = name.multiplier.as_deref().and_then(|m| m.parse::<f64>().ok())
        && (claimed - multiplier).abs() > f64::EPSILON
    {
        return Err(refused(format!(
            "{asked}: the name claims a multiplier of {claimed} and the contract carries {multiplier}"
        )));
    }
    if row.price_magnifier > 1 {
        return Err(refused(format!(
            "{asked}: the contract carries a price magnifier of {}, so the unit its prices are reported in is not known",
            row.price_magnifier
        )));
    }
    // ONE position, ONE name: the exchange the VENUE names (`CME` for a name that said `GLOBEX`) and the last trading
    // DAY (for a name that said the month). The node admits and orders under the name it asked, so a name that is
    // not the canonical one is refused here with the canonical one, never admitted under a second spelling.
    let exchange = if row.exchange.is_empty() {
        name.exchange.clone()
    } else {
        row.exchange.to_ascii_uppercase()
    };
    let symbol = match &name.multiplier {
        Some(m) => format!("{}.{exchange}.{}.FUT.{day}.{m}", name.root, name.currency),
        None => format!("{}.{exchange}.{}.FUT.{day}", name.root, name.currency),
    };
    let typed = match &name.multiplier {
        Some(m) => format!("{asked}.{m}"),
        None => asked.clone(),
    };
    if !symbol.eq_ignore_ascii_case(&typed) {
        return Err(refused(format!(
            "{typed}: this contract is named `{symbol}` here (the venue's exchange and its last trading day): \
             spell it that way, so that one position has one name"
        )));
    }
    let tick = grid_tick(lookup, row, &asked)?;
    let (step, min_qty) = size_rules(row, &asked)?;
    let margin_rate = lookup.reference_price(row).ok().flatten().and_then(|price| {
        let side =
            |buy: bool| lookup.what_if(row, buy, 1.0).ok().and_then(|w| w.initial_margin_change);
        future_margin_rate(side(true), side(false), price, multiplier)
    });
    Ok(FutureFacts {
        symbol,
        conid: row.conid,
        properties: SymbolProperties {
            tick_size: tick,
            step_size: step,
            min_qty,
            contract_size: multiplier,
            asset_class: Some(AssetClass::Future),
            ..SymbolProperties::default()
        },
        contract_value: ContractValue::linear(multiplier),
        currency: row.currency.to_ascii_uppercase(),
        last_trade_ms,
        margin_rate,
    })
}

#[cfg(test)]
#[path = "any_symbol_forex_future_tests.rs"]
mod any_symbol_forex_future_tests;
