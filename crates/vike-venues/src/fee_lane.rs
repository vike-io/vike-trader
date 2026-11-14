//! `fee_lane` — the [`crate::fee_schedule_for`] key of a `(venue, symbol)` pair — and the symbol
//! STRING readers it rests on: the `.P` perp split ([`PERP_SUFFIX`], [`split_perp`]), the fiat-code
//! set ([`fiat_code`]), the day-month-year expiry ([`is_day_month_year`]), hyperliquid's pair shape
//! ([`is_hyperliquid_spot`], with [`PAIR_SEPARATOR`] and [`DEX_MARKER`]) and the spot spelling a venue
//! states as a row ([`spot_pair_row`]: bybit's `.S`, okx's two-part instId), which the spot readers take
//! as a parameter instead of spelling one venue's rule.
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
/// { run_spot }`. bybit also uses the suffix (`vike_catalog::uses_perp_suffix`), but its exec does
/// not branch on `.P` (the bare name is the perpetual, whatever suffix is typed), so it is not
/// dual-lane in this sense: its bare row IS its perp row. Since decision 0231 its exec also signs a
/// SPOT pair, spelled `PAIR.S`, and that is priced by the shape of the symbol (below) like
/// hyperliquid's. vike-catalog's `fee_lane_is_declared_for_every_perp_suffix_venue` is what forces
/// that question to be answered for a NEW `.P` venue instead of letting it inherit one lane's fees
/// silently.
///
/// ## A venue may have MORE than two lanes — aster has three
///
/// "Spot vs perp" is not the only split a venue can price on. Aster runs ONE perp order API and
/// charges three different taker rates on it, by CONTRACT CLASS, so a `.P` aster symbol resolves to
/// either `"aster-perp"` or `"aster-perp-usd1"` depending on what the contract settles in. See
/// [`USD1_SETTLED_SUFFIX`] for the rule and for the third class this function deliberately does
/// NOT resolve.
///
/// ## More lanes are decided by the symbol's SHAPE, not by a `.P` suffix
///
/// **deribit** prices an options book and a flat book under one id: a perpetual, a dated future and
/// a future spread resolve to `"deribit-futures"`, a spot pair (`BTC_USDC`) to `"deribit-spot"`, and
/// an option (or a name of no deribit shape) keeps the bare options row. **hyperliquid** prices
/// perps and spot pairs apart: a spot pair (`HYPE/USDC`, `HYPE-USDC`) resolves to
/// `"hyperliquid-spot"`, every perp keeps the bare row. **bybit** and **okx** price a spot pair
/// apart from their perpetual row, each spelled as its [`spot_pair_row`] states (`DOGEUSDT.S`; the
/// two-part instId `BTC-USDT`): `"bybit-spot"`, `"okx-spot"`. **ctrader**'s
/// bare row is the FX and metals commission, and a symbol that is not a currency or metal pair (an
/// index, energy, share or crypto CFD) resolves to `"ctrader-cfd"`. **ibkr** prices a forex pair
/// (`EUR.USD.IDEALPRO`) as `"ibkr-fx"` and a future (`ES.CME.USD.FUT.20261218`) as `"ibkr-futures"`,
/// read as its bridge's `parse_simplified` reads the name; every other name keeps the stock row.
/// All are decidable from the string alone, so they clear the bar the paper/backtest mount sets (it
/// fetches no venue metadata). The HIP-3 builder-dex perps (`xyz:TSLA`, `TSLA.d-xyz`) are NOT a
/// lane: their schedule depends on the deployer's growth mode, which only the venue's metadata states.
pub fn fee_lane<'a>(venue: &'a str, symbol: &str) -> &'a str {
    // A spot pair in its venue's stated spelling (the venue's row, never a venue named here).
    if let Some(row) = spot_pair_row(venue).filter(|row| row.spelling.names_spot(symbol)) {
        return row.fee_lane;
    }
    // The lanes decided by the symbol's SHAPE rather than a `.P` suffix (see the section below).
    match venue {
        "deribit" if is_deribit_perpetual(symbol) || is_deribit_dated(symbol) => {
            return DERIBIT_FUTURES_LANE;
        }
        "deribit" if is_deribit_spot(symbol) => return DERIBIT_SPOT_LANE,
        "hyperliquid" if is_hyperliquid_spot(symbol) => return HYPERLIQUID_SPOT_LANE,
        // The empty name is the bare-venue lookup (a backtest door with no series): it keeps the bare row.
        "ctrader" if !symbol.is_empty() && !is_currency_or_metal_pair(symbol) => {
            return CTRADER_CFD_LANE;
        }
        "ibkr" | "ibkr_cpapi" if is_ibkr_forex(symbol) => return IBKR_FX_LANE,
        "ibkr" | "ibkr_cpapi" if is_ibkr_future(symbol) => return IBKR_FUTURES_LANE,
        _ => {}
    }
    let (exchange_symbol, perp) = split_perp(symbol);
    if !perp {
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

/// The lane key of a Deribit PERPETUAL or dated FUTURE (`BTC-PERPETUAL`, `BTC_USDC-PERPETUAL`,
/// `BTC-27DEC26`, `BTC-FS-27DEC26_PERP`): the venue's bare `deribit` row is its OPTIONS schedule
/// (`min(0.03 % of the underlying, 12.5 % of the premium)`), and a future is charged a flat taker rate
/// on its notional instead (measured 5.0 bps on ETH-PERPETUAL and on dated futures, testnet).
const DERIBIT_FUTURES_LANE: &str = "deribit-futures";

/// The lane key of a Deribit SPOT pair (`BTC_USDC`): published apart from the futures schedule, and
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

/// Whether `symbol` names a Deribit dated future or a future spread: a base, a hyphen, then a
/// day-month-year expiry and nothing more (`BTC-27DEC26`, `BTC_USDC-27DEC26`), or the `FS` combo word
/// (`BTC-FS-27DEC26_PERP`). An option carries a strike and a right after its expiry, so it is neither.
fn is_deribit_dated(symbol: &str) -> bool {
    let Some((base, tail)) = symbol.split_once('-') else { return false };
    !base.is_empty()
        && (is_day_month_year(tail) || tail.get(..3).is_some_and(|w| w.eq_ignore_ascii_case("FS-")))
}

/// Whether `symbol` names a Deribit spot pair: two non-empty alphanumeric names joined by ONE
/// underscore and no hyphen (`BTC_USDC`, `STETH_ETH`). A USDC book's perpetual or future carries the
/// underscore too, but always after a hyphen-free base and before a hyphen.
fn is_deribit_spot(symbol: &str) -> bool {
    let Some((base, quote)) = symbol.split_once('_') else { return false };
    let name = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    name(base) && name(quote)
}

/// Whether `symbol` names a Deribit perpetual: an instrument name ending in `-PERPETUAL`.
///
/// Decidable from the symbol alone, which is the bar a lane must clear (`crate::fees`'
/// `fee_schedule_for`): Deribit's instrument names are `<CCY>-PERPETUAL` and `<CCY>_<CCY>-PERPETUAL`,
/// dated futures are `<CCY>-<DDMMMYY>` and options `<CCY>-<DDMMMYY>-<strike>-<C|P>`. Deribit's names
/// are upper-case, but the match ignores case: a lower-cased mount must not silently take the
/// OPTIONS row. Dated futures share the lane through [`is_deribit_dated`].
fn is_deribit_perpetual(symbol: &str) -> bool {
    const SUFFIX: &str = "-PERPETUAL";
    symbol.len() > SUFFIX.len()
        && symbol.get(symbol.len() - SUFFIX.len()..).is_some_and(|s| s.eq_ignore_ascii_case(SUFFIX))
}

/// `9OCT26` or `27DEC26`: a day of one or two digits, three letters, a two-digit year — the expiry
/// word of a Deribit dated future or option (and of the inverse futures `vike_catalog` reads a face
/// value off).
pub fn is_day_month_year(s: &str) -> bool {
    let b = s.as_bytes();
    matches!(b.len(), 6 | 7)
        && b[b.len() - 2..].iter().all(u8::is_ascii_digit)
        && b[b.len() - 5..b.len() - 2].iter().all(u8::is_ascii_alphabetic)
        && b[..b.len() - 5].iter().all(u8::is_ascii_digit)
}

/// **[`fee_lane`] with the symbol's KEPT row in hand** (the datahub's list; the owner's ruling of 2026-11-13: a class
/// is the row's, never the name's): ctrader prices a symbol by its row's class, an FX pair or a metal CFD at the bare
/// row (the measured broker's FX and metals commission) and any other CFD at `ctrader-cfd`. With no row on ctrader
/// the answer is [`fee_lane`]'s, read off the NAME as before, and the second value is `true`: the caller warns,
/// once, that the lane was read off the name ([`NAMED_FEE_LANE_WARNING`]). Every other venue is [`fee_lane`] exactly
/// (its lanes are its own wire grammar), `false`. Allocation-free, like [`fee_lane`].
#[must_use]
pub fn kept_fee_lane<'a>(
    venue: &'a str,
    symbol: &str,
    row: Option<&vike_model::Instrument>,
) -> (&'a str, bool) {
    use vike_model::{Contract, ContractCfdUnderlying};
    if venue != "ctrader" || symbol.is_empty() {
        return (fee_lane(venue, symbol), false);
    }
    let Some(row) = row else { return (fee_lane(venue, symbol), true) };
    let commission = matches!(
        &row.contract,
        Contract::Fx(_)
            | Contract::Cfd(vike_model::ContractCfd {
                underlying_kind: ContractCfdUnderlying::Metal,
                ..
            })
    );
    (if commission { venue } else { CTRADER_CFD_LANE }, false)
}

