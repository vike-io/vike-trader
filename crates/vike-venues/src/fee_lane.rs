//! `fee_lane` — the [`crate::fee_schedule_for`] key of a `(venue, symbol)` pair — and the symbol
//! STRING readers it rests on: the `.P` perp split ([`PERP_SUFFIX`], [`split_perp`]), a dated user
//! name's delivery day ([`is_delivery_day`]), the deribit expiry
//! ([`deribit_expiry`]) and the spot spelling a venue states as a row ([`spot_pair_row`]: bybit's, hyperliquid's,
//! okx's and deribit's unmarked pair, decision 0290), which the spot readers take as a parameter instead of spelling
//! one venue's rule.
//!
//! They came DOWN from `vike-catalog` (layer 20) to sit beside the table they key: a crate at or
//! below that layer could not resolve a lane, so `vike-strategy`'s `funding_carry` (layer 20 too)
//! keyed the table off the bare venue and priced a binance carry at the SPOT row. `vike-catalog`
//! keeps what reads its own addressing table (`uses_perp_suffix`, `split_perp_at`, `names_spot_pair`)
//! and calls these. Every caller names `vike_venues::…`; there is no second spelling.
//!
//! Pure string functions: nothing here allocates (the latency gate measures this crate), and the
//! paper/backtest mount that reads the lane fetches no venue metadata, which is the bar a lane must
//! clear (`crate::fees`' `fee_schedule_for` doc).

/// The market suffix marking a core symbol as a PERPETUAL contract.
///
/// Siblings exist in the same convention (`.F` futures, `.O` options) but no venue adapter maps
/// them today, so only this one is named here.
pub const PERP_SUFFIX: &str = ".P";

/// Split a CORE symbol into its EXCHANGE symbol and whether it names a perpetual.
///
/// `"BTCUSDT.P"` → `("BTCUSDT", true)`; `"BTCUSDT"` → `("BTCUSDT", false)`.
///
/// The returned symbol borrows from `symbol` — the WIRE form, for URLs, stream names and REST
/// params. The caller's original (suffixed) string stays the SERIES/sink label, so a perp's series
/// key never collides with its spot twin's. A bridge that knows its venue asks
/// `vike_catalog::split_perp_at`, which is this at a suffix venue and the identity elsewhere.
pub fn split_perp(symbol: &str) -> (&str, bool) {
    match symbol.strip_suffix(PERP_SUFFIX) {
        Some(base) => (base, true),
        None => (symbol, false),
    }
}

