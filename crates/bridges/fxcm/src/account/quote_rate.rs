//! FXCM's half of decision 0183 (a symbol's money is booked in the ACCOUNT's currency): the account's
//! currency, an instrument's quote currency, and the rate between them, from what ForexConnect states.
//! PURE: it parses the two JSON reads the shim exports (`forex_connect_system_properties`, `forex_connect_offers`) and judges
//! them; nothing here logs in, opens a socket or touches the FFI ([`crate::account::reads`] is the session half).
//!
//! # What the venue states, and where (the SDK's own documentation, `vendor/fcsdk/doc`)
//!
//! * **The account's currency** is the system property `BASE_CRNCY` ("the base currency of the system ...
//!   the currency of the account", e.g. `USD`; available "immediately after login"). The Accounts table has
//!   NO currency column: its balance is "expressed in the account currency" and the currency is never named.
//!   [`account_ccy`] reads the property and, when the API cannot give it, ASSUMES USD under the owner's
//!   ruling of 2026-10-19 ("check the currency by API; if you can't, USD"): the assumption is a distinct
//!   [`AccountCurrency::Assumed`] that travels with the value and is said out loud, never a silent default.
//! * **An instrument's quote currency**: the Offers table's `ContractCurrency` is the BASE currency of a
//!   regular forex pair ("`EUR` in `EUR/USD`") and its price is "the counter currency per one unit of base
//!   currency", so a forex pair (type 1, multiplier 1) is quoted in the second half of its name, cross-checked
//!   against `ContractCurrency`. Every OTHER class is a CFD, priced "in the instrument's native currency per one
//!   contract", and for those the SDK's own column IS that currency (MEASURED on the 449 non-forex rows of the
//!   EUR demo, 2026-10-30: `GER30` EUR, `JPN225` JPY, `HKG33` HKD, `USOil` USD, `XAU/USD` USD, `BTC/EUR` EUR,
//!   `AAPL.ext` USD, and where the name is a pair its second half agrees on all of them): [`quote_currency_of`].
//!   The CLASS is the row's `InstrumentType` ([`InstrumentType`]), never its name.
//! * **The rate** is the two-sided quote of the conversion pair in the SAME fresh Offers read
//!   ([`quote_rate`]): no extra session, no extra login. Orientation is named in every type: an
//!   [`AccountPerQuote`] is ACCOUNT-currency units per ONE QUOTE-currency unit (USD/JPY on a USD account
//!   is about `1/150`, never `150`: the 22,500x bug).
//! * **Through USD** when FXCM lists no pair of the quote currency and the ledger's (the EUR demo lists
//!   no HKD, ZAR, MXN or CNH pair of EUR): the rate is the product of the quote currency's rate in USD and
//!   USD's rate in the ledger's, both read from the SAME table ([`hops`], [`quote_rate`]). Where a direct pair
//!   is listed nothing changes, bit for bit. The pick is the one every converting bridge shares
//!   (`vike_model::ConversionRoute::pick`); FXCM's listing is the only part of it written here.
//!
//! # Fail closed
//!
//! No usable rate is no admission: a missing pair, an unsubscribed one, a closed market, a one-sided,
//! zero, negative, non-finite or crossed quote, or TWO candidate pairs (`Q/A` and `A/Q` both listed)
//! is an `Err` in the operator's words, never a guess at par. A composed rate needs BOTH legs, each judged as a
//! single pair is, and the words name the leg that failed.

use serde_json::Value;
use vike_bridge_core::quote_rate::SymbolPropertiesQuote;
use vike_math::powers::power_of_ten;
use vike_model::{
    AccountCurrency, AccountPerQuote, CONVERSION_HUB, ContractValue, ConversionLeg,
    ConversionRefusal, ConversionRoute, QuoteRate, SymbolProperties, currency_code, same_currency,
};

use crate::refusals::FxcmSymbolRefusal;

/// The system property that carries the account's currency (`IO2GSystemPropertiesReader`'s table).
pub const BASE_CURRENCY_PROPERTY: &str = "BASE_CRNCY";

/// `O2GInstrumentType::InstrumentForex`: a regular forex instrument, the class whose quote currency is the second half
/// of its name and whose conversion pairs the rate is made of.
pub const INSTRUMENT_FOREX: i32 = 1;

