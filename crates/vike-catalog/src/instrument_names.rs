//! `instrument_names` — **decision 0290's ONE translator: an instrument's USER name, its STORE name and its WIRE
//! name, per venue and per class, read off its KEPT row.**
//!
//! - The **user** name is what a person reads and types everywhere (the desktop, a backtest profile, a strategy's
//!   `[inputs]`, a mount, an account's symbol, the journal): one grammar per class on every venue (FX `USDJPY`,
//!   crypto spot `BTCUSDT`, a perpetual `BTCUSDT.P`, a dated future `BTCUSDT_261225`, a stock `AAPL`, a CFD on a
//!   stock `AAPL.CFD`). The venue stays a separate key, so `BTCUSDT.P` on binance and on bybit are two instruments.
//! - The **store** name is the history store's `symbol=`: the user name, except a builder-dex perpetual's `:`
//!   (`XYZ:TSLAUSDC.P`), which no store path may hold and is written `-` there (`XYZ-TSLAUSDC.P`). That rule is the
//!   store's, not this file's: `vike_paths::store_path::store_name_of_user_name`, beside the store's refusal of a
//!   path-hostile symbol, through which every store path is written and read.
//! - The **wire** name is the venue's own spelling, for its bridge alone: the kept row's own fields
//!   ([`wire_name_of_row`]: `USD_JPY` on oanda, `BTC-USDT-SWAP` on okx, `@107` for hyperliquid's `HYPEUSDC`,
//!   `AAPL.US/USD` for dukascopy's `AAPL.CFD`).
//!
//! **The class is the KEPT row's** (`<project>/instruments/<venue>.json`: the venue's listing plus the operator's own
//! rows, `crate::kept_list`), never read off a name: the row's contract decides which grammar spells it. No list,
//! no row, or two rows answering one name is a REFUSAL that says which, never a guess (the owner's rule of
//! 2026-11-13, decision 0290). Lookups compare a typed user name ignoring ASCII case and answer the canonical one.
//!
//! **Spelled ONCE, then read** (the owner's ruling «Б», 2026-11-16, simplified the same day: ONE stored name): a user
//! name is not spelled again at every start or lookup. The kept list's WRITER spells each row once
//! ([`spell_naming_of_row`], over every row it writes that states none: the datahub's `vike-cli data catalog
//! refresh`, and an operator's `vike-cli data instruments add` folded in by the same file's write,
//! `crates/vike-datahub/src/instrument_lists.rs`) and stores the answer ON the row (`vike_model::Instrument::naming`:
//! the USER name, or why the grammar names none), beside the venue's own name; a naming a venue's catalog stated on
//! its row is kept as stated. The wire name is read off the row's own fields and the store name derived from the user
//! name, so neither is stored. Every READER reads what is stored ([`user_name_of_row`], [`names_of_row`], and the
//! lookups [`kept_row_of_user_name`] and [`kept_row_of_store_name`], the ONE inverse), so the two directions cannot
//! disagree, and nothing a hub, a door, the backtest, the store or a bridge does at its start or per lookup spells a
//! user name: hyperliquid's `Symbology` joins its own network's listing to the kept rows by the venue's listing name
//! and fxcm's Offers reads join by the Offers `Instrument`, each reading the stored name (2026-11-16). A
//! row with nothing stored (a kept file written before this rule) is refused in words that name the refresh that
//! cures it ([`predates_symbol_names`]); the deploy refreshes every list. A test writer holds spelled rows the same
//! way ([`spelled`], `crate::hold_kept_list`).

use vike_model::{
    Contract, ContractCfdUnderlying, ContractFutureFamily, Instrument, InstrumentNames,
    InstrumentNaming,
};
use vike_venues::PERP_SUFFIX;

use crate::refusals::SymbolRefusal;

/// The suffix of a CFD's user name whose underlying is not a currency or metal pair (`AAPL.CFD`): a CFD is a
/// different product from the stock, coin or index it tracks.
pub const CFD_SUFFIX: &str = ".CFD";

/// Where a builder-dex (HIP-3) perpetual's user name puts its dex: `XYZ:TSLAUSDC.P` (the owner's "1.A", 2026-11-16).
pub const DEX_SEPARATOR: char = ':';