/// The FEE-table lane key for a `(venue, symbol)` pair — the [`crate::fee_schedule_for`] argument a
/// caller must use once the venue's exec routes SPOT vs PERP on [`PERP_SUFFIX`].
///
/// Returns `venue` unchanged for every single-lane venue, and the venue's PERP LANE SUB-KEY
/// (`"binance-perp"` / `"aster-perp"`) for a `.P` symbol on a DUAL-LANE venue. Borrowed either way
/// — nothing allocates.
///
/// ## Why a lane key rather than a second registry
///
/// This is the SAME convention [`crate::venue_time_in_force::venue_time_in_force`] already established for the one
/// axis that had already hit this problem: a lane sub-key is a plain string that gets its own arm
/// in the ONE authority table, the BARE venue id keeps meaning the lane it always meant, and the
/// caller that knows which lane it is on passes the sub-key (binance's perp order builder passes
/// `vike_binance::execution::rest::perp::TIME_IN_FORCE_LANE`; here the mount passes what this function returns). A lane
/// sub-key is NOT a roster venue — `vike_model::VENUES` completeness tests classify roster ids
/// only, exactly as `venue_time_in_force`'s do.
///
/// ## Which venues are dual-lane, and why bybit is not
///
/// A venue is dual-lane here iff its `exec.rs` branches on the suffix — binance's and aster's
/// `run()` both do `let (api_symbol, is_perp) = split_symbol(&symbol); if is_perp { run_perp } else
/// { run_spot }`. bybit also uses the suffix (`vike_catalog::uses_perp_suffix`), but its exec signs every
/// book through one client per kept row (decision 0290: the user name's row says the book), so it is not
/// dual-lane in this sense: its bare row IS its perp row, and a SPOT pair (the unmarked `PAIR`) is priced
/// apart by [`spot_pair_row`] (below), by its kept row where the caller holds one ([`kept_fee_lane`]).
/// vike-catalog's `fee_lane_is_declared_for_every_perp_suffix_venue` is what forces
/// that question to be answered for a NEW `.P` venue instead of letting it inherit one lane's fees
/// silently.
///
/// ## A venue may have MORE than two lanes — aster has three
///
/// "Spot vs perp" is not the only split a venue can price on. Aster runs ONE perp order API and
/// charges three different taker rates on it, by CONTRACT CLASS, so a `.P` aster symbol resolves to
/// either `"aster-perp"` or `"aster-perp-usd1"` depending on what the contract settles in. See
/// [`USD1_SETTLED_SUFFIX`] for the rule. The third class (a stock perpetual, 0.9 bps) has no name shape: only its
/// KEPT row says it, so [`kept_fee_lane`] resolves it and this function does not.
///
/// ## More lanes are decided by the symbol's SHAPE, not by a `.P` suffix
///
/// **deribit** prices an options book and a flat book under one id, by decision 0290's user names: a perpetual
/// (`BTCUSD.P`, `BTCUSDC.P`) and a dated future (`BTCUSD_261103`) resolve to `"deribit-futures"`, a spot pair
/// (`BTCUSDC`, the venue's `BTC_USDC`) to `"deribit-spot"`, and an option (the venue's own name) or a name of no user
/// shape keeps the bare options row. **bybit**, **hyperliquid** and **okx** price a spot pair apart from their
/// perpetual row, each spelled as its [`spot_pair_row`] states (the unmarked `DOGEUSDT` beside `DOGEUSDT.P` and
/// `DOGEUSDT_261225`, the unmarked `HYPEUSDC` beside `BTCUSDC.P`): `"bybit-spot"`, `"hyperliquid-spot"`,
/// `"okx-spot"`. **ctrader**'s
/// bare row is the FX and metals commission, and a symbol that is not a currency or metal pair (an
/// index, energy, share or crypto CFD) resolves to `"ctrader-cfd"`. **ibkr** prices a forex pair
/// (`USDJPY`) as `"ibkr-fx"` and a future (`ES_261218`) as `"ibkr-futures"`, but only by the KEPT row's class
/// ([`kept_fee_lane`]): a user name states no class, so here every ibkr name keeps the stock row.
/// All are decidable from the string alone, so they clear the bar the paper/backtest mount sets (it
/// fetches no venue metadata). The HIP-3 builder-dex perps (`XYZ:TSLAUSDC.P`) are NOT a
/// lane: their schedule depends on the deployer's growth mode, which only the venue's metadata states.
pub fn fee_lane<'a>(venue: &'a str, symbol: &str) -> &'a str {
    // A spot pair in its venue's stated spelling (the venue's row, never a venue named here).
    if let Some(row) = spot_pair_row(venue).filter(|row| row.spelling.names_spot(symbol)) {
        return row.fee_lane;
    }
    // The lanes decided by the symbol's SHAPE rather than a `.P` suffix (see the section below).
    match venue {
        // any case: a lower-cased mount must not silently take the OPTIONS row
        "deribit" if names_perpetual(symbol) || is_dated(symbol) => return DERIBIT_FUTURES_LANE,
        // The empty name is the bare-venue lookup (a backtest door with no series): it keeps the bare row.
        "ctrader" if !symbol.is_empty() && !is_currency_or_metal_pair(symbol) => {
            return CTRADER_CFD_LANE;
        }
        _ => {}
    }
    let (exchange_symbol, perp) = split_perp(symbol);
    // A dated contract's user name carries no `.P` (`BTCUSDT_261225`, decision 0290) and is priced on the futures lane.
    if !perp && !is_dated(symbol) {
        return venue;
    }
    match venue {
        "binance" => "binance-perp",
        // Aster's perp lane is priced by CONTRACT CLASS, not by one flat rate.
        "aster" if exchange_symbol.ends_with(USD1_SETTLED_SUFFIX) => "aster-perp-usd1",
        "aster" => "aster-perp",
        // Single-fee-lane venues (bybit and every non-suffix venue): the bare row is the only row.
        _ => venue,
    }
}

/// The lane key of a Deribit PERPETUAL or dated FUTURE (`BTCUSD.P`, `BTCUSDC.P`, `BTCUSD_261227`: the venue's
/// `BTC-PERPETUAL`, `BTC_USDC-PERPETUAL`, `BTC-27DEC26`): the venue's bare `deribit` row is its OPTIONS schedule
/// (`min(0.03 % of the underlying, 12.5 % of the premium)`), and a future is charged a flat taker rate
/// on its notional instead (measured 5.0 bps on ETH-PERPETUAL and on dated futures, testnet).
const DERIBIT_FUTURES_LANE: &str = "deribit-futures";

