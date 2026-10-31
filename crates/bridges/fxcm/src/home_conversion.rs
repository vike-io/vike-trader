//! FXCM's half of decision 0183 (a symbol's money is booked in the ACCOUNT's currency): the account's
//! currency, an instrument's quote currency, and the rate between them, from what ForexConnect states.
//! PURE: it parses the two JSON reads the shim exports (`fc_system_properties`, `fc_offers`) and judges
//! them; nothing here logs in, opens a socket or touches the FFI ([`crate::reads`] is the session half).
//!
//! # What the venue states, and where (the SDK's own documentation, `vendor/fcsdk/doc`)
//!
//! * **The account's currency** is the system property `BASE_CRNCY` ("the base currency of the system ...
//!   the currency of the account", e.g. `USD`; available "immediately after login"). The Accounts table has
//!   NO currency column: its balance is "expressed in the account currency" and the currency is never named.
//!   [`account_ccy`] reads the property and, when the API cannot give it, ASSUMES USD under the owner's
//!   ruling of 2026-10-19 ("check the currency by API; if you can't, USD"): the assumption is a distinct
//!   [`CcySource::Assumed`] that travels with the value and is said out loud, never a silent default.
//! * **An instrument's quote currency**: the Offers table's `ContractCurrency` is the BASE currency of a
//!   regular forex pair ("`EUR` in `EUR/USD`") and its price is "the counter currency per one unit of base
//!   currency", so a forex pair (type 1, multiplier 1) is quoted in the second half of its name, cross-checked
//!   against `ContractCurrency`. Every OTHER class is a CFD, priced "in the instrument's native currency per one
//!   contract", and for those the SDK's own column IS that currency (MEASURED on the 449 non-forex rows of the
//!   EUR demo, 2026-10-30: `GER30` EUR, `JPN225` JPY, `HKG33` HKD, `USOil` USD, `XAU/USD` USD, `BTC/EUR` EUR,
//!   `AAPL.ext` USD, and where the name is a pair its second half agrees on all of them): [`quote_currency_of`].
//!   The CLASS is the row's `InstrumentType` ([`Class`]), never its name.
//! * **The rate** is the two-sided quote of the conversion pair in the SAME fresh Offers read
//!   ([`quote_rate`]): no extra session, no extra login. Orientation is named in every type: an
//!   [`AccountPerQuote`] is ACCOUNT-currency units per ONE QUOTE-currency unit (USD/JPY on a USD account
//!   is about `1/150`, never `150`: the 22,500x bug).
//! * **Through USD** when FXCM lists no pair of the quote currency and the ledger's (the EUR demo lists
//!   no HKD, ZAR, MXN or CNH pair of EUR): the rate is the product of the quote currency's rate in USD and
//!   USD's rate in the ledger's, both read from the SAME table ([`hops`], [`quote_rate`]). Where a direct pair
//!   is listed nothing changes, bit for bit.
//!
//! # Fail closed
//!
//! No usable rate is no admission: a missing pair, an unsubscribed one, a closed market, a one-sided,
//! zero, negative, non-finite or crossed quote, or TWO candidate pairs (`Q/A` and `A/Q` both subscribed)
//! is an `Err` in the operator's words, never a guess at par. A composed rate needs BOTH legs, each judged as a
//! single pair is, and the words name the leg that failed.

use serde_json::Value;
use vike_model::{ContractValue, SymbolProperties};

/// The system property that carries the account's currency (`IO2GSystemPropertiesReader`'s table).
pub const BASE_CURRENCY_PROPERTY: &str = "BASE_CRNCY";

/// What the account's currency is taken to be when the API cannot state it (owner ruling 2026-10-19:
/// "check the currency by API; if you can't, USD"). Only ever as [`CcySource::Assumed`].
pub const ASSUMED_ACCOUNT_CURRENCY: &str = "USD";

/// `O2GInstrumentType::InstrumentForex`: a regular forex instrument, the class whose quote currency is the second half
/// of its name and whose conversion pairs the rate is made of.
pub const INSTRUMENT_FOREX: i32 = 1;

/// **The class of an instrument, from the SDK's own `O2GInstrumentType`** (`vendor/fcsdk/.../O2GEnum.h`), never from
/// the name: `US30` and `Copper` say nothing, the row's type does. The nine the SDK names; a number outside them is no
/// class this bridge knows ([`Class::of`] is `None`) and the instrument is refused by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// 1: a regular forex pair.
    Forex,
    /// 2: a stock index CFD (`US30`, `GER30`, `SPX500`).
    Index,
    /// 3: a commodity CFD (`USOil`, `NGAS`, `Copper`).
    Commodity,
    /// 4: a treasury or bond CFD (`Bund`, `10USNote`).
    Treasury,
    /// 5: bullion, the spot metals (`XAU/USD`, `XAG/USD`).
    Bullion,
    /// 6: shares.
    Share,
    /// 7: an FX index (`USDOLLAR`).
    FxIndex,
    /// 8: a share CFD (`AAPL.ext`).
    CfdShare,
    /// 9: a crypto CFD (`BTC/USD`).
    Crypto,
}

impl Class {
    /// The class of `O2GInstrumentType` `instrument_type`, or `None` for a number the SDK does not name.
    #[must_use]
    pub fn of(instrument_type: i32) -> Option<Self> {
        Some(match instrument_type {
            1 => Self::Forex,
            2 => Self::Index,
            3 => Self::Commodity,
            4 => Self::Treasury,
            5 => Self::Bullion,
            6 => Self::Share,
            7 => Self::FxIndex,
            8 => Self::CfdShare,
            9 => Self::Crypto,
            _ => return None,
        })
    }

