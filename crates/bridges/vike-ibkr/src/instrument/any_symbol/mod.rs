//! **One US stock, admitted by NAME** (any-symbol, decisions 0173/0174): what the gatewayhub answers
//! when the node asks for the facts of `AAPL`, an instrument the account's engine was not
//! mounted on. Every name here is a kept row's WIRE (`AAPL.SMART.USD`): the gatewayhub reads the asked user name
//! through the operator's row first (`crate::instrument::contract::wire_name_of_user_name`, decision 0290).
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
//! Only `TICKER.SMART.CCY` (the three-field engine name, `crate::instrument::contract::parse_simplified`'s cash equity)
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
//!   other than 1, or give no usable minimum tick, size increment or market rule. (A price magnifier other
//!   than 1, the one field IBKR gives for the unit prices are reported in, refuses a stock of ANOTHER currency
//!   than the account's on the converting path only: [`price_magnifier_refusal`].)
//!
//! For an account that CONVERTS a quote currency (decision 0183, `crate::account::quote_rate`) the currency fence is lifted
//! by [`stock_facts_any_currency`]: the name's currency may be any currency, and the rate that books a stock of
//! another currency in the ledger's own is read apart, fresh on every ask, never from the kept facts.
//!
//! # The grid (decision 0220: the band of the CURRENT price, never the coarsest)
//!
//! `SymbolGrid` carries ONE tick, but a stock's tick depends on its price (`minTick` is only "the smallest of the
//! different minimum tick sizes", IB's own words), and a market rule is a TABLE of price bands. The grid tick is
//! the increment of the band the stock's price falls in ([`ContractLookup::reference_price`]: a delayed or closing
//! price is enough, only the band is read off it); when the contract names several rules (one per listed
//! exchange) it is the coarsest of THEIR increments at that price, and only when every other divides it. With NO
//! price known it is the FINEST increment of every band (every increment must be a multiple of it). Rules that do
//! not nest, or that cannot be read, are refused.
//!
//! ⚠ **Why never the coarsest band** (the first reading of this record took it, so that a price on that grid was valid
//! in every band): the coarsest band of a European stock's rule is the one for the HIGHEST prices (the captured
//! rule of `SAP.SMART.EUR`, `instrument/any_symbol/any_symbol_tests.rs`'s `SAP_RULE`, ends `50000: 10`), so SAP's facts answered a tick of
//! 10.0 and the engine would have rounded a 200 EUR limit to a multiple of 10 EUR while the venue's own step there
//! is 0.05. The wire owes the venue's real increment, never a coarser one.
//!
//! ⚠ **What the facts do NOT hold**: the tick is the band of the price AT THE ASK. The facts are kept ten minutes
//! by the gatewayhub and an hour by the node, so a price that leaves its band afterwards is judged at ORDER time,
//! by this crate's exec (`crate::instrument::tick_grid`'s `refusal`: a limit price off the grid of every rule the contract names,
//! at its own price, is refused by name with the venue's real increment, never sent), and a refresh of the facts
//! re-reads the band.
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

// (Its docs are the module's own `//!` block.)
pub mod forex_future;
/// The socket backend of [`crate::instrument::any_symbol`]: the lookup over one data connection, and the source the
/// gatewayhub asks.
#[cfg(feature = "ibkr-socket")]
pub mod socket;

use vike_model::{AssetClass, SymbolProperties};

use crate::instrument::contract::{ibkr_symbol, same_ticker};
use crate::refusals::IbkrSymbolRefusal;

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
    /// IB's `priceMagnifier` ("allows execution and strike prices to be reported consistently with market data":
    /// the factor between a price in the contract's quoting unit and the one the API reports). `1` for a share
    /// quoted in the whole units of its currency; anything else is a price this adapter does not read.
    pub price_magnifier: i32,
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