/// What a caller of [`kept_fee_lane`] says when it answered `true`, after the venue and symbol: the lane was read off
/// the name, the only answer a symbol with no kept row has.
pub const NAMED_FEE_LANE_WARNING: &str = "has no kept instrument row (the datahub's \
     <project>/instruments/<venue>.json holds none; ctrader's is listed while one of its accounts runs), so its fee \
     lane was read off its name";

/// Whether `symbol` is a currency pair or a precious metal quoted in a currency, the instruments a
/// cTrader broker charges its per-million commission on: six letters (or two names around a `/`)
/// whose quote is a [`fiat_code`] and whose base is one too or a metal (`EURUSD`, `XAUUSD`, `XAG/USD`).
/// The NAME's reading [`kept_fee_lane`] keeps for a symbol with no kept row.
fn is_currency_or_metal_pair(symbol: &str) -> bool {
    const METALS: [&str; 4] = ["XAU", "XAG", "XPT", "XPD"];
    let symbol = symbol.trim();
    let (base, quote) = match symbol.split_once('/') {
        Some(halves) => halves,
        None if symbol.len() == 6 && symbol.is_ascii() => symbol.split_at(3),
        None => return false,
    };
    let metal = METALS.iter().any(|m| m.eq_ignore_ascii_case(base));
    (metal || fiat_code(base).is_some()) && fiat_code(quote).is_some()
}

