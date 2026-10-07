//! IBKR contract model: ONE secType-tagged contract (STK/OPT/FUT/CASH/IND/CRYPTO + Other) covering
//! every asset class, plus symbology and the contract-details→`SymbolProperties` bridge.
//!
//! # Symbology — what a canonical vike symbol means at this venue
//!
//! A canonical is dot-delimited and its FIRST THREE fields are always `SYMBOL.EXCHANGE.CURRENCY`
//! (`AAPL.SMART.USD`). Everything after that is an EXPLICIT CLAIM about what kind of contract the
//! operator means:
//!
//! | spelling | contract |
//! |---|---|
//! | `AAPL.SMART.USD` | equity — the three-field form, allowed ONLY on a cash-equity exchange |
//! | `EUR.IDEALPRO.USD` | forex EUR/USD on IDEALPRO (the stated field order) |
//! | `EUR.USD.IDEALPRO` | the SAME contract, legacy field order (see the ⚠ below) |
//! | `VOD.LSE.GBP.STK` | equity, claimed explicitly — works on ANY exchange |
//! | `ES.GLOBEX.USD.FUT.20251219` | the December-2025 E-mini S&P future |
//! | `ES.GLOBEX.USD.FUT.202512.50` | the same, month-precision expiry plus an explicit multiplier |
//! | `SPY.SMART.USD.OPT.20251219.500.C` | a listed call |
//! | `BTC.PAXOS.USD.CRYPTO`, `SPX.CBOE.USD.IND` | the two remaining secTypes |
//!
//! **Anything else is REFUSED**, with a message naming the field it could not read and the
//! spellings it accepts. That refusal is the whole point of this module and it replaces a SILENT
//! EQUITY DEFAULT: until 2026-09-16 every canonical whose last field was not `IDEALPRO` was
//! returned as a STOCK, so `ESZ5.GLOBEX.USD` — the E-mini S&P future — was submitted as
//! `SecType::STK`, on the order path, with nothing warning anybody. A *better guess* would be the
//! same defect wearing a nicer coat; the cure for a missing claim is to make the caller supply one.
//!
//! ⚠ **A futures month code is NOT decoded, deliberately.** `ESZ5` encodes December 2025 in the
//! exchange's own single-digit-year convention, and that convention is ambiguous across decades:
//! read in 2035 the same five characters name December 2035, and read against a historical tape
//! they name December 2015. IBKR's own field is `lastTradeDateOrContractMonth` — `YYYYMM` or
//! `YYYYMMDD` — so this grammar asks for exactly that and refuses to invent the century. An order
//! on the wrong expiry is a real position in the wrong contract, and no error is raised for it.
//!
//! ⚠ **The three-field form is EXCHANGE-GATED, and the gate is an ALLOWLIST**
//! ([`EQUITY_EXCHANGES`]). An unknown exchange is refused rather than read as equity, because the
//! two failures are not symmetric: a missing allowlist row costs a NAMED refusal carrying its own
//! remedy (spell the secType), while a wrong equity default costs an order in the wrong
//! instrument. `SMART` is the one row that is a convention rather than a fact — it is a ROUTER
//! reaching futures and options too — and it is kept because `AAPL.SMART.USD` is the spelling this
//! tree already mounts.
//!
//! ⚠ **The legacy forex order is kept because it is what the tree already writes**
//! (`EUR.USD.IDEALPRO`). It inverts the stated field order, which used to mean the obedient
//! spelling `EUR.IDEALPRO.USD` silently produced an EQUITY named EUR — a trap this crate's own
//! `CLAUDE.md` documents. Both spellings now name the one contract, so no caller can be wrong.
//!
//! ⚠ **A symbol containing a dot is UNREACHABLE through this grammar** — so are the dotted IBKR
//! exchange codes (`NASDAQ.NMS`, `ENEXT.BE`). They are refused as a malformed canonical rather
//! than mis-split. Reaching one needs a different delimiter, which is a grammar change.
//!
//! When decision 0061 (*An instrument names its kind*, accepted 2026-09-16 — its record is not in
//! this tree yet, so no in-tree path is cited for it) lands its claim seam, the claim this grammar
//! spells as a field is the same claim `Option<AssetClass>` carries, and that enum's relevant
//! variants map one for one onto [`SecType`]. This module deliberately does NOT depend on
//! `vike-catalog` today: the seam is in flight, and the fix on the order path could not wait for
//! it. `vike-ibkr` is layer 40 and `vike-catalog` layer 20, so the edge would be legal when it is
//! wanted.
//!
//! A `conId ↔ symbol` bidirectional map lets inbound events (keyed by numeric conId) map back to
//! the canonical symbol.