/// **The class of an instrument, from the SDK's own `O2GInstrumentType`** (`vendor/fcsdk/.../O2GEnum.h`), never from
/// the name: `US30` and `Copper` say nothing, the row's type does. The nine the SDK names; a number outside them is no
/// class this bridge knows ([`InstrumentType::of`] is `None`) and the instrument is refused by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentType {
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

impl InstrumentType {
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

/// The keys of one row of `forex_connect_offers` (the shim's `snprintf` literal), held equal to the C++ by
/// `tests/fxcm_shim/fxcm_shim_conversion_reads.rs`.
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

/// The keys of the SECOND half of a `forex_connect_offers` row, the instrument's TERMS (the same `snprintf` literal): sizes in lots,
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

// ---- the account's currency ----------------------------------------------------------------------

/// What reading the login's system properties came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertiesRead {
    /// The shim's JSON object of property name to value.
    Json(String),
    /// The API cannot state them (no session was registered, a shim installed before `forex_connect_system_properties`,
    /// a read the venue's library failed): the reason, in words. This is the API's silence, which the owner's
    /// ruling fills with an ASSUMED USD.
    Unreadable(String),
    /// The session did not ANSWER (a timeout, the session thread gone): the reason, in words. This is not the
    /// API's silence but ours, so nothing is assumed over it: [`account_ccy`] refuses, and the caller asks again
    /// later.
    Failed(String),
}

/// **The account's currency, by the owner's ruling**: the venue's `BASE_CRNCY` when it states one; USD, ASSUMED
/// and flagged, when the API cannot give it (the property is absent, the shim predates the read, there is no
/// session to ask); an `Err` when it states something that is NOT a currency code (a malformed answer is not
/// "silence": guessing over it would be a guess over a wrong venue).
///
/// # Errors
/// The session did not answer ([`PropertiesRead::Failed`]), the venue's system properties are not a JSON object,
/// or `BASE_CRNCY` is not a currency code (`vike_model::currency_code`).
pub fn account_ccy(read: PropertiesRead) -> Result<AccountCurrency, FxcmSymbolRefusal> {
    let body = match read {
        PropertiesRead::Unreadable(why) => return Ok(AccountCurrency::Assumed { why }),
        PropertiesRead::Failed(why) => return Err(FxcmSymbolRefusal::CurrencyUnanswered { why }),
        PropertiesRead::Json(body) => body,
    };
    let value: Value = serde_json::from_str(&body)
        .map_err(|e| FxcmSymbolRefusal::PropertiesNotJson { error: e.to_string() })?;
    let Some(object) = value.as_object() else {
        return Err(FxcmSymbolRefusal::PropertiesNotObject);
    };
    // `of_venue` reads the word through `currency_code`, the reading this property always had.
    Ok(AccountCurrency::of_venue(
        object.get(BASE_CURRENCY_PROPERTY).and_then(Value::as_str),
        || {
            format!(
                "the venue's system properties carry no {BASE_CURRENCY_PROPERTY} ({} properties read)",
                object.len()
            )
        },
    ))
}

// ---- the Offers table ------------------------------------------------------------------------------

/// One row of the current Offers table, as `forex_connect_offers` writes it.
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
    /// The row's `OfferID` (the second half's `offer_id`; `None` from a shim older than it, or empty): what a cancel of a
    /// resting entry a previous process placed needs (`crate::execution`'s `adopt_taught`).
    pub offer_id: Option<String>,
    /// The row's pip (`PointSize`, the second half's `point_size`): `None` from a shim older than it, or a value that
    /// is not a positive number. Read by the instrument list only (`crate::catalog`).
    pub point_size: Option<f64>,
}

/// What FXCM's trading settings say about one instrument for the tradable account: the second half of an `forex_connect_offers` row
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
        offer_id: text(v, "offer_id").filter(|id| !id.is_empty()),
        point_size: number(v, "point_size").filter(|p| p.is_finite() && *p > 0.0),
    })
}