/// Whether an IBKR canonical, read as `crates/bridges/vike-ibkr/src/instrument/contract.rs`'s `parse_simplified`
/// reads it (fields split on `.`), is a forex pair: the three-field form naming `IDEALPRO` in either
/// field order, or a fourth field claiming `CASH`.
fn is_ibkr_forex(symbol: &str) -> bool {
    let is = |i: usize, word: &str| {
        symbol.split('.').nth(i).is_some_and(|f| f.eq_ignore_ascii_case(word))
    };
    match symbol.split('.').count() {
        3 => is(1, "IDEALPRO") || is(2, "IDEALPRO"),
        n => n >= 4 && is(3, "CASH"),
    }
}

/// Whether an IBKR canonical (read as [`is_ibkr_forex`] reads it) claims `FUT` in its fourth field.
fn is_ibkr_future(symbol: &str) -> bool {
    symbol.split('.').count() >= 4
        && symbol.split('.').nth(3).is_some_and(|f| f.eq_ignore_ascii_case("FUT"))
}

/// How a venue spells a SPOT pair it lists beside its derivatives under ONE venue id: the venue fact the spot
/// readers take as a parameter ([`fee_lane`], `crate::venue_caps`' `balance_is_wallet_cash`,
/// `vike_catalog::names_spot_pair`), so none of them spells one venue's rule and all of them read one rule. The
/// spelling is the venue BRIDGE's, and a test in that bridge holds its row to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotPairSpelling {
    /// A spot pair carries this suffix, and every other name is a derivative.
    Suffix(&'static str),
    /// A spot pair is two names around ONE `separator`; a derivative has three parts or more.
    TwoParts(char),
}