use std::collections::HashMap;
use vike_model::{AssetClass, SymbolProperties};

/// The IBKR exchange codes the THREE-FIELD canonical is allowed to read as a CASH EQUITY.
///
/// **The admission rule for a new row: the venue lists cash equities, and an order routed there
/// with no secType can mean nothing else.** A row that is wrong in that direction reinstates the
/// silent equity default for that venue, which is the defect this module exists to remove — so
/// when in doubt leave the code out and let the operator spell `SYMBOL.EXCHANGE.CCY.STK`.
///
/// ⚠ `SMART` is the ONE row admitted against that rule. It is IBKR's smart ROUTER, not an
/// exchange, and it reaches futures and options as well as stocks — so `ES.SMART.USD` is exactly
/// as ambiguous as `ES.GLOBEX.USD` and this table cannot see the difference. It stays because
/// `AAPL.SMART.USD` is the symbol `crates/vike-tradehub/src/wired_markets.rs`'s `IBKR_MARKET` mounts and the one
/// every test in this crate uses; removing it would be a behaviour change dressed as a fix. The
/// explicit form overrides it on any symbol where it is wrong.
///
/// Dotted codes (`NASDAQ.NMS`, `ENEXT.BE`) are deliberately absent: this grammar splits on `.`, so
/// they cannot be written at all. See the module doc.
/// The rows, in the order they are written: the US venues, then Europe (`LSE` … `BVL`), then
/// Asia-Pacific and the Americas ex-US (`SEHK` onward). Region comments are deliberately NOT
/// interleaved — a trailing `//` inside an array is rustfmt-unstable, and this array is a table an
/// order path reads.
#[rustfmt::skip]
pub const EQUITY_EXCHANGES: &[&str] = &[
    "SMART", "NYSE", "NASDAQ", "ISLAND", "ARCA", "AMEX", "BATS", "IEX", "BEX", "PSX", "DRCTEDGE",
    "LSE", "LSEETF", "IBIS", "SBF", "AEB", "BVME", "EBS", "SWB", "FWB", "GETTEX", "TGATE", "VSE",
    "OMXNO", "SFB", "CPH", "HEX", "BVL",
    "SEHK", "TSEJ", "ASX", "TSE", "VENTURE", "MEXI", "NSE", "BSE",
];

/// IBKR's forex venue. The one exchange code this grammar recognises in EITHER of the two
/// positions the tree spells it in — see the module doc's ⚠ on the legacy field order.
const FOREX_EXCHANGE: &str = "IDEALPRO";

/// The secType codes an OPERATOR may claim in a canonical. `Other` is deliberately NOT reachable
/// from a canonical: [`SecType::from_ib_code`] answers `Other` for anything it does not know, and
/// forwarding an unrecognised code to the wire is a guess wearing a claim's clothes. `Other` stays
/// on the INBOUND side, where IBKR itself is the one saying the word.
const CLAIMABLE_SEC_TYPES: &[&str] = &["STK", "OPT", "FUT", "CASH", "IND", "CRYPTO"];

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SecType {
    Stk,
    Opt,
    Fut,
    Cash,
    Ind,
    Crypto,
    Other(String),
}

impl SecType {
    pub fn as_ib_code(&self) -> &str {
        match self {
            SecType::Stk => "STK",
            SecType::Opt => "OPT",
            SecType::Fut => "FUT",
            SecType::Cash => "CASH",
            SecType::Ind => "IND",
            SecType::Crypto => "CRYPTO",
            SecType::Other(s) => s.as_str(),
        }
    }

