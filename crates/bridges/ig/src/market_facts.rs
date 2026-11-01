//! IG's DEALING FACTS of one epic, for the control door's any-symbol admission (decision 0174).
//!
//! The engine can judge an order on an epic it was not started on only if it is told the epic's tick, its
//! minimum deal size and what ONE unit of `size` is worth. IG publishes none of it in a list: it is
//! `GET /markets/{epic}` on an authenticated session, and the account's currency is `GET /accounts`.
//! [`IgFactsSource`] reads both over ONE logged-in session it keeps (a 401 re-logs it in,
//! `crate::rest`'s ladder) and [`market_facts`] turns the pair into facts, or into a REFUSAL by name.
//!
//! # What is admitted, and why only that
//!
//! **An epic of a MEASURED type ([`MEASURED_TYPES`]: CURRENCIES, INDICES, COMMODITIES, RATES) quoted in the
//! account's currency, on an account whose currency is USD** (for a node that does not convert; a node that does is
//! answered for any account currency, below). What `size` MEANS differs per epic class and was MEASURED by
//! minimum deals on the demo gateway (2026-10-28 and 2026-10-30; `crates/bridges/ig/CLAUDE.md`): a count of contracts,
//! each worth `contractSize` of the epic's money per level, on a currency pair, an index, a commodity, a coin and a bond
//! future, dated or not. Every other type word (a share, an option, a binary) is refused by name ([`class_refusal`]).
//! IG's `type` word is a poor class (it types Bitcoin and spot gold CURRENCIES), so an epic is admitted only when the
//! payload agrees with itself:
//!
//! * the **contract multiplier** is `contractSize`, and it is accepted only if it equals `valueOfOnePip` over the level
//!   grid (`contractSize x pip` for a currency pair; `valueOfOnePip x scalingFactor` for every other epic, IG quoting
//!   levels in pips): two independent fields of the one payload, in the epic's own money. A market whose size
//!   semantics are not "contracts of `contractSize`" fails this and is refused ([`ig_size_semantics_refusal`] words it);
//! * the **price grid** is the PIP, never a finer step: `onePipMeans` for a pair, `1 / scalingFactor` for an epic that
//!   is no pair (its `onePipMeans` is words: '1 Index Point'). A level on the pip grid is on every finer grid IG may
//!   use, and it is what [`crate::exec`]'s five-decimal `fmt_level` writes exactly. A pip that is not a whole multiple
//!   of 0.00001 is refused;
//! * the **size grid** is `minDealSize` as BOTH minimum and step (a multiple of the minimum is valid
//!   whatever the finer step is; the venue publishes no step); `lotSize` is no fence (it equals `contractSize` on an
//!   index and plays no part: measured on USDJPY Mini, whose lot is 100);
//! * the **money**: the quote currency of a pair is named by the pair; the money of every other epic is its ONE listed
//!   dealing currency ([`sole_currency`]). This adapter writes `currencyCode: "USD"` whenever the epic lists it (the
//!   epic's own listed currency otherwise, `crate::exec::order_currency`) and the epic's own `expiry` (`-`, or a dated
//!   contract's word: [`expiry_refusal`], `crate::exec::build_request_expiring`), so a spread bet (`DFB`, refused by IG
//!   for a CFD login) is refused, and so is a non-USD account on the plain door.
//!   The engine's ledger is one currency and
//!   [`market_facts`] converts no quote currency (decision 0170), so an epic not quoted in the account's
//!   currency is refused by it: its PnL would be mis-valued, and overstated PnL feeds the margin-call sweep.
//!   A node that converts (a `demo` account with an exposure ceiling, decision 0183) is answered by
//!   [`quoted_market_facts`] instead, which admits such an epic only with the rate it checked against a price.
//!
//! * the **margin rate** is `instrument.marginFactor` as a fraction (3.33 with `marginFactorUnit` `PERCENTAGE` is
//!   0.0333) in [`IgMarketFacts::margin_rate`], which the node writes as the epic's own `im_requirement`; an epic
//!   whose factor is absent, in another unit or not a rate is margined at the engine's own rate, as before. It is
//!   the BASE factor: it equalled the margin IG booked on every minimum deal measured, but the body also carries
//!   `marginDepositBands` (a rate that rises with the SIZE), which are not read, so a large order is margined too low.
//!
//! Not carried, and owed a demo proof: a maximum deal size, a minimum notional.
//!
//! The market-details shape is CAPTURED (`tests/fixtures/markets/`, read by `market_facts_tests.rs`). A field that is
//! absent or unreadable is a refusal, never a default.
//!
//! # A quote currency other than the account's (decision 0183)
//!
//! [`market_facts`] keeps the fence above: it is what a node that does not convert is answered. A node that
//! DOES (`IgFactsSource::facts_quoted`, [`quoted_market_facts`]) is answered an epic quoted in another currency
//! with its quote currency and a checked rate (`crate::quote_rate`) instead of the refusal, and only then: the
//! account's currency read from `GET /accounts`, the epic's quote currency from `onePipMeans`, and a rate that
//! agrees with a price are ALL required, or the epic is refused in words. The account may be in ANY currency
//! (a pound or euro CFD account: the node's ledger is then the account's own money and the mounted EUR/USD epic
//! is one more epic quoted in another currency, rated like the rest; the answer states the account's currency,
//! `IgQuotedFacts::account_ccy`). Every other fence stands: a spread bet, a share, an option and a non-CFD account are
//! still refused, so the conversion opens every measured epic on a CFD account and nothing else (an epic that is no
//! currency pair, an index in euros, is witnessed by IG's EUR/USD Mini: `crate::quote_rate`'s `fx_mini_pairs`). The
//! `valueOfOnePip` cross-check also accepts the figure in the ACCOUNT's currency (`contractSize x pip x rate`),
//! because IG's reference does not say which currency that field is in.
//!
//! One fence is relaxed for a CONVERTED epic only: "lists no USD among its dealing currencies" (two public
//! captures list USDJPY and USDCHF Mini with their quote currency ALONE). Such an epic is admitted on the rate the
//! price route gives (`crate::quote_rate`'s `Reading::PriceOnly`), and an order on it is written in the
//! currency its table lists (`crate::exec::order_currency`: EURGBP lists GBP alone, measured on the demo
//! gateway), no longer `USD`, which IG would have refused as not tradeable in that currency. An epic quoted
//! in the account's own currency still has to list it.
//!
//! # What the source spends of IG's allowance
//!
//! Every HTTP request this source makes (the login, `GET /operations/application`, `GET /accounts`, every market
//! read) is counted against a per-minute cap, [`MAX_READS_PER_MINUTE`] or two thirds of the key's own
//! `allowanceAccountOverall` once read; a first look at an epic is held to half of it so the re-read of an epic
//! that has answered with a rate keeps the rest (`ReadBudget`). A login IG refuses for good, and an account row
//! that is missing or states no currency, are remembered ([`IgFactsError::Account`]) and not asked again for five
//! minutes. One lookup has one deadline ([`FACTS_LOOKUP_WAIT`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::json::get_f64;
use vike_model::{ContractValue, Instrument, SymbolProperties};

use crate::config::IgConfig;
use crate::quote_rate::{self, QuoteRate, RATE_TOLERANCE};
use crate::rest::{IgSession, STATUS_ACCOUNT_REFUSED};

/// **This adapter's own cap on the HTTP reads its facts make of IG for one account, per minute, when IG's own
/// allowance could not be read** (`POST /session`, `GET /operations/application`, `GET /accounts`, and `GET
/// /markets/{epic}` for an epic and for a conversion pair). IG meters non-trading requests per account and per
/// API key, and the exec and reconcile lanes spend from the same account allowance with reads of their own.
///
/// ⚠ **UNMEASURED.** 20 is two thirds of the 30 a minute per account that IG's FAQ lists as the DEFAULT
/// non-trading allowance (60 a minute per application; 100 trading), a figure of this adapter's choosing: no
/// live demo run has read the key's real numbers back, which one `GET /operations/application` does (the
/// converting path reads it once per source and sets the cap to two thirds of `allowanceAccountOverall`,
/// `allowance_cap`; this constant is what is used when that read fails). Which IG bucket each endpoint falls
/// into is not documented either. A cross costs two or three reads, the daemon admits 12 fetches a minute, and a
/// refresh storm hits this cap (a failed read the node retries) before it hits IG's.
pub const MAX_READS_PER_MINUTE: usize = 20;

/// The least and most reads a minute [`allowance_cap`] will set: a key whose allowance reads absurdly small or
/// large does not turn the cap off or into nothing.
const MIN_READS_PER_MINUTE: usize = 4;
const MAX_READS_CEILING: usize = 200;