    /// The class in the operator's words.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Forex => "a forex pair",
            Self::Index => "an index CFD",
            Self::Commodity => "a commodity CFD",
            Self::Treasury => "a treasury CFD",
            Self::Bullion => "a metal (bullion)",
            Self::Share => "a share",
            Self::FxIndex => "an FX index",
            Self::CfdShare => "a share CFD",
            Self::Crypto => "a crypto CFD",
        }
    }
}

/// The keys of one row of `fc_offers` (the shim's `snprintf` literal), held equal to the C++ by
/// `tests/fxcm_shim_conversion_reads.rs`.
pub const OFFER_KEYS: [&str; 13] = [
    "instrument",
    "bid",
    "ask",
    "digits",
    "instrument_type",
    "contract_currency",
    "contract_multiplier",
    "subscription",
    "trading_status",
    "bid_tradable",
    "ask_tradable",
    "base_unit",
    "quote_age_ms",
];

/// The keys of the SECOND half of a `fc_offers` row, the instrument's TERMS (the same `snprintf` literal): sizes in lots,
/// the margin in the account's currency, the market status and the account's order permissions. Optional on the wire
/// (a shim older than them sends none), so the parser requires none of them; the C++ gate holds the literal to
/// [`OFFER_KEYS`] plus these.
pub const OFFER_TERMS_KEYS: [&str; 21] = [
    "offer_id",
    "point_size",
    "buy_interest",
    "sell_interest",
    "min_qty",
    "max_qty",
    "mmr",
    "three_level",
    "level_mmr",
    "emr",
    "lmr",
    "progressive_margin",
    "market_status",
    "cond_dist_stop",
    "cond_dist_limit",
    "cond_dist_entry_stop",
    "cond_dist_entry_limit",
    "can_market_open",
    "can_market_close",
    "can_entry",
    "can_stop_limit",
];

/// The oldest a conversion pair's quote may be, by ITS OWN time, to be a rate. The node calls a rate stale at the
/// same 120 s (`vike_exec::DEFAULT_QUOTE_RATE_TTL_MS`, applied to the stamp), and the two agree because
/// [`quote_rate`] stamps a rate with the QUOTE's time, not the ask's.
pub const MAX_QUOTE_AGE_MS: i64 = 120_000;

/// How far AHEAD of the server's clock a quote's time may read before the pair is refused. A few seconds is
/// clock jitter; hours is the row time and the server time not sharing a time zone, in which case no age is
/// trustworthy at all.
pub const MAX_QUOTE_AHEAD_MS: i64 = 5_000;

// ---- the account's currency ----------------------------------------------------------------------

/// How the account's currency is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CcySource {
    /// The venue stated it (`BASE_CRNCY`).
    Venue,
    /// The venue did not, and the owner's ruling fills in USD. `why` is the reason the API could not say.
    Assumed {
        /// Why the venue's word was not available.
        why: String,
    },
}

/// The account's currency and how it is known. Travels to the node as `account_ccy` plus an `assumed` flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCcy {
    /// An upper-case three-letter code.
    pub code: String,
    /// Venue-stated or assumed.
    pub source: CcySource,
}

impl AccountCcy {
    /// `true` when the currency is the ruling's USD and not the venue's word.
    #[must_use]
    pub fn is_assumed(&self) -> bool {
        matches!(self.source, CcySource::Assumed { .. })
    }
}

/// What reading the login's system properties came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertiesRead {
    /// The shim's JSON object of property name to value.
    Json(String),
    /// The API cannot state them (no session was registered, a shim installed before `fc_system_properties`,
    /// a read the venue's library failed): the reason, in words. This is the API's silence, which the owner's
    /// ruling fills with an ASSUMED USD.
    Unreadable(String),
    /// The session did not ANSWER (a timeout, the session thread gone): the reason, in words. This is not the
    /// API's silence but ours, so nothing is assumed over it: [`account_ccy`] refuses, and the caller asks again
    /// later.
    Failed(String),
}