/// The Offers table `forex_connect_offers` wrote. A row missing a key or holding a value of the wrong type is SKIPPED
/// (and said once): it can then never be the pair a rate is built from, so a malformed row costs the
/// instrument it names, not the table.
///
/// # Errors
/// The body is not a JSON array.
pub fn parse_offers(body: &str) -> Result<Vec<Offer>, FxcmSymbolRefusal> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| FxcmSymbolRefusal::OffersNotJson { error: e.to_string() })?;
    let rows = value.as_array().ok_or(FxcmSymbolRefusal::OffersNotArray)?;
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

/// `("EUR", "USD")` from `EUR/USD`: two upper-case currency codes around one slash.
fn pair_of(instrument: &str) -> Option<(String, String)> {
    let (base, quote) = instrument.split_once('/')?;
    let (base, quote) = (currency_code(base)?, currency_code(quote)?);
    (instrument.trim() == format!("{base}/{quote}")).then_some((base, quote))
}

/// **The instrument's quote currency, from what the venue states, or a refusal naming why it is not
/// determinable.** Decided by the row's CLASS ([`InstrumentType`], the SDK's `InstrumentType`), never by its name:
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
/// disagree, or a CFD's contract currency is not a currency code (`vike_model::currency_code`).
pub fn quote_currency_of(o: &Offer) -> Result<String, FxcmSymbolRefusal> {
    let instrument = || o.instrument.clone();
    let Some(class) = InstrumentType::of(o.instrument_type) else {
        return Err(FxcmSymbolRefusal::UnknownType {
            instrument: instrument(),
            instrument_type: o.instrument_type,
        });
    };
    if class != InstrumentType::Forex {
        return native_currency_of(o, class);
    }
    if (o.contract_multiplier - 1.0).abs() > 1e-9 {
        return Err(FxcmSymbolRefusal::ForexMultiplier {
            instrument: instrument(),
            multiplier: o.contract_multiplier,
        });
    }
    let Some((base, quote)) = pair_of(&o.instrument) else {
        return Err(FxcmSymbolRefusal::NotAPair { instrument: instrument() });
    };
    match currency_code(&o.contract_currency) {
        Some(contract) if contract == base => Ok(quote),
        _ => Err(FxcmSymbolRefusal::ContractCurrencyDisagrees {
            instrument: instrument(),
            contract_currency: o.contract_currency.clone(),
            base,
        }),
    }
}

/// The native (quote) currency of a CFD-class row: its `ContractCurrency`, cross-checked against the second half of a
/// pair-shaped name.
fn native_currency_of(o: &Offer, class: InstrumentType) -> Result<String, FxcmSymbolRefusal> {
    let (instrument, class) = (o.instrument.clone(), class.word());
    if !(o.contract_multiplier.is_finite() && o.contract_multiplier > 0.0) {
        return Err(FxcmSymbolRefusal::CfdMultiplier {
            instrument,
            class,
            multiplier: o.contract_multiplier,
        });
    }
    let Some(native) = currency_code(&o.contract_currency) else {
        return Err(FxcmSymbolRefusal::CfdCurrencyNotACode {
            instrument,
            class,
            contract_currency: o.contract_currency.clone(),
        });
    };
    if let Some((_, named)) = pair_of(&o.instrument)
        && named != native
    {
        return Err(FxcmSymbolRefusal::CfdNameDisagrees { instrument, class, named, native });
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
    let class = InstrumentType::of(o.instrument_type)?;
    (class != InstrumentType::Forex && (o.contract_multiplier - 1.0).abs() > 1e-9)
        .then(|| ContractValue::linear(o.contract_multiplier))
}

/// **What one unit of the MOUNTED symbol stands for**, read from its own row of `offers` by [`contract_value_of`],
/// the rule an admitted symbol is answered by ([`facts_from_offers`]): `Ok(None)` for a forex pair and a multiplier
/// of 1, the row's `ContractMultiplier` otherwise. `Err` when no row names the symbol, or its `InstrumentType` is
/// no class this bridge knows: the mount then does not run, rather than book a CFD at 1.
///
/// # Errors
/// The symbol has no row, more than one, or a row of an unknown type.
pub fn mount_contract_value(
    offers: &[Offer],
    symbol: &str,
) -> Result<Option<ContractValue>, FxcmSymbolRefusal> {
    let row = row_for(offers, symbol)?
        .ok_or_else(|| FxcmSymbolRefusal::NotInOffers { symbol: symbol.to_string() })?;
    if InstrumentType::of(row.instrument_type).is_none() {
        return Err(FxcmSymbolRefusal::MountTypeUnknown {
            instrument: row.instrument.clone(),
            instrument_type: row.instrument_type,
        });
    }
    Ok(contract_value_of(row))
}

/// The ONE row of `offers` whose USER name is `symbol` (decision 0290: `crate::instrument::names::user_name_of_offer`,
/// the translator over the row the catalog makes of it, ignoring ASCII case as the translator does), so `USOIL.CFD`
/// finds `USOil`, `XAUUSD` finds `XAU/USD` and `NVDA.EXT.CFD` finds `NVDA.ext`; the venue's own spelling is no user
/// name and finds nothing. Read off the table in hand, so it needs no kept list. `None` when no row has that name (a
/// row the catalog leaves out has none); more than one is `Err` with the names (FXCM lists none today).
pub(crate) fn row_for<'a>(
    offers: &'a [Offer],
    symbol: &str,
) -> Result<Option<&'a Offer>, FxcmSymbolRefusal> {
    let want = symbol.trim();
    let mut named = offers.iter().filter(|o| {
        crate::instrument::names::user_name_of_offer(o)
            .is_some_and(|user| user.eq_ignore_ascii_case(want))
    });
    let first = named.next();
    match (first, named.next()) {
        (Some(a), Some(b)) => Err(FxcmSymbolRefusal::SeveralRows {
            symbol: symbol.to_string(),
            first: a.instrument.clone(),
            second: b.instrument.clone(),
        }),
        (first, _) => Ok(first),
    }
}

