//! **Which `(venue, symbol)` names a coin-margined (INVERSE) contract, read off the NAME**: for a process
//! that holds no row of the contract and must not guess `Linear` (decision 0170, item 6).
//!
//! The authority is the venue's row: its `Contract`'s [`vike_model::Margining`], through
//! [`vike_model::ContractValue::of`]. Whoever holds a row reads that. A backtest holds none: the harness
//! opens no venue connection, the recorded `kind=properties` tape states no margining and leaves inverse
//! rows out (`crates/vike-datahub/src/properties_tape.rs`'s `grid_of`), and the shipped baseline lists alpaca
//! and oanda only. The four venues that list such a contract name it by decision 0290's ONE user grammar, and the
//! VENUE is still an argument: `BTCUSD.P` is an inverse perpetual on the four while `BTCUSD` is bybit's spot pair and
//! a linear CFD on ctrader. The answer is the name's KEPT row's margining where this process holds the venue's list
//! (`crate::kept_list`, BYB-4, BIN-5), the grammar's only without a row.
//!
//! | venue (user names, decision 0290) | inverse | what stays linear |
//! |---|---|---|
//! | deribit | `BTCUSD.P` (`BTC-PERPETUAL`), `BTCUSD_261103` (`BTC-3NOV26`) | `BTCUSDC.P`, `BTCUSDC_261127`, the pair `BTCUSDC`, every option |
//! | bybit | `BTCUSD.P` (`BTCUSD`), `BTCUSD_261225` (`BTCUSDZ26`) | `BTCUSDT.P`, `BTCUSDC.P`, `ETHUSDT_261127`, the pairs `BTCUSD`, `USDTUSD` |
//! | binance | `BTCUSD.P` (`BTCUSD_PERP`), `BTCUSD_261225` | `BTCUSDT.P`, `BTCUSDT_261225`, spot |
//! | okx | `BTCUSD.P` (`BTC-USD-SWAP`), `BTCUSD_261225` (`BTC-USD-261225`) | `BTCUSDT.P`, `BTCUSDT_261225`, the pair `BTCUSDT` |
//!
//! An option is not inverse here, whichever coin it settles in: it is priced `qty · premium`
//! ([`vike_model::ContractValue::of`]). Each venue's row states what this reads: the four bridges hold this
//! function to their committed instrument fixtures (`the_name_rule_agrees_with_every_row`, in each).
//!
//! [`contract_value`] is the WHOLE value a name gives, the catalog's one answer for a process that holds no
//! row (decision 0170's catalog reopener; the backtest's profile check reads it). The four bridges' tests above,
//! fxcm's and the gatewayhub's ibkr facts tests hold it equal to the value the live path books for every
//! committed row. [`kept_contract_value`] is the same answer with the symbol's KEPT row in hand (what the paper
//! book and the backtest's coin ledger read): whether it is inverse is the row's margining
//! ([`kept_is_inverse_contract`]: a row is read before the name, so bybit's spot `USDTUSD` is not a one-dollar
//! inverse contract), and a binance COIN-M or okx inverse face is that row's alone, so a symbol with no kept row
//! there has no value and its reader refuses it, never a guessed face.
//!
//! [`option_underlyings`] is the same kind of answer for an option's fee: what a deribit USDC option is
//! written on, which the backtest charges its fee on.

use vike_model::{Contract, ContractValue, Instrument, fiat_code};
use vike_venues::PERP_SUFFIX;

/// Whether the USER name `symbol` on `venue` is a coin-margined future or perpetual. The module doc carries the
/// rule, and every venue but the four is `false`: none of them lists one.
#[must_use]
pub fn is_inverse_contract(venue: &str, symbol: &str) -> bool {
    let symbol = symbol.to_ascii_uppercase();
    match venue {
        "deribit" | "bybit" | "binance" | "okx" => coin_margined(venue, &symbol),
        _ => false,
    }
}