/// The lane key of a Deribit SPOT pair (`BTCUSDC`, the venue's `BTC_USDC`): published apart from the futures schedule, and
/// measured at the same 5 bps taker on `BTC_USDC` (testnet).
const DERIBIT_SPOT_LANE: &str = "deribit-spot";

/// The lane key of a cTrader CFD (an index, energy, share or crypto CFD: `US500`, `XTIUSD`, `BTCUSD`):
/// the venue's bare row is the measured broker's FX and metals commission, and its CFDs are charged
/// nothing beyond the spread.
const CTRADER_CFD_LANE: &str = "ctrader-cfd";

/// The lane key of an IBKR forex pair (`EUR.USD.IDEALPRO`, `EUR.IDEALPRO.USD`, `EUR.IDEALPRO.USD.CASH`):
/// the venue's bare row is its US-stock schedule, and spot currencies are charged a fraction of a basis
/// point with a per-order minimum.
const IBKR_FX_LANE: &str = "ibkr-fx";

/// The lane key of an IBKR future (`ES.CME.USD.FUT.20261218`): charged per contract.
const IBKR_FUTURES_LANE: &str = "ibkr-futures";

/// The lane key of a Hyperliquid SPOT pair: the venue's bare row is its PERP schedule, and a spot pair
/// is charged more (4 bps maker, 7 bps taker at the base tier, against 1.5 and 4.5).
const HYPERLIQUID_SPOT_LANE: &str = "hyperliquid-spot";

/// **A deribit `DDMMMYY` expiry as `(year, month, day)`** (`27MAR26` is 2026-03-27, `3NOV26` 2026-11-03): one or
/// two day digits, an upper-case month, two year digits (2000 to 2099), and a day the calendar has
/// (`vike_utility::calendar::is_real_day`: `31FEB26`, `00JAN26` and `29FEB25` are `None`). The ONE reading of the
/// venue's own grammar, which lives in the deribit bridge (decision 0290): `crates/bridges/deribit/src/names.rs` and
/// `crates/bridges/deribit/src/feed/options/chain.rs`'s `parse_instrument_name` ask it, and so does the door's
/// option name (`vike_catalog::door_symbol`), so no reader names a day the venue would refuse (`ETH-31FEB26`). A USER
/// name's `_YYMMDD` is [`is_delivery_day`]'s.
#[must_use]
pub fn deribit_expiry(expiry: &str) -> Option<(i64, u32, u32)> {
    const MONTHS: [&str; 12] =
        ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    if !expiry.is_ascii() || !(6..=7).contains(&expiry.len()) {
        return None;
    }
    let (day, rest) = expiry.split_at(expiry.len() - 5);
    let (month, year) = rest.split_at(3);
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(day) || !digits(year) {
        return None;
    }
    let month = (1_u32..).zip(MONTHS).find(|(_, word)| *word == month)?.0;
    let (year, day) = (2000 + year.parse::<i64>().ok()?, day.parse::<u32>().ok()?);
    vike_utility::calendar::is_real_day(year, month, day).then_some((year, month, day))
}

/// Whether `symbol` ends in [`PERP_SUFFIX`] in any case, with something before it: a perpetual's user name.
fn names_perpetual(symbol: &str) -> bool {
    let b = symbol.as_bytes();
    b.len() > PERP_SUFFIX.len()
        && b[b.len() - PERP_SUFFIX.len()..].eq_ignore_ascii_case(PERP_SUFFIX.as_bytes())
}

/// Whether `symbol` is the USER name of a dated contract (decision 0290): a pair, an underscore, its delivery day
/// ([`is_delivery_day`]) and nothing more (`BTCUSDT_261225`, deribit's `BTCUSD_261103` for `BTC-3NOV26`). A deribit
/// option keeps the venue's own name (`BTC_USDC-27MAR26-2500-C`), whose tail after the underscore is no day. The ONE
/// reading of that shape: [`fee_lane`], `vike_catalog::on_futures_lane` and `vike_secrets`' lane twin ask it.
#[must_use]
pub fn is_dated(symbol: &str) -> bool {
    symbol.split_once('_').is_some_and(|(pair, day)| !pair.is_empty() && is_delivery_day(day))
}