/// **The WRITER's spelling of a row's user name**, by its contract's class (decision 0290's grammar table):
///
/// | the row's contract | user name |
/// |---|---|
/// | FX pair, crypto spot pair, a CFD on a currency or metal pair | base + quote: `USDJPY`, `BTCUSDT`, `XAUUSD` |
/// | perpetual | base + quote + `.P`: `BTCUSDT.P`, an inverse `BTCUSD.P`, hyperliquid's `KPEPEUSDC.P`; a builder-dex coin `XYZ:TSLAUSDC.P` (its quote is the dex's collateral token as the row states it) |
/// | dated future | base + quote + `_YYMMDD` of its expiry (UTC): `BTCUSDT_261225`; an exchange future root + `_YYMMDD`: `ES_261218` |
/// | option | deribit's own instrument name (the door's spelling, `ETH_USDC-27MAR26-2500-C`); refused on any other venue |
/// | share, ETF | the ticker the row states: `AAPL`, `BRK.B` |
/// | CFD on a share | the ticker + `.CFD`: `AAPL.CFD` (dukascopy's `AAPL.US/USD`, whose row states no base: its name's first word) |
/// | CFD on a coin | base + quote + `.CFD`: `BTCUSD.CFD` |
/// | CFD on an index, a commodity, a bond or else | the base the row states + `.CFD`: `US500.CFD`; a row that states none is refused |
/// | prediction outcome | the token id, unchanged |
///
/// Upper case throughout but an option's venue name (deribit's lower-case decimal `d`) and a token's digits.
///
/// # Errors
/// The row's class has no grammar here (an index, a bond, a fund, a warrant, a spot commodity, a combo: none is
/// traded), it is an option on a venue other than deribit, or the row does not state what its grammar spells (a
/// base and quote of letters and digits, an expiry, a ticker): each in words naming the row.
fn spell_user_name_of_row(row: &Instrument) -> Result<String, String> {
    let base = row.base.trim().to_ascii_uppercase();
    let quote = vike_model::currency_code(&row.quote).unwrap_or_default();
    let unnamed = |what: &'static str| {
        SymbolRefusal::Unnamed {
            venue: row.venue.clone(),
            raw_symbol: row.raw_symbol.clone(),
            what,
        }
        .to_string()
    };
    let pair = || {
        if is_word(&base) && is_word(&quote) {
            Ok(format!("{base}{quote}"))
        } else {
            Err(unnamed("base and quote of letters and digits"))
        }
    };
    match &row.contract {
        Contract::Fx(_) | Contract::CryptoSpot => pair(),
        Contract::Perp(_) => {
            let name = format!("{}{PERP_SUFFIX}", pair()?);
            Ok(match builder_dex(row) {
                Some(dex) if is_word(dex) => {
                    format!("{}{DEX_SEPARATOR}{name}", dex.to_ascii_uppercase())
                }
                Some(_) => return Err(unnamed("dex of letters and digits")),
                None => name,
            })
        }
        Contract::Future(future) => {
            let root = match future.family {
                ContractFutureFamily::Crypto => pair()?,
                ContractFutureFamily::Traditional if is_word(&base) => base.clone(),
                ContractFutureFamily::Traditional => return Err(unnamed("root")),
            };
            let day = delivery_day(future.expiry_ms).ok_or_else(|| unnamed("expiry"))?;
            Ok(format!("{root}_{day}"))
        }
        Contract::Option(_) if row.venue == "deribit" => {
            crate::door_symbol(&row.venue, &row.raw_symbol)
                .unwrap_or_else(|| Ok(row.raw_symbol.clone()))
        }
        Contract::Option(_) => Err(SymbolRefusal::OptionOffDeribit {
            venue: row.venue.clone(),
            raw_symbol: row.raw_symbol.clone(),
        }
        .to_string()),
        Contract::Equity(_) | Contract::Etf(_) => {
            if is_ticker(&base) {
                Ok(base)
            } else {
                Err(unnamed("ticker"))
            }
        }
        Contract::Cfd(cfd) => match cfd.underlying_kind {
            ContractCfdUnderlying::Fx | ContractCfdUnderlying::Metal => pair(),
            ContractCfdUnderlying::Crypto => Ok(format!("{}{CFD_SUFFIX}", pair()?)),
            ContractCfdUnderlying::Share => {
                // dukascopy's share rows state no base: the ticker is the first word of its own name (`AAPL.US/USD`).
                let ticker = if base.is_empty() {
                    row.raw_symbol
                        .split(['.', '/'])
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_uppercase()
                } else {
                    base.clone()
                };
                if is_ticker(&ticker) {
                    Ok(format!("{ticker}{CFD_SUFFIX}"))
                } else {
                    Err(unnamed("ticker"))
                }
            }
            ContractCfdUnderlying::Index
            | ContractCfdUnderlying::Commodity
            | ContractCfdUnderlying::Bond
            | ContractCfdUnderlying::Other => {
                if is_word(&base) {
                    Ok(format!("{base}{CFD_SUFFIX}"))
                } else {
                    Err(unnamed(
                        "name for this CFD (its base; the operator writes one on his own row)",
                    ))
                }
            }
        },
        Contract::PredictionOutcome(_) => {
            let token = row.raw_symbol.trim();
            if crate::is_outcome_token(token) {
                Ok(token.to_string())
            } else {
                Err(unnamed("outcome token id"))
            }
        }
        Contract::Index
        | Contract::Bond
        | Contract::Fund
        | Contract::Warrant
        | Contract::Commodity
        | Contract::Combo(_) => Err(SymbolRefusal::ClassUnnamed {
            venue: row.venue.clone(),
            raw_symbol: row.raw_symbol.clone(),
            class: row.asset_class().sql_word(),
        }
        .to_string()),
    }
}