/// **[`is_inverse_contract`] with the symbol's KEPT `row` in hand** (its row of the datahub's
/// `<project>/instruments/<venue>.json`; the owner's ruling of 2026-11-16: a fact is the row's, the name only a
/// fallback): a perpetual or dated future whose row states [`vike_model::Margining::Inverse`]; with no row, the
/// name's answer. bybit's spot `USDTUSD` and `BTCUSD` (data-plane names whose row says spot) are not inverse here,
/// whatever [`is_inverse_contract`] reads off the name.
#[must_use]
pub fn kept_is_inverse_contract(venue: &str, symbol: &str, row: Option<&Instrument>) -> bool {
    row.map_or_else(|| is_inverse_contract(venue, symbol), |row| row.contract.is_inverse())
}

/// **The coin a coin-margined contract settles in, read off the NAME** (decision 0258: an inverse engine keeps
/// its books in that coin, one per engine): `None` for every name [`is_inverse_contract`] does not read as
/// inverse. On all four venues the inverse contract settles in its BASE coin, which the user name spells first:
/// `BTCUSD.P` and `BTCUSD_261225` settle in `BTC`. The bridges hold it to their committed rows' `settleCoin`
/// (`the_name_rule_agrees_with_every_row`).
#[must_use]
pub fn inverse_settle_coin(venue: &str, symbol: &str) -> Option<String> {
    if !is_inverse_contract(venue, symbol) {
        return None;
    }
    let symbol = symbol.to_ascii_uppercase();
    let coin = dollar_pair(&symbol)?.strip_suffix("USD")?;
    (!coin.is_empty()).then(|| coin.to_string())
}

/// **What one unit of an inverse order on `venue` is worth, in US dollars**: the venue's fixed unit where it fixes
/// one for every coin; where only the contract's row says it, the face its KEPT `row` states (its row of the
/// datahub's `<project>/instruments/<venue>.json`, found by `crate::kept_row`); `None` otherwise.
///
/// | venue | one unit | where it comes from |
/// |---|---|---|
/// | deribit | 1 USD | an inverse instrument's order `amount` is dollars, and its `contract_size` (10 on `BTC-PERPETUAL`) is the lot, not the unit (`crates/bridges/deribit/src/catalog.rs`'s `contract_value_of`, decision 0170 item 4) |
/// | bybit | 1 USD | one inverse contract is one dollar, and the instrument row has no size field (decision 0231 item 4) |
/// | binance COIN-M | `contractSize` dollars: 100 on BTC, 10 on most other coins | the kept row's `contract_size` (`crates/bridges/binance/src/catalog.rs`'s `parse_dapi`) |
/// | okx | `ctVal · ctMult` dollars: 100 on BTC-USD-SWAP, 10 on ETH | the kept row's `contract_size` (`crates/bridges/okx/src/catalog.rs`'s `row_contract_value`) |
///
/// A `None` is never filled in, from the name or with the common 100: the unit differs by coin, and a guess books
/// a number wrong by that factor. No row, or a row stating no positive size, is no face.
fn inverse_face(venue: &str, row: Option<&Instrument>) -> Option<f64> {
    match venue {
        "deribit" | "bybit" => Some(1.0),
        "binance" | "okx" => {
            row.map(|r| r.properties.contract_size).filter(|f| f.is_finite() && *f > 0.0)
        }
        _ => None,
    }
}

/// The venues on which one unit of a LINEAR order's quantity is one unit of what it buys (a coin, a share, a
/// currency unit, an outcome share) for every symbol: their live mounts state no contract value, and the facts
/// lane values a deribit or bybit row at a unit of 1.0. A deribit coin-settled option is the one name of
/// theirs with no value at all ([`contract_value`]). ibkr, ig and fxcm are not here: their unit differs by
/// symbol, and [`contract_value`] reads the shapes whose name fixes it.
const ONE_UNIT_VENUES: &[&str] = &[
    "deribit",
    "bybit",
    "binance",
    "okx",
    "hyperliquid",
    "aster",
    "alpaca",
    "oanda",
    "ctrader",
    "dukascopy",
    "polymarket",
];