/// **Whether `yymmdd` is the delivery day a dated USER name ends in** (decision 0290's `_YYMMDD`): six digits on a
/// day the calendar has (2000 to 2099; `260231` and `260229` are none, as the venue lists no such contract, flag
/// DER-4). The ONE reading of that tail: [`fee_lane`]'s futures lane, `vike_catalog::is_inverse_contract` and
/// `vike_catalog::on_futures_lane` ask it.
#[must_use]
pub fn is_delivery_day(yymmdd: &str) -> bool {
    let number = |range: std::ops::Range<usize>| yymmdd.get(range)?.parse::<u32>().ok();
    yymmdd.len() == 6
        && yymmdd.bytes().all(|b| b.is_ascii_digit())
        && matches!((number(0..2), number(2..4), number(4..6)), (Some(year), Some(month), Some(day))
            if vike_utility::calendar::is_real_day(2000 + i64::from(year), month, day))
}

/// **[`fee_lane`] with the symbol's KEPT row in hand** (the datahub's list; the owner's rulings of 2026-11-13 and
/// 2026-11-16: a class is the row's, never the name's): on every venue whose lanes split by class, the row's class
/// decides (`row_fee_lane`: a bybit spot row is `bybit-spot` whatever its name, BYB-3; an aster stock perpetual
/// `aster-perp-stock`; a ctrader FX pair or metal CFD the bare row and any other CFD `ctrader-cfd`; an ibkr forex pair
/// row `ibkr-fx`, a future row `ibkr-futures` and a stock row the bare row, IBKR-3, decision 0290: `USDJPY` and
/// `ES_261218` are user names, which state no class). With no row the answer is [`fee_lane`]'s, read off the NAME as
/// before; on ctrader, bybit and ibkr the second value is then `true`, and the caller warns, once, that the lane was
/// read off the name ([`NAMED_FEE_LANE_WARNING`]). Allocation-free, like [`fee_lane`].
#[must_use]
pub fn kept_fee_lane<'a>(
    venue: &'a str,
    symbol: &str,
    row: Option<&vike_model::Instrument>,
) -> (&'a str, bool) {
    if symbol.is_empty() {
        return (fee_lane(venue, symbol), false);
    }
    match row.and_then(|row| row_fee_lane(venue, row)) {
        Some(lane) => (lane, false),
        None => (
            fee_lane(venue, symbol),
            row.is_none() && matches!(venue, "ctrader" | "bybit" | "ibkr"),
        ),
    }
}

/// The lane a KEPT `row`'s class prices at on `venue`, or `None` where the venue has one lane for every class (the
/// name's [`fee_lane`] answers there). The same lanes as [`fee_lane`]'s, read off the row's [`vike_model::Contract`]
/// instead of the name's shape; aster's stock class has no name shape at all, only the row's `underlyingSubType`
/// ([`ASTER_STOCK_SUB_TYPES`]).
fn row_fee_lane<'a>(venue: &'a str, row: &vike_model::Instrument) -> Option<&'a str> {
    use vike_model::{Contract, ContractCfdUnderlying, ContractComboKind};
    let spot = matches!(row.contract, Contract::CryptoSpot);
    let future = matches!(row.contract, Contract::Perp(_) | Contract::Future(_));
    Some(match venue {
        "ctrader" => match &row.contract {
            Contract::Fx(_)
            | Contract::Cfd(vike_model::ContractCfd {
                underlying_kind: ContractCfdUnderlying::Metal,
                ..
            }) => venue,
            _ => CTRADER_CFD_LANE,
        },
        "ibkr" => match row.contract {
            Contract::Fx(_) => IBKR_FX_LANE,
            Contract::Future(_) => IBKR_FUTURES_LANE,
            _ => venue,
        },
        "bybit" | "okx" if spot => spot_pair_row(venue)?.fee_lane,
        "hyperliquid" if spot => HYPERLIQUID_SPOT_LANE,
        "bybit" | "okx" | "hyperliquid" => venue,
        "deribit" => match &row.contract {
            Contract::Combo(c) if c.kind == ContractComboKind::FutureSpread => DERIBIT_FUTURES_LANE,
            _ if future => DERIBIT_FUTURES_LANE,
            _ if spot => DERIBIT_SPOT_LANE,
            _ => venue,
        },
        // A dual-lane venue's bare row is its SPOT lane's schedule, so every class off the futures lane prices there.
        _ if !future && spot_pair_lanes_for(venue).lanes == SpotPairLanes::TwoExecLanes => venue,
        "binance" => "binance-perp",
        "aster" if row.quote.eq_ignore_ascii_case(USD1_SETTLED_SUFFIX) => "aster-perp-usd1",
        "aster" if row.underlying_sub_type.iter().any(|word| is_aster_stock_word(word)) => {
            ASTER_STOCK_LANE
        }
        "aster" => "aster-perp",
        _ => return None,
    })
}