    /// This secType's [`AssetClass`], for [`contract_details_to_properties`] — IBKR's OWN word for
    /// what the contract IS, which is the whole of what
    /// `docs/decisions/0061-an-instrument-names-its-kind.md` asks a producer to carry. It is also
    /// the only honest source here: IBKR's symbology gives `AAPL` and `ESZ5` the same shape, so
    /// the ticker cannot answer, and this module's doc already records what a *better guess* off
    /// one cost (`ESZ5.GLOBEX.USD` submitted as `SecType::Stk`).
    ///
    /// - `STK` → [`AssetClass::Equity`], and NOT [`AssetClass::Etf`]: IBKR files ETFs under `STK`
    ///   too, so the venue makes no distinction for this parser to read. `SPY` answering `Equity`
    ///   is the venue's answer, not an approximation of one.
    /// - `CASH` → [`AssetClass::Fx`] — IB's word for a spot currency pair (`FOREX_EXCHANGE`,
    ///   IDEALPRO), not for cash equities.
    /// - `CRYPTO` → [`AssetClass::CryptoSpot`]: the PAXOS-routed spot coins are the only crypto
    ///   IBKR lists — it offers no perpetual, so `CryptoPerp` is unreachable from this venue.
    /// - [`SecType::Other`] → `other_asset_class`, which is a DECISION PER WORD rather than one
    ///   blanket `None`. It is still the INBOUND-only variant (`CLAIMABLE_SEC_TYPES` keeps an
    ///   operator from minting one), so the word inside it is always IBKR's own; two of those
    ///   words name a class this taxonomy already has, and the rest are argued absences.
    #[must_use]
    pub fn asset_class(&self) -> Option<AssetClass> {
        match self {
            SecType::Stk => Some(AssetClass::Equity),
            SecType::Opt => Some(AssetClass::Option),
            SecType::Fut => Some(AssetClass::Future),
            SecType::Cash => Some(AssetClass::Fx),
            SecType::Ind => Some(AssetClass::Index),
            SecType::Crypto => Some(AssetClass::CryptoSpot),
            SecType::Other(code) => SecType::other_asset_class(code),
        }
    }

    /// The class an inbound [`SecType::Other`] names — IBKR's REMAINING secType vocabulary, one
    /// written decision per word, reached only through [`SecType::asset_class`].
    ///
    /// The list this was decided against is `ibapi` 3.2.1's `SecurityType`
    /// (<https://docs.rs/ibapi/3.2.1/ibapi/contracts/enum.SecurityType.html>), whose `from` reads
    /// the wire words IBKR actually sends. ⚠ It is WIDER than the five this catch-all was first
    /// described by (`CFD`/`BOND`/`WAR`/`FOP`/`BAG`): `CONTFUT`, `CMDTY`, `NEWS` and `FUND` fall in
    /// here too, and a decision that stopped at five would have left four words looking undecided
    /// when they are not.
    ///
    /// ⚠ **Matched CASE-SENSITIVELY, on the word as the venue spells it, and deliberately NOT
    /// upper-cased first.** [`SecType::from_ib_code`]'s own arms are already case-sensitive — a
    /// lowercase `stk` lands in `Other` — so normalising HERE and nowhere else would make one type
    /// answer two ways about one defect: `cfd` would resolve a class while `stk` would not. IBKR
    /// sends these uppercase; a lowercase one is a caller bug and should keep looking like one.
    ///
    /// # The two that answer
    ///
    /// - `CFD` → [`AssetClass::Cfd`]. IB's own expansion of the code is *contract for difference*
    ///   and the taxonomy's variant is that instrument — one for one, nothing inferred, the same
    ///   act `STK` → `Equity` already is. This is the arm the residual that opened this work named.
    /// - `FOP` → [`AssetClass::Option`], an option ON a future. This is a TRUE statement made at
    ///   the only granularity the taxonomy has, not a guess: the enum carries no underlying axis
    ///   anywhere, so a listed index option (`OPT` on `SPX`) and a listed share option (`OPT` on
    ///   `AAPL`) already answer the SAME variant, and `FOP` joining them flattens nothing `OPT`
    ///   was not already flattening. ⚠ The honest cost, because the next reader will ask: a
    ///   consumer that filters on `Option` and assumes equity-option settlement now sees contracts
    ///   that settle into a FUTURES position. `None` does not avoid that cost — it makes the same
    ///   consumer MISS them in silence, and an absence is read back as "the producer never asked"
    ///   rather than "this is an option". If the taxonomy ever grows an underlying axis, this arm
    ///   is the site to revisit, and it is the only one here that a new variant would move.
    ///
    /// # The rest answer `None`, and that is a FINISHED answer rather than a TODO
    ///
    /// [`vike_model::SymbolProperties::asset_class`] is read back by consumers that cannot tell a
    /// guess from a fetch, so a near-miss variant is strictly worse than an honest absence.
    ///
    /// - `BOND`, `WAR`, `CMDTY` and `FUND` are instruments this taxonomy has NO word for. Each has
    ///   a tempting neighbour and every one of them is wrong: a warrant is an issuer-written
    ///   security rather than a listed `Option`, a mutual fund is not an `Etf`, spot metal is not
    ///   `Fx`, and a bond is not anything here at all. Giving them a variant is a TAXONOMY change
    ///   with its own argument, made in `crates/vike-model/src/instrument/asset_class.rs` where the list is
    ///   declared once — never smuggled in from a bridge.
    /// - `BAG` is refused for a DIFFERENT reason, and the difference is the whole of it: it is
    ///   IB's COMBO, a multi-leg spread, so it is not one instrument with a class. No variant
    ///   could ever be right for it — including one somebody adds later — because the LEGS have
    ///   classes and the bag does not. It is the one row here that a wider taxonomy does not fix.
    /// - `NEWS` is `BAG`'s case by a different road: a news-feed subscription contract is not a
    ///   tradable instrument in any sense, so there is nothing for a class to be about.
    /// - `CONTFUT` is a CONTINUOUS future — a stitched front-month SERIES with no expiry and no
    ///   order path. [`AssetClass::Future`] is the neighbour and this is the decision that came
    ///   closest to taking it; refused because the series is a market-data construct rather than a
    ///   contract, and tagging it `Future` would put a class on a thing no `FUT` order can name.
    fn other_asset_class(code: &str) -> Option<AssetClass> {
        match code {
            "CFD" => Some(AssetClass::Cfd),
            "FOP" => Some(AssetClass::Option),
            // Named rather than swept into `_`, so grepping the word IBKR sends finds the decision
            // instead of finding nothing. Their reasons differ per code and are on this doc.
            "BOND" | "WAR" | "CMDTY" | "FUND" | "BAG" | "NEWS" | "CONTFUT" => None,
            _ => None,
        }
    }