/// The requests a name is resolved with. The production impl runs them on one connection of the
/// logged-in gateway (`crate::instrument::any_symbol::socket`); a test scripts them. An `Err` is a request that did not
/// answer (the gateway is not ready), never a verdict about the symbol.
pub trait ContractLookup {
    /// `reqMatchingSymbols(pattern)`.
    fn matching_symbols(&self, pattern: &str) -> Result<Vec<MatchRow>, String>;
    /// `reqContractDetails` of the one contract `contract` names.
    fn contract_details(&self, contract: &MatchRow) -> Result<Vec<DetailsRow>, String>;
    /// `reqMarketRule(id)`.
    fn market_rule(&self, id: i32) -> Result<Vec<PriceBand>, String>;
    /// A recent price of `contract` in its own currency (delayed, frozen and closing prices count: only the BAND
    /// the price falls in is read off it), or `None` when the gateway sent none within a short bound. Not a verdict
    /// and never an error: without a price the grid is the finest band.
    fn reference_price(&self, _contract: &MatchRow) -> Option<f64> {
        None
    }
}

/// Why no facts: a verdict about the name, or a gateway that could not say. Either carries its refusal, whose text an
/// operator reads verbatim.
#[derive(Clone, Debug, PartialEq)]
pub enum IbkrPropertiesError {
    /// The name cannot be admitted. A definitive answer.
    Refused(IbkrSymbolRefusal),
    /// The gateway did not answer (not logged in, a request failed or timed out): ask again later.
    Unavailable(IbkrSymbolRefusal),
}

/// The facts of one admitted stock.
#[derive(Clone, Debug, PartialEq)]
pub struct IbkrPropertiesStock {
    /// The name asked, `TICKER.SMART.CCY`, upper case: a kept row's WIRE name (decision 0290; the gatewayhub asks by
    /// it, `crate::instrument::contract::wire_name_of_user_name`).
    pub symbol: String,
    /// IB's contract id of the stock the name resolved to.
    pub conid: i32,
    /// The grid and minimums, in shares and the stock's currency.
    pub properties: SymbolProperties,
    /// The contract's `priceMagnifier` as the gateway reported it (`1` for a share quoted in whole units, `0` when
    /// the gateway left it unset). Recorded, never judged here: only a FOREIGN-currency stock is refused for it
    /// ([`price_magnifier_refusal`]), and the plain path ignores it.
    pub price_magnifier: i32,
    /// What the grid tick was read from: the price of the ask and the contract's market rules.
    pub tick_grid: TickGrid,
}

/// **What `IbkrPropertiesStock`' grid tick was read from** (module doc, "The grid"): the price the band was chosen for and every
/// market rule the contract names, each as IB answered it and sorted by low edge. The exec holds an order's price to the
/// rules ([`crate::instrument::tick_grid`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickGrid {
    /// The price the band was chosen for; `None` when none was known and the FINEST band was taken.
    pub price: Option<f64>,
    /// The market rules, one table of bands each.
    pub rules: Vec<Vec<PriceBand>>,
}

/// Why a forex pair is not a STOCK name, in the operator's words: the refusal of THIS (stock) entry. A pair is admitted
/// by `crate::instrument::any_symbol::forex_future` (the gatewayhub dispatches a pair's name there before it reaches the stock
/// lookup), so this text is read only by a caller that hands a pair to the stock lookup directly. An IBKR forex pair is a CASH position
/// (secType `CASH`): trading it changes the account's cash balance in TWO currencies and has no margin PnL, while
/// this engine books a position's profit against one ledger. Converting a quote currency (`crate::account::quote_rate`) does
/// not make that right, so the refusal stands (decision 0183, "an account holds foreign cash" reopens it).
pub const FOREX_REFUSAL: &str = "an IBKR forex pair is a CASH position: buying it changes the \
    account's cash balances in two currencies and books no margin profit, which this engine's \
    margin-account model does not hold, so it is refused and not converted";