/// The window the cap is counted over.
const READ_WINDOW: Duration = Duration::from_secs(60);

/// **The share of the per-minute read cap only a REFRESH may spend**: half. The daemon keeps 6 of its 12 fetches
/// a minute for the re-read of an epic that has already answered with a rate (`RATE_REFRESH_RESERVE` of
/// `vike-gatewayhub`'s `ig_facts`), but a fetch is not a read: a first look at a cross in a new quote currency
/// costs two or three reads (the epic, the conversion pair's spellings), against the cap below that is ONE
/// bucket, so a minute of first looks left ~4 reads for the 6 reserved refreshes, a refresh that failed was
/// `NotNow` for 5 s and retried at the next 60 s tick, and a held position's rate could sit at 120 s with
/// opening orders denied. So the reserve is made real HERE, where the reads are counted: a first look may
/// spend `cap - cap / 2` reads in a window, a refresh the whole cap ([`ReadBudget`]).
const REFRESH_RESERVE_DIVISOR: usize = 2;

/// How long a conversion pair's market details are shared between the crosses that need them: a refresh round
/// over several held crosses quoted in one currency (EURJPY, GBPJPY, AUDJPY all witnessed by USDJPY) reads the
/// pair once. A witness only CHECKS a stated rate to 2%, so a body this old is as good as a fresh one.
const CONVERSION_PAIR_KEEP: Duration = Duration::from_secs(10);

/// How long a conversion pair spelling is remembered as "no such epic" when IG's answer SAID so: its `errorCode` is
/// one that names a missing epic ([`says_no_such_epic`]). `{quote}{account}` (`JPYUSD`) does not exist at IG for
/// most currencies, so without this memory every refresh of a yen/franc/dollar-quoted cross spent a call on a
/// certain 404 before the call that works. An hour, not forever.
const MISSING_SPELLING_KEEP: Duration = Duration::from_secs(3600);

/// How long a 400/404 that does NOT say the epic is missing (a generic `invalid.input`, a gateway's bare 404, an
/// `errorCode` this adapter has never seen) is remembered for a conversion pair spelling. It used to be an hour
/// for any 400/404, so ONE hiccup on `CS.D.GBPUSD.MINI.IP` made every pound-quoted cross `NoRate` for an hour,
/// against an engine that denies opening orders at 120 s. Five minutes, and a successful read of the same name
/// forgets it at once ([`ConversionMemory`]).
const UNCODED_MISSING_KEEP: Duration = Duration::from_secs(300);

/// How long [`IgFactsSource::account_currency_within`] is waited on by the start path, which holds the
/// session's adapter lock meanwhile. The login and `GET /accounts` share an agent with a 30 s timeout, so
/// without a bound a slow IG held the start for a minute.
pub const ACCOUNT_CURRENCY_WAIT: Duration = Duration::from_secs(8);

/// How long [`IgFactsSource::facts_quoted_within`] and [`IgFactsSource::facts_within`] wait for ONE lookup. A cross
/// is up to four serial HTTP calls (login, `GET /accounts`, the epic, the conversion pair) at 30 s each on the
/// shared agent, and the daemon admits four lookups at once for ALL venues: four hung IG lookups held every slot
/// for ~120 s and every other venue's lookup was answered `busy`. A lookup that is over this bound is "not now" and
/// frees its slot; the read goes on in the background and its kept login and account row serve the next ask.
pub const FACTS_LOOKUP_WAIT: Duration = Duration::from_secs(15);

/// How long a login or account failure that only a CHANGE can cure (the credentials are refused, the account
/// row is missing or states no currency) is remembered by the source, so a caller that keeps asking does not make
/// a login a second (IG may lock an account on repeated failed logins: unverified). A rotated credential is a
/// new source ([`IgFactsSource::serves`]), so a fix is picked up at once.
const PERMANENT_FAILURE_KEEP: Duration = Duration::from_secs(300);

/// IG's `errorCode` words that name an epic that does not exist: the documented one on `GET /markets/{epic}`
/// (`404 error.public-api.epic-not-found`, labs.ig.com/reference/markets-epic.html) and the older spelling this
/// crate's tests were written against (`error.service.marketdata.instrument.epic.unavailable`; hand-written, never
/// captured). Matched as a SUBSTRING of the error message, which is IG's `errorCode` when it sent one.
const EPIC_MISSING_CODES: [&str; 2] =
    ["error.public-api.epic-not-found", "instrument.epic.unavailable"];

/// Whether an error message (IG's `errorCode` when present, `crate::rest`'s `IgApiError`) says the epic is not one.
fn says_no_such_epic(message: &str) -> bool {
    EPIC_MISSING_CODES.iter().any(|code| message.contains(code))
}

/// The `currencyCode` [`crate::exec::build_request`] writes whenever the epic's table lists it
/// (`crate::exec::order_currency`) and the fallback when the table cannot be read; every order was
/// written this, whatever the epic, before the table was read. The facts still admit only an epic this
/// value is right for (a USD account, an epic quoted in the account's currency that lists it).
pub const WIRE_CURRENCY: &str = "USD";

/// The `expiry` every order body carries (cash/daily only: this bridge wires no dated epic).
pub(crate) const WIRE_EXPIRY: &str = "-";

/// The finest price step `fmt_level` can write (it writes five decimals), so the finest an admitted epic's
/// pip may be.
pub(crate) const WIRE_PRICE_STEP: f64 = 0.00001;

/// The facts of one admitted epic: the grid in the engine's units (size in IG contracts) and what one unit
/// of size is worth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IgMarketFacts {
    /// Tick = the pip, step = minimum = `minDealSize`; no maximum, no minimum notional, no contract size
    /// (the multiplier rides [`Self::contract_value`], so it is not scaled twice).
    pub properties: SymbolProperties,
    /// Linear, `contractSize` units of the base currency per unit of size.
    pub contract_value: ContractValue,
    /// The epic's INITIAL-MARGIN rate as a fraction of notional (IG's `instrument.marginFactor` of 3.33 with
    /// `marginFactorUnit` `PERCENTAGE` is `0.0333`), for the engine to margin the epic at
    /// (`vike_exec::SymbolAdmission::im_requirement`). `None` when IG states none, states it in another unit
    /// (`POINTS`), or states a figure outside `(0, 100]` percent: the epic is then margined at the engine's own rate,
    /// as it always was. ⚠ This is the BASE factor only (the margin deposit bands, a higher rate for larger sizes,
    /// are not read) and IG's reference is its only source: it has never been measured against a booked margin.
    pub margin_rate: Option<f64>,
}

/// [`IgMarketFacts`] for a node that converts a quote currency: the facts, and the rate that converts the
/// epic's quote currency when it is not the account's.
#[derive(Debug, Clone, PartialEq)]
pub struct IgQuotedFacts {
    /// The grid and the unit, exactly as [`market_facts`] states them.
    pub facts: IgMarketFacts,
    /// `Some` exactly when the epic is quoted in a currency other than the account's; `None` = quoted in
    /// the account's own, which needs no rate.
    pub quote: Option<QuoteRate>,
    /// The account's currency (upper case, three letters): the ledger the answer is expressed in. The gateway
    /// stamps it on the wire so the node can refuse an answer for an engine anchored to another currency (the
    /// start's read missed and the node assumed USD, while this later read states the account's real money).
    pub account_ccy: String,
}

/// The epic's pair as `onePipMeans` states it (`"0.0001 USD/EUR"`: the pip, the QUOTE, the BASE).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pair {
    pub(crate) quote: String,
    pub(crate) base: String,
}

/// The `instrument.type` words whose `size` was MEASURED on the demo gateway (2026-10-28 and 2026-10-30): a minimum
/// deal of each moved the margin by `size x contractSize x level x marginFactor` and the balance by `size x
/// contractSize x the level move`, in the epic's own currency. IG's `type` is a poor class word (it types Bitcoin
/// and Spot Gold CURRENCIES), so this list admits a TYPE only to the payload checks of [`facts_with`], never alone.
const MEASURED_TYPES: [&str; 4] = ["CURRENCIES", "INDICES", "COMMODITIES", "RATES"];