/// A three-letter currency code, upper-cased, or `None`.
fn iso(code: &str) -> Option<String> {
    let code = code.trim();
    (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

/// **The account's currency, by the owner's ruling**: the venue's `BASE_CRNCY` when it states one; USD, ASSUMED
/// and flagged, when the API cannot give it (the property is absent, the shim predates the read, there is no
/// session to ask); an `Err` when it states something that is NOT a currency code (a malformed answer is not
/// "silence": guessing over it would be a guess over a wrong venue).
///
/// # Errors
/// The session did not answer ([`PropertiesRead::Failed`]), the venue's system properties are not a JSON object,
/// or `BASE_CRNCY` is not a three-letter code.
pub fn account_ccy(read: PropertiesRead) -> Result<AccountCcy, String> {
    let assumed = |why: String| AccountCcy {
        code: ASSUMED_ACCOUNT_CURRENCY.to_string(),
        source: CcySource::Assumed { why },
    };
    let body = match read {
        PropertiesRead::Unreadable(why) => return Ok(assumed(why)),
        PropertiesRead::Failed(why) => {
            return Err(format!(
                "fxcm: the session did not answer the account's currency ({why}); it is not assumed over a \
                 session that said nothing"
            ));
        }
        PropertiesRead::Json(body) => body,
    };
    let value: Value = serde_json::from_str(&body).map_err(|e| {
        format!(
            "fxcm: the venue's system properties are not JSON ({e}); the account's currency is not \
             assumed over a malformed answer"
        )
    })?;
    let Some(object) = value.as_object() else {
        return Err("fxcm: the venue's system properties are not a JSON object; the account's \
                    currency is not assumed over a malformed answer"
            .to_string());
    };
    match object.get(BASE_CURRENCY_PROPERTY).and_then(Value::as_str).map(str::trim) {
        None | Some("") => Ok(assumed(format!(
            "the venue's system properties carry no {BASE_CURRENCY_PROPERTY} ({} properties read)",
            object.len()
        ))),
        Some(code) => iso(code).map(|code| AccountCcy { code, source: CcySource::Venue }).ok_or_else(
            || {
                format!(
                    "fxcm: {BASE_CURRENCY_PROPERTY} is `{code}`, which is not a three-letter currency \
                     code; the account's currency is not guessed over it"
                )
            },
        ),
    }
}

// ---- the Offers table ------------------------------------------------------------------------------

/// One row of the current Offers table, as `fc_offers` writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// The venue's instrument name (`EUR/USD`).
    pub instrument: String,
    /// The two-sided quote.
    pub bid: f64,
    /// The two-sided quote.
    pub ask: f64,
    /// Price precision (decimals).
    pub digits: i32,
    /// `O2GInstrumentType` (1 = forex).
    pub instrument_type: i32,
    /// The BASE currency of the instrument (not its quote currency).
    pub contract_currency: String,
    /// The contract multiplier (1 for forex).
    pub contract_multiplier: f64,
    /// `T` = subscribed and available for trading, `V` view only, `D` disabled.
    pub subscription: String,
    /// `O` open, `C` closed.
    pub trading_status: String,
    /// `T` / `I` / `N` (not available for trading).
    pub bid_tradable: String,
    /// `T` / `I` / `N` (not available for trading).
    pub ask_tradable: String,
    /// The lot on the tradable account, in base units; 0 when no account was eligible.
    pub base_unit: i64,
    /// How old this row's quote was when the shim read it, in ms: the row's own last-update time against the
    /// session's server time (`None` = the venue gave no usable time, or the key is absent). NEGATIVE = the row
    /// reads ahead of the server clock. See [`quote_age`].
    pub quote_age_ms: Option<i64>,
    /// The instrument's TERMS on the tradable account, `None` from a shim older than them (and then only the grid of
    /// the first half is known: a metal, an index or a CFD is not admitted without its minimum and maximum).
    pub terms: Option<Terms>,
}

/// What FXCM's trading settings say about one instrument for the tradable account: the second half of an `fc_offers` row
/// ([`OFFER_TERMS_KEYS`]). Only what the bridge uses; the rest of the half stays in the raw row.
#[derive(Clone, Debug, PartialEq)]
pub struct Terms {
    /// The smallest order, in BASE UNITS (the unit of `Amount`; the SDK header says "lots" and the venue answers in units:
    /// EUR/USD reads 1000 with a base unit of 1000, US30 reads 1 with a base unit of 1).
    pub min_qty: i64,
    /// The largest order, in base units.
    pub max_qty: i64,
    /// The margin the venue asks to open ONE lot (`base_unit` units), in the ACCOUNT's currency: the entry margin of a
    /// three-level account, else the minimum margin requirement. `None` when the session could not state it (the
    /// SDK answers -1; MEASURED on the EUR demo, every row).
    pub margin_per_lot: Option<f64>,
}

fn terms_from(v: &Value) -> Option<Terms> {
    let (min_qty, max_qty) = (v.get("min_qty")?.as_i64()?, v.get("max_qty")?.as_i64()?);
    if min_qty < 1 || max_qty < min_qty {
        return None;
    }
    let figure = |key: &str| number(v, key).filter(|x| x.is_finite() && *x > 0.0);
    let three_level = v.get("three_level").and_then(Value::as_i64) == Some(1);
    let margin_per_lot = if three_level { figure("emr") } else { figure("mmr") };
    Some(Terms { min_qty, max_qty, margin_per_lot })
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn offer_from(v: &Value) -> Option<Offer> {
    Some(Offer {
        instrument: text(v, "instrument")?,
        bid: number(v, "bid")?,
        ask: number(v, "ask")?,
        digits: i32::try_from(v.get("digits")?.as_i64()?).ok()?,
        instrument_type: i32::try_from(v.get("instrument_type")?.as_i64()?).ok()?,
        contract_currency: text(v, "contract_currency")?,
        contract_multiplier: number(v, "contract_multiplier")?,
        subscription: text(v, "subscription")?,
        trading_status: text(v, "trading_status")?,
        bid_tradable: text(v, "bid_tradable")?,
        ask_tradable: text(v, "ask_tradable")?,
        base_unit: v.get("base_unit")?.as_i64()?,
        // Optional on purpose: a row without a usable time still gives its OWN grid and lot; only a RATE built
        // from it is refused ([`quote_age`]).
        quote_age_ms: v.get("quote_age_ms").and_then(Value::as_i64),
        // Optional too: a row without terms is a first-half row, usable as ever for a forex pair.
        terms: terms_from(v),
    })
}

/// The Offers table `fc_offers` wrote. A row missing a key or holding a value of the wrong type is SKIPPED
/// (and said once): it can then never be the pair a rate is built from, so a malformed row costs the
/// instrument it names, not the table.
///
/// # Errors
/// The body is not a JSON array.
pub fn parse_offers(body: &str) -> Result<Vec<Offer>, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| format!("fxcm: the Offers table is not JSON ({e})"))?;
    let rows =
        value.as_array().ok_or_else(|| "fxcm: the Offers table is not a JSON array".to_string())?;
    let offers: Vec<Offer> = rows.iter().filter_map(offer_from).collect();
    if offers.len() != rows.len() {
        tracing::warn!(
            venue = "fxcm",
            skipped = rows.len() - offers.len(),
            "fxcm: some Offers rows were missing a field or held a wrong type and are ignored"
        );
    }
    Ok(offers)
}