/// How a stock is named: the hint of every refusal of a name that is not one ([`parse_stock_name`]).
pub const STOCK_NAME_HINT: &str = "an IBKR stock is admitted as `TICKER.SMART.CCY`, e.g. `AAPL.SMART.USD` (a ticker of \
    letters and digits, `_` for the space of a class share such as `BRK_B`, routed SMART, with its \
    currency; pin one contract with `TICKER.SMART.CCY.STK.<conId>`, e.g. \
    `AAPL.SMART.USD.STK.265598`; forex, futures, options and an explicit exchange are not \
    admitted by name)";

/// The three parts of an engine stock name, upper-cased: `TICKER.SMART.CCY`, or the same pinned to one contract,
/// `TICKER.SMART.CCY.STK.<conId>` (decision 0218).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StockName {
    /// The ticker as the ENGINE spells it: letters and digits, with `_` standing for IB's space in a class
    /// share (`BRK_B` is IB's `BRK B`, `crate::instrument::contract::ibkr_symbol`).
    pub ticker: String,
    /// The currency the name states.
    pub currency: String,
    /// IB's contract id when the name pins one: it names the contract itself, so a ticker listed on more than one
    /// exchange is no longer ambiguous.
    pub con_id: Option<i32>,
}

impl StockName {
    /// The engine name these parts spell: `TICKER.SMART.CCY`, or `TICKER.SMART.CCY.STK.<conId>` when pinned.
    #[must_use]
    pub fn engine_name(&self) -> String {
        match self.con_id {
            Some(id) => format!("{}.{ROUTE}.{}.STK.{id}", self.ticker, self.currency),
            None => stock_name(&self.ticker, &self.currency),
        }
    }
}

/// The parts of an engine name `TICKER.SMART.CCY` or `TICKER.SMART.CCY.STK.<conId>`, upper-cased, or why it is
/// not one.
///
/// # Errors
/// The refusal naming the form: any other field count, any other route, a ticker with a character a ticker does not
/// have (a class share's dot is spelled `_` in the engine name, `crate::instrument::contract`), a currency that is not three
/// letters, a conId that is not a positive whole number.
pub fn parse_stock_name(name: &str) -> Result<StockName, IbkrSymbolRefusal> {
    let not_a_stock_name = || IbkrSymbolRefusal::NotAStockName { name: name.to_string() };
    let parts: Vec<&str> = name.trim().split('.').collect();
    // The forex exchange in the route or currency field (`EUR.IDEALPRO.USD`, `EUR.USD.IDEALPRO`) or a claimed
    // CASH secType (`EUR.IDEALPRO.USD.CASH`). Not the ticker field: `CASH` is a stock's ticker.
    let forex = parts.iter().skip(1).any(|p| p.eq_ignore_ascii_case("IDEALPRO"))
        || parts.get(3).is_some_and(|p| p.eq_ignore_ascii_case("CASH"));
    if forex {
        return Err(IbkrSymbolRefusal::ForexIsNotAStock { name: name.to_string() });
    }
    let (ticker, route, currency, con_id) = match parts.as_slice() {
        [ticker, route, currency] => (*ticker, *route, *currency, None),
        [ticker, route, currency, claim, id] if claim.eq_ignore_ascii_case("STK") => {
            let con_id = id.parse::<i32>().ok().filter(|n| *n > 0);
            let Some(con_id) = con_id else {
                return Err(IbkrSymbolRefusal::StockContractIdNotPositive {
                    name: name.to_string(),
                    id: (*id).to_string(),
                });
            };
            (*ticker, *route, *currency, Some(con_id))
        }
        _ => return Err(not_a_stock_name()),
    };
    let ticker_ok = !ticker.is_empty()
        && ticker.len() <= MAX_TICKER_LEN
        && ticker.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !ticker.starts_with('_')
        && !ticker.ends_with('_');
    let currency_ok = currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_alphabetic());
    if !route.eq_ignore_ascii_case(ROUTE) || !ticker_ok || !currency_ok {
        return Err(not_a_stock_name());
    }
    Ok(StockName {
        ticker: ticker.to_ascii_uppercase(),
        currency: currency.to_ascii_uppercase(),
        con_id,
    })
}

