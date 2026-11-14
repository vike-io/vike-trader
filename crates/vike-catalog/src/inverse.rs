//! **Which `(venue, symbol)` names a coin-margined (INVERSE) contract, read off the NAME**: for a process
//! that holds no row of the contract and must not guess `Linear` (decision 0170, item 6).
//!
//! The authority is the venue's row: its `Contract`'s [`vike_model::Margining`], through
//! [`vike_model::ContractValue::of`]. Whoever holds a row reads that. A backtest holds none: the harness
//! opens no venue connection, the recorded `kind=properties` tape states no margining and leaves inverse
//! rows out (`crates/vike-datahub/src/properties_tape.rs`'s `grid_of`), and the shipped baseline lists alpaca
//! and oanda only. The name carries the answer on the four venues that list such a contract, each by its own
//! grammar, which is why the VENUE is an argument: `BTC-27DEC26` is an inverse future on deribit and a linear
//! USDC future on bybit, and `BTCUSD` is an inverse perpetual on bybit and a linear CFD on ctrader.
//!
//! | venue | inverse | what stays linear |
//! |---|---|---|
//! | deribit | `BTC-PERPETUAL`, `BTC-27DEC26` | `BTC_USDC-PERPETUAL`, `BTC_USDC-27DEC26`, spot, every option |
//! | bybit | `BTCUSD`, `BTCUSDZ26` (`.P` or not) | `BTCUSDT`, `BTCUSDC`, `BTCPERP`, `ETHUSDT-27NOV26`, `BTC-27DEC26` |
//! | binance | `BTCUSD_PERP.P`, `BTCUSD_261225.P` | `BTCUSDT.P`, `BTCUSDT_261225.P`, spot |
//! | okx | `BTC-USD-SWAP`, `BTC-USD-261225` | `BTC-USDT-SWAP`, `BTC-USD_UM-261127`, `BTC-USD` spot, every option |
//!
//! An option is not inverse here, whichever coin it settles in: it is priced `qty · premium`
//! ([`vike_model::ContractValue::of`]). Each venue's row states what this reads: the four bridges hold this
//! function to their committed instrument fixtures (`the_name_rule_agrees_with_every_row`, in each).
//!
//! [`contract_value`] is the WHOLE value a name gives, the catalog's one answer for a process that holds no
//! row (decision 0170's catalog reopener; the paper book and the backtest's profile check read it). The four
//! bridges' tests above, fxcm's and the gatewayhub's ibkr facts tests hold it equal to the value the live path
//! books for every committed row.
//!
//! [`option_underlyings`] is the same kind of answer for an option's fee: what a deribit USDC option is
//! written on, which the backtest charges its fee on.

use vike_model::{ContractValue, Instrument};
use vike_venues::{fiat_code, is_day_month_year, split_perp};

/// Whether `symbol` on `venue` is a coin-margined future or perpetual. The module doc carries the rule, and
/// every venue but the four is `false`: none of them lists one.
#[must_use]
pub fn is_inverse_contract(venue: &str, symbol: &str) -> bool {
    let symbol = symbol.to_ascii_uppercase();
    let (wire, _perp) = split_perp(&symbol);
    match venue {
        "deribit" => deribit_inverse(wire),
        "bybit" => bybit_inverse(wire),
        "binance" => binance_inverse(wire),
        "okx" => okx_inverse(wire),
        _ => false,
    }
}