/// The lane key of an aster STOCK perpetual (a share, an ETF or a commodity: `AAPLUSDT.P`, `SPYUSDT.P`,
/// `XAUUSDT.P`): 0.9 bps taker against the crypto perpetual's 4 (`crate::fees`' `fee_schedule_for`). Only its KEPT
/// row says so ([`ASTER_STOCK_SUB_TYPES`]); a name never does.
const ASTER_STOCK_LANE: &str = "aster-perp-stock";

/// The `underlyingSubType` words of aster's `exchangeInfo` that its fee page prices as a Stock Perpetual: the venue's
/// own tags, measured 2026-08-05 (`STOCK`, `ETF`, `Commodities`, and blends such as `STOCK+ETF`). A word is read
/// whole or as one part of a `+` blend, in any case.
const ASTER_STOCK_SUB_TYPES: [&str; 3] = ["STOCK", "ETF", "COMMODITIES"];

/// Whether one `underlyingSubType` word of an aster row names a Stock-Perpetual class ([`ASTER_STOCK_SUB_TYPES`]).
fn is_aster_stock_word(word: &str) -> bool {
    word.split('+')
        .any(|part| ASTER_STOCK_SUB_TYPES.iter().any(|w| w.eq_ignore_ascii_case(part.trim())))
}

/// What a caller of [`kept_fee_lane`] says when it answered `true`, after the venue and symbol: the lane was read off
/// the name, the only answer a symbol with no kept row has.
pub const NAMED_FEE_LANE_WARNING: &str = "has no kept instrument row (the datahub's \
     <project>/instruments/<venue>.json holds none; ctrader's is listed while one of its accounts runs), so its fee \
     lane was read off its name";

/// Whether `symbol` is a currency pair or a precious metal quoted in a currency, the instruments a
/// cTrader broker charges its per-million commission on: six letters (or two names around a `/`)
/// whose quote is a [`vike_model::fiat_code`] and whose base is one too or a metal (`EURUSD`, `XAUUSD`,
/// `XAG/USD`). The NAME's reading [`kept_fee_lane`] keeps for a symbol with no kept row.
fn is_currency_or_metal_pair(symbol: &str) -> bool {
    const METALS: [&str; 4] = ["XAU", "XAG", "XPT", "XPD"];
    let symbol = symbol.trim();
    let (base, quote) = match symbol.split_once('/') {
        Some(halves) => halves,
        None if symbol.len() == 6 && symbol.is_ascii() => symbol.split_at(3),
        None => return false,
    };
    let metal = METALS.iter().any(|m| m.eq_ignore_ascii_case(base));
    (metal || vike_model::fiat_code(base).is_some()) && vike_model::fiat_code(quote).is_some()
}

/// How a venue spells a SPOT pair it lists beside its derivatives under ONE venue id: the venue fact the spot
/// readers take as a parameter ([`fee_lane`], `crate::venue_caps`' `balance_is_wallet_cash`,
/// `vike_catalog::names_spot_pair`), so none of them spells one venue's rule and all of them read one rule. The
/// spelling is the venue BRIDGE's, and a test in that bridge holds its row to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotPairSpelling {
    /// A spot pair is the bare name, letters and digits (`BTCUSDT`), and every derivative is MARKED, decision 0290's
    /// user grammar: a perpetual by [`PERP_SUFFIX`] (`BTCUSDT.P`), a dated contract by the `_` before its delivery day
    /// (`ETHUSDT_261106`). The empty name (the bare-venue lookup) and another venue's spelling (`BTC-USDT`) are
    /// neither.
    Unmarked,
}

impl SpotPairSpelling {
    /// Whether `symbol` is spelled as a spot pair.
    pub fn names_spot(self, symbol: &str) -> bool {
        match self {
            SpotPairSpelling::Unmarked => {
                !symbol.is_empty() && symbol.bytes().all(|b| b.is_ascii_alphanumeric())
            }
        }
    }