/// The engine name of `ticker` in `currency`.
#[must_use]
pub fn stock_name(ticker: &str, currency: &str) -> String {
    format!("{ticker}.{ROUTE}.{currency}")
}

/// The parts of `name`, after the two refusals that cost no request: its form ([`parse_stock_name`]) and its
/// currency against `account_currency` (the ledger is one currency and nothing converts, so a stock of another
/// currency is a fence, not a conversion).
///
/// # Errors
/// [`IbkrPropertiesError::Refused`] naming the form or the two currencies.
pub fn checked_name(name: &str, account_currency: &str) -> Result<StockName, IbkrPropertiesError> {
    let parsed = parse_stock_name(name).map_err(IbkrPropertiesError::Refused)?;
    if !parsed.currency.eq_ignore_ascii_case(account_currency) {
        return Err(IbkrPropertiesError::Refused(IbkrSymbolRefusal::StockOfAnotherCurrency {
            name: name.to_string(),
            currency: parsed.currency,
            account_currency: account_currency.to_string(),
        }));
    }
    Ok(parsed)
}

/// A request of the gateway that did not answer (`what` names it, `why` is the gateway's error): "not now".
pub(crate) fn unavailable(what: String, why: String) -> IbkrPropertiesError {
    IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::DidNotAnswer { what, why })
}

fn refused(refusal: IbkrSymbolRefusal) -> IbkrPropertiesError {
    IbkrPropertiesError::Refused(refusal)
}

/// **Resolve `name` (`TICKER.SMART.CCY`) to the facts of one stock** through `lookup`. `account_currency`
/// is the currency of the account's money: a name stating another currency is refused before any request
/// (the ledger is one currency and nothing converts).
///
/// At most `2 + MAX_MARKET_RULES` requests are made (plus one bounded price read, [`ContractLookup::reference_price`]),
/// and none when the name is refused by its form or its currency ([`checked_name`]).
///
/// # Errors
/// [`IbkrPropertiesError::Refused`] for every verdict in the module doc; [`IbkrPropertiesError::Unavailable`] when a request
/// did not answer.
pub fn stock_facts(
    lookup: &dyn ContractLookup,
    name: &str,
    account_currency: &str,
) -> Result<IbkrPropertiesStock, IbkrPropertiesError> {
    let parsed = checked_name(name, account_currency)?;
    resolve_stock(lookup, &parsed)
}

/// [`stock_facts`] for an account that CONVERTS a quote currency (decision 0183): the same resolution, the same
/// refusals, and NO currency fence. The name's currency may be any currency; whether it is the ledger's own or
/// needs a rate is [`crate::account::quote_rate`]'s to say, from the account's base currency, and the caller attaches it. The
/// grid and minimums are in the stock's own currency (`SymbolProperties` carries no currency, as for every venue).
///
/// # Errors
/// As [`stock_facts`], minus the currency fence.
pub fn stock_facts_any_currency(
    lookup: &dyn ContractLookup,
    name: &str,
) -> Result<IbkrPropertiesStock, IbkrPropertiesError> {
    let parsed = parse_stock_name(name).map_err(IbkrPropertiesError::Refused)?;
    resolve_stock(lookup, &parsed)
}