    pub fn from_ib_code(code: &str) -> SecType {
        match code {
            "STK" => SecType::Stk,
            "OPT" => SecType::Opt,
            "FUT" => SecType::Fut,
            "CASH" => SecType::Cash,
            "IND" => SecType::Ind,
            "CRYPTO" => SecType::Crypto,
            other => SecType::Other(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IbkrContract {
    pub sec_type: SecType,
    pub symbol: String,
    pub exchange: String,
    pub currency: String,
    pub expiry: Option<String>,     // YYYYMM | YYYYMMDD (OPT/FUT)
    pub strike: Option<f64>,        // OPT
    pub right: Option<char>,        // 'C' | 'P' (OPT)
    pub multiplier: Option<String>, // OPT/FUT
    pub con_id: Option<i64>,
}

impl IbkrContract {
    fn stk(symbol: &str, exchange: &str, currency: &str) -> Self {
        IbkrContract {
            sec_type: SecType::Stk,
            symbol: symbol.into(),
            exchange: exchange.into(),
            currency: currency.into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    }

    fn cash(symbol: &str, exchange: &str, currency: &str) -> Self {
        IbkrContract { sec_type: SecType::Cash, ..IbkrContract::stk(symbol, exchange, currency) }
    }

    /// True when this contract's identity depends on a field a `secdef/search?symbol=&secType=`
    /// query CANNOT express — today, exactly the expiry.
    ///
    /// ⚠ **This is an ORDER-PATH guard, not a hint.**
    /// `crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`'s `decode_conid`
    /// takes the FIRST row of the search result, and the search is keyed on symbol + secType
    /// alone. For an equity that first row is a listing choice; for a FUTURE it is a different
    /// EXPIRY — a wholly different contract with its own price, and the operator named the one
    /// they wanted. So the cpapi conId lanes REFUSE to resolve rather than pick, and the caller
    /// fails loudly (`conid: 0`, which the gateway itself rejects) instead of filling on a contract
    /// nobody asked for. The socket backend is unaffected: it sends
    /// symbol/secType/exchange/currency/expiry and lets IBKR resolve or answer error 200.
    pub fn conid_search_is_ambiguous(&self) -> bool {
        self.expiry.is_some()
    }
}

/// Parse a canonical vike symbol into the IBKR contract it names, or say why it could not.
///
/// The grammar, the refusal and the reasons behind both are in this module's doc. The one thing to
/// carry here: **there is no fall-through.** A canonical either matches a spelling that names
/// exactly one contract, or it is an `Err` whose string names the offending field and lists what
/// this venue accepts — because the caller is on an order path, and a default there is an order in
/// the wrong instrument.
pub fn parse_simplified(canonical: &str) -> Result<IbkrContract, String> {
    let parts: Vec<&str> = canonical.split('.').collect();
    if parts.len() < 3 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "IBKR symbol {canonical:?}: expected at least SYMBOL.EXCHANGE.CURRENCY with no empty \
             field (a symbol or exchange code containing a '.' cannot be written in this grammar)"
        ));
    }
    let (symbol, exchange, currency) = (parts[0], parts[1], parts[2]);
    if parts.len() == 3 {
        return parse_three_field(canonical, symbol, exchange, currency);
    }
    parse_claimed(canonical, &parts)
}

/// The THREE-FIELD form: forex on IDEALPRO (either field order), or a cash equity on an
/// allowlisted exchange. Anything else is refused — see the module doc for why the allowlist runs
/// this way round.
fn parse_three_field(
    canonical: &str,
    symbol: &str,
    exchange: &str,
    currency: &str,
) -> Result<IbkrContract, String> {
    // Forex, stated field order: `EUR.IDEALPRO.USD`.
    if exchange.eq_ignore_ascii_case(FOREX_EXCHANGE) {
        check_currency(canonical, currency)?;
        return Ok(IbkrContract::cash(symbol, exchange, currency));
    }
    // Forex, LEGACY field order: `EUR.USD.IDEALPRO` — the spelling this tree already writes. The
    // SAME contract as the arm above, reached by swapping the two trailing fields back.
    if currency.eq_ignore_ascii_case(FOREX_EXCHANGE) {
        check_currency(canonical, exchange)?;
        return Ok(IbkrContract::cash(symbol, currency, exchange));
    }
    if EQUITY_EXCHANGES.iter().any(|e| e.eq_ignore_ascii_case(exchange)) {
        check_currency(canonical, currency)?;
        return Ok(IbkrContract::stk(symbol, exchange, currency));
    }
    Err(format!(
        "IBKR symbol {canonical:?}: the three-field form SYMBOL.EXCHANGE.CURRENCY means a CASH \
         EQUITY, and {exchange:?} is not a known cash-equity exchange — it is refused rather than \
         defaulted, because reading a derivatives exchange as equity places the order in the wrong \
         instrument. Name the security type: SYMBOL.{exchange}.{currency}.STK|CASH|IND|CRYPTO, \
         SYMBOL.{exchange}.{currency}.FUT.YYYYMMDD[.MULTIPLIER], or \
         SYMBOL.{exchange}.{currency}.OPT.YYYYMMDD.STRIKE.C|P[.MULTIPLIER]"
    ))
}

/// The EXPLICIT form: the fourth field is the operator's secType claim, and the fields after it are
/// whatever that claim requires. No exchange allowlist applies — the claim has already said what
/// the contract is, which is the entire point of spelling it.
fn parse_claimed(canonical: &str, parts: &[&str]) -> Result<IbkrContract, String> {
    let (symbol, exchange, currency) = (parts[0], parts[1], parts[2]);
    let claim = parts[3].to_ascii_uppercase();
    if !CLAIMABLE_SEC_TYPES.contains(&claim.as_str()) {
        return Err(format!(
            "IBKR symbol {canonical:?}: {:?} is not a security type this venue accepts in a \
             canonical — one of {} (an unrecognised code is refused rather than forwarded, because \
             the wire would take it and mean something by it)",
            parts[3],
            CLAIMABLE_SEC_TYPES.join(" / ")
        ));
    }
    check_currency(canonical, currency)?;
    let sec_type = SecType::from_ib_code(&claim);
    let base = IbkrContract {
        sec_type: sec_type.clone(),
        ..IbkrContract::stk(symbol, exchange, currency)
    };
    match sec_type {
        // SYMBOL.EXCHANGE.CCY.FUT.EXPIRY[.MULTIPLIER]
        SecType::Fut => {
            if !(5..=6).contains(&parts.len()) {
                return Err(format!(
                    "IBKR symbol {canonical:?}: a FUT canonical is \
                     SYMBOL.EXCHANGE.CURRENCY.FUT.YYYYMMDD[.MULTIPLIER] — a futures contract \
                     without its expiry names a whole ladder of contracts rather than one, and the \
                     exchange's own month code (the Z5 in ESZ5) is NOT decoded because its \
                     single-digit year is ambiguous across decades"
                ));
            }
            Ok(IbkrContract {
                expiry: Some(check_expiry(canonical, parts[4])?),
                multiplier: check_multiplier(canonical, parts.get(5).copied())?,
                ..base
            })
        }
        // SYMBOL.EXCHANGE.CCY.OPT.EXPIRY.STRIKE.RIGHT[.MULTIPLIER]
        SecType::Opt => {
            if !(7..=8).contains(&parts.len()) {
                return Err(format!(
                    "IBKR symbol {canonical:?}: an OPT canonical is \
                     SYMBOL.EXCHANGE.CURRENCY.OPT.YYYYMMDD.STRIKE.C|P[.MULTIPLIER] — expiry, \
                     strike and right are all required, because any two of them name many contracts"
                ));
            }
            Ok(IbkrContract {
                expiry: Some(check_expiry(canonical, parts[4])?),
                strike: Some(check_strike(canonical, parts[5])?),
                right: Some(check_right(canonical, parts[6])?),
                multiplier: check_multiplier(canonical, parts.get(7).copied())?,
                ..base
            })
        }
        // STK / CASH / IND / CRYPTO need nothing beyond the claim itself, so a trailing field is a
        // field this parser would silently IGNORE — refused, since an ignored field is an operator
        // believing they said something.
        _ => {
            if parts.len() != 4 {
                return Err(format!(
                    "IBKR symbol {canonical:?}: a {claim} canonical takes no field after the \
                     security type (got {} extra) — only FUT and OPT do",
                    parts.len() - 4
                ));
            }
            Ok(base)
        }
    }
}

/// An ISO-4217 currency: exactly three ASCII letters. Cheap, and it is the check that catches a
/// canonical whose trailing fields were transposed (`AAPL.SMART.NASDAQ`) before the wire does.
fn check_currency(canonical: &str, currency: &str) -> Result<(), String> {
    if currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Ok(());
    }
    Err(format!(
        "IBKR symbol {canonical:?}: {currency:?} is not a three-letter currency code (IBKR names \
         the settlement currency, e.g. USD / EUR / GBP) — check the field order"
    ))
}

/// IBKR's `lastTradeDateOrContractMonth`: `YYYYMM` or `YYYYMMDD`, digits only, with a real month
/// (and day, when one is given). Nothing here guesses a century — see the module doc's ⚠.
fn check_expiry(canonical: &str, expiry: &str) -> Result<String, String> {
    let bad = || {
        format!(
            "IBKR symbol {canonical:?}: expiry {expiry:?} is not YYYYMM or YYYYMMDD. The \
             exchange's month code (Z5, H6, …) is deliberately NOT accepted: its single-digit year \
             names December 2025, 2035 and 2015 alike, and guessing which is how an order lands on \
             the wrong contract"
        )
    };
    if !matches!(expiry.len(), 6 | 8) || !expiry.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let month: u32 = expiry[4..6].parse().map_err(|_| bad())?;
    if !(1..=12).contains(&month) {
        return Err(bad());
    }
    if expiry.len() == 8 {
        let day: u32 = expiry[6..8].parse().map_err(|_| bad())?;
        if !(1..=31).contains(&day) {
            return Err(bad());
        }
    }
    Ok(expiry.to_string())
}

/// The option strike: a finite, strictly-positive decimal. A zero or negative strike is not a
/// contract, and a NaN would reach the wire as whatever the formatter made of it.
fn check_strike(canonical: &str, strike: &str) -> Result<f64, String> {
    match strike.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(format!(
            "IBKR symbol {canonical:?}: strike {strike:?} is not a positive decimal number"
        )),
    }
}