/// Why an epic of `class` (`instrument.type`) is refused, or `None` for a class whose size was measured
/// ([`MEASURED_TYPES`]): every other word is refused BY NAME with what is measured of it and what is owed.
fn class_refusal(epic: &str, class: &str) -> Option<String> {
    if MEASURED_TYPES.contains(&class) {
        return None;
    }
    let what = match class {
        "SHARES" => {
            "a share CFD pays a MINIMUM COMMISSION (measured on the demo gateway: 15.00 USD at each of the open and the \
             close of one Apple share), which this client reads from nowhere (every mapper books commission 0), \
             and IG streams no price for it on this key (`streamingPricesAvailable` is false), so an order \
             would wait for a price that never comes and the fee ledger would be wrong"
        }
        c if c.starts_with("OPT_") || c == "UNKNOWN" => {
            "an option is priced as a premium and its payoff is not linear in the level (IG types its listed \
             options UNKNOWN or OPT_*), so `size x contractSize x level` is not what it is worth: not measured, \
             not admitted"
        }
        "BINARY" | "SPRINT_MARKET" | "BUNGEE_CAPPED" | "BUNGEE_COMMODITIES"
        | "BUNGEE_CURRENCIES" | "BUNGEE_INDICES" => {
            "a binary, sprint or bungee market is not a linear position"
        }
        _ => "its type word is not one whose size was measured",
    };
    Some(format!(
        "ig: {epic} is a {class} market and {what}; admitted types are {}",
        MEASURED_TYPES.join(", ")
    ))
}

/// Why an epic with this `instrument.expiry` is refused, or `None` for an undated cash CFD (`-`) and for a DATED one
/// (a month word such as `DEC-26`, a day such as `30-NOV-26`: measured on the demo gateway on 2026-10-30 as
/// `IX.D.DAX.FBMU1.IP`, `MT.D.GC.FBMU3.IP`, `IR.D.BOND100.FBMU2.IP`, `IR.D.FGBL.FWM2.IP`: a deal carries the epic's own
/// `expiry` and is accepted and closed with it, `crate::exec`'s `build_request_expiring`). `DFB` is a spread bet's
/// daily funded bet: a deal on one answered HTTP 403 `apiUser has no access to the relevant exchange` for this CFD login.
fn expiry_refusal(epic: &str, expiry: &str) -> Option<String> {
    if expiry == WIRE_EXPIRY || wire_expiry(expiry).is_some() {
        return None;
    }
    Some(if expiry == "DFB" {
        format!(
            "ig: {epic} is a spread-bet market (expiry `DFB`, a daily funded bet): measured on the demo gateway, a \
             deal on one is refused HTTP 403 (`apiUser has no access to the relevant exchange`) for a CFD \
             account, and a SPREADBET account sizes a deal as a stake per point, which nothing here has measured; \
             trade the same index or commodity as a CFD epic"
        )
    } else {
        format!(
            "ig: {epic} has expiry `{expiry}`, which is neither `-` (undated) nor a dated contract's own word, so \
             what the order must write is not known"
        )
    })
}

/// A DATED contract's `expiry` as the order wire writes it (`DEC-26`, `30-NOV-26`): letters, digits and `-`, 3 to 12
/// of them, and not `DFB` (a spread bet) nor the undated `-`. `None` for anything else.
pub(crate) fn wire_expiry(expiry: &str) -> Option<&str> {
    (expiry != "DFB"
        && (3..=12).contains(&expiry.len())
        && expiry.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        && expiry.bytes().any(|b| b.is_ascii_alphabetic()))
    .then_some(expiry)
}

/// The sentence for a CURRENCIES market whose own fields disagree about what one unit of size is worth.
/// `converted` is the same product in the ACCOUNT's currency when a rate was available to try it with.
fn ig_size_semantics_refusal(
    epic: &str,
    contract: f64,
    pip: f64,
    per_pip: f64,
    converted: Option<f64>,
) -> String {
    let also = converted.map_or_else(String::new, |x| {
        format!(" (nor, converted into the account's currency, {x})")
    });
    format!(
        "ig: {epic}: contractSize {contract} x pip {pip} = {}{also} but valueOfOnePip says {per_pip}, so \
         the market's size is not 'contracts of contractSize units' (UNMEASURED); it is not admitted",
        contract * pip
    )
}

/// The sentence for an epic quoted in the account's own currency that lists no dealing currency the order can say
/// (`currency`: [`WIRE_CURRENCY`] on a USD account, the account's own on any other).
pub(crate) fn no_wire_currency(epic: &str, currency: &str) -> String {
    format!(
        "ig: {epic} lists no {currency} among its dealing currencies, and this adapter sends \
         currencyCode {currency}"
    )
}

/// `onePipMeans`, `"0.0001 USD/EUR"`: the pip in quote units, then `QUOTE/BASE`. `None` for any other shape.
pub(crate) fn pip_and_pair(text: &str) -> Option<(f64, String, String)> {
    let mut words = text.split_whitespace();
    let pip: f64 = words.next()?.parse().ok()?;
    let pair = words.next()?;
    if words.next().is_some() || !pip.is_finite() || pip <= 0.0 {
        return None;
    }
    let (quote, base) = pair.split_once('/')?;
    let code = |c: &str| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic());
    (code(quote) && code(base))
        .then(|| (pip, quote.to_ascii_uppercase(), base.to_ascii_uppercase()))
}

/// The facts of `epic` out of its `GET /markets/{epic}` body and the account's currency, or the reason
/// they cannot be had. Pure: the module doc is the authority on every refusal. An epic quoted in another
/// currency than the account's is REFUSED here ([`quoted_market_facts`] is the node that converts's door).
pub fn market_facts(
    epic: &str,
    market: &Value,
    account_currency: &str,
) -> Result<IgMarketFacts, String> {
    facts_with(epic, market, account_currency, None)
        .map(|(facts, _)| facts)
        .map_err(|why| why.text().to_string())
}

/// How [`facts_with`] gets the rate of an epic whose quote currency is not the account's: asked once, at the
/// place [`market_facts`] refuses it.
type Convert<'a> = &'a mut dyn FnMut(&Pair) -> Result<QuoteRate, IgFactsError>;

/// [`market_facts`] for a node that CONVERTS a quote currency (module doc): an epic quoted in another currency
/// than the account's is answered with its quote currency and the rate IG states for it, checked against a
/// price (`crate::quote_rate`), instead of being refused for that alone. `asked_ms` is when the market
/// details were asked (the rate's freshness stamp); `helper` reads a conversion pair's market details
/// (`Ok(None)` = IG has no such epic).
pub fn quoted_market_facts(
    epic: &str,
    market: &Value,
    account_currency: &str,
    asked_ms: i64,
    helper: &mut dyn FnMut(&str) -> Result<Option<Value>, String>,
) -> Result<IgQuotedFacts, IgFactsError> {
    let account = account_currency.trim().to_ascii_uppercase();
    // Any account currency converts (the rate derivation is account-generic: `quote_rate::derive`), not only the
    // wire's USD: a pound or euro account's ledger is its own money and the node books the mounted epic through a
    // rate (decision 0183's mounted-symbol amendment). Only the plain path ([`market_facts`]) keeps the USD fence.
    let mut convert = |pair: &Pair| -> Result<QuoteRate, IgFactsError> {
        quote_rate::derive(epic, market, pair, &account, asked_ms, &mut *helper)
    };
    facts_with(epic, market, &account, Some(&mut convert)).map(|(facts, quote)| IgQuotedFacts {
        facts,
        quote,
        account_ccy: account.clone(),
    })
}

fn refused(text: String) -> IgFactsError {
    IgFactsError::Refused(text)
}