impl SpotPairSpelling {
    /// Whether `symbol` is spelled as a spot pair.
    pub fn names_spot(self, symbol: &str) -> bool {
        match self {
            SpotPairSpelling::Suffix(suffix) => symbol.ends_with(suffix),
            SpotPairSpelling::TwoParts(separator) => symbol.matches(separator).count() == 1,
        }
    }

    /// Whether `symbol` is spelled as one of the venue's DERIVATIVES. Not always "not spot": a
    /// [`SpotPairSpelling::TwoParts`] name with no separator at all is neither.
    pub fn names_derivative(self, symbol: &str) -> bool {
        match self {
            SpotPairSpelling::Suffix(suffix) => !symbol.ends_with(suffix),
            SpotPairSpelling::TwoParts(separator) => symbol.matches(separator).count() >= 2,
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
/// else `None`. binance's and aster's spot pair is the bare name beside a [`PERP_SUFFIX`] perpetual, and
/// hyperliquid's and deribit's spot shapes are [`fee_lane`]'s own arms: none is a row here.
pub fn spot_pair_row(venue: &str) -> Option<SpotPairRow> {
    match venue {
        // `DOGEUSDT.S` (decision 0231): `crates/bridges/bybit/src/instrument/class.rs`'s `SPOT_SUFFIX`. The bare row is the
        // linear-perpetual schedule (2 / 5.5 bps); a spot pair is charged 10 / 10 bps at VIP0.
        "bybit" => {
            Some(SpotPairRow { spelling: SpotPairSpelling::Suffix(".S"), fee_lane: "bybit-spot" })
        }
        // `BTC-USDT` beside `BTC-USDT-SWAP` and `BTC-USD-261225` (decision 0232):
        // `crates/bridges/okx/src/instrument.rs`'s `InstrumentType::of_inst_id`. The bare row is the SWAP schedule
        // (2 / 5 bps); a regular user's spot pair is charged 8 / 10 bps.
        "okx" => {
            Some(SpotPairRow { spelling: SpotPairSpelling::TwoParts('-'), fee_lane: "okx-spot" })
        }
        _ => None,
    }
}

/// Whether `symbol` names a Hyperliquid SPOT pair, in either of its two spellings: the venue-side
/// `HYPE/USDC` the bridge mints today, and the core `HYPE-USDC` of `vike_catalog::to_core_symbol`.
/// Both are ONE separator between two non-empty names. A PERP is a bare coin (`BTC`, `kPEPE`), a
/// builder-dex perp is `xyz:TSLA` / `TSLA.d-xyz` ([`DEX_MARKER`]), and none of the 523 perp or the
/// spot token names measured on 2026-09-19 contains a hyphen (see `vike_catalog::to_exchange_symbol`),
/// so the shape decides it. A symbol with two separators (`BTC-USDT-SWAP`, another venue's spelling)
/// is not a Hyperliquid pair.
pub fn is_hyperliquid_spot(symbol: &str) -> bool {
    if symbol.contains(':') || symbol.contains(DEX_MARKER) {
        return false;
    }
    let mut parts = symbol.split(['/', PAIR_SEPARATOR]);
    matches!((parts.next(), parts.next(), parts.next()), (Some(base), Some(quote), None) if !base.is_empty() && !quote.is_empty())
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
/// class is deliberately NOT resolved here — `AAPLUSDT` and `ASTERUSDT` are the same string shape,
/// and only the venue's `exchangeInfo` `underlyingSubType` separates them. A pure string function
/// has no honest way to know that, and baking in a dated ~105-symbol membership list is exactly the
/// drift this module's doc exists to argue against; `fee_schedule_for`'s `"aster-perp"` arm carries
/// the measured numbers and the reason it stays a documented gap.
///
/// ⚠ Matched as a SUFFIX of the EXCHANGE symbol (post-`.P`-strip), never as a substring:
/// `USD1USDT` is USDT-settled with USD1 as the BASE asset and must keep the crypto lane.
/// `usd1_lane_matches_settlement_not_substring` (in `fee_lane_tests`) is the pin.
const USD1_SETTLED_SUFFIX: &str = "USD1";

/// The separator a CORE symbol writes where a venue writes `/`.
///
/// `/` cannot appear in a core symbol at all: the symbol becomes the directory name
/// `symbol=<symbol>` in the hist store, so a slash silently adds a path level —
/// `vike_paths::store_path::refuse_a_path_hostile_symbol` carries that measurement and refuses it
/// at the store's door. A hyphen is already the workspace's pair separator in every venue that
/// needs one (`BTC-1JAN27-100000-C`, `EUR_USD`'s sibling spellings), so this is the spelling that
/// was already here rather than a new convention.
pub const PAIR_SEPARATOR: char = '-';

/// The marker a CORE symbol writes for an instrument listed on a venue's THIRD-PARTY sub-venue.
///
/// Hyperliquid's builder-deployed perp DEXes name their instruments `<dex>:<coin>` (`xyz:TSLA`),
/// and a colon is a Win32-reserved character — a directory holding one cannot be created on
/// Windows at all, measured natively. So the core spelling moves the dex to the END behind this
/// marker: `TSLA.d-xyz`.
///
/// ⚠ **The `d-` half is load-bearing and is not decoration.** The suffix namespace already carries
/// PRODUCT CLASSES ([`PERP_SUFFIX`] `.P`, with `.F`/`.O` reserved beside it) — a closed set this
/// workspace owns. Dex names are chosen by third parties and the set is open: eleven exist today
/// (`xyz`, `flx`, `vntl`, `hyna`, `km`, `abcd`, `cash`, `para`, `mkts`, `io`), and nothing stops a
/// twelfth being called `s` or `p`. Without `d-`, `TSLA.S` would be unreadable — a spot TSLA, or a
/// TSLA perp on a dex named `s`? With it, the two namespaces cannot collide by construction.
pub const DEX_MARKER: &str = ".d-";

/// The fiat currencies a roster FX or CFD venue quotes an instrument in: the closed set
/// `vike_catalog::fiat_quote_of` recognises. Anything outside it (a crypto asset, a stablecoin, a
/// metal) is NOT a fiat quote, and a name ending in one is unrecognised rather than guessed at.
/// `CLP`, `COP` and `TWD` are fxcm's committed forex rows
/// (`crates/bridges/fxcm/tests/fixtures/eur_demo_offers.json`: `USD/CLP`, `USD/COP`, `USD/TWD`),
/// which were read as stating no quote and so booked at par.
const FIAT_CODES: &[&str] = &[
    "AUD", "CAD", "CHF", "CLP", "CNH", "CNY", "COP", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF",
    "ILS", "INR", "JPY", "KRW", "MXN", "NOK", "NZD", "PLN", "RUB", "SEK", "SGD", "THB", "TRY",
    "TWD", "USD", "ZAR",
];

/// `token` (trimmed, any case) as a [`FIAT_CODES`] entry, upper-cased, or `None`: the one door a
/// ledger currency a person TYPED (`engine.account_currency = "jpy"`) goes through.
#[must_use]
pub fn fiat_code(token: &str) -> Option<&'static str> {
    let token = token.trim();
    FIAT_CODES.iter().copied().find(|code| code.eq_ignore_ascii_case(token))
}

#[path = "fee_lane_tests.rs"]
#[cfg(test)]
mod fee_lane_tests;