// ---- an instrument's quote currency ----------------------------------------------------------------

/// `("EUR", "USD")` from `EUR/USD`: two upper-case three-letter codes around one slash.
fn pair_of(instrument: &str) -> Option<(String, String)> {
    let (base, quote) = instrument.split_once('/')?;
    let (base, quote) = (iso(base)?, iso(quote)?);
    (instrument.trim() == format!("{base}/{quote}")).then_some((base, quote))
}

/// The quote currency named by a symbol's SPELLING alone (`EURUSD` or `EUR/USD` -> `USD`), or `None` when it is not
/// a pair of two currency codes. For the ENGINE's mounted symbol, which no Offers row is read for: its fills are
/// booked in this money. A spelling is not the venue's statement about an instrument ([`quote_currency_of`] is),
/// so nothing admits a symbol on it.
#[must_use]
pub fn quote_of_symbol(symbol: &str) -> Option<String> {
    pair_of(&crate::exec::to_fxcm_instrument(symbol)).map(|(_, quote)| quote)
}

/// **The instrument's quote currency, from what the venue states, or a refusal naming why it is not
/// determinable.** Decided by the row's CLASS ([`Class`], the SDK's `InstrumentType`), never by its name:
///
/// * a regular forex pair (type 1, contract multiplier 1): its price is "the counter currency per one unit of base
///   currency" (the Offers table page), the counter currency is the second half of its name, and `ContractCurrency`
///   (the BASE currency) must agree with the first half;
/// * every other class: a CFD priced "in the instrument's native currency per one contract", and the row's
///   `ContractCurrency` IS that currency (MEASURED on all 449 non-forex rows of the EUR demo). Where the name is a
///   pair (`XAU/USD`, `BTC/EUR`, `ETH/BTC`) its second half must agree with it, else the row is not trusted. The
///   contract multiplier must be a positive finite number: it is what one unit of `Amount` stands for
///   ([`contract_value_of`]).
///
/// # Errors
/// The type is none of the SDK's nine, the contract multiplier is unusable, the name and the contract currency
/// disagree, or a CFD's contract currency is not a three-letter code.
pub fn quote_currency_of(o: &Offer) -> Result<String, String> {
    let Some(class) = Class::of(o.instrument_type) else {
        return Err(format!(
            "fxcm: `{}` is an instrument of type {}, which is none of the nine the SDK names, so its quote \
             currency is not determinable and it is not admitted",
            o.instrument, o.instrument_type
        ));
    };
    if class != Class::Forex {
        return native_currency_of(o, class);
    }
    if (o.contract_multiplier - 1.0).abs() > 1e-9 {
        return Err(format!(
            "fxcm: `{}` has a contract multiplier of {} (a forex instrument's is 1), so one unit is not \
             one unit of its base currency and its money is not converted",
            o.instrument, o.contract_multiplier
        ));
    }
    let Some((base, quote)) = pair_of(&o.instrument) else {
        return Err(format!(
            "fxcm: `{}` is not a `BASE/QUOTE` pair of two currency codes, so it names no quote currency",
            o.instrument
        ));
    };
    match iso(&o.contract_currency) {
        Some(contract) if contract == base => Ok(quote),
        _ => Err(format!(
            "fxcm: `{}` states ContractCurrency `{}` (the base currency of the instrument) and its name \
             says {base}; the two disagree, so its quote currency is not trusted",
            o.instrument, o.contract_currency
        )),
    }
}

/// The native (quote) currency of a CFD-class row: its `ContractCurrency`, cross-checked against the second half of a
/// pair-shaped name.
fn native_currency_of(o: &Offer, class: Class) -> Result<String, String> {
    if !(o.contract_multiplier.is_finite() && o.contract_multiplier > 0.0) {
        return Err(format!(
            "fxcm: `{}` ({}) has a contract multiplier of {}, which is not a positive number, so what one unit \
             of it is worth is unknown and it is not admitted",
            o.instrument,
            class.word(),
            o.contract_multiplier
        ));
    }
    let Some(native) = iso(&o.contract_currency) else {
        return Err(format!(
            "fxcm: `{}` ({}) states ContractCurrency `{}`, which is not a three-letter currency code, so its \
             quote currency is not determinable and it is not admitted",
            o.instrument,
            class.word(),
            o.contract_currency
        ));
    };
    if let Some((_, named)) = pair_of(&o.instrument)
        && named != native
    {
        return Err(format!(
            "fxcm: `{}` ({}) is named like a pair quoted in {named} and states ContractCurrency `{native}`; the \
             two disagree, so its quote currency is not trusted",
            o.instrument,
            class.word()
        ));
    }
    Ok(native)
}