/// **What one unit of `symbol` on `venue` is worth, read off the NAME**: the catalog's one answer for a process
/// that holds no row of the contract, `None` where only the row says it.
///
/// | venue | `Linear` 1.0, or the inverse face | `None` |
/// |---|---|---|
/// | deribit, bybit | an inverse contract at the venue's dollar ([`inverse_face`]); every other symbol linear | a deribit coin-settled option (`BTC-5OCT26-74000-C`): its premium is coin, and the live mount refuses it (`crates/bridges/deribit/src/catalog.rs`'s `coin_option_refusal`) |
/// | binance, okx | every linear symbol | every inverse contract: only its row's `contractSize` / `ctVal` says its face ([`kept_contract_value`]) |
/// | the rest of `ONE_UNIT_VENUES` | every symbol | |
/// | ibkr | a KEPT row of a stock, an ETF, a pair or a coin (`AAPL`, `USDJPY`: [`kept_contract_value`] with the row; decision 0290, a user name states no class) | every name without its row; a future (its multiplier is the row's), an index, an option |
/// | fxcm | a currency pair, two fiat codes (`EURUSD`, `EUR/USD`) | a metal, index, commodity, treasury, share or crypto CFD (`XAUUSD`, `US30`, `USOIL`, `NVDA.EXT`): the row's `ContractMultiplier` |
/// | ig | | every epic: its `contractSize` is the market row's |
/// | outside the roster | | every symbol |
///
/// A `None` is never filled in with 1.0 here: the caller decides what it does without the row.
#[must_use]
pub fn contract_value(venue: &str, symbol: &str) -> Option<ContractValue> {
    kept_contract_value(venue, symbol, None)
}

/// [`contract_value`] with the symbol's KEPT row in hand (`row`, as [`inverse_face`] reads it): whether the
/// contract is inverse is the row's margining ([`kept_is_inverse_contract`]), and an inverse contract's face is the
/// row's where only the row states it (binance COIN-M, okx), `None` without one; every other answer is the name's,
/// as without a row. What a paper book values a symbol by and a backtest books a coin ledger by.
#[must_use]
pub fn kept_contract_value(
    venue: &str,
    symbol: &str,
    row: Option<&Instrument>,
) -> Option<ContractValue> {
    if kept_is_inverse_contract(venue, symbol, row) {
        return inverse_face(venue, row).map(ContractValue::inverse);
    }
    let one = match venue {
        "deribit" => !deribit_coin_option(symbol),
        "ibkr" => row.is_some_and(ibkr_unit_is_one),
        "fxcm" => is_fiat_pair(symbol),
        _ => ONE_UNIT_VENUES.contains(&venue),
    };
    one.then(ContractValue::default)
}

/// **The instruments whose price is an option's UNDERLYING, read off the NAME**, for a process that holds no
/// row and charges the option's fee on it (`vike_model::FeeSchedule::PercentOfUnderlying`: deribit charges
/// 0.03 % of the underlying, capped at 12.5 % of the premium). A deribit USDC-settled option
/// (`BTC_USDC-27NOV26-80000-C`) is written on its book's index, which the venue also lists as the linear
/// perpetual and the spot pair of that book, by their USER names (decision 0290): `["BTCUSDC.P", "BTCUSDC"]`
/// (`BTC_USDC-PERPETUAL`, `BTC_USDC`), either of which prices it. `None` for every other name: a coin-settled option
/// (its premium is coin, which [`contract_value`] does not value), a future, a perpetual, another venue.
#[must_use]
pub fn option_underlyings(venue: &str, symbol: &str) -> Option<[String; 2]> {
    if venue != "deribit" {
        return None;
    }
    let base = deribit_option_base(symbol).filter(|b| b.contains('_'))?.to_ascii_uppercase();
    let pair = base.replace('_', "");
    Some([format!("{pair}{PERP_SUFFIX}"), pair])
}