// ---- the rate -------------------------------------------------------------------------------------

/// The mid of a live two-sided quote, or why the quote is not one.
fn mid_of(o: &Offer) -> Result<f64, FxcmSymbolRefusal> {
    live_sides(o).map(|(bid, ask)| (bid + ask) / 2.0)
}

/// **The two sides of a live quote, or why the row has none**: the market open (`O`), BOTH sides tradable (`T`: an
/// indicative side `I`, an unavailable one `N`, an empty or unknown flag are not), both sides finite and
/// positive, the ask not below the bid. The ONE judgement of "is this row a quote" that the conversion rate
/// ([`quote_rate`]) and the price lane ([`crate::feed`]) share; the row's AGE is [`quote_age`]'s.
///
/// # Errors
/// The market is not open, a side is not tradable, a side is not a positive number, or the quote is crossed.
pub fn live_sides(o: &Offer) -> Result<(f64, f64), FxcmSymbolRefusal> {
    let instrument = || o.instrument.clone();
    if o.trading_status != "O" {
        return Err(FxcmSymbolRefusal::MarketNotOpen {
            instrument: instrument(),
            status: o.trading_status.clone(),
        });
    }
    if o.bid_tradable != "T" || o.ask_tradable != "T" {
        return Err(FxcmSymbolRefusal::SideNotTradable {
            instrument: instrument(),
            bid: o.bid_tradable.clone(),
            ask: o.ask_tradable.clone(),
        });
    }
    let good = |p: f64| p.is_finite() && p > 0.0;
    if !good(o.bid) || !good(o.ask) {
        return Err(FxcmSymbolRefusal::NotTwoSided {
            instrument: instrument(),
            bid: o.bid,
            ask: o.ask,
        });
    }
    if o.bid > o.ask {
        return Err(FxcmSymbolRefusal::Crossed {
            instrument: instrument(),
            bid: o.bid,
            ask: o.ask,
        });
    }
    Ok((o.bid, o.ask))
}

