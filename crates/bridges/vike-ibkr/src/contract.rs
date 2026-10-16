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
//! tree already starts.
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
//! # The typed contract (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`)
//!
//! [`IbkrContract::to_contract`] is the ONE mapping from IBKR's secType-tagged contract to
//! [`vike_model::Contract`], and [`SecType::asset_class`] is its projection: the two must agree for
//! every secType that maps (`contract_tests.rs` pins it). It reads only the contract's own fields —
//! never the symbol text — and answers `None` rather than invent a field the contract does not
//! carry (a month-precision expiry, a combo's legs, a continuous future). IBKR publishes no
//! catalog, so nothing here builds a `vike_model::Instrument`; the engine symbol's grid is
//! [`contract_details_to_properties`].
//!
//! A `conId ↔ symbol` bidirectional map lets inbound events (keyed by numeric conId) map back to
//! the canonical symbol.

use std::collections::HashMap;
use vike_marketdata::OptionKind;
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, Delivery, EquitySpec,
    ExerciseStyle, FutureFamily, FutureSpec, FxSpec, Margining, OptionSpec, SymbolProperties,
};
use vike_util::calendar::{civil_from_days, days_from_civil};

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
/// `AAPL.SMART.USD` is the symbol `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` ibkr row starts and the one
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
    /// what the contract IS. It is the only honest source here: IBKR's symbology gives `AAPL` and
    /// `ESZ5` the same shape, so the ticker cannot answer, and this module's doc already records
    /// what a *better guess* off one cost (`ESZ5.GLOBEX.USD` submitted as `SecType::Stk`).
    ///
    /// ⚠ It is the PROJECTION of [`IbkrContract::to_contract`]: wherever that answers a
    /// [`Contract`], `Contract::class()` equals this, for every secType
    /// (`contract_tests.rs`'s `asset_class_is_the_class_of_the_typed_contract`). It answers even
    /// where `to_contract` cannot (a month-precision future, a `BAG` without legs): the secType
    /// alone names the class, while the contract also needs fields the canonical may not carry.
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
    ///   operator from minting one), so the word inside it is always IBKR's own.
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
    /// # The words that answer — each is IB's own word for a class the taxonomy has
    ///
    /// - `CFD` → [`AssetClass::Cfd`]: IB's expansion of the code is *contract for difference*.
    /// - `FOP` → [`AssetClass::Option`], an option ON a future. ⚠ A consumer that filters on
    ///   `Option` and assumes equity-option settlement sees contracts that settle into a FUTURES
    ///   position; the typed [`OptionSpec::underlying`] (the future's root) is what tells them
    ///   apart, not the class.
    /// - `BOND` → [`AssetClass::Bond`], `WAR` → [`AssetClass::Warrant`] (an issuer-written
    ///   security, NOT a listed `Option`), `CMDTY` → [`AssetClass::Commodity`] (spot metal, NOT
    ///   `Fx`), `FUND` → [`AssetClass::Fund`] (a mutual fund, NOT an `Etf`). Decision 0126 added
    ///   these classes; until then each answered `None` rather than its tempting neighbour.
    /// - `BAG` → [`AssetClass::Combo`]: IB's COMBO is a multi-leg instrument, which 0126 made a
    ///   class. Its typed contract is NOT built (`to_contract` answers `None`): the legs, and so
    ///   whether it is an option combo or a futures spread, are not on [`IbkrContract`].
    ///
    /// # The rest answer `None`, and that is a FINISHED answer rather than a TODO
    ///
    /// - `NEWS`: a news-feed subscription contract is not a tradable instrument in any sense.
    /// - `CONTFUT` is a CONTINUOUS future — a stitched front-month SERIES with no expiry and no
    ///   order path. [`AssetClass::Future`] is the neighbour and is refused: the series is a
    ///   market-data construct rather than a contract, and a `Future` contract needs an expiry the
    ///   series does not have.
    fn other_asset_class(code: &str) -> Option<AssetClass> {
        match code {
            "CFD" => Some(AssetClass::Cfd),
            "FOP" => Some(AssetClass::Option),
            "BOND" => Some(AssetClass::Bond),
            "WAR" => Some(AssetClass::Warrant),
            "CMDTY" => Some(AssetClass::Commodity),
            "FUND" => Some(AssetClass::Fund),
            "BAG" => Some(AssetClass::Combo),
            // Named rather than swept into `_`, so grepping the word IBKR sends finds the decision
            // instead of finding nothing. Their reasons are on this doc.
            "NEWS" | "CONTFUT" => None,
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
    pub right: Option<OptionKind>,  // OPT
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

    /// The typed [`Contract`] this IBKR contract IS, read from its own fields — or `None` when the
    /// contract lacks a field the class needs. Pure; no network. [`SecType::asset_class`] is its
    /// projection.
    ///
    /// - `STK` → `Equity`: `exchange` is the routing exchange, except that `SMART` (IB's router,
    ///   not a listing — see [`EQUITY_EXCHANGES`]) answers `None`; `currency` the contract's.
    ///   Never `Etf`: IBKR files ETFs under `STK` too.
    /// - `OPT` and `FOP` → `Option`: right, strike and a DAY-precision expiry are all required, else
    ///   `None`. `underlying` is IB's `symbol` (the share or the future's root), `settle` and
    ///   `value_ccy` the currency, `margining` `Linear` (premium and payoff in the quote currency).
    ///   `style` is `Unknown`: US equity options are American, but the contract does not say so.
    /// - `FUT` → `Future`: a DAY-precision expiry is required, else `None`; `Linear`, settled in
    ///   the currency, `family` `Traditional`; `delivery` `Unknown` and `tenor` `None` (the contract
    ///   carries neither). The
    ///   `multiplier` is not a contract field here: it is the grid's `contract_size`
    ///   ([`contract_details_to_properties`]).
    /// - `CASH` → `Fx` (pip, lot and swaps unknown: the contract carries none), `IND` → `Index`,
    ///   `CRYPTO` → `CryptoSpot`, `CFD` → `Cfd` with every unknown left `Other`/`Unknown`/`None`,
    ///   `BOND`/`WAR`/`CMDTY`/`FUND` → `Bond`/`Warrant`/`Commodity`/`Fund`.
    /// - `None` for `BAG` (the legs, and so the combo kind, are not on this struct), `CONTFUT` (a
    ///   series with no expiry, not a contract), `NEWS`, and any word IB has not been decided for.
    ///
    /// ⚠ **A `YYYYMM` expiry answers `None`.** IBKR's `lastTradeDateOrContractMonth` names a
    /// contract MONTH in that form, not the last trading day, and the day differs per product (the
    /// third Friday, the business day before the 25th, …); a month-end or first-of-month convention
    /// would be a fabricated date on a typed field consumers read as fact. A `YYYYMMDD` expiry is
    /// that day at 00:00 UTC: the exchange-local last-trade TIME (`lastTradeTime`, `timeZoneId` on
    /// `contractDetails`) is not on this struct, so the instant is day-precise only.
    #[must_use]
    pub fn to_contract(&self) -> Option<Contract> {
        let ccy = &self.currency;
        match &self.sec_type {
            SecType::Stk => Some(Contract::Equity(EquitySpec {
                exchange: (!self.exchange.eq_ignore_ascii_case("SMART"))
                    .then(|| self.exchange.clone()),
                currency: Some(ccy.clone()),
            })),
            SecType::Opt => self.option_contract(),
            SecType::Fut => Some(Contract::Future(FutureSpec {
                margining: Margining::Linear,
                settle: ccy.clone(),
                value_ccy: Some(ccy.clone()),
                expiry_ms: expiry_day_ms(self.expiry.as_deref()?)?,
                tenor: None,
                delivery: Delivery::Unknown,
                family: FutureFamily::Traditional,
            })),
            SecType::Cash => Some(Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            })),
            SecType::Ind => Some(Contract::Index),
            SecType::Crypto => Some(Contract::CryptoSpot),
            SecType::Other(code) => match code.as_str() {
                "CFD" => Some(Contract::Cfd(CfdSpec {
                    underlying_kind: CfdUnderlying::Other,
                    underlying: None,
                    product: CfdProduct::Cfd,
                    expiry: CfdExpiry::Unknown,
                    margin_rate: None,
                    swap_long: None,
                    swap_short: None,
                })),
                "FOP" => self.option_contract(),
                "BOND" => Some(Contract::Bond),
                "WAR" => Some(Contract::Warrant),
                "CMDTY" => Some(Contract::Commodity),
                "FUND" => Some(Contract::Fund),
                // Named, with their reasons on this fn's doc.
                "BAG" | "CONTFUT" | "NEWS" => None,
                _ => None,
            },
        }
    }

    /// `OPT`/`FOP`'s [`Contract::Option`], or `None` when right, strike or a day-precision expiry
    /// is missing — see [`IbkrContract::to_contract`].
    fn option_contract(&self) -> Option<Contract> {
        let right = self.right?;
        let strike = self.strike.filter(|s| s.is_finite() && *s > 0.0)?;
        Some(Contract::Option(OptionSpec {
            right,
            strike,
            expiry_ms: expiry_day_ms(self.expiry.as_deref()?)?,
            style: ExerciseStyle::Unknown,
            margining: Margining::Linear,
            settle: self.currency.clone(),
            value_ccy: Some(self.currency.clone()),
            underlying: Some(self.symbol.clone()),
        }))
    }
}