/// The one body of [`market_facts`] and [`quoted_market_facts`]: with no `convert`, an epic quoted in another
/// currency than the account's is refused; with one, it is asked for that epic's rate at the same place.
fn facts_with(
    epic: &str,
    market: &Value,
    account_currency: &str,
    convert: Option<Convert<'_>>,
) -> Result<(IgMarketFacts, Option<QuoteRate>), IgFactsError> {
    let Some(instrument) = market.get("instrument").filter(|i| i.is_object()) else {
        return Err(refused(format!(
            "ig: the market details of {epic} carry no `instrument` block"
        )));
    };
    let text = |key: &str| instrument.get(key).and_then(Value::as_str).unwrap_or("");
    // The venue's own spelling must be the one asked (the recon client and the engine compare it
    // exactly): an answer for another epic, or for the same one in another case, is not this epic.
    if text("epic") != epic {
        return Err(refused(format!(
            "ig: GET /markets/{epic} answered for `{}`, so this epic's spelling is not the venue's",
            text("epic")
        )));
    }
    if let Some(why) = class_refusal(epic, text("type")) {
        return Err(refused(why));
    }
    // A spread bet (`DFB`) is named before its missing stream: it is the first thing wrong with it.
    if let Some(why) = expiry_refusal(epic, text("expiry")) {
        return Err(refused(why));
    }
    // An epic IG says it does not stream to this account would be admitted and then never priced: the door
    // would hold every order for a price that cannot come (the lane's SUBOK/refusal cannot tell the node
    // why). Only an explicit `false` is a verdict; the measured FX Minis say `true`.
    if instrument.get("streamingPricesAvailable").and_then(Value::as_bool) == Some(false) {
        return Err(refused(format!(
            "ig: {epic} has no streaming price on this account (`instrument.streamingPricesAvailable` is \
             false), so no price would ever arrive for it"
        )));
    }
    // `lotSize` is NOT a fence any more: it equals `contractSize` on every index and commodity measured (FTSE 10,
    // DAX 25, US 500 250, Bund 2) and is 1 on a Mini, and USDJPY Mini's 100 was measured on 2026-10-28 to play no part
    // (one unit of `size` buys `contractSize` base units there as on every Mini: margin 0.2 x 10000 x 0.5%). The
    // money is `contractSize` and `valueOfOnePip`, cross-checked below; the lot was never what `size` counts.
    if text("unit") != "CONTRACTS" {
        return Err(refused(format!(
            "ig: {epic} is sized in `{}`, not in CONTRACTS, so its size is not a contract count",
            text("unit")
        )));
    }
    // The pricing shape: a CURRENCY PAIR (`onePipMeans` '<pip> <QUOTE>/<BASE>', the epic naming the pair) or a
    // CONTRACT on an index, a commodity, a coin or a bond (no pair; the quote currency is the epic's one listed
    // dealing currency, and the level grid is one pip, `1 / snapshot.scalingFactor` in level units: IG quotes
    // those levels in pips, so the pip is a level step of 1 whatever `onePipMeans` says in words).
    let (pip, quote, base) = if let Some((pip, quote, base)) = pip_and_pair(text("onePipMeans")) {
        // A second, independent witness of the pair: the epic itself (`CS.D.EURUSD.MINI.IP`).
        if epic.split('.').nth(2) != Some(format!("{base}{quote}").as_str()) {
            return Err(refused(format!(
                "ig: {epic}: the epic does not name the pair {base}/{quote} that `onePipMeans` states, so \
                 the quote currency is not certain"
            )));
        }
        (pip, quote, Some(base))
    } else {
        let scaling = market
            .pointer("/snapshot/scalingFactor")
            .and_then(vike_bridge_core::json::json_num)
            .filter(|s| s.is_finite() && *s > 0.0)
            .ok_or_else(|| {
                refused(format!(
                    "ig: {epic}: `onePipMeans` (`{}`) is not '<pip> <QUOTE>/<BASE>' (a currency pair) and its \
                     snapshot states no scalingFactor, so neither the quote currency nor the level grid is certain",
                    text("onePipMeans")
                ))
            })?;
        (1.0 / scaling, sole_currency(epic, instrument).map_err(refused)?, None)
    };
    let converting = convert.is_some();
    let mut rate = None;
    if !quote.eq_ignore_ascii_case(account_currency) {
        let Some(convert) = convert else {
            return Err(refused(format!(
                "ig: {epic} is quoted in {quote} but this account's currency is {account_currency}, and \
                 the engine books money in ONE currency (nothing converts a quote currency), so a \
                 position in it would be mis-valued; only instruments quoted in {account_currency} are \
                 admitted"
            )));
        };
        // An empty base = no pair: the conversion pair is `{quote}{account}` in IG's FX Mini family (`quote_rate`).
        rate =
            Some(convert(&Pair { quote: quote.clone(), base: base.clone().unwrap_or_default() })?);
    }
    // The plain path (a node that does not convert) deals a USD account only; the converting path deals any
    // account currency, the ledger being the account's own money (decision 0183's mounted-symbol amendment).
    if !converting && !account_currency.eq_ignore_ascii_case(WIRE_CURRENCY) {
        return Err(refused(quote_rate::wire_account(account_currency)));
    }
    // ⚠ The fence is for an epic QUOTED in the account's currency (no rate): its own table must list it (the order
    // is written in it, `crate::exec::order_currency`). A CONVERTED epic whose table lists no row for the account
    // currency (two public captures: USDJPY and USDCHF Mini list ONLY their quote currency) is admitted on the rate
    // the price route gives (`quote_rate`'s `PriceOnly`), and an order on it is written in the currency its table
    // lists (measured: EURGBP lists GBP alone), not USD (the facts source says so at `info`,
    // `IgFactsSource::facts_quoted`).
    if rate.is_none() && !lists_currency(market, account_currency) {
        return Err(refused(no_wire_currency(epic, account_currency)));
    }
    let (contract, per_pip) =
        (get_f64(instrument, "contractSize"), get_f64(instrument, "valueOfOnePip"));
    if !(contract.is_finite() && contract > 0.0 && per_pip.is_finite() && per_pip > 0.0) {
        return Err(refused(format!(
            "ig: {epic} carries no contractSize or no valueOfOnePip (missing or zero), so what one unit \
             of size is worth is unknown"
        )));
    }
    // What one pip of one contract is worth: `contractSize x pip` in the QUOTE currency, which is what the
    // figure is when the quote currency is the account's. IG's reference does not say which currency
    // `valueOfOnePip` is in, so for a converted epic the same product in the ACCOUNT's currency is accepted
    // too (within the rate's own tolerance): the check is about the SEMANTICS of `size`, not the currency.
    let in_quote = contract * pip;
    let in_account = rate.as_ref().map(|r| in_quote * r.account_per_quote);
    let agrees = (in_quote - per_pip).abs() <= 1e-6 * per_pip.max(1.0)
        || in_account.is_some_and(|x| (x - per_pip).abs() <= RATE_TOLERANCE * per_pip);
    if !agrees {
        return Err(refused(ig_size_semantics_refusal(epic, contract, pip, per_pip, in_account)));
    }
    let min_deal = market
        .pointer("/dealingRules/minDealSize/value")
        .and_then(vike_bridge_core::json::json_num)
        .unwrap_or(0.0);
    if !min_deal.is_finite() || min_deal <= 0.0 {
        return Err(refused(format!(
            "ig: {epic} publishes no minimum deal size (missing or zero)"
        )));
    }
    let steps = pip / WIRE_PRICE_STEP;
    if steps.round() < 1.0 || (steps - steps.round()).abs() > 1e-6 {
        return Err(refused(format!(
            "ig: {epic} has a pip of {pip}, and this adapter writes a level with five decimals, so a \
             pip that is not a whole multiple of {WIRE_PRICE_STEP} is not admitted"
        )));
    }
    let facts = IgMarketFacts {
        properties: SymbolProperties {
            tick_size: pip,
            step_size: min_deal,
            min_qty: min_deal,
            ..SymbolProperties::default()
        },
        contract_value: ContractValue::linear(contract),
        margin_rate: margin_rate(instrument),
    };
    Ok((facts, rate))
}

/// The epic's initial-margin rate out of its `instrument` block: `marginFactor` (a number or a numeric string) when
/// `marginFactorUnit` says `PERCENTAGE`, divided by 100, and only when it is a rate anybody could hold (above 0 and
/// at most 100 percent: the engine ignores any other figure). `POINTS`, an absent unit or an absent factor state no
/// rate here, and the epic is margined at the engine's own.
fn margin_rate(instrument: &Value) -> Option<f64> {
    let percent = instrument.get("marginFactor").and_then(vike_bridge_core::json::json_num)?;
    let unit = instrument.get("marginFactorUnit").and_then(Value::as_str)?;
    (unit.trim().eq_ignore_ascii_case("PERCENTAGE")
        && percent.is_finite()
        && percent > 0.0
        && percent <= 100.0)
        .then_some(percent / 100.0)
}

/// The account's currency out of a `GET /accounts` body: the row of `account_id` and no other (a first-row
/// fallback would name another account's currency), upper-cased. An unknown currency is an ERROR, never a
/// default.
pub(crate) fn parse_account_currency(body: &Value, account_id: &str) -> Result<String, String> {
    let row = body
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter().find(|a| a.get("accountId").and_then(Value::as_str) == Some(account_id))
        })
        .ok_or_else(|| format!("ig: GET /accounts lists no account `{account_id}`"))?;
    row.get("currency")
        .and_then(Value::as_str)
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .ok_or_else(|| format!("ig: the account `{account_id}` carries no readable currency"))
}

/// The account's `accountType` (`CFD`, `SPREADBET`, `PHYSICAL`) out of a `GET /accounts` body, upper-cased:
/// the row of `account_id` and no other. `None` when the row states none.
pub(crate) fn parse_account_type(body: &Value, account_id: &str) -> Option<String> {
    body.get("accounts")
        .and_then(Value::as_array)?
        .iter()
        .find(|a| a.get("accountId").and_then(Value::as_str) == Some(account_id))?
        .get("accountType")
        .and_then(Value::as_str)
        .map(|t| t.trim().to_ascii_uppercase())
        .filter(|t| !t.is_empty())
}