/// **The WIRE name of a row**: the venue's own spelling its bridge sends, read off the row's own fields (no user name
/// is spelled). The row's own name, but where the row keeps a lane suffix the venue never sees (binance, aster and
/// bybit's `.P`, `crate::split_perp_at`), where the venue signs on a coin the row carries beside its name
/// (hyperliquid's `wire.coin`: `@107` for `HYPE/USDC`), and deribit, whose list upper-cases a decimal strike the venue
/// spells with a lower-case `d` (the door's spelling).
#[must_use]
pub fn wire_name_of_row(row: &Instrument) -> String {
    let raw = row.raw_symbol.as_str();
    match row.venue.as_str() {
        "hyperliquid" => row.wire.coin.clone().unwrap_or_else(|| raw.to_string()),
        "deribit" => crate::door_symbol("deribit", raw)
            .and_then(Result::ok)
            .unwrap_or_else(|| raw.to_string()),
        venue => crate::split_perp_at(venue, raw).0.to_string(),
    }
}

/// **The WRITER's spelling of a row's naming**: its USER name by its class, or why its grammar names it not. What
/// the writer of a kept list stores on each row it writes (`vike_model::Instrument::naming`). A READER never calls it:
/// it reads [`user_name_of_row`].
#[must_use]
pub fn spell_naming_of_row(row: &Instrument) -> InstrumentNaming {
    match spell_user_name_of_row(row) {
        Ok(user) => InstrumentNaming::User(user),
        Err(why) => InstrumentNaming::Unnamed(why),
    }
}

/// **`rows`, each that states no naming yet spelled** ([`spell_naming_of_row`]); a row that states its naming keeps
/// it as stated (a venue's catalog may name its own rows: the writer never spells over that). The writers' one
/// helper: the datahub's kept-list write, a test's held list (`crate::hold_kept_list`, `crate::recorded_kept_lists`)
/// and a test's hand-built slice before it asks a lookup.
#[must_use]
pub fn spelled(mut rows: Vec<Instrument>) -> Vec<Instrument> {
    for row in rows.iter_mut().filter(|row| row.naming.is_none()) {
        row.naming = Some(spell_naming_of_row(row));
    }
    rows
}

/// **Why a row that stores no naming is read as none**: its venue's kept file was written before names were stored
/// (the owner's ruling «Б» of 2026-11-16), and the refresh that names it.
#[must_use]
pub fn predates_symbol_names(venue: &str) -> String {
    SymbolRefusal::PredatesNames { venue: venue.to_string() }.to_string()
}

/// **The USER name a KEPT row stores** (`vike_model::Instrument::naming`): read, never spelled.
///
/// # Errors
/// The grammar names the row not (an okx option, an index CFD that states no base: the stored words), or the row
/// stores nothing ([`predates_symbol_names`]).
pub fn user_name_of_row(row: &Instrument) -> Result<String, String> {
    match &row.naming {
        Some(InstrumentNaming::User(user)) => Ok(user.clone()),
        Some(InstrumentNaming::Unnamed(why)) => Err(why.clone()),
        None => Err(predates_symbol_names(&row.venue)),
    }
}

/// **A KEPT row's user name ([`user_name_of_row`]) beside its wire name ([`wire_name_of_row`])**.
///
/// # Errors
/// [`user_name_of_row`]'s.
pub fn names_of_row(row: &Instrument) -> Result<InstrumentNames, String> {
    Ok(InstrumentNames { user: user_name_of_row(row)?, wire: wire_name_of_row(row) })
}

/// **The kept row a USER name names on `venue`**, among `rows` (the venue's kept list): the one row whose stored user
/// name is `user`, ignoring ASCII case and blanks around it.
///
/// # Errors
/// `rows` holds no row of `venue` (no list is kept on this box), a row of it stores no names
/// ([`predates_symbol_names`]), none answers `user`, or two do (`kPEPE` and `KPEPE` on hyperliquid, okx's inverse and
/// USD-margined dated futures of one day): each in words naming `user`.
pub fn kept_row_of_user_name<'a>(
    rows: &'a [Instrument],
    venue: &str,
    user: &str,
) -> Result<&'a Instrument, String> {
    let wanted = user.trim();
    one_row(rows, venue, user, "user", |stored| stored.eq_ignore_ascii_case(wanted))
}