/// **How old the quote is, by its own time**, or why that cannot be told: the age (clamped to 0 when it reads a
/// little ahead of the server clock) of a row whose quote is no older than the engine's staleness window
/// ([`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`]: the two agree because [`quote_rate`] stamps a rate with the QUOTE's
/// time, not the ask's). Fails CLOSED: a row with no usable time, a quote older than the limit (a frozen price while the
/// status still reads open; the first seconds after a weekend open, which still carry Friday's price), and a quote that
/// reads more than [`vike_execution::QUOTE_RATE_FUTURE_SKEW_MS`] ahead (a few seconds is clock jitter; more is the row
/// time and the server time not in one frame, so no age can be trusted) are each refused in words.
///
/// # Errors
/// The row has no usable time, is older than [`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`], or reads more than
/// [`vike_execution::QUOTE_RATE_FUTURE_SKEW_MS`] ahead of the server clock.
pub fn quote_age(o: &Offer) -> Result<i64, FxcmSymbolRefusal> {
    let instrument = o.instrument.clone();
    match o.quote_age_ms {
        None => Err(FxcmSymbolRefusal::QuoteUntimed { instrument }),
        Some(age) if age > vike_execution::DEFAULT_QUOTE_RATE_TTL_MS => {
            Err(FxcmSymbolRefusal::QuoteStale {
                instrument,
                age_s: age / 1_000,
                limit_s: vike_execution::DEFAULT_QUOTE_RATE_TTL_MS / 1_000,
            })
        }
        Some(age) if age < -vike_execution::QUOTE_RATE_FUTURE_SKEW_MS => {
            Err(FxcmSymbolRefusal::QuoteAhead { instrument, ahead_s: -age / 1_000 })
        }
        Some(age) => Ok(age.max(0)),
    }
}

/// The regular forex row called `name` (`EUR/USD`), subscribed or not.
fn listed_forex<'a>(offers: &'a [Offer], name: &str) -> Option<&'a Offer> {
    offers.iter().find(|o| o.instrument == name && o.instrument_type == INSTRUMENT_FOREX)
}

/// Whether FXCM lists a regular forex pair of `a` and `b` in either orientation, subscribed or not.
fn listed_either(offers: &[Offer], a: &str, b: &str) -> bool {
    listed_forex(offers, &format!("{a}/{b}")).is_some()
        || listed_forex(offers, &format!("{b}/{a}")).is_some()
}

/// **The conversions a quote currency's rate is made of**: FXCM's listing bound to the ONE pick every converting bridge
/// shares (`vike_model::ConversionRoute::pick`). One, `quote -> account`, when FXCM LISTS a pair of the two in either
/// orientation (subscribed or not: a listed pair that is not tradable is a subscription's to fix and is never bypassed);
/// else two, `quote -> USD` then `USD -> account` (owner, 2026-10-26: "a user must be able to trade ANY pair of every
/// venue, a real account is not always USD": FXCM lists no HKD, ZAR, MXN or CNH pair of a euro ledger); a pair listed
/// BOTH ways round is refused, never chosen between (owner, 2026-11-22). The one rule of the rate ([`quote_rate`]) and
/// of the rows the bridge subscribes (`crate::instrument::subscription::wanted`).
///
/// # Errors
/// [`ConversionRefusal`]: a code that is empty or one currency twice, a pair listed both ways, or no route.
pub fn hops<'a>(
    offers: &'a [Offer],
    quote: &str,
    account: &str,
) -> Result<ConversionRoute<&'a Offer>, ConversionRefusal> {
    ConversionRoute::pick(quote, account, |base, counter| {
        listed_forex(offers, &format!("{base}/{counter}"))
    })
}

/// The refusal of a refused route of `quote` into `account` ([`hops`]), in the words a leg's own refusal had before
/// the pick was shared: a missing direct pair, the leg through USD that FXCM lists in neither orientation, or a pair
/// listed both ways.
fn route_refusal(
    offers: &[Offer],
    quote: &str,
    account: &str,
    refusal: ConversionRefusal,
) -> FxcmSymbolRefusal {
    let unlisted = |from: &str, to: &str| FxcmSymbolRefusal::NoPair {
        from: from.to_string(),
        to: to.to_string(),
    };
    match refusal {
        ConversionRefusal::NoPair
            if same_currency(quote, CONVERSION_HUB) || same_currency(account, CONVERSION_HUB) =>
        {
            unlisted(quote, account)
        }
        ConversionRefusal::NoPair => {
            // The route through USD lacks a whole leg: name the first one FXCM lists in neither orientation.
            let (from, to) = if listed_either(offers, quote, CONVERSION_HUB) {
                (CONVERSION_HUB, account)
            } else {
                (quote, CONVERSION_HUB)
            };
            through_words(quote, account, from, to, unlisted(from, to))
        }
        ConversionRefusal::BothOrientations { from, to } => {
            let direct = same_currency(&from, quote) && same_currency(&to, account);
            let why = FxcmSymbolRefusal::BothListed { from: from.clone(), to: to.clone() };
            if direct { why } else { through_words(quote, account, &from, &to, why) }
        }
        ConversionRefusal::NotACurrency => FxcmSymbolRefusal::NotCurrencyCodes {
            quote: quote.to_string(),
            account: account.to_string(),
        },
        ConversionRefusal::SameCurrency => {
            FxcmSymbolRefusal::SameCurrency { quote: quote.to_string() }
        }
    }
}