/// **What one unit of an order's quantity stands for**, as a contract value: the row's `ContractMultiplier` for a
/// CFD-class row whose multiplier is not 1, `None` (the default, 1) for a forex pair and for a multiplier of 1. A unit
/// of quantity is one base unit of the lot (`Amount = base_unit * lots`), and the SDK's own words for the multiplier
/// are "the contract multiplier ... used for simulation of the real-life trading conditions" (Copper 25000 lbs),
/// i.e. how much of the underlying one unit controls.
///
/// ⚠ UNMEASURED IN MONEY: that FXCM books a CFD's P/L as `(close - open) * amount * multiplier` is the SDK's reading
/// of that sentence, not a fill; the kit's classes proof (`fxcmclasses-roundtrip.sh`) compares the closed
/// trade's gross P/L with it on each class and says FAIL where it does not hold.
#[must_use]
pub fn contract_value_of(o: &Offer) -> Option<ContractValue> {
    let class = Class::of(o.instrument_type)?;
    (class != Class::Forex && (o.contract_multiplier - 1.0).abs() > 1e-9)
        .then(|| ContractValue::linear(o.contract_multiplier))
}

/// The ONE row of `offers` that `symbol` names, compared the way the engine and the fills spell an instrument
/// (`from_fxcm_instrument`: no slash, upper case), so `USOIL` finds `USOil`, `XAUUSD` finds `XAU/USD` and `NVDA.EXT`
/// finds `NVDA.ext`. `None` when no row has that name; more than one is `Err` with the names (FXCM lists none today).
pub(crate) fn row_for<'a>(offers: &'a [Offer], symbol: &str) -> Result<Option<&'a Offer>, String> {
    let want = crate::event_mapper::from_fxcm_instrument(symbol);
    let mut named =
        offers.iter().filter(|o| crate::event_mapper::from_fxcm_instrument(&o.instrument) == want);
    let first = named.next();
    match (first, named.next()) {
        (Some(a), Some(b)) => Err(format!(
            "fxcm: `{symbol}` names more than one row of the Offers table (`{}` and `{}`), so it is not admitted",
            a.instrument, b.instrument
        )),
        (first, _) => Ok(first),
    }
}

// ---- the rate -------------------------------------------------------------------------------------

/// **Account-currency units per ONE quote-currency unit.** The orientation lives in the type: USD/JPY on a
/// USD account is `AccountPerQuote(1 / 150)` (one yen is worth 0.00667 dollars), never `150`. Finite and
/// positive, or it does not exist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccountPerQuote(f64);

impl AccountPerQuote {
    /// The rate, or `None` for anything that is not a finite positive number.
    #[must_use]
    pub fn new(v: f64) -> Option<Self> {
        (v.is_finite() && v > 0.0).then_some(Self(v))
    }

    /// The number: account-currency units per one quote-currency unit.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// An amount in the QUOTE currency, worth this many ACCOUNT-currency units (a JPY gain of 10,000 on a
    /// USD account at USD/JPY 150.10 is 66.62 USD).
    #[must_use]
    pub fn account_amount(self, quote_amount: f64) -> f64 {
        quote_amount * self.0
    }
}

/// What FXCM's quote of the conversion pair makes one unit of an instrument's quote currency worth in the
/// account's own currency, at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct QuoteRate {
    /// The instrument's quote currency (upper case).
    pub quote_ccy: String,
    /// Account-currency units per one `quote_ccy` unit.
    pub account_per_quote: AccountPerQuote,
    /// The moment of the QUOTE the rate is made of, epoch ms on this process's clock: the moment the venue was
    /// ASKED minus the row's own age ([`quote_age`]), so it is never newer than the quote. A quote frozen while
    /// the market status still says `O` ages here and is refused past [`MAX_QUOTE_AGE_MS`]; it is not stamped
    /// fresh by the refresh that fetched it. The row's time and the server's are compared only as a DIFFERENCE
    /// (the zone cancels when they share one, and a frame that is not shared reads as hours and is refused).
    /// A rate composed through USD ([`hops`]) carries the OLDEST of its two legs' moments.
    pub as_of_ms: i64,
}

/// Which half of the conversion pair is the QUOTE currency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pair {
    /// `Q/A`: the quote currency is the pair's BASE, so its price is account units per quote unit already.
    QuoteIsBase,
    /// `A/Q`: the account currency is the pair's BASE, so its price is quote units per account unit and
    /// the rate is its reciprocal.
    AccountIsBase,
}

/// The mid of a live two-sided quote, or why the quote is not one.
fn mid_of(o: &Offer) -> Result<f64, String> {
    live_sides(o).map(|(bid, ask)| (bid + ask) / 2.0)
}

/// **The two sides of a live quote, or why the row has none**: the market open (`O`), BOTH sides tradable (`T`: an
/// indicative side `I`, an unavailable one `N`, an empty or unknown flag are not), both sides finite and
/// positive, the ask not below the bid. The ONE judgement of "is this row a quote" that the conversion rate
/// ([`quote_rate`]) and the price lane ([`crate::quotes`]) share; the row's AGE is [`quote_age`]'s.
///
/// # Errors
/// The market is not open, a side is not tradable, a side is not a positive number, or the quote is crossed.
pub fn live_sides(o: &Offer) -> Result<(f64, f64), String> {
    if o.trading_status != "O" {
        return Err(format!(
            "fxcm: the market for `{}` is not open (trading status `{}`), so it states no live rate",
            o.instrument, o.trading_status
        ));
    }
    if o.bid_tradable != "T" || o.ask_tradable != "T" {
        return Err(format!(
            "fxcm: `{}` is not available for trading on a side (bid `{}`, ask `{}`; only `T` is tradable, not \
             indicative `I` or unavailable `N`), so its quote is not a price or a rate",
            o.instrument, o.bid_tradable, o.ask_tradable
        ));
    }
    let good = |p: f64| p.is_finite() && p > 0.0;
    if !good(o.bid) || !good(o.ask) {
        return Err(format!(
            "fxcm: `{}` has no two-sided positive quote (bid {}, ask {})",
            o.instrument, o.bid, o.ask
        ));
    }
    if o.bid > o.ask {
        return Err(format!(
            "fxcm: the quote of `{}` is crossed (bid {} above ask {})",
            o.instrument, o.bid, o.ask
        ));
    }
    Ok((o.bid, o.ask))
}