/// **The kept row a STORE name names on `venue`** (a series' `symbol=`, exactly as the store spells it): the one
/// row whose stored user name's store name (`vike_paths::store_path::store_name_of_user_name`, the store's one rule)
/// is `store`. The ONE inverse of the store's naming: a stored series to its instrument (a backtest's chart or input,
/// a listing of the store), compared through the same rule and never rewritten back.
///
/// # Errors
/// [`kept_row_of_user_name`]'s, for the store name.
pub fn kept_row_of_store_name<'a>(
    rows: &'a [Instrument],
    venue: &str,
    store: &str,
) -> Result<&'a Instrument, String> {
    one_row(rows, venue, store, "store", |stored| {
        vike_paths::store_path::store_name_of_user_name(stored) == store
    })
}

/// [`names_of_row`] of [`kept_row_of_user_name`]: a typed user name's user and wire names, the user one canonical.
///
/// # Errors
/// [`kept_row_of_user_name`]'s.
pub fn names_of_user_name(
    rows: &[Instrument],
    venue: &str,
    user: &str,
) -> Result<InstrumentNames, String> {
    names_of_row(kept_row_of_user_name(rows, venue, user)?)
}

/// [`names_of_row`] of [`kept_row_of_store_name`]: a stored series' user and wire names.
///
/// # Errors
/// [`kept_row_of_store_name`]'s.
pub fn names_of_store_name(
    rows: &[Instrument],
    venue: &str,
    store: &str,
) -> Result<InstrumentNames, String> {
    names_of_row(kept_row_of_store_name(rows, venue, store)?)
}

/// The one row of `venue` among `rows` whose stored user name `answers`, or why there is none: no list, a list that
/// predates stored names, no row, or two.
fn one_row<'a>(
    rows: &'a [Instrument],
    venue: &str,
    name: &str,
    which: &'static str,
    answers: impl Fn(&str) -> bool,
) -> Result<&'a Instrument, String> {
    let mut listed = rows.iter().filter(|r| r.venue == venue).peekable();
    if listed.peek().is_none() {
        return Err(SymbolRefusal::NoList {
            name: name.to_string(),
            venue: venue.to_string(),
            no_list: crate::no_kept_list_refusal(venue),
        }
        .to_string());
    }
    if rows.iter().any(|r| r.venue == venue && r.naming.is_none()) {
        return Err(SymbolRefusal::NoNamesYet {
            name: name.to_string(),
            venue: venue.to_string(),
            predates: predates_symbol_names(venue),
        }
        .to_string());
    }
    let mut hits =
        listed.filter(|r| matches!(&r.naming, Some(InstrumentNaming::User(user)) if answers(user)));
    match (hits.next(), hits.next()) {
        (Some(row), None) => Ok(row),
        (None, _) => {
            Err(SymbolRefusal::NoRow { name: name.to_string(), which, venue: venue.to_string() }
                .to_string())
        }
        (Some(a), Some(b)) => Err(SymbolRefusal::TwoRows {
            name: name.to_string(),
            venue: venue.to_string(),
            first: a.raw_symbol.clone(),
            second: b.raw_symbol.clone(),
            which,
        }
        .to_string()),
    }
}

/// The `YYMMDD` of the UTC day `expiry_ms` falls on, `None` before 2000 (no listed contract, and a row that left its
/// expiry unset).
fn delivery_day(expiry_ms: i64) -> Option<String> {
    let (year, month, day, ..) = vike_model::time::utc_parts(expiry_ms);
    (2000..2100).contains(&year).then(|| format!("{:02}{month:02}{day:02}", year % 100))
}

/// The builder dex of a hyperliquid HIP-3 row (`xyz` of `xyz:TSLA`), `None` for every other row.
fn builder_dex(row: &Instrument) -> Option<&str> {
    if row.venue != "hyperliquid" {
        return None;
    }
    row.raw_symbol.split_once(DEX_SEPARATOR).map(|(dex, _)| dex)
}

/// Letters and digits, at least one, at most 32.
fn is_word(text: &str) -> bool {
    !text.is_empty() && text.len() <= 32 && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A ticker: [`is_word`] parts joined by at most one `.` (a class share, `BRK.B`).
fn is_ticker(text: &str) -> bool {
    match text.split_once('.') {
        None => is_word(text),
        Some((head, tail)) => is_word(head) && is_word(tail),
    }
}

#[path = "instrument_names_tests.rs"]
#[cfg(test)]
mod instrument_names_tests;