/// The sentence for a market whose `snapshot.marketStatus` is STATED and is not `TRADEABLE` (`CLOSED`,
/// `EDITS_ONLY`, `OFFLINE`, `SUSPENDED`, an auction ...), or `None` for a tradeable one and for one that states
/// none. It is said at the admission and never a refusal: a market shut for the weekend is still the epic the
/// operator asked for, and refusing it would spend nothing but make the door answer differently on a Sunday.
pub(crate) fn not_tradeable_words(epic: &str, market: &Value) -> Option<String> {
    let status = market.pointer("/snapshot/marketStatus")?.as_str()?.trim();
    (!status.is_empty() && !status.eq_ignore_ascii_case("TRADEABLE")).then(|| {
        format!(
            "ig: {epic} reads marketStatus `{status}`, not TRADEABLE: it is admitted, but no deal can open on \
             it until IG says TRADEABLE, and an opening order waits for a quote the shut market does not send"
        )
    })
}

/// An epic that may stand in a URL path: the characters an IG epic has, nothing else.
fn wire_safe(epic: &str) -> bool {
    !epic.is_empty()
        && epic.len() <= 64
        && epic.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// What `GET /accounts` says of the session's own account, read once.
#[derive(Clone)]
struct AccountRow {
    /// Upper-case, three letters ([`parse_account_currency`]), or the ruling's [`ASSUMED_CURRENCY`] when the row
    /// states none (`assumed`).
    currency: String,
    /// `accountType`, when the row states one ([`parse_account_type`]).
    kind: Option<String>,
    /// Whether `currency` is the owner's USD default and not IG's word.
    assumed: bool,
}

/// The currency the owner's ruling of 2026-10-19 assumes for an account whose row states none.
const ASSUMED_CURRENCY: &str = "USD";

/// Whether `GET /accounts` lists the row of `account_id` (stating a currency or not).
fn lists_account(body: &Value, account_id: &str) -> bool {
    body.get("accounts").and_then(Value::as_array).is_some_and(|rows| {
        rows.iter().any(|a| a.get("accountId").and_then(Value::as_str) == Some(account_id))
    })
}

/// One account's way to ask IG for an epic's facts: a kept, logged-in [`IgSession`] (one login however many
/// epics are asked; a 401 re-authenticates it in place), the account's currency (read once) and the market
/// read per epic. Cache, rate and failure memory
/// are the CALLER's (`vike-gatewayhub`'s `IgFacts`): this type spends a request per call.
pub struct IgFactsSource {
    config: IgConfig,
    session: Mutex<Option<Arc<IgSession>>>,
    /// The account's row, read once: it is the SESSION's account's, so it cannot differ per epic, and a
    /// rotated credential replaces this whole source ([`Self::serves`]). A failed read is not kept.
    account: Mutex<Option<AccountRow>>,
    /// When the reads of the last [`READ_WINDOW`] were made, and which bucket each was charged to
    /// ([`ReadBudget`]).
    reads: Mutex<VecDeque<(Instant, ReadBudget)>>,
    /// The per-minute cap of THIS key, set once from `GET /operations/application` ([`allowance_cap`]); until it
    /// is, and when that read failed, [`MAX_READS_PER_MINUTE`].
    cap: OnceLock<usize>,
    /// The epics that have answered with a conversion rate: an ask for one of them is a REFRESH (a held position's
    /// rate being kept inside the engine's window), anything else a first look ([`ReadBudget`]).
    rated: Mutex<HashSet<String>>,
    /// A failure only a change can cure (credentials refused, no account row, no readable currency), and when it
    /// happened: answered without a call for [`PERMANENT_FAILURE_KEEP`].
    refused: Mutex<Option<(Instant, String)>>,
    /// What this source remembers about conversion pairs ([`ConversionMemory`]).
    conversion: Mutex<ConversionMemory>,
}

/// Which bucket of the per-minute cap a read is charged to. A first look at an epic may spend only what the
/// refresh reserve leaves ([`REFRESH_RESERVE_DIVISOR`]); everything else may spend the whole cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadBudget {
    /// Everything that is not a first look at a cross: a login, the key's allowance, the account row, the plain
    /// (non-converting) facts, and the re-read of an epic that has answered with a rate.
    Full,
    /// A first look at an epic that has not answered with a rate yet.
    FirstLook,
}

/// What a source remembers about the conversion pairs a cross is witnessed by, so a refresh of several crosses
/// does not ask IG the same question again. Both maps are small (one entry per currency a held cross is quoted in,
/// and per spelling of its pair) and are emptied past [`CONVERSION_MEMORY_MAX`].
#[derive(Default)]
struct ConversionMemory {
    /// A pair spelling IG answered "no such epic" for, when, and for how long it is believed
    /// ([`MISSING_SPELLING_KEEP`] when IG's `errorCode` said so, [`UNCODED_MISSING_KEEP`] otherwise).
    missing: HashMap<String, (Instant, Duration)>,
    /// A pair's market details and when they were read.
    bodies: HashMap<String, (Instant, Value)>,
}

/// The most pair spellings a source remembers; a clear past it keeps the memory bounded whatever is asked.
const CONVERSION_MEMORY_MAX: usize = 32;

/// The most epics a source remembers as having answered with a rate (the daemon tracks 64 at a time).
const RATED_MAX: usize = 64;

/// The per-minute read cap for a key whose `allowanceAccountOverall` (IG's "per account request per minute
/// allowance", `GET /operations/application`) is `allowance`: two thirds of it, the rest left to the exec and
/// reconcile lanes that spend from the same account allowance (30 a minute by default: 20, this module's
/// [`MAX_READS_PER_MINUTE`] fallback). ⚠ The two thirds are this adapter's own share, unmeasured.
fn allowance_cap(allowance: usize) -> usize {
    (allowance * 2 / 3).clamp(MIN_READS_PER_MINUTE, MAX_READS_CEILING)
}

/// `allowanceAccountOverall` out of a `GET /operations/application` body: the object itself, or the row of the
/// list whose `apiKey` is `api_key`. `None` for any other shape, and for a figure that is not a positive number:
/// the fallback cap then stands.
fn parse_account_allowance(body: &Value, api_key: &str) -> Option<usize> {
    let app = match body {
        Value::Array(rows) => rows.iter().find(|row| {
            row.get("apiKey")
                .and_then(Value::as_str)
                .is_some_and(|k| k.eq_ignore_ascii_case(api_key))
        })?,
        Value::Object(_) => body,
        _ => return None,
    };
    let allowance =
        app.get("allowanceAccountOverall").and_then(vike_bridge_core::json::json_num)?;
    // shortcut: a fractional or huge figure is cut to a whole count here; allowance_cap bounds the result.
    (allowance.is_finite() && allowance >= 1.0).then_some(allowance.min(1.0e6) as usize)
}

impl IgFactsSource {
    /// A source over `config`; nothing is dialled until the first ask.
    #[must_use]
    pub fn new(config: IgConfig) -> Self {
        Self {
            config,
            session: Mutex::new(None),
            account: Mutex::new(None),
            reads: Mutex::new(VecDeque::new()),
            cap: OnceLock::new(),
            rated: Mutex::new(HashSet::new()),
            refused: Mutex::new(None),
            conversion: Mutex::new(ConversionMemory::default()),
        }
    }

    /// The per-minute cap now in force: this key's own ([`allowance_cap`]) once read, else [`MAX_READS_PER_MINUTE`].
    fn cap(&self) -> usize {
        self.cap.get().copied().unwrap_or(MAX_READS_PER_MINUTE)
    }

    /// Take one read from this source's own per-minute cap, charged to `budget`, or say it is spent: "not now",
    /// which asking again after the window may resolve. A first look is held to the cap less the refresh reserve,
    /// counted over the first looks alone, AND to the cap over every read: so a flood of first looks leaves the
    /// reserve to the refreshes, and refreshes can still use all of it.
    fn spend_read(&self, budget: ReadBudget) -> Result<(), IgFactsError> {
        let cap = self.cap();
        let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        while reads.front().is_some_and(|(at, _)| now.duration_since(*at) >= READ_WINDOW) {
            reads.pop_front();
        }
        let first_looks = reads.iter().filter(|(_, b)| *b == ReadBudget::FirstLook).count();
        let first_look_cap = cap - cap / REFRESH_RESERVE_DIVISOR;
        let spent = reads.len() >= cap
            || (budget == ReadBudget::FirstLook && first_looks >= first_look_cap);
        if spent {
            return Err(IgFactsError::Unavailable(format!(
                "ig: this adapter reads at most {cap} times a minute for an account{} (a cap of its own, \
                 below the allowance IG meters); retry shortly",
                if budget == ReadBudget::FirstLook {
                    format!(
                        ", and at most {first_look_cap} of them on first looks at an epic (the rest is \
                         kept for re-reading the rates already held)"
                    )
                } else {
                    String::new()
                }
            )));
        }
        reads.push_back((now, budget));
        Ok(())
    }