/// **How old the quote is, by its own time**, or why that cannot be told: the age (clamped to 0 when it reads a
/// little ahead of the server clock) of a row whose quote is no older than [`MAX_QUOTE_AGE_MS`]. Fails CLOSED:
/// a row with no usable time, a quote older than the limit (a frozen price while the status still reads open;
/// the first seconds after a weekend open, which still carry Friday's price), and a quote that reads more than
/// [`MAX_QUOTE_AHEAD_MS`] ahead (the row time and the server time are not in one frame, so no age can be trusted)
/// are each refused in words.
///
/// # Errors
/// The row has no usable time, is older than [`MAX_QUOTE_AGE_MS`], or reads more than [`MAX_QUOTE_AHEAD_MS`]
/// ahead of the server clock.
pub fn quote_age(o: &Offer) -> Result<i64, String> {
    match o.quote_age_ms {
        None => Err(format!(
            "fxcm: the quote of `{}` carries no time of its own, so its age is not known and it is not a rate",
            o.instrument
        )),
        Some(age) if age > MAX_QUOTE_AGE_MS => Err(format!(
            "fxcm: the quote of `{}` is {} s old by its own time (the limit is {} s), so it is not a live rate",
            o.instrument,
            age / 1_000,
            MAX_QUOTE_AGE_MS / 1_000
        )),
        Some(age) if age < -MAX_QUOTE_AHEAD_MS => Err(format!(
            "fxcm: the quote of `{}` is stamped {} s ahead of the server clock; the row time and the server \
             time do not share a time frame, so no age is trusted and it is not a rate",
            o.instrument,
            -age / 1_000
        )),
        Some(age) => Ok(age.max(0)),
    }
}

/// The regular forex row called `name` (`EUR/USD`), subscribed or not.
fn listed_forex<'a>(offers: &'a [Offer], name: &str) -> Option<&'a Offer> {
    offers.iter().find(|o| o.instrument == name && o.instrument_type == INSTRUMENT_FOREX)
}

/// The one subscribed forex pair of `quote` and `account` in either orientation, or why there is not exactly one.
fn conversion_pair<'a>(
    offers: &'a [Offer],
    quote: &str,
    account: &str,
) -> Result<(&'a Offer, Pair), String> {
    let names = [
        (format!("{quote}/{account}"), Pair::QuoteIsBase),
        (format!("{account}/{quote}"), Pair::AccountIsBase),
    ];
    let listed = |name: &str| listed_forex(offers, name);
    let subscribed: Vec<(&Offer, Pair)> = names
        .iter()
        .filter_map(|(name, pair)| {
            listed(name).filter(|o| o.subscription == "T").map(|o| (o, *pair))
        })
        .collect();
    match subscribed.as_slice() {
        [one] => Ok(*one),
        [] => {
            let seen: Vec<String> = names
                .iter()
                .filter_map(|(name, _)| {
                    listed(name)
                        .map(|o| format!("{} (subscription `{}`)", o.instrument, o.subscription))
                })
                .collect();
            Err(if seen.is_empty() {
                format!(
                    "fxcm lists no {quote}/{account} or {account}/{quote} pair, so one {quote} cannot be \
                     converted to {account}"
                )
            } else {
                format!(
                    "fxcm's conversion pair for {quote} and {account} is not subscribed for trading: {}",
                    seen.join(", ")
                )
            })
        }
        _ => Err(format!(
            "fxcm lists BOTH {quote}/{account} and {account}/{quote} as subscribed, so the conversion pair \
             is ambiguous and no rate is chosen"
        )),
    }
}

/// The hub a rate is composed through when FXCM lists no pair of the quote currency and the ledger's.
const THROUGH: &str = "USD";

/// **The conversions a quote currency's rate is made of**: one, `quote -> account`, when FXCM LISTS a pair of the two
/// in either orientation (subscribed or not: a listed pair that is not tradable is a subscription's to fix and is
/// never bypassed) or when USD is one of them (a missing pair is then `conversion_pair`'s refusal); else two,
/// `quote -> USD` then `USD -> account` (owner, 2026-10-26: "a user must be able to trade ANY pair of every venue, a
/// real account is not always USD": FXCM lists no HKD, ZAR, MXN or CNH pair of a euro ledger). The one rule of the
/// rate ([`quote_rate`]) and of the rows the bridge subscribes (`crate::subscription::wanted`).
#[must_use]
pub fn hops<'a>(offers: &[Offer], quote: &'a str, account: &'a str) -> Vec<(&'a str, &'a str)> {
    let listed = |a: &str, b: &str| {
        listed_forex(offers, &format!("{a}/{b}")).is_some()
            || listed_forex(offers, &format!("{b}/{a}")).is_some()
    };
    if quote == THROUGH || account == THROUGH || quote == account || listed(quote, account) {
        vec![(quote, account)]
    } else {
        vec![(quote, THROUGH), (THROUGH, account)]
    }
}