    /// Whether `symbol` is spelled as one of the venue's DERIVATIVES. Not always "not spot": the empty name and another
    /// venue's spelling are neither.
    pub fn names_derivative(self, symbol: &str) -> bool {
        match self {
            SpotPairSpelling::Unmarked => symbol.ends_with(PERP_SUFFIX) || symbol.contains('_'),
        }
    }
}

/// A venue's SPOT book beside its derivatives under one venue id: how its bridge spells a spot pair, and the
/// [`crate::fee_schedule_for`] lane that prices one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotPairRow {
    pub spelling: SpotPairSpelling,
    pub fee_lane: &'static str,
}

/// The [`SpotPairRow`] of `venue`: one NAMED row per venue whose spot pair is told apart by a spelling of its own,
/// else `None`. binance's and aster's spot pair is the bare name beside a [`PERP_SUFFIX`] perpetual, so neither is a
/// row here.
pub fn spot_pair_row(venue: &str) -> Option<SpotPairRow> {
    match venue {
        // The pair `DOGEUSDT` beside `DOGEUSDT.P` and `ETHUSDT_261106` (decision 0290's user grammar; the bridge reads
        // a name's book off its kept row, `crates/bridges/bybit/src/instrument/class.rs`'s `venue_symbol`, and
        // `kept_fee_lane` prices by that row). The bare row is the linear-perpetual schedule (2 / 5.5 bps); a spot pair
        // is charged 10 / 10 bps at VIP0.
        "bybit" => {
            Some(SpotPairRow { spelling: SpotPairSpelling::Unmarked, fee_lane: "bybit-spot" })
        }
        // The pair `BTCUSDT` (the venue's `BTC-USDT`) beside `BTCUSDT.P` and `BTCUSDT_261225` (decision 0290's user
        // grammar; the bridge signs the kept row's instId). The bare row is the SWAP schedule (2 / 5 bps); a regular
        // user's spot pair is charged 8 / 10 bps.
        "okx" => Some(SpotPairRow { spelling: SpotPairSpelling::Unmarked, fee_lane: "okx-spot" }),
        // The pair `HYPEUSDC` beside the perpetual `BTCUSDC.P` and the builder-dex `XYZ:TSLAUSDC.P` (decision 0290's
        // user grammar; the bridge reads a name's row off its universe,
        // `crates/bridges/hyperliquid/src/instrument/symbology.rs`'s `Symbology`). The bare row is the PERP schedule
        // (1.5 / 4.5 bps at the base tier); a spot pair is charged more, 4 / 7 bps.
        "hyperliquid" => Some(SpotPairRow {
            spelling: SpotPairSpelling::Unmarked,
            fee_lane: HYPERLIQUID_SPOT_LANE,
        }),
        // The pair `BTCUSDC` (the venue's `BTC_USDC`) beside `BTCUSDC.P` and an option under the venue's own name
        // (decision 0290); published apart from the futures schedule and measured at the same 5 bps taker.
        "deribit" => {
            Some(SpotPairRow { spelling: SpotPairSpelling::Unmarked, fee_lane: DERIBIT_SPOT_LANE })
        }
        _ => None,
    }
}

/// How many EXEC lanes trade a venue's spot pairs and its derivatives under one venue id: the column beside
/// [`spot_pair_row`] for the two venues it has no row for, binance and aster, whose spot pair is the bare name beside a
/// [`PERP_SUFFIX`] perpetual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotPairLanes {
    /// Two exec lanes under one id: spot and the futures lane, an engine on ONE of them picked from the symbol it is
    /// mounted on (decision 0290's user grammar: a `.P` perpetual or a `_YYMMDD` dated contract is the futures lane,
    /// the bare pair spot), so the spot lane's book is not the futures lane's and the venue's one capability row is
    /// the UNION of both lanes.
    TwoExecLanes,
    /// One exec lane signs every class the venue lists under its id.
    OneExecLane,
}

/// One venue's [`SpotPairLanes`] and where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpotPairLanesRow {
    pub lanes: SpotPairLanes,
    /// Why, citing the adapter code (a `path`'s `symbol`, never a line number).
    pub why: &'static str,
}