/// The base of a deribit option name, four dash-separated parts ending in the right: `BTC` of
/// `BTC-5OCT26-74000-C`, `SOL_USDC` of `SOL_USDC-12OCT26-96-P`; `None` for any other shape.
fn deribit_option_base(symbol: &str) -> Option<&str> {
    let mut parts = symbol.split('-');
    let (Some(base), Some(_expiry), Some(_strike), Some(right), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    (right.eq_ignore_ascii_case("C") || right.eq_ignore_ascii_case("P")).then_some(base)
}

/// A deribit option settled in its coin: an option name on a base that names no USDC book
/// (`BTC-5OCT26-74000-C`, not `SOL_USDC-12OCT26-96-P`).
fn deribit_coin_option(symbol: &str) -> bool {
    deribit_option_base(symbol).is_some_and(|base| !base.contains('_'))
}

/// **Whether `symbol` on `venue` is a coin-settled option**: a deribit option on a base that names no USDC book
/// (`BTC-5OCT26-74000-C`; `SOL_USDC-12OCT26-96-P` is linear), whose premium is coin, which [`contract_value`] does not
/// value. `false` on every other venue.
#[must_use]
pub fn is_coin_settled_option(venue: &str, symbol: &str) -> bool {
    venue == "deribit" && deribit_coin_option(symbol)
}

/// An ibkr KEPT row whose class fixes one unit as one share, one unit of the base currency or one coin: a stock, an
/// ETF, a forex pair or a coin (IBKR-3, decision 0290: the class is the row's, never the name's).
fn ibkr_unit_is_one(row: &Instrument) -> bool {
    matches!(
        row.contract,
        Contract::Equity(_) | Contract::Etf(_) | Contract::Fx(_) | Contract::CryptoSpot
    )
}

/// Two fiat codes, written `EURUSD` (fxcm's engine spelling) or `EUR/USD` (its venue spelling).
fn is_fiat_pair(symbol: &str) -> bool {
    let symbol = symbol.trim();
    let (base, quote) = match symbol.split_once('/') {
        Some(halves) => halves,
        None if symbol.len() == 6 && symbol.is_ascii() => symbol.split_at(3),
        None => return false,
    };
    fiat_code(base).is_some() && fiat_code(quote).is_some()
}

/// **deribit, bybit, binance and okx, by the USER name** (decision 0290; BYB-4, BIN-5): the name's KEPT row's
/// margining where this process holds the venue's list and a row of it (`crate::kept_row_of`); with no
/// row, the grammar's [`dollar_pair`]. A bare `BTCUSD` or `USDTUSD` is a SPOT pair, never inverse; every stablecoin
/// book quotes its own coin (`BTCUSDT.P`, `BTCUSDC.P`, `BTCUSDT_261225`); an option is the venue's own name and has
/// no `.P` and no `_YYMMDD` tail.
fn coin_margined(venue: &str, user: &str) -> bool {
    if let Some(row) = crate::kept_row_of(venue, user) {
        return row.contract.is_inverse();
    }
    dollar_pair(user).is_some()
}

/// The `<COIN>USD` of a user name that spells a contract quoted in US dollars: a perpetual `<COIN>USD.P` or a dated
/// contract `<COIN>USD_YYMMDD`, with a coin before the dollar. `None` for every other name.
fn dollar_pair(user: &str) -> Option<&str> {
    let pair = match user.strip_suffix(PERP_SUFFIX) {
        Some(pair) => pair,
        None => match user.split_once('_') {
            Some((pair, day)) if vike_venues::is_delivery_day(day) => pair,
            _ => return None,
        },
    };
    (pair.len() > "USD".len() && pair.ends_with("USD")).then_some(pair)
}

#[path = "inverse_tests.rs"]
#[cfg(test)]
mod inverse_tests;