/// The resolution of an already-parsed stock name (the body both entry points share). A name that PINS a conId
/// skips the paced symbol search altogether: the contract is named, so the request is the contract's own details,
/// and `checked_details` holds the ticker the name states against the contract the id is.
fn resolve_stock(
    lookup: &dyn ContractLookup,
    parsed: &StockName,
) -> Result<IbkrPropertiesStock, IbkrPropertiesError> {
    let (ticker, currency) = (parsed.ticker.clone(), parsed.currency.clone());
    let name = parsed.engine_name();
    let chosen = match parsed.con_id {
        Some(conid) => MatchRow {
            conid,
            symbol: ibkr_symbol(&ticker),
            sec_type: "STK".to_string(),
            primary_exchange: String::new(),
            currency: currency.clone(),
        },
        None => choose_stock(lookup, &name, &ticker, &currency)?,
    };
    let rows = lookup
        .contract_details(&chosen)
        .map_err(|why| unavailable(format!("contract details of {name}"), why))?;
    let details = checked_details(&rows, &chosen, &name, &currency)?;
    let price = lookup.reference_price(&chosen);
    let (tick, tick_grid) = grid_tick(lookup, details, &name, price)?;
    // TWS sent NO size rules at all (neither a usable minSize nor sizeIncrement): the order's size grid and
    // minimum are unknown, so the stock is refused by name rather than admitted on a guessed floor.
    let usable = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
    if usable(details.size_increment).is_none() && usable(details.min_size).is_none() {
        return Err(refused(IbkrSymbolRefusal::NoSizeRules { name }));
    }
    let step = details
        .size_increment
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| refused(IbkrSymbolRefusal::StockNoSizeIncrement { name: name.clone() }))?;
    let min_qty = details.min_size.unwrap_or(0.0);
    if !min_qty.is_finite() || min_qty < 0.0 {
        return Err(refused(IbkrSymbolRefusal::MinimumSize { name, min_qty }));
    }
    Ok(IbkrPropertiesStock {
        symbol: name,
        conid: chosen.conid,
        properties: SymbolProperties {
            tick_size: tick,
            step_size: step,
            min_qty,
            asset_class: Some(AssetClass::Equity),
            ..SymbolProperties::default()
        },
        price_magnifier: details.price_magnifier,
        tick_grid,
    })
}

/// The ONE stock of `ticker` in `currency` the matching request names, or the refusal.
fn choose_stock(
    lookup: &dyn ContractLookup,
    name: &str,
    ticker: &str,
    currency: &str,
) -> Result<MatchRow, IbkrPropertiesError> {
    // The search takes the ticker's first word (`BRK` for the class share `BRK_B`: IB lists it as `BRK B`, and a
    // space in the pattern is not something the search is known to take); the rows are then held to the WHOLE ticker.
    let pattern = ticker.split('_').next().unwrap_or(ticker);
    let rows = lookup
        .matching_symbols(pattern)
        .map_err(|why| unavailable(format!("the symbol search for {ticker}"), why))?;
    // `reqMatchingSymbols` answers every contract whose symbol STARTS with the pattern.
    let named: Vec<&MatchRow> = rows.iter().filter(|r| same_ticker(&r.symbol, ticker)).collect();
    if named.is_empty() {
        return Err(refused(IbkrSymbolRefusal::NoContractForTicker {
            name: name.to_string(),
            ticker: ticker.to_string(),
        }));
    }
    let stocks: Vec<&MatchRow> = named.iter().copied().filter(|r| r.sec_type == "STK").collect();
    if stocks.is_empty() {
        let mut kinds: Vec<&str> = named.iter().map(|r| r.sec_type.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        return Err(refused(IbkrSymbolRefusal::TickerNamesNoStock {
            name: name.to_string(),
            ticker: ticker.to_string(),
            kinds: kinds.join(", "),
        }));
    }
    let mut ours: Vec<&MatchRow> =
        stocks.iter().copied().filter(|r| r.currency.eq_ignore_ascii_case(currency)).collect();
    if ours.is_empty() {
        let mut others: Vec<String> =
            stocks.iter().map(|r| r.currency.to_ascii_uppercase()).collect();
        others.sort_unstable();
        others.dedup();
        return Err(refused(IbkrSymbolRefusal::StockInOtherCurrencies {
            name: name.to_string(),
            ticker: ticker.to_string(),
            others: others.join(", "),
            currency: currency.to_string(),
        }));
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
    Err(refused(IbkrSymbolRefusal::AmbiguousStock {
        name: name.to_string(),
        ticker: ticker.to_string(),
        count: ours.len(),
        currency: currency.to_string(),
        listed: listed.join("; "),
        first: ours[0].conid,
    }))
}

/// The one details row the reply is, checked against the match: every row names the chosen contract,
/// and the contract is a share of the right currency with a multiplier of 1.
fn checked_details<'a>(
    rows: &'a [DetailsRow],
    chosen: &MatchRow,
    name: &str,
    currency: &str,
) -> Result<&'a DetailsRow, IbkrPropertiesError> {
    let Some(first) = rows.first() else {
        return Err(refused(IbkrSymbolRefusal::NoStockDetails {
            name: name.to_string(),
            conid: chosen.conid,
        }));
    };
    for row in rows {
        if row.conid != chosen.conid
            || row.sec_type != "STK"
            || !row.currency.eq_ignore_ascii_case(currency)
            || !same_ticker(&row.symbol, &chosen.symbol)
        {
            return Err(refused(IbkrSymbolRefusal::StockDetailsDisagree {
                name: name.to_string(),
                row: format!("{} {} {}", row.symbol, row.sec_type, row.currency),
                conid: row.conid,
                chosen: chosen.conid,
            }));
        }
    }
    let multiplier = first.multiplier.trim();
    if !multiplier.is_empty() {
        match multiplier.parse::<f64>() {
            Ok(1.0) => {}
            _ => {
                return Err(refused(IbkrSymbolRefusal::StockMultiplier {
                    name: name.to_string(),
                    multiplier: multiplier.to_string(),
                }));
            }
        }
    }
    Ok(first)
}