/// The refusal of a route through USD whose leg `from -> to` has no rate (`why`).
fn through_words(
    quote: &str,
    account: &str,
    from: &str,
    to: &str,
    why: FxcmSymbolRefusal,
) -> FxcmSymbolRefusal {
    FxcmSymbolRefusal::ThroughHub {
        quote: quote.to_string(),
        account: account.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        why: Box::new(why),
    }
}

/// **The rate of one unit of `quote_currency` in `account_currency`, from FXCM's own quote of the conversion pair**,
/// stamped with the quote's own moment: `as_of_ms` (when the venue was asked) minus the row's age
/// ([`quote_age`]), on this process's clock, so the rate is never newer than the quote. The mid of the pair,
/// inverted when the pair is written account-currency first. A quote frozen while the market status still says `O`
/// ages here and is refused past [`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`]; it is not stamped fresh by the refresh
/// that fetched it. The row's time and the server's are compared only as a DIFFERENCE (the zone cancels when they share
/// one, and a frame that is not shared reads as hours and is refused).
///
/// **Through USD** ([`hops`]) when FXCM lists no pair of the two currencies: the product of the rate of `quote_currency`
/// in USD and the rate of USD in `account_currency`, each leg judged exactly like a single pair (subscribed `T`, live,
/// uncrossed, no older than [`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`]) and the product stamped with the OLDEST
/// leg's moment. HKD on a euro ledger is `1 / mid(USD/HKD) * 1 / mid(EUR/USD)`: with the mids 7.84451 and 1.118035,
/// 0.1140194 euro per dirham.
///
/// # Errors
/// There is no pair, the pair is listed both ways or not subscribed, its quote is not a live two-sided one, or its own
/// time says it is older than [`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`] or cannot be told; through USD, the same of
/// one leg, which the words name (`the HKD to USD leg`, `the USD to EUR leg`).
pub fn quote_rate(
    offers: &[Offer],
    quote_ccy: &str,
    account_ccy: &str,
    as_of_ms: i64,
) -> Result<QuoteRate, FxcmSymbolRefusal> {
    let route = hops(offers, quote_ccy, account_ccy)
        .map_err(|refusal| route_refusal(offers, quote_ccy, account_ccy, refusal))?;
    let legs = match &route {
        ConversionRoute::Direct(leg) => return pair_rate(leg, as_of_ms),
        ConversionRoute::ThroughHub(legs) => legs,
    };
    let (mut rate, mut oldest) = (1.0, as_of_ms);
    for leg in legs {
        let got = pair_rate(leg, as_of_ms)
            .map_err(|why| through_words(quote_ccy, account_ccy, &leg.from, &leg.to, why))?;
        rate *= got.account_per_quote.get();
        oldest = oldest.min(got.as_of_ms);
    }
    let account_per_quote =
        AccountPerQuote::new(rate).ok_or_else(|| FxcmSymbolRefusal::ComposedRateNotFinite {
            quote: quote_ccy.to_string(),
            account: account_ccy.to_string(),
        })?;
    Ok(QuoteRate { quote_currency: quote_ccy.to_string(), account_per_quote, as_of_ms: oldest })
}