/// A `YYYYMMDD` `lastTradeDateOrContractMonth` as that day's 00:00 UTC in epoch ms, or `None` for
/// anything else — a `YYYYMM` month included, deliberately (see [`IbkrContract::to_contract`]).
/// A day the month does not have (`20250231`) is `None`: [`check_expiry`] only bounds the day by
/// 31, so the calendar round-trip is the check.
fn expiry_day_ms(expiry: &str) -> Option<i64> {
    if expiry.len() != 8 || !expiry.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = expiry[0..4].parse().ok()?;
    let m: u32 = expiry[4..6].parse().ok()?;
    let d: u32 = expiry[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    (civil_from_days(days) == (y, m, d)).then_some(days * 86_400_000)
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

/// The option right, as IBKR spells it: `C`/`P` (or the words), case-insensitive, read into the
/// [`OptionKind`] `IbkrContract::right` carries.
fn check_right(canonical: &str, right: &str) -> Result<OptionKind, String> {
    match right.to_ascii_uppercase().as_str() {
        "C" | "CALL" => Ok(OptionKind::Call),
        "P" | "PUT" => Ok(OptionKind::Put),
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
/// (`vike_model::RiskLimits::from_properties`, applied by `vike-tradehub-core`'s fold to the grid
/// `IbkrVenueAdapter`'s start pre-fetches through `crate::fetch_ibkr_properties`, which exists under
/// the `ibkr-socket` feature only).
///
/// - `min_tick` → `tick_size`: already in display units; IB's `priceMagnifier` (bonds) does NOT
///   scale it, so the tick stays `min_tick` verbatim.
/// - `size_increment` → `step_size`, `min_size` → `min_qty`: the order-size grid IB publishes on
///   the same `contractDetails` reply. Older TWS builds may report these as `0.0` ("not sent"),
///   which `RiskLimits::from_properties` folds to unconstrained via `nz_step` — the inert case,
///   byte-identical to a venue that reports no grid.
///
/// - `multiplier` → `contract_size`, already resolved by [`resolve_multiplier`] (`0.0` = unknown,
///   the struct's absent convention, which `SymbolProperties::multiplier` folds to the inert
///   `1.0`). It used to stay `0.0` because the `Account` multiplier grid it feeds was the
///   deribit-path follow-up; that seam now exists (`vike_bridge_core`'s `LiveExec::contract_size`,
///   which the start fills from this grid), so an E-mini's `50` reaches the notional.
///
/// `sec_type` carries the class: the `contractDetails` reply's grid says nothing about WHAT it is
/// a grid for, while the contract the caller asked about does. [`SecType::asset_class`] is the
/// mapping, including which words answer nothing.
pub fn contract_details_to_properties(
    sec_type: &SecType,
    min_tick: f64,
    size_increment: f64,
    min_size: f64,
    multiplier: f64,
) -> SymbolProperties {
    SymbolProperties {
        tick_size: min_tick,
        step_size: size_increment,
        min_qty: min_size,
        contract_size: multiplier,
        asset_class: sec_type.asset_class(),
        // Everything else absent: no per-contract max/min-notional, a flat grid
        // (`contractDetails` reports ONE `minTick`, no tiers), and no venue taker hold.
        // FRU rather than an exhaustive literal so a new `SymbolProperties` field costs this
        // parser nothing.
        ..Default::default()
    }
}

/// The per-contract multiplier for [`contract_details_to_properties`], or `0.0` (unknown) — from
/// the operator's CLAIM when the canonical spells one, else from the `contractDetails` reply only
/// when EVERY row of it names the same positive number.
///
/// ⚠ **The reply is a LADDER, not one contract.** `crate::fetch_ibkr_properties` asks by symbol,
/// secType, exchange and currency (the market-data contract carries no expiry or multiplier), so a
/// future answers one row per expiry, and one symbol can list several sizes (EUREX's `DAX`: the
/// 25, 5 and 1 contracts). Taking the first row's multiplier would book a position at another
/// contract's size; a disagreement therefore answers `0.0`, which is the pre-existing inert `1.0`.
/// A claimed multiplier wins because it is what the order path sends
/// (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `build_contract`), so IBKR either trades
/// exactly that or refuses. An empty or non-numeric
/// field (IB sends `""` for a share) is "the row says nothing" and also answers `0.0`.
#[must_use]
pub fn resolve_multiplier<'a>(
    claimed: Option<&str>,
    reply: impl IntoIterator<Item = &'a str>,
) -> f64 {
    let positive = |s: &str| s.trim().parse::<f64>().ok().filter(|m| m.is_finite() && *m > 0.0);
    if let Some(m) = claimed.and_then(positive) {
        return m;
    }
    let mut agreed: Option<f64> = None;
    for row in reply {
        match (positive(row), agreed) {
            (None, _) => return 0.0,
            (Some(m), Some(a)) if m != a => return 0.0,
            (Some(m), _) => agreed = Some(m),
        }
    }
    agreed.unwrap_or(0.0)
}

#[path = "contract_tests.rs"]
#[cfg(test)]
mod contract_tests;