/// The `why` of a venue whose spot pair beside its derivatives is a [`spot_pair_row`] row.
const ONE_LANE_WITH_A_SPOT_ROW: &str = "one exec lane signs the spot pair beside the derivatives, told apart by the \
     name's spelling (its `spot_pair_row`), so either holds a stop (`crates/vike-catalog/src/symbol.rs`'s \
     `spot_lane_holds_no_stop`)";

/// The `why` of a venue that lists no spot pair beside derivatives under one id.
const ONE_LANE: &str = "one exec lane signs every class the venue lists under its id: no spot pair stands beside a \
     derivative of the same name";

/// The dual-lane set: one NAMED row per roster venue, read wherever the set is asked for: `vike_catalog`'s
/// `spot_lane_holds_no_stop` (which spot engine holds no stop), the kept-row fee lane ([`kept_fee_lane`]: a dual-lane
/// venue prices every class off its futures lane at its bare row) and the tradehub's symbol admission. An unknown venue
/// string has one lane.
///
/// ⚠ NOT `crate::venue_caps`' `balance_is_wallet_cash`, whose `"binance" | "aster"` arm happens to name the same two
/// venues: it is a MEASURED membership (each venue's perpetual balance read from its reconcile parser), and a venue
/// joins it by its own measured arm, never by having two lanes. Reading this column there would adopt an unmeasured
/// balance as wallet cash beside an open position.
#[must_use]
pub fn spot_pair_lanes_for(venue: &str) -> SpotPairLanesRow {
    const fn row(lanes: SpotPairLanes, why: &'static str) -> SpotPairLanesRow {
        SpotPairLanesRow { lanes, why }
    }
    match venue {
        "binance" => row(
            SpotPairLanes::TwoExecLanes,
            "`crates/bridges/binance/src/adapter.rs`'s `VenueAdapterBinance` routes on the engine symbol (its `.P` \
             routing: a `.P` name the USDⓈ-M or COIN-M perp lane, anything else spot)",
        ),
        "aster" => row(
            SpotPairLanes::TwoExecLanes,
            "the binance family's `.P` routing: a `.P` symbol is the perp client \
             (`crates/bridges/aster/src/execution/mod.rs`'s `VenueRestAsterPerp`), anything else spot",
        ),
        "bybit" | "okx" | "hyperliquid" | "deribit" => {
            row(SpotPairLanes::OneExecLane, ONE_LANE_WITH_A_SPOT_ROW)
        }
        "oanda" | "ig" | "fxcm" | "dukascopy" | "polymarket" | "ibkr" | "ctrader" | "alpaca" => {
            row(SpotPairLanes::OneExecLane, ONE_LANE)
        }
        // vike:new-venue:row "{venue}" => row(SpotPairLanes::OneExecLane, ONE_LANE), // TODO(new-venue: {venue}): two exec lanes under one id?
        _ => row(SpotPairLanes::OneExecLane, "not a roster venue"),
    }
}

/// The settlement asset that puts an aster perp on its own fee lane.
///
/// Aster charges three different taker rates on ONE perp order API, split by contract class — a
/// live sweep of the venue's own `GET /fapi/v3/commissionRate` across 17 symbols on 2026-08-05
/// found crypto at 4 bps, equity/ETF/commodity at 0.9 bps and USD1-settled at 0.5 bps (see
/// `crate::fees`' `fee_schedule_for` for the full table and its sourcing).
///
/// Of those three classes, **only the USD1 one is decidable from the symbol**, because the
/// settlement asset is part of the name: `BTCUSD1` settles in USD1, `BTCUSDT` in USDT. The equity
/// class is NOT resolved from a name — `AAPLUSDT` and `ASTERUSDT` are the same string shape, and only the
/// venue's `exchangeInfo` `underlyingSubType` separates them, which the KEPT row carries
/// (`vike_model::Instrument::underlying_sub_type`): [`kept_fee_lane`] reads it ([`ASTER_STOCK_SUB_TYPES`]), never a
/// dated membership list.
///
/// ⚠ Matched as a SUFFIX of the EXCHANGE symbol (post-`.P`-strip), never as a substring:
/// `USD1USDT` is USDT-settled with USD1 as the BASE asset and must keep the crypto lane.
/// `usd1_lane_matches_settlement_not_substring` (in `fee_lane_tests`) is the pin.
const USD1_SETTLED_SUFFIX: &str = "USD1";

#[path = "fee_lane_tests.rs"]
#[cfg(test)]
mod fee_lane_tests;