/// **The rate of one unit of `quote_ccy` in `account_ccy`, from FXCM's own quote of the conversion pair**,
/// stamped with the quote's own moment: `as_of_ms` (when the venue was asked) minus the row's age
/// ([`quote_age`]). The mid of the pair, inverted when the pair is written account-currency first.
///
/// **Through USD** ([`hops`]) when FXCM lists no pair of the two currencies: the product of the rate of `quote_ccy`
/// in USD and the rate of USD in `account_ccy`, each leg judged exactly like a single pair (subscribed `T`, live,
/// uncrossed, no older than [`MAX_QUOTE_AGE_MS`]) and the product stamped with the OLDEST leg's moment. HKD on a euro
/// ledger is `1 / mid(USD/HKD) * 1 / mid(EUR/USD)`: with the mids 7.84451 and 1.118035, 0.1140194 euro per dirham.
///
/// # Errors
/// There is no (or more than one) subscribed pair, the pair's quote is not a live two-sided one, or its own time
/// says it is older than [`MAX_QUOTE_AGE_MS`] or cannot be told; through USD, the same of one leg, which the words
/// name (`the HKD to USD leg`, `the USD to EUR leg`).
pub fn quote_rate(
    offers: &[Offer],
    quote_ccy: &str,
    account_ccy: &str,
    as_of_ms: i64,
) -> Result<QuoteRate, String> {
    let legs = hops(offers, quote_ccy, account_ccy);
    if let [(quote, account)] = legs.as_slice() {
        return pair_rate(offers, quote, account, as_of_ms);
    }
    let (mut rate, mut oldest) = (1.0, as_of_ms);
    for (from, to) in legs {
        let leg = pair_rate(offers, from, to, as_of_ms).map_err(|why| {
            format!(
                "fxcm: {quote_ccy} is converted to {account_ccy} through {THROUGH} (no \
                 {quote_ccy}/{account_ccy} or {account_ccy}/{quote_ccy} pair is listed), and the {from} to {to} \
                 leg has no rate: {why}"
            )
        })?;
        rate *= leg.account_per_quote.get();
        oldest = oldest.min(leg.as_of_ms);
    }
    let account_per_quote = AccountPerQuote::new(rate).ok_or_else(|| {
        format!(
            "fxcm: the rate of {quote_ccy} in {account_ccy} composed through {THROUGH} is not a finite \
             positive number"
        )
    })?;
    Ok(QuoteRate { quote_ccy: quote_ccy.to_string(), account_per_quote, as_of_ms: oldest })
}

/// The rate of one unit of `quote_ccy` in `account_ccy` from the ONE subscribed pair of the two
/// ([`conversion_pair`]): [`quote_rate`] of a single hop, and each leg of a composed one.
fn pair_rate(
    offers: &[Offer],
    quote_ccy: &str,
    account_ccy: &str,
    as_of_ms: i64,
) -> Result<QuoteRate, String> {
    let (offer, pair) = conversion_pair(offers, quote_ccy, account_ccy)?;
    let mid = mid_of(offer)?;
    let age_ms = quote_age(offer)?;
    let rate = match pair {
        Pair::QuoteIsBase => mid,
        Pair::AccountIsBase => 1.0 / mid,
    };
    let account_per_quote = AccountPerQuote::new(rate).ok_or_else(|| {
        format!(
            "fxcm: the rate derived from `{}` is not a finite positive number",
            offer.instrument
        )
    })?;
    Ok(QuoteRate {
        quote_ccy: quote_ccy.to_string(),
        account_per_quote,
        as_of_ms: as_of_ms.saturating_sub(age_ms),
    })
}

// ---- a symbol's facts --------------------------------------------------------------------------------

/// What the gatewayhub answers for a symbol of a node that converts a quote currency: the grid, and the rate
/// when the symbol is quoted in another currency than the account's (`None` = quoted in the account's own).
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertedFacts {
    /// Tick from the price precision, lot from the account's base unit size.
    pub properties: SymbolProperties,
    /// The quote currency and its rate, absent at par.
    pub quote: Option<QuoteRate>,
    /// The ledger currency this answer is expressed in: the code of the `AccountCcy` the facts were judged
    /// for. The gatewayhub stamps it on the wire so the node can refuse a rate that is not in the currency its
    /// engine was anchored to.
    pub account_ccy: String,
    /// What one unit of quantity stands for, when it is not 1: a CFD-class row's contract multiplier
    /// ([`contract_value_of`]). `None` for a forex pair.
    pub contract_value: Option<ContractValue>,
    /// The symbol's own initial-margin RATE (`margin_per_lot / the lot's value`, in the account's currency, within
    /// `(0, 1]`) when the venue states a margin for it, for a CFD-class row only; a forex pair keeps the engine's rate.
    pub margin_rate: Option<f64>,
}

/// Why the facts of a symbol could not be answered, split by WHAT is wrong: the node remembers a verdict on
/// a symbol (it is not a regular forex instrument, it is not listed) but must not remember that a rate was
/// unavailable now (a closed market on a Saturday, a refresh that timed out).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactsError {
    /// The symbol itself cannot be admitted: not in the Offers table, not subscribed, not a regular forex
    /// instrument, or without a usable grid.
    Symbol(String),
    /// The symbol is fine and its conversion is not available NOW: the table or the currency could not be
    /// read, or the conversion pair has no live two-sided quote.
    Rate(String),
}

impl FactsError {
    /// The words, in the operator's language.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Symbol(m) | Self::Rate(m) => m,
        }
    }

    /// `true` when the failure is the rate's (transient), not a verdict on the symbol.
    #[must_use]
    pub fn is_rate(&self) -> bool {
        matches!(self, Self::Rate(_))
    }
}