    /// A permanent failure remembered by this source, as its error, or `Ok` when none stands (or it has expired).
    fn check_refused(&self) -> Result<(), IgFactsError> {
        let mut held = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        match held.as_ref() {
            Some((at, why)) if at.elapsed() < PERMANENT_FAILURE_KEEP => {
                Err(IgFactsError::Account(why.clone()))
            }
            Some(_) => {
                *held = None;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Remember `why` as a failure only a change can cure, and return it as the error to answer.
    fn refuse_until_changed(&self, why: String) -> IgFactsError {
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), why.clone()));
        IgFactsError::Account(why)
    }

    /// The account's row: the kept one, or `GET /accounts` (v1) once. The lock is held across the read so
    /// concurrent first askers share ONE request. A row the account does not have, or one that states no readable
    /// currency, is a failure only a change can cure and is remembered ([`PERMANENT_FAILURE_KEEP`]).
    fn account(&self, session: &IgSession) -> Result<AccountRow, IgFactsError> {
        let mut held = self.account.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(row) = held.as_ref() {
            return Ok(row.clone());
        }
        self.check_refused()?;
        // The list the login read to choose the account (`IgSession::login`) is this very answer: asking IG
        // for it again would double the reads of every kept login, and it is read once per source anyway. It
        // is still a read of IG's allowance, so it is charged here exactly as the request it replaces was.
        self.spend_read(ReadBudget::Full)?;
        let accounts = match &session.accounts {
            Some(body) => body.clone(),
            None => session
                .get("/accounts", "1", "")
                .map_err(|e| IgFactsError::Unavailable(format!("ig GET /accounts: {e}")))?,
        };
        let (currency, assumed) = match parse_account_currency(&accounts, &session.account_id) {
            Ok(currency) => (currency, false),
            // The row EXISTS and states no readable currency: IG's silence, filled in with the owner's USD (2026-10-19:
            // "the account currency is what the venue API states; if the API does not state it: USD"), flagged
            // ([`IgFactsSource::currency_is_assumed`]) for the gatewayhub to tell the node and warn.
            Err(_) if lists_account(&accounts, &session.account_id) => {
                (ASSUMED_CURRENCY.to_string(), true)
            }
            // No row for the session's account says nothing about THIS account: still an error, remembered.
            Err(why) => return Err(self.refuse_until_changed(why)),
        };
        let kind = parse_account_type(&accounts, &session.account_id);
        let row = AccountRow { currency, kind, assumed };
        *held = Some(row.clone());
        Ok(row)
    }

    /// **Learn this key's own per-minute allowance, once per source** (`GET /operations/application`, v1,
    /// counted in the budget): the cap is then two thirds of `allowanceAccountOverall` ([`allowance_cap`]).
    /// Falls back to [`MAX_READS_PER_MINUTE`] when the read fails, is not understood, or the budget cannot spare
    /// it, and is not asked again either way. Called by the converting path only: a plain ask keeps the figure
    /// it always had and makes no extra request.
    fn learn_allowance(&self, session: &IgSession) {
        self.cap.get_or_init(|| {
            if self.spend_read(ReadBudget::Full).is_err() {
                return MAX_READS_PER_MINUTE;
            }
            let read = session.get("/operations/application", "1", "");
            let allowance = read
                .as_ref()
                .ok()
                .and_then(|body| parse_account_allowance(body, &self.config.api_key));
            match (allowance, read) {
                (Some(allowance), _) => {
                    let cap = allowance_cap(allowance);
                    tracing::info!(
                        target: "vike_ig::market_facts",
                        allowance, cap,
                        "ig: this key's per-account allowance is {allowance} requests a minute; the facts \
                         read at most {cap} a minute"
                    );
                    cap
                }
                (None, Ok(_)) => {
                    tracing::warn!(
                        target: "vike_ig::market_facts",
                        "ig: GET /operations/application carries no usable allowanceAccountOverall for this \
                         key; the facts read at most {MAX_READS_PER_MINUTE} a minute (unmeasured)"
                    );
                    MAX_READS_PER_MINUTE
                }
                (None, Err(e)) => {
                    tracing::warn!(
                        target: "vike_ig::market_facts",
                        "ig: GET /operations/application failed ({e}); the facts read at most \
                         {MAX_READS_PER_MINUTE} a minute (unmeasured)"
                    );
                    MAX_READS_PER_MINUTE
                }
            }
        });
    }

    /// The account's currency: the kept one, or `GET /accounts` (v1) once.
    fn currency(&self, session: &IgSession) -> Result<String, IgFactsError> {
        self.account(session).map(|row| row.currency)
    }

    /// **The currency the venue says this account's money is in** (`GET /accounts`, the row of the account the
    /// session is scoped to), for the gateway to hand the node when the account starts: the engine's ledger is
    /// anchored to it (decision 0183, item 1). Logs in on the first ask and keeps the session (and the row) for
    /// every later one.
    ///
    /// # Errors
    /// [`IgFactsError::Unavailable`] when the login or the read failed, or the account carries no readable
    /// currency: an unknown currency is an error, never a default.
    pub fn account_currency(&self) -> Result<String, IgFactsError> {
        let session = self.session()?;
        self.currency(&session)
    }