/// **Why a FOREIGN-currency stock whose contract carries `magnifier` is not admitted, or `None`.** IBKR's pages
/// never say that a contract's `currency` is the unit its PRICES are quoted in (pence, agorot and cents quoting
/// is third-party knowledge only), so the one field IBKR does give for the unit prices are reported in
/// (`priceMagnifier`) is read: a share is `1`. Whatever else it holds, and `0` (a field the gateway left unset),
/// is a price whose unit is not known, and booking it as whole units could be 100 times too high.
///
/// ⚠ Judged by the CONVERTING path only, and only for a stock whose currency differs from the account's base
/// (`vike-gatewayhub`'s `symbol_properties::quoted_facts`, once the rate read has said so): [`stock_facts`] (the plain path
/// of an account that converts nothing) and a stock in the base currency are NOT refused for it, so a gateway
/// that reports `0` for every ordinary stock changes nothing for an account that never converted.
#[must_use]
pub fn price_magnifier_refusal(
    name: &str,
    currency: &str,
    magnifier: i32,
) -> Option<IbkrSymbolRefusal> {
    (magnifier != 1).then(|| IbkrSymbolRefusal::ForeignPriceMagnifier {
        name: name.to_string(),
        magnifier,
        currency: currency.to_string(),
    })
}

/// The increment of the band of `bands` (sorted by low edge, never empty) that `price` falls in: the last band whose
/// low edge is not above it, and the first band for a price below every edge.
pub(crate) fn increment_at(bands: &[PriceBand], price: f64) -> f64 {
    bands
        .iter()
        .rev()
        .find(|b| b.low_edge <= price)
        .or_else(|| bands.first())
        .map_or(0.0, |b| b.increment)
}

/// The grid tick and what it was read from: the increment of the band the price falls in (the coarsest of the
/// rules' increments there, when they all divide it), or the finest increment of every band when no price is
/// known. See the module doc.
fn grid_tick(
    lookup: &dyn ContractLookup,
    details: &DetailsRow,
    name: &str,
    price: Option<f64>,
) -> Result<(f64, TickGrid), IbkrPropertiesError> {
    grid_tick_of(
        &|id| lookup.market_rule(id),
        details.min_tick,
        &details.market_rule_ids,
        name,
        price,
    )
}