/// **The coin a coin-margined contract settles in, read off the NAME** (decision 0258: an inverse engine keeps
/// its books in that coin, one per engine): `None` for every name [`is_inverse_contract`] does not read as
/// inverse. On all four venues the inverse contract settles in its BASE coin, which each grammar names first:
/// deribit `BTC-PERPETUAL`, bybit `BTCUSD` / `BTCUSDZ26`, binance `BTCUSD_PERP`, okx `BTC-USD-SWAP` all settle in
/// `BTC`. The bridges hold it to their committed rows' `settleCoin` (`the_name_rule_agrees_with_every_row`).
#[must_use]
pub fn inverse_settle_coin(venue: &str, symbol: &str) -> Option<String> {
    if !is_inverse_contract(venue, symbol) {
        return None;
    }
    let symbol = symbol.to_ascii_uppercase();
    let (wire, _perp) = split_perp(&symbol);
    let coin = match venue {
        "deribit" | "okx" => wire.split('-').next()?,
        "bybit" => &wire[..wire.rfind("USD")?],
        "binance" => wire.split('_').next()?.strip_suffix("USD")?,
        _ => return None,
    };
    (!coin.is_empty()).then(|| coin.to_string())
}

/// **What one unit of an inverse order on `venue` is worth, in US dollars, for a process that holds only
/// the NAME**: the venue's fixed unit where it fixes one for every coin; where only the contract's row says
/// it, the face a COMMITTED row of `symbol` states; `None` otherwise.
///
/// | venue | one unit | where it comes from |
/// |---|---|---|
/// | deribit | 1 USD | an inverse instrument's order `amount` is dollars, and its `contract_size` (10 on `BTC-PERPETUAL`) is the lot, not the unit (`crates/bridges/deribit/src/catalog.rs`'s `contract_value_of`, decision 0170 item 4) |
/// | bybit | 1 USD | one inverse contract is one dollar, and the instrument row has no size field (decision 0231 item 4) |
/// | binance COIN-M | `contractSize` dollars: 100 on BTC, 10 on most other coins | the dapi row (`crates/bridges/binance/src/catalog.rs`'s `parse_dapi`): [`BINANCE_COIN_M_FACES`] |
/// | okx | `ctVal · ctMult` dollars: 100 on BTC-USD-SWAP, 10 on ETH | the row (`crates/bridges/okx/src/catalog.rs`'s `row_contract_value`): [`OKX_INVERSE_FACES`] |
///
/// A `None` is never filled in with the common 100: the unit differs by coin, and a guess books a number
/// wrong by that factor. `symbol` is read on the two venues whose unit is the row's, in any case and with
/// or without the `.P` lane suffix.
#[must_use]
pub fn inverse_face_value(venue: &str, symbol: &str) -> Option<f64> {
    inverse_face(venue, symbol, None)
}

/// [`inverse_face_value`] with the contract's KEPT row in hand (`row`: its row of the datahub's
/// `<project>/instruments/<venue>.json`, found by `crate::kept_row`): on the two venues whose face only the row
/// states, the row's `contract_size` (binance's `contractSize`, okx's `ctVal · ctMult`) when it states one;
/// the venue's fixed unit and the committed faces otherwise, exactly as without a row.
fn inverse_face(venue: &str, symbol: &str, row: Option<&Instrument>) -> Option<f64> {
    let faces = match venue {
        "deribit" | "bybit" => return Some(1.0),
        "binance" => BINANCE_COIN_M_FACES,
        "okx" => OKX_INVERSE_FACES,
        _ => return None,
    };
    let stated = row.map(|r| r.properties.contract_size).filter(|f| f.is_finite() && *f > 0.0);
    if stated.is_some() {
        return stated;
    }
    let symbol = symbol.to_ascii_uppercase();
    let (wire, _perp) = split_perp(&symbol);
    faces.iter().find(|(name, _)| *name == wire).map(|&(_, face)| face)
}

/// **The face of each binance COIN-M contract a committed row states**: `(symbol, dollars per contract)`,
/// the venue's spelling without the `.P` lane suffix. A copy of the TRADING rows of the bridge's committed
/// dapi list (`contractSize`), held equal to them FROM THE BRIDGE side, both ways, because this crate cannot
/// see a bridge: `crates/bridges/binance/src/catalog_class_tests.rs`'s `the_name_rule_agrees_with_every_row`.
/// A contract that list does not carry has no face here.
pub const BINANCE_COIN_M_FACES: &[(&str, f64)] = &[
    ("BTCUSD_PERP", 100.0),
    ("ETHUSD_PERP", 10.0),
    ("BTCUSD_261225", 100.0),
    ("BTCUSD_270326", 100.0),
];