/// The option right, as IBKR spells it: `C`/`P` (or the words). Returned as the single uppercase
/// char `IbkrContract::right` carries.
fn check_right(canonical: &str, right: &str) -> Result<char, String> {
    match right.to_ascii_uppercase().as_str() {
        "C" | "CALL" => Ok('C'),
        "P" | "PUT" => Ok('P'),
        _ => Err(format!(
            "IBKR symbol {canonical:?}: option right {right:?} must be C / CALL or P / PUT"
        )),
    }
}

/// IBKR's `multiplier` is a NUMERIC STRING on the wire (`"50"`, `"100"`). Optional — omit it and
/// IBKR resolves the listed one; supply garbage and the contract silently stops matching.
fn check_multiplier(canonical: &str, multiplier: Option<&str>) -> Result<Option<String>, String> {
    let Some(m) = multiplier else { return Ok(None) };
    if !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Some(m.to_string()));
    }
    Err(format!(
        "IBKR symbol {canonical:?}: multiplier {m:?} must be a whole number of units (IBKR sends \
         it as a numeric string, e.g. 50 for the E-mini or 100 for a listed option)"
    ))
}

/// Bidirectional `conId ↔ canonical symbol` map (inbound events are keyed by numeric conId).
#[derive(Default)]
pub struct ConIdMap {
    by_con_id: HashMap<i64, String>,
    by_symbol: HashMap<String, i64>,
}