impl std::fmt::Display for FactsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// Whether FXCM lists a regular forex pair with `code` in it: a currency the bridge can convert. A crypto asset such as
/// the BTC of `ETH/BTC` is quoted in a "currency" no forex pair carries.
fn forex_lists(offers: &[Offer], code: &str) -> bool {
    offers
        .iter()
        .filter(|o| o.instrument_type == INSTRUMENT_FOREX)
        .any(|o| pair_of(&o.instrument).is_some_and(|(base, quote)| base == code || quote == code))
}

/// The initial-margin RATE of one lot of a CFD-class row, in the account's currency: the venue's margin for the lot over
/// the lot's value (`mid * base_unit * multiplier * rate`), or `None` when the venue states no margin, the quote is not a
/// positive number, or the ratio is outside `(0, 1]` (a rate the engine would ignore anyway).
fn margin_rate_of(row: &Offer, margin_per_lot: Option<f64>, account_per_quote: f64) -> Option<f64> {
    let margin = margin_per_lot?;
    let mid = (row.bid + row.ask) / 2.0;
    let value = mid * row.base_unit as f64 * row.contract_multiplier * account_per_quote;
    let rate = margin / value;
    (mid.is_finite() && mid > 0.0 && value.is_finite() && value > 0.0 && rate > 0.0 && rate <= 1.0)
        .then_some(rate)
}

/// **The facts of `symbol` (engine spelling, `USDJPY` or `USD/JPY`, `US30`, `XAUUSD`, `USOIL`) from one fresh Offers
/// read**, for an account whose money is `account`: its grid (tick `10^-digits`, lot one base unit size, the venue's
/// own minimum and maximum order when the shim sends them), what one unit of quantity stands for, and, if it is
/// quoted in another currency, the rate. All of it comes from the SAME table, so the instrument and its conversion
/// pair are one consistent moment.
///
/// The row is found by the engine's own spelling of its name and judged by its CLASS ([`Class`]): a forex pair as it
/// always was, every other class (metals, indices, commodities, treasuries, FX indices, share and crypto CFDs) by the
/// SDK's own columns (its quote currency [`quote_currency_of`], its multiplier [`contract_value_of`], its terms
/// [`Terms`]). A CFD-class row without terms (a shim older than them) is not admitted: its minimum and maximum are
/// unknown.
///
/// # Errors
/// [`FactsError::Symbol`]: the symbol is not listed, names several rows, is not subscribed, is of no class the SDK
/// names, has no usable grid or lot, or is quoted in something FXCM has no forex pair for (a crypto asset).
/// [`FactsError::Rate`]: no rate can be had for it now, or the installed shim sends no terms.
pub fn facts_from_offers(
    offers: &[Offer],
    symbol: &str,
    account: &AccountCcy,
    as_of_ms: i64,
) -> Result<ConvertedFacts, FactsError> {
    let asked = crate::exec::to_fxcm_instrument(symbol);
    let row = match row_for(offers, symbol) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Err(FactsError::Symbol(format!(
                "fxcm: `{asked}` is not in the venue's Offers table"
            )));
        }
        Err(why) => return Err(FactsError::Symbol(why)),
    };
    let name = row.instrument.as_str();
    if row.subscription != "T" {
        // The bridge subscribes any class the SDK names itself (`crate::subscription`); it names why it did not for
        // anything else, instead of leaving the bare status.
        let not_ours = quote_currency_of(row)
            .err()
            .map(|why| format!("; the bridge does not subscribe it: {why}"))
            .unwrap_or_default();
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` is not subscribed for trading on this login (subscription `{}`){not_ours}",
            row.subscription
        )));
    }
    let quote = quote_currency_of(row).map_err(FactsError::Symbol)?;
    let class = Class::of(row.instrument_type).unwrap_or(Class::Forex);
    if !(0..=8).contains(&row.digits) {
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` reports an unusable price precision ({})",
            row.digits
        )));
    }
    if row.base_unit <= 0 {
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` has no base unit size on the tradable account, so it has no lot"
        )));
    }
    if class != Class::Forex && row.terms.is_none() {
        return Err(FactsError::Rate(format!(
            "fxcm: `{name}` is {} and the installed shim sends no trading terms (the minimum and maximum order), \
             so it is not admitted: install the shim of this release",
            class.word()
        )));
    }
    if quote != account.code && !forex_lists(offers, &quote) {
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` ({}) is quoted in {quote}, which no forex pair FXCM lists carries, so it has no \
             conversion to {} and is not admitted",
            class.word(),
            account.code
        )));
    }
    let lot = row.base_unit as f64;
    let mut properties = SymbolProperties {
        tick_size: 10f64.powi(-row.digits),
        step_size: lot,
        min_qty: lot,
        ..SymbolProperties::default()
    };
    if let Some(terms) = &row.terms {
        // The venue's own minimum and maximum, in base units (never below the lot).
        properties.min_qty = (terms.min_qty as f64).max(lot);
        properties.max_qty = terms.max_qty as f64;
    }
    let quote = if quote == account.code {
        None
    } else {
        Some(quote_rate(offers, &quote, &account.code, as_of_ms).map_err(FactsError::Rate)?)
    };
    let margin_rate = if class == Class::Forex {
        None
    } else {
        row.terms.as_ref().and_then(|terms| {
            margin_rate_of(
                row,
                terms.margin_per_lot,
                quote.as_ref().map_or(1.0, |q| q.account_per_quote.get()),
            )
        })
    };
    Ok(ConvertedFacts {
        properties,
        quote,
        account_ccy: account.code.clone(),
        contract_value: contract_value_of(row),
        margin_rate,
    })
}

#[path = "home_conversion_tests.rs"]
#[cfg(test)]
mod home_conversion_tests;