    /// Whether the currency [`Self::account_currency`] answers is only ASSUMED (the owner's ruling of 2026-10-19:
    /// the account's row states no readable currency, so USD): the gatewayhub flags it to the node and says it at
    /// `warn`. `false` before the row is read, and for every currency IG states.
    #[must_use]
    pub fn currency_is_assumed(&self) -> bool {
        self.account
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|row| row.assumed)
    }

    /// [`Self::account_currency`] waited on for `limit` at most, for the start path, which holds the session's
    /// adapter lock while it asks (a login and `GET /accounts` share an agent with a 30 s timeout each). The read
    /// runs on its own thread and is NOT abandoned when the wait ends: the login and the account row it finishes
    /// are kept by the source, so the next ask (the next start, a facts lookup) finds them and costs nothing.
    ///
    /// # Errors
    /// [`IgFactsError::Unavailable`] as [`Self::account_currency`], or when `limit` ran out first.
    pub fn account_currency_within(
        self: &Arc<Self>,
        limit: Duration,
    ) -> Result<String, IgFactsError> {
        let source = Arc::clone(self);
        run_within(
            limit,
            "account-currency read",
            format!(
                "ig: the account's currency was not read within {limit:?} (the read goes on in the \
                 background and is kept for the next ask)"
            ),
            move || source.account_currency(),
        )
    }

    /// [`Self::facts`] under ONE overall deadline of `limit` ([`FACTS_LOOKUP_WAIT`] in production), for the
    /// daemon's lookup slots: a lookup is up to four serial HTTP calls at 30 s each, and the daemon admits four
    /// lookups at once for every venue. Over the deadline it is "not now" and the slot is free; the read goes on
    /// in the background and its kept login and rows serve the next ask.
    ///
    /// # Errors
    /// [`IgFactsError`] as [`Self::facts`], or [`IgFactsError::Unavailable`] when `limit` ran out first.
    pub fn facts_within(
        self: &Arc<Self>,
        epic: &str,
        limit: Duration,
    ) -> Result<IgMarketFacts, IgFactsError> {
        let (source, epic) = (Arc::clone(self), epic.to_string());
        run_within(limit, "facts lookup", lookup_overrun(&epic, limit), move || source.facts(&epic))
    }

    /// [`Self::facts_quoted`] under ONE overall deadline of `limit`, as [`Self::facts_within`].
    ///
    /// # Errors
    /// [`IgFactsError`] as [`Self::facts_quoted`], or [`IgFactsError::Unavailable`] when `limit` ran out first.
    pub fn facts_quoted_within(
        self: &Arc<Self>,
        epic: &str,
        limit: Duration,
    ) -> Result<IgQuotedFacts, IgFactsError> {
        let (source, epic) = (Arc::clone(self), epic.to_string());
        run_within(limit, "facts lookup", lookup_overrun(&epic, limit), move || {
            source.facts_quoted(&epic)
        })
    }

    /// Whether this source logs in with exactly `config` (a rotated key or password needs a new source).
    #[must_use]
    pub fn serves(&self, config: &IgConfig) -> bool {
        self.config == *config
    }

    /// The kept session, logging in on the first ask. The lock is held across the login so concurrent
    /// askers share ONE login.
    ///
    /// A login is a request out of the account's allowance and is COUNTED in the cap ([`Self::spend_read`]). One
    /// IG refuses for good (the credentials are wrong or the key or account is disabled, [`login_refused_for_good`])
    /// is remembered for [`PERMANENT_FAILURE_KEEP`] and answered without a call: a caller that keeps asking
    /// must not make a login a second, and IG may lock an account on repeated failed ones (unverified). A
    /// timeout, a 5xx or a 429 is only "not now".
    fn session(&self) -> Result<Arc<IgSession>, IgFactsError> {
        let mut held = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(session) = held.as_ref() {
            return Ok(Arc::clone(session));
        }
        self.check_refused()?;
        self.spend_read(ReadBudget::Full)?;
        let session = IgSession::login(&self.config).map_err(|e| {
            let text = format!("ig login: {e}");
            if login_refused_for_good(e.status, &e.message) {
                self.refuse_until_changed(text)
            } else {
                IgFactsError::Unavailable(text)
            }
        })?;
        let session = Arc::new(session);
        *held = Some(Arc::clone(&session));
        Ok(session)
    }

    /// The facts of `epic`: the account's currency (`GET /accounts` (v1), read ONCE per source), `GET
    /// /markets/{epic}` (v3) for the market, then [`market_facts`]. One request per call after the first. IG's own 400 / 404 on the epic is a
    /// [`IgFactsError::Refused`] (the epic is not one); every other failure to read is
    /// [`IgFactsError::Unavailable`] (asking again may work).
    pub fn facts(&self, epic: &str) -> Result<IgMarketFacts, IgFactsError> {
        if !wire_safe(epic) {
            return Err(IgFactsError::Refused(format!(
                "ig: `{epic}` is not an epic (letters, digits, `.`, `_`, `-`)"
            )));
        }
        let session = self.session()?;
        let currency = self.currency(&session)?;
        // The plain ask keeps the whole cap (it has no refreshes to leave a reserve for).
        let market = self.market(&session, epic, ReadBudget::Full)?;
        if let Some(words) = not_tradeable_words(epic, &market) {
            tracing::warn!(target: "vike_ig::market_facts", "{words}");
        }
        market_facts(epic, &market, &currency).map_err(IgFactsError::Refused)
    }

    /// **[`Self::facts`] for a node that CONVERTS a quote currency** (decision 0183): an epic quoted in
    /// another currency than the account's comes back with its quote currency and the rate IG states for it,
    /// checked against a price (`crate::quote_rate`), where [`Self::facts`] refuses it. The rate is stamped
    /// with the moment this call began (IG states none). Costs the market read, plus up to two reads of a
    /// conversion pair for a cross (the epic's own price witnesses a pair whose base is the account currency);
    /// the first cross pays for finding out which spelling of the pair exists, and every cross after it asks IG
    /// only for its own epic (a spelling IG said is no epic is remembered for an hour, a 400/404 that did not say
    /// so for five minutes, and a pair's details are shared for 10 s). Every read counts against the per-minute
    /// cap ([`MAX_READS_PER_MINUTE`], or two thirds of this key's own allowance once read), and a FIRST LOOK at an
    /// epic is held to half of it so that the re-read of an epic that has answered with a rate (a held position)
    /// always has the other half (`ReadBudget`).
    ///
    /// Only a `CFD` account is converted for: a `SPREADBET` or `PHYSICAL` one sizes a deal differently, which
    /// nothing here has measured, and an account row that states NO type is not assumed to be a CFD one (IG
    /// always states it, so its absence means the row is not the shape this was read against).
    ///
    /// # Errors
    /// [`IgFactsError::Refused`] as [`Self::facts`]; [`IgFactsError::NoRate`] when the epic needs a rate and
    /// none could be had or believed; [`IgFactsError::Unavailable`] when a read failed.
    pub fn facts_quoted(&self, epic: &str) -> Result<IgQuotedFacts, IgFactsError> {
        if !wire_safe(epic) {
            return Err(IgFactsError::Refused(format!(
                "ig: `{epic}` is not an epic (letters, digits, `.`, `_`, `-`)"
            )));
        }
        let asked_ms = vike_model::now_ms();
        let session = self.session()?;
        let account = self.account(&session)?;
        // This key's own allowance, once per source (the converting path is the one that spends it).
        self.learn_allowance(&session);
        match account.kind.as_deref() {
            Some("CFD") => {}
            Some(kind) => {
                return Err(IgFactsError::Refused(format!(
                    "ig: this account is a {kind} account, and only a CFD account is admitted (a spread \
                     bet's size is a stake per point and a physical account's a share count: UNMEASURED)"
                )));
            }
            None => {
                return Err(IgFactsError::Refused(
                    "ig: GET /accounts states no accountType for this account, and only a CFD account is \
                     admitted (a spread bet's size is a stake per point and a physical account's a share \
                     count: UNMEASURED), so an account whose type is not stated is not assumed to be one"
                        .to_string(),
                ));
            }
        }
        // A REFRESH (the re-read of an epic that has answered with a rate) may spend the whole cap; a first look
        // only what the reserve leaves.
        let budget = if self.rated.lock().unwrap_or_else(PoisonError::into_inner).contains(epic) {
            ReadBudget::Full
        } else {
            ReadBudget::FirstLook
        };
        let market = self.market(&session, epic, budget)?;
        if let Some(words) = not_tradeable_words(epic, &market) {
            tracing::warn!(target: "vike_ig::market_facts", "{words}");
        }
        let mut helper = |name: &str| self.conversion_pair(&session, name, budget);
        let facts = quoted_market_facts(epic, &market, &account.currency, asked_ms, &mut helper)?;
        if let Some(rate) = &facts.quote {
            let mut rated = self.rated.lock().unwrap_or_else(PoisonError::into_inner);
            if rated.len() >= RATED_MAX {
                rated.clear();
            }
            rated.insert(epic.to_string());
            drop(rated);
            // The reading that held against the price is the one thing the first live read teaches.
            tracing::info!(
                target: "vike_ig::quote_rate", %epic, quote = %rate.quote_ccy,
                account = %account.currency, account_per_quote = rate.account_per_quote,
                "ig: {epic} is converted at {} {} per {} (IG's currencies table read as {})",
                rate.account_per_quote, account.currency, rate.quote_ccy, rate.reading.word()
            );
            if !lists_currency(&market, WIRE_CURRENCY)
                && !lists_currency(&market, &account.currency)
            {
                tracing::warn!(
                    target: "vike_ig::market_facts", %epic,
                    "ig: {epic} lists neither {WIRE_CURRENCY} nor the account's {} among its dealing currencies; \
                     it is admitted on the rate above, and an order on it is written in the currency its table \
                     lists (`crate::exec::order_currency`), which IG then books its profit and loss in",
                    account.currency
                );
            }
        }
        Ok(facts)
    }

    /// The market details of a conversion pair for [`quote_rate::derive`]'s `helper`: `Ok(None)` = IG has no
    /// such epic. **Remembered**, so a refresh of several held crosses costs IG the fewest reads: a body read
    /// within [`CONVERSION_PAIR_KEEP`] is shared (EURJPY, GBPJPY and AUDJPY are witnessed by one USDJPY read),
    /// and a spelling IG said does not exist is not asked again for [`MISSING_SPELLING_KEEP`] (the derivation
    /// tries `{quote}{account}` first, which for most currencies is a certain 404 before the call that works).
    /// ⚠ **Only an answer whose `errorCode` NAMES a missing epic ([`says_no_such_epic`]) is believed that long**:
    /// any other 400/404 is believed for [`UNCODED_MISSING_KEEP`], and a successful read of the name forgets it.
    fn conversion_pair(
        &self,
        session: &IgSession,
        name: &str,
        budget: ReadBudget,
    ) -> Result<Option<Value>, String> {
        self.remembered_pair(name, Instant::now(), || self.market(session, name, budget))
    }

    /// [`Self::conversion_pair`]'s memory, with the clock and the read handed in (so the keeps are testable
    /// without waiting an hour).
    fn remembered_pair(
        &self,
        name: &str,
        now: Instant,
        read: impl FnOnce() -> Result<Value, IgFactsError>,
    ) -> Result<Option<Value>, String> {
        {
            let memory = self.conversion.lock().unwrap_or_else(PoisonError::into_inner);
            if memory
                .missing
                .get(name)
                .is_some_and(|(at, keep)| now.saturating_duration_since(*at) < *keep)
            {
                return Ok(None);
            }
            if let Some((at, body)) = memory.bodies.get(name)
                && now.saturating_duration_since(*at) < CONVERSION_PAIR_KEEP
            {
                return Ok(Some(body.clone()));
            }
        }
        match read() {
            Ok(body) => {
                let mut memory = self.conversion.lock().unwrap_or_else(PoisonError::into_inner);
                if memory.bodies.len() >= CONVERSION_MEMORY_MAX {
                    memory.bodies.clear();
                }
                memory.missing.remove(name);
                memory.bodies.insert(name.to_string(), (now, body.clone()));
                Ok(Some(body))
            }
            // IG's own "no such epic" for a conversion pair is an answer (try the other spelling).
            Err(IgFactsError::Refused(text)) => {
                let keep = if says_no_such_epic(&text) {
                    MISSING_SPELLING_KEEP
                } else {
                    UNCODED_MISSING_KEEP
                };
                let mut memory = self.conversion.lock().unwrap_or_else(PoisonError::into_inner);
                if memory.missing.len() >= CONVERSION_MEMORY_MAX {
                    memory.missing.clear();
                }
                memory.missing.insert(name.to_string(), (now, keep));
                Ok(None)
            }
            Err(other) => Err(other.text().to_string()),
        }
    }

    /// `GET /markets/{epic}` (v3). IG's own 400 / 404 on the epic is a [`IgFactsError::Refused`] (the epic is
    /// not one); every other failure is [`IgFactsError::Unavailable`], and so is this source's own per-minute
    /// cap being spent ([`MAX_READS_PER_MINUTE`]). A successful read of a name forgets any "no such epic" this
    /// source remembered for it.
    fn market(
        &self,
        session: &IgSession,
        epic: &str,
        budget: ReadBudget,
    ) -> Result<Value, IgFactsError> {
        self.spend_read(budget)?;
        let body = session.get(&format!("/markets/{epic}"), "3", "").map_err(|e| {
            let text = format!("ig GET /markets/{epic}: {e}");
            if matches!(e.status, 400 | 404) {
                IgFactsError::Refused(text)
            } else {
                IgFactsError::Unavailable(text)
            }
        })?;
        self.conversion.lock().unwrap_or_else(PoisonError::into_inner).missing.remove(epic);
        Ok(body)
    }

    /// **IG's market search for `term`** (`GET /markets?searchTerm=`, v1), parsed into [`Instrument`]s, over this
    /// source's kept session and charged to the same per-minute cap as every other read of it. This is the
    /// bridge half of the symbol picker's per-query seam (`crate::catalog::IgCatalog`): a picker asks on every
    /// (debounced) keystroke, and a `POST /session` for each of them is exactly the login pressure IG's FAQ warns
    /// about (the catalog used to log in once per search).
    ///
    /// # Errors
    /// [`IgFactsError::Unavailable`] when the login or the read failed or the cap is spent (asking again may
    /// work); [`IgFactsError::Account`] for a login IG refuses for good.
    pub fn search(&self, term: &str) -> Result<Vec<Instrument>, IgFactsError> {
        let session = self.session()?;
        self.spend_read(ReadBudget::Full)?;
        let query = format!("searchTerm={}", vike_util::percent::percent_encode(term));
        let body = session
            .get("/markets", "1", &query)
            .map_err(|e| IgFactsError::Unavailable(format!("ig GET /markets: {e}")))?;
        Ok(crate::catalog::parse_markets(&body))
    }
}