/// The rate of one unit of `leg.from` in `leg.to` from the pair [`hops`] picked for it, which must be subscribed for
/// trading (`T`): [`quote_rate`] of a single hop, and each leg of a composed one.
fn pair_rate(leg: &ConversionLeg<&Offer>, as_of_ms: i64) -> Result<QuoteRate, FxcmSymbolRefusal> {
    let offer = leg.pair;
    if offer.subscription != "T" {
        return Err(FxcmSymbolRefusal::PairNotSubscribed {
            from: leg.from.clone(),
            to: leg.to.clone(),
            instrument: offer.instrument.clone(),
            subscription: offer.subscription.clone(),
        });
    }
    let mid = mid_of(offer)?;
    let age_ms = quote_age(offer)?;
    let account_per_quote = AccountPerQuote::of_mid(leg.orientation, mid).map_err(|_| {
        FxcmSymbolRefusal::DerivedRateNotFinite { instrument: offer.instrument.clone() }
    })?;
    Ok(QuoteRate {
        quote_currency: leg.from.clone(),
        account_per_quote,
        as_of_ms: as_of_ms.saturating_sub(age_ms),
    })
}

// ---- a symbol's facts --------------------------------------------------------------------------------

/// Why the facts of a symbol could not be answered, split by WHAT is wrong: the node remembers a verdict on
/// a symbol (it is not a regular forex instrument, it is not listed) but must not remember that a rate was
/// unavailable now (a closed market on a Saturday, a refresh that timed out).
#[derive(Clone, Debug, PartialEq)]
pub enum FxcmPropertiesError {
    /// The symbol itself cannot be admitted: not in the Offers table, not subscribed, not a regular forex
    /// instrument, or without a usable grid.
    Symbol(FxcmSymbolRefusal),
    /// The symbol is fine and its conversion is not available NOW: the table or the currency could not be
    /// read, or the conversion pair has no live two-sided quote.
    Rate(FxcmSymbolRefusal),
}

impl FxcmPropertiesError {
    /// The refusal, whichever kind.
    #[must_use]
    pub fn refusal(&self) -> &FxcmSymbolRefusal {
        match self {
            Self::Symbol(refusal) | Self::Rate(refusal) => refusal,
        }
    }

    /// `true` when the failure is the rate's (transient), not a verdict on the symbol.
    #[must_use]
    pub fn is_rate(&self) -> bool {
        matches!(self, Self::Rate(_))
    }
}

impl std::fmt::Display for FxcmPropertiesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal().fmt(f)
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
/// the lot's value (`base_unit * mid * (multiplier * rate)`, the contract value in the account's money:
/// `ContractValue::in_account`), or `None` when the venue states no margin, the rate or the quote is not a positive
/// number, or the ratio is outside `(0, 1]` (a rate the engine would ignore anyway). At a rate of 1 (a CFD quoted in the
/// account's own currency) the value is `(base_unit * mid) * multiplier`, bit for bit what it always was.
fn margin_rate_of(row: &Offer, margin_per_lot: Option<f64>, account_per_quote: f64) -> Option<f64> {
    let margin = margin_per_lot?;
    let mid = (row.bid + row.ask) / 2.0;
    // one lot's notional in the account's currency
    let value = ContractValue::linear(row.contract_multiplier)
        .in_account(account_per_quote)?
        .signed_notional(row.base_unit as f64, mid);
    let rate = margin / value;
    (mid.is_finite() && mid > 0.0 && value.is_finite() && value > 0.0 && rate > 0.0 && rate <= 1.0)
        .then_some(rate)
}