/// **THE grid rule** (module doc, "The grid"; decision 0220), over any lookup's `market_rule`: the stock half above,
/// the forex and future half (`crate::instrument::any_symbol::forex_future`) and a MOUNT's grid pre-fetch (`crate::instrument::properties`) all
/// read their tick through it, so no second copy of the rule can drift. `rule_ids` are the contract's market-rule words
/// (comma lists allowed), `min_tick` its details' minimum (refused when unusable), `price` the price the band is chosen
/// for (`None`, or one that is not a price: zero, negative, not finite, is the finest band).
pub(crate) fn grid_tick_of(
    market_rule: &dyn Fn(i32) -> Result<Vec<PriceBand>, String>,
    min_tick: f64,
    rule_ids: &[String],
    name: &str,
    price: Option<f64>,
) -> Result<(f64, TickGrid), IbkrPropertiesError> {
    let price = price.filter(|p| p.is_finite() && *p > 0.0);
    let name_of = || name.to_string();
    if !min_tick.is_finite() || min_tick <= 0.0 {
        return Err(refused(IbkrSymbolRefusal::NoMinimumTick { name: name_of() }));
    }
    let mut ids: Vec<i32> = Vec::new();
    for word in rule_ids.iter().flat_map(|w| w.split(',')) {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        let id: i32 = word.parse().map_err(|_| {
            refused(IbkrSymbolRefusal::MarketRuleIdNotANumber {
                name: name_of(),
                word: word.to_string(),
            })
        })?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(refused(IbkrSymbolRefusal::NoMarketRule { name: name_of(), min_tick }));
    }
    if ids.len() > MAX_MARKET_RULES {
        return Err(refused(IbkrSymbolRefusal::TooManyMarketRules {
            name: name_of(),
            count: ids.len(),
        }));
    }
    let mut rules: Vec<Vec<PriceBand>> = Vec::new();
    for id in ids {
        let mut bands = market_rule(id)
            .map_err(|why| unavailable(format!("market rule {id} of {name}"), why))?;
        if bands.is_empty() {
            return Err(refused(IbkrSymbolRefusal::MarketRuleNoBand { name: name_of(), id }));
        }
        for band in &bands {
            if !band.increment.is_finite() || band.increment <= 0.0 {
                return Err(refused(IbkrSymbolRefusal::MarketRuleIncrement {
                    name: name_of(),
                    id,
                    increment: band.increment,
                }));
            }
            if !band.low_edge.is_finite() || band.low_edge < 0.0 {
                return Err(refused(IbkrSymbolRefusal::MarketRuleLowEdge {
                    name: name_of(),
                    id,
                    low_edge: band.low_edge,
                }));
            }
        }
        bands.sort_by(|a, b| a.low_edge.total_cmp(&b.low_edge));
        rules.push(bands);
    }
    let divides = |coarse: f64, inc: f64| {
        let ratio = coarse / inc;
        (ratio - ratio.round()).abs() <= 1e-6 * ratio
    };
    let tick = match price {
        // The band of the CURRENT price: the coarsest of the rules' increments there (a price on it is valid at every
        // exchange the contract lists, at that price), when they nest.
        Some(price) => {
            let at: Vec<f64> = rules.iter().map(|bands| increment_at(bands, price)).collect();
            let coarsest = at.iter().copied().fold(0.0_f64, f64::max);
            if let Some(inc) = at.iter().find(|inc| !divides(coarsest, **inc)) {
                return Err(refused(IbkrSymbolRefusal::RulesDoNotNestAtPrice {
                    name: name_of(),
                    price,
                    increment: *inc,
                    coarsest,
                }));
            }
            coarsest
        }
        // No price: the FINEST increment of every band, never a coarser one. Every increment must be a multiple of it.
        None => {
            let all: Vec<f64> = rules.iter().flatten().map(|b| b.increment).collect();
            let finest = all.iter().copied().fold(f64::INFINITY, f64::min);
            if let Some(inc) = all.iter().find(|inc| !divides(**inc, finest)) {
                return Err(refused(IbkrSymbolRefusal::BandsDoNotNest {
                    name: name_of(),
                    finest,
                    increment: *inc,
                }));
            }
            finest
        }
    };
    Ok((tick, TickGrid { price, rules }))
}

#[cfg(test)]
#[path = "any_symbol_tests.rs"]
mod any_symbol_tests;