/// **The face of each okx inverse contract a committed row states**: `(instId, dollars per contract)`,
/// `ctVal · ctMult` of the bridge's committed `public/instruments` rows, held equal to them from the bridge
/// side, both ways: `crates/bridges/okx/src/catalog.rs`'s `the_name_rule_agrees_with_every_row`.
pub const OKX_INVERSE_FACES: &[(&str, f64)] = &[("BTC-USD-SWAP", 100.0), ("BTC-USD-261030", 100.0)];

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
/// that holds no row of the contract (a backtest, a paper book), `None` where only the row says it.
///
/// | venue | `Linear` 1.0, or the inverse face | `None` |
/// |---|---|---|
/// | deribit, bybit | an inverse contract at its face ([`inverse_face_value`]); every other symbol linear | a deribit coin-settled option (`BTC-5OCT26-74000-C`): its premium is coin, and the live mount refuses it (`crates/bridges/deribit/src/catalog.rs`'s `coin_option_refusal`) |
/// | binance, okx | every linear symbol; an inverse contract a committed row states, at that row's face ([`inverse_face_value`]) | an inverse contract no committed row states: only its row's `contractSize` / `ctVal` says its face |
/// | the rest of `ONE_UNIT_VENUES` | every symbol | |
/// | ibkr | a stock routed `SMART` (`AAPL.SMART.USD`), a pair on `IDEALPRO` (`EUR.USD.IDEALPRO`, `EUR.IDEALPRO.USD`), and an explicit `STK`, `CASH` or `CRYPTO` claim (`VOD.LSE.GBP.STK`, `BTC.PAXOS.USD.CRYPTO`) | a future or an option (the exchange multiplier is the row's), an index, a bare ticker, any other three-field form |
/// | fxcm | a currency pair, two fiat codes (`EURUSD`, `EUR/USD`) | a metal, index, commodity, treasury, share or crypto CFD (`XAUUSD`, `US30`, `USOIL`, `NVDA.EXT`): the row's `ContractMultiplier` |
/// | ig | | every epic: its `contractSize` is the market row's |
/// | outside the roster | | every symbol |
///
/// A `None` is never filled in with 1.0 here: the caller decides what it does without the row.
#[must_use]
pub fn contract_value(venue: &str, symbol: &str) -> Option<ContractValue> {
    kept_contract_value(venue, symbol, None)
}

/// [`contract_value`] with the symbol's KEPT row in hand (`row`, as [`inverse_face`] reads it): an inverse
/// contract's face is the row's where only the row states it (binance COIN-M, okx); every other answer is the
/// name's, as without a row. What a backtest handed the kept rows books a coin ledger by.
#[must_use]
pub fn kept_contract_value(
    venue: &str,
    symbol: &str,
    row: Option<&Instrument>,
) -> Option<ContractValue> {
    if is_inverse_contract(venue, symbol) {
        return inverse_face(venue, symbol, row).map(ContractValue::inverse);
    }
    let one = match venue {
        "deribit" => !deribit_coin_option(symbol),
        "ibkr" => ibkr_unit_is_one(symbol),
        "fxcm" => is_fiat_pair(symbol),
        _ => ONE_UNIT_VENUES.contains(&venue),
    };
    one.then(ContractValue::default)
}