impl ConIdMap {
    pub fn insert(&mut self, con_id: i64, symbol: &str) {
        self.by_con_id.insert(con_id, symbol.to_string());
        self.by_symbol.insert(symbol.to_string(), con_id);
    }
    pub fn symbol_of(&self, con_id: i64) -> Option<&str> {
        self.by_con_id.get(&con_id).map(|s| s.as_str())
    }
    pub fn con_id_of(&self, symbol: &str) -> Option<i64> {
        self.by_symbol.get(symbol).copied()
    }
}

/// Map IB `contractDetails` fields to vike `SymbolProperties` for the live-`RiskGate` pre-fetch
/// (`vike_exec::RiskLimits::from_properties`, applied by `vike-mount`'s fold to the grid
/// `IbkrVenueMount`'s mount pre-fetches through `crate::fetch_ibkr_properties`, which exists under
/// the `ibkr-socket` feature only).
///
/// - `min_tick` → `tick_size`: already in display units; IB's `priceMagnifier` (bonds) does NOT
///   scale it, so the tick stays `min_tick` verbatim.
/// - `size_increment` → `step_size`, `min_size` → `min_qty`: the order-size grid IB publishes on
///   the same `contractDetails` reply. Older TWS builds may report these as `0.0` ("not sent"),
///   which `RiskLimits::from_properties` folds to unconstrained via `nz_step` — the inert case,
///   byte-identical to a venue that reports no grid.
///
/// `contract_size` stays `0.0` (= the inert `1.0` multiplier): IBKR DOES publish a per-contract
/// multiplier for OPT/FUT, but folding it into the `Account` multiplier grid is the deribit-path
/// follow-up (out of scope for the RiskGate tick/size grid this builds).
///
/// `sec_type` carries the class (`docs/decisions/0061-an-instrument-names-its-kind.md`) and is the
/// reason this takes a fourth argument rather than three loose numbers: the `contractDetails`
/// reply's grid says nothing about WHAT it is a grid for, while the contract the caller asked
/// about does. [`SecType::asset_class`] is the mapping, including which words answer nothing.
pub fn contract_details_to_properties(
    sec_type: &SecType,
    min_tick: f64,
    size_increment: f64,
    min_size: f64,
) -> SymbolProperties {
    SymbolProperties {
        tick_size: min_tick,
        step_size: size_increment,
        min_qty: min_size,
        asset_class: sec_type.asset_class(),
        // Everything else absent: no per-contract max/min-notional, no contract multiplier, a
        // flat grid (`contractDetails` reports ONE `minTick`, no tiers), and no venue taker hold.
        // FRU rather than an exhaustive literal so a new `SymbolProperties` field costs this
        // parser nothing.
        ..Default::default()
    }
}

#[path = "contract_tests.rs"]
#[cfg(test)]
mod contract_tests;