/// **The facts of `symbol` (its USER name, `USDJPY`, `US30.CFD`, `XAUUSD`, `USOIL.CFD`) from one fresh Offers
/// read**, for an account whose money is `account`: its grid (tick `10^-digits`, lot one base unit size, the venue's
/// own minimum and maximum order when the shim sends them), what one unit of quantity stands for, and, if it is
/// quoted in another currency, the rate. All of it comes from the SAME table, so the instrument and its conversion
/// pair are one consistent moment.
///
/// The row is found by its user name ([`row_for`]) and judged by its CLASS ([`InstrumentType`]): a forex pair as it
/// always was, every other class (metals, indices, commodities, treasuries, FX indices, share and crypto CFDs) by the
/// SDK's own columns (its quote currency [`quote_currency_of`], its multiplier [`contract_value_of`], its terms
/// [`Terms`]). A CFD-class row without terms (a shim older than them) is not admitted: its minimum and maximum are
/// unknown. Only a CFD-class row answers its own margin rate (`margin_rate_of`); a forex pair keeps the engine's.
/// The answer states `account`'s code as the currency it is expressed in.
///
/// # Errors
/// [`FxcmPropertiesError::Symbol`]: the symbol is not listed, names several rows, is not subscribed, is of no class the SDK
/// names, has no usable grid or lot, or is quoted in something FXCM has no forex pair for (a crypto asset).
/// [`FxcmPropertiesError::Rate`]: no rate can be had for it now, or the installed shim sends no terms.
pub fn facts_from_offers(
    offers: &[Offer],
    symbol: &str,
    account: &AccountCurrency,
    as_of_ms: i64,
) -> Result<SymbolPropertiesQuote, FxcmPropertiesError> {
    let row = match row_for(offers, symbol) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Err(FxcmPropertiesError::Symbol(FxcmSymbolRefusal::NotInOffers {
                symbol: symbol.to_string(),
            }));
        }
        Err(why) => return Err(FxcmPropertiesError::Symbol(why)),
    };
    let name = row.instrument.clone();
    if row.subscription != "T" {
        // The bridge subscribes any class the SDK names itself (`crate::instrument::subscription`); it names why it did not for
        // anything else, instead of leaving the bare status.
        let subscription = row.subscription.clone();
        return Err(FxcmPropertiesError::Symbol(match quote_currency_of(row) {
            Ok(_) => FxcmSymbolRefusal::NotSubscribed { name, subscription },
            Err(why) => {
                FxcmSymbolRefusal::NotSubscribedNotOurs { name, subscription, why: Box::new(why) }
            }
        }));
    }
    let quote = quote_currency_of(row).map_err(FxcmPropertiesError::Symbol)?;
    let class = InstrumentType::of(row.instrument_type).unwrap_or(InstrumentType::Forex);
    if !(0..=8).contains(&row.digits) {
        return Err(FxcmPropertiesError::Symbol(FxcmSymbolRefusal::UnusablePrecision {
            name,
            digits: row.digits,
        }));
    }
    if row.base_unit <= 0 {
        return Err(FxcmPropertiesError::Symbol(FxcmSymbolRefusal::NoBaseUnit { name }));
    }
    if class != InstrumentType::Forex && row.terms.is_none() {
        return Err(FxcmPropertiesError::Rate(FxcmSymbolRefusal::NoTerms {
            name,
            class: class.word(),
        }));
    }
    if quote != account.code() && !forex_lists(offers, &quote) {
        return Err(FxcmPropertiesError::Symbol(FxcmSymbolRefusal::NoForexForQuote {
            name,
            class: class.word(),
            quote,
            account: account.code().to_string(),
        }));
    }
    let properties = grid_of(row);
    let quote = if quote == account.code() {
        None
    } else {
        Some(
            quote_rate(offers, &quote, account.code(), as_of_ms)
                .map_err(FxcmPropertiesError::Rate)?,
        )
    };
    let margin_rate = if class == InstrumentType::Forex {
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
    Ok(SymbolPropertiesQuote {
        properties,
        quote,
        account_currency: account.code().to_string(),
        contract_value: contract_value_of(row),
        margin_rate,
    })
}

/// **A row's grid**, the one rule the facts and the instrument list (`crate::catalog`) both read: tick `10^-digits`,
/// the lot one base unit size, the venue's own minimum and maximum order when the shim sends them (in base units,
/// never below the lot). For a row whose `digits` lie in `0..=8` and whose base unit is positive; the caller checks.
#[must_use]
pub fn grid_of(row: &Offer) -> SymbolProperties {
    let lot = row.base_unit as f64;
    let mut properties = SymbolProperties {
        // Outside the caller-checked `0..=8` there is no grid: `0.0` is the absent tick.
        tick_size: row.digits.checked_neg().and_then(power_of_ten).unwrap_or(0.0),
        step_size: lot,
        min_qty: lot,
        ..SymbolProperties::default()
    };
    if let Some(terms) = &row.terms {
        properties.min_qty = (terms.min_qty as f64).max(lot);
        properties.max_qty = terms.max_qty as f64;
    }
    properties
}

#[path = "quote_rate_tests.rs"]
#[cfg(test)]
mod quote_rate_tests;