/// The ONE dealing currency a contract epic (an index, a commodity, a coin, a bond: no currency pair to name its
/// money) lists in `instrument.currencies[]`: its levels, its `valueOfOnePip` and its profit are in it (measured
/// 2026-10-30 on the DAX in EUR, the FTSE in GBP and the US 500, gold, oil, Bitcoin and the bond futures in USD, each
/// listing exactly one). An epic that lists none or several is refused: which one the money is in is not stated.
fn sole_currency(epic: &str, instrument: &Value) -> Result<String, String> {
    let listed: Vec<String> = instrument
        .get("currencies")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|c| c.get("code").and_then(Value::as_str))
                .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
                .map(str::to_ascii_uppercase)
                .collect()
        })
        .unwrap_or_default();
    match listed.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "ig: {epic} lists no dealing currency in instrument.currencies[], so the currency its levels and \
             valueOfOnePip are in is not known"
        )),
        many => Err(format!(
            "ig: {epic} lists {} dealing currencies ({}), and which of them its levels and valueOfOnePip are in \
             is not stated (every epic measured lists exactly one)",
            many.len(),
            many.join(", ")
        )),
    }
}

/// Whether a market body lists `currency` among the currencies an order may be dealt in.
fn lists_currency(market: &Value, currency: &str) -> bool {
    market.pointer("/instrument/currencies").and_then(Value::as_array).is_some_and(|rows| {
        rows.iter().any(|c| {
            c.get("code").and_then(Value::as_str).is_some_and(|c| c.eq_ignore_ascii_case(currency))
        })
    })
}

/// Whether a login that failed with `status` and IG's `message` (its `errorCode` when it sent one) failed for a
/// reason only a CHANGE cures: the details are refused (401: `error.security.invalid-details`,
/// `too-many-failed-attempts`, a disabled key or account), the request is malformed (400), or the key is refused
/// for another reason than its allowance (403; an `...exceeded-...-allowance` code is "not now"). A timeout, a
/// reset, a 429 and a 5xx are not.
pub(crate) fn login_refused_for_good(status: u16, message: &str) -> bool {
    match status {
        400 | 401 => true,
        403 => !message.contains("allowance"),
        // The login was REFUSED for want of an account to trade (`crate::account_pick`): only the operator's
        // change (a book, an account opened in IG) cures it.
        STATUS_ACCOUNT_REFUSED => true,
        _ => false,
    }
}

/// `work` on a thread of its own, waited on for `limit` at most: the result, or "not now" when `limit` ran out
/// (`overrun` is the sentence; the work is NOT abandoned, it goes on and its side effects, the kept login and
/// rows, serve the next ask).
fn run_within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    overrun: String,
    work: impl FnOnce() -> Result<T, IgFactsError> + Send + 'static,
) -> Result<T, IgFactsError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("ig-{what}").replace(' ', "-"))
        .spawn(move || {
            // The waiter may be gone (timed out): the work's side effects are the point then.
            let _ = tx.send(work());
        })
        .map_err(|e| IgFactsError::Unavailable(format!("ig: could not start the {what}: {e}")))?;
    match rx.recv_timeout(limit) {
        Ok(read) => read,
        Err(RecvTimeoutError::Timeout) => Err(IgFactsError::Unavailable(overrun)),
        Err(RecvTimeoutError::Disconnected) => {
            Err(IgFactsError::Unavailable(format!("ig: the {what} ended without an answer")))
        }
    }
}

/// The sentence for a lookup of `epic` that was over its deadline.
fn lookup_overrun(epic: &str, limit: Duration) -> String {
    format!(
        "ig: the lookup of {epic} did not finish within {limit:?} (the read goes on in the background and \
         its login and rows are kept for the next ask)"
    )
}

/// Why [`IgFactsSource::facts`] gave no facts: the venue's answer about the epic, or no answer at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgFactsError {
    /// A definitive "no" about THIS epic (not an epic, unknown to IG, or a fence of [`market_facts`]).
    Refused(String),
    /// "Not now": the login, the transport or a read failed, so asking again may work.
    Unavailable(String),
    /// "Not now", about the RATE: the epic is quoted in a currency other than the account's and none could be
    /// had or believed (IG states none, the market is shut so no price checks it, the two disagree). Only
    /// [`IgFactsSource::facts_quoted`] says it. Nothing is learned about the epic itself.
    NoRate(String),
    /// Not now AND not soon: the login was refused for good (wrong details, a disabled key or account), or the
    /// account's row is not in `GET /accounts` or states no readable currency. Only a CHANGE (new credentials, a
    /// fixed account) cures it, so the source remembers it for a while and answers without a call
    /// ([`IgFactsSource`]'s `refused`), and the caller keeps its own failure memory long, not seconds.
    Account(String),
}

impl IgFactsError {
    /// The sentence, whichever kind.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            IgFactsError::Refused(t)
            | IgFactsError::Unavailable(t)
            | IgFactsError::NoRate(t)
            | IgFactsError::Account(t) => t,
        }
    }
}

#[cfg(test)]
#[path = "market_facts_tests.rs"]
mod market_facts_tests;