/// **The instruments whose price is an option's UNDERLYING, read off the NAME**, for a process that holds no
/// row and charges the option's fee on it (`vike_model::FeeSchedule::PercentOfUnderlying`: deribit charges
/// 0.03 % of the underlying, capped at 12.5 % of the premium). A deribit USDC-settled option
/// (`BTC_USDC-27NOV26-80000-C`) is written on its book's index, which the venue also lists as the linear
/// perpetual and the spot pair of that book: `["BTC_USDC-PERPETUAL", "BTC_USDC"]`, either of which prices it.
/// `None` for every other name: a coin-settled option (its premium is coin, which [`contract_value`] does not
/// value), a future, a perpetual, another venue.
#[must_use]
pub fn option_underlyings(venue: &str, symbol: &str) -> Option<[String; 2]> {
    if venue != "deribit" {
        return None;
    }
    let base = deribit_option_base(symbol).filter(|b| b.contains('_'))?.to_ascii_uppercase();
    Some([format!("{base}-PERPETUAL"), base])
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

/// An ibkr canonical (read as `crates/bridges/vike-ibkr/src/instrument/contract.rs`'s `parse_simplified` reads it) whose claim fixes one unit as one
/// share, one unit of the base currency or one coin: the three-field form routed `SMART` (the bridge reads it as
/// a cash equity) or naming `IDEALPRO` (a pair, in either field order), or a fourth field claiming `STK`, `CASH`
/// or `CRYPTO`.
fn ibkr_unit_is_one(symbol: &str) -> bool {
    let fields: Vec<&str> = symbol.split('.').collect();
    let is = |i: usize, word: &str| fields.get(i).is_some_and(|f| f.eq_ignore_ascii_case(word));
    match fields.len() {
        3 => is(1, "SMART") || is(1, "IDEALPRO") || is(2, "IDEALPRO"),
        n if n >= 4 => is(3, "STK") || is(3, "CASH") || is(3, "CRYPTO"),
        _ => false,
    }
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

/// Coin, a dash, then `PERPETUAL` or a day-month-year expiry and nothing more. A USDC book writes its base
/// `BTC_USDC`, and an option carries a strike and a right after the expiry.
fn deribit_inverse(wire: &str) -> bool {
    let Some((base, tail)) = wire.split_once('-') else { return false };
    !base.contains('_') && (tail == "PERPETUAL" || is_day_month_year(tail))
}

/// `<COIN>USD`, or that and a futures month code and a two-digit year. Every stablecoin book of the venue
/// ends in its own word (`USDT`, `USDC`, `PERP`).
fn bybit_inverse(wire: &str) -> bool {
    const MONTH_CODES: &str = "FGHJKMNQUVXZ";
    let is_pair = |s: &str| s.len() > "USD".len() && s.ends_with("USD");
    if is_pair(wire) {
        return true;
    }
    let Some(split) = wire.len().checked_sub(3) else { return false };
    let Some((pair, tail)) = wire.split_at_checked(split) else { return false };
    let t = tail.as_bytes();
    is_pair(pair) && MONTH_CODES.contains(t[0] as char) && t[1..].iter().all(u8::is_ascii_digit)
}

/// `<COIN>USD_PERP` or `<COIN>USD_YYMMDD`: the dapi book's two shapes. The USDⓈ-M dated book is
/// `<COIN>USDT_YYMMDD`, and a bare symbol is spot or USDⓈ-M.
fn binance_inverse(wire: &str) -> bool {
    let Some((pair, tail)) = wire.split_once('_') else { return false };
    pair.len() > "USD".len() && pair.ends_with("USD") && (tail == "PERP" || is_all_digits(tail, 6))
}

/// `<COIN>-USD-SWAP` or `<COIN>-USD-YYMMDD`. A linear book names its own settlement (`USDT`, `USDC`,
/// `USD_UM`), a spot pair has two tokens and an option five.
fn okx_inverse(wire: &str) -> bool {
    let mut tokens = wire.split('-');
    let (Some(_coin), Some(quote), Some(tail), None) =
        (tokens.next(), tokens.next(), tokens.next(), tokens.next())
    else {
        return false;
    };
    quote == "USD" && (tail == "SWAP" || is_all_digits(tail, 6))
}

fn is_all_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

#[path = "inverse_tests.rs"]
#[cfg(test)]
mod inverse_tests;
