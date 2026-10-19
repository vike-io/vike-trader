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
//! * **An instrument's quote currency**: the Offers table's `ContractCurrency` is the BASE currency
//!   ("`EUR` in `EUR/USD`"); prices are "the counter currency per one unit of base currency" for a forex
//!   instrument and "the native currency per one contract" for a CFD, and the native currency of a CFD is
//!   a column nobody exposes. So only a regular forex instrument (type 1, multiplier 1) has a determinable
//!   quote currency ([`quote_currency_of`]: the second half of its name, cross-checked against
//!   `ContractCurrency`); a metal, an index or any other CFD is refused by name.
//! * **The rate** is the two-sided quote of the conversion pair in the SAME fresh Offers read
//!   ([`quote_rate`]): no extra session, no extra login. Orientation is named in every type: an
//!   [`AccountPerQuote`] is ACCOUNT-currency units per ONE QUOTE-currency unit (USD/JPY on a USD account
//!   is about `1/150`, never `150`: the 22,500x bug).
//!
//! # Fail closed
//!
//! No usable rate is no admission: a missing pair, an unsubscribed one, a closed market, a one-sided,
//! zero, negative, non-finite or crossed quote, or TWO candidate pairs (`Q/A` and `A/Q` both subscribed)
//! is an `Err` in the operator's words, never a guess at par.

use serde_json::Value;
use vike_model::SymbolProperties;

/// The system property that carries the account's currency (`IO2GSystemPropertiesReader`'s table).
pub const BASE_CURRENCY_PROPERTY: &str = "BASE_CRNCY";

/// What the account's currency is taken to be when the API cannot state it (owner ruling 2026-10-19:
/// "check the currency by API; if you can't, USD"). Only ever as [`CcySource::Assumed`].
pub const ASSUMED_ACCOUNT_CURRENCY: &str = "USD";

/// `O2GInstrumentType::InstrumentForex`: a regular forex instrument, the only class whose quote currency
/// the venue's own words determine.
pub const INSTRUMENT_FOREX: i32 = 1;

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
/// determinable.** Only a regular forex instrument (type 1, contract multiplier 1) qualifies: its price is
/// "the counter currency per one unit of base currency" (the Offers table page), the counter currency is
/// the second half of its name, and `ContractCurrency` (the BASE currency) must agree with the first half.
/// A CFD's price is "in the instrument's native currency", a column the venue does not expose, so a metal,
/// an index, a share or a crypto CFD is refused: its lot, its contract size and its money are unmeasured.
///
/// # Errors
/// The instrument is not a regular forex instrument, or its name and contract currency disagree.
pub fn quote_currency_of(o: &Offer) -> Result<String, String> {
    if o.instrument_type != INSTRUMENT_FOREX {
        return Err(format!(
            "fxcm: `{}` is an instrument of type {} (not a regular forex instrument), and the venue does \
             not state a CFD's native currency, so its quote currency is not determinable and it is \
             not converted",
            o.instrument, o.instrument_type
        ));
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
    if o.trading_status != "O" {
        return Err(format!(
            "fxcm: the market for `{}` is not open (trading status `{}`), so it states no live rate",
            o.instrument, o.trading_status
        ));
    }
    if o.bid_tradable == "N" || o.ask_tradable == "N" {
        return Err(format!(
            "fxcm: `{}` is not available for trading on a side (bid `{}`, ask `{}`), so its quote is not \
             a rate",
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
    Ok((o.bid + o.ask) / 2.0)
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
    let listed = |name: &str| {
        offers.iter().find(|o| o.instrument == name && o.instrument_type == INSTRUMENT_FOREX)
    };
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

/// **The rate of one unit of `quote_ccy` in `account_ccy`, from FXCM's own quote of the conversion pair**,
/// stamped with the quote's own moment: `as_of_ms` (when the venue was asked) minus the row's age
/// ([`quote_age`]). The mid of the pair, inverted when the pair is written account-currency first.
///
/// # Errors
/// There is no (or more than one) subscribed pair, the pair's quote is not a live two-sided one, or its own time
/// says it is older than [`MAX_QUOTE_AGE_MS`] or cannot be told.
pub fn quote_rate(
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

/// **The facts of `symbol` (engine spelling, `USDJPY` or `USD/JPY`) from one fresh Offers read**, for an
/// account whose money is `account`: its grid (tick `10^-digits`, lot and minimum one base unit size) and, if
/// it is quoted in another currency, the rate. Both come from the SAME table, so the instrument and its
/// conversion pair are one consistent moment.
///
/// # Errors
/// [`FactsError::Symbol`]: the symbol is not a subscribed regular forex instrument with a usable grid.
/// [`FactsError::Rate`]: no rate can be had for it now.
pub fn facts_from_offers(
    offers: &[Offer],
    symbol: &str,
    account: &AccountCcy,
    as_of_ms: i64,
) -> Result<ConvertedFacts, FactsError> {
    let name = crate::exec::to_fxcm_instrument(symbol);
    let Some(row) = offers.iter().find(|o| o.instrument == name) else {
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` is not in the venue's Offers table"
        )));
    };
    if row.subscription != "T" {
        return Err(FactsError::Symbol(format!(
            "fxcm: `{name}` is not subscribed for trading on this login (subscription `{}`)",
            row.subscription
        )));
    }
    let quote = quote_currency_of(row).map_err(FactsError::Symbol)?;
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
    let lot = row.base_unit as f64;
    let properties = SymbolProperties {
        tick_size: 10f64.powi(-row.digits),
        step_size: lot,
        min_qty: lot,
        ..SymbolProperties::default()
    };
    let quote = if quote == account.code {
        None
    } else {
        Some(quote_rate(offers, &quote, &account.code, as_of_ms).map_err(FactsError::Rate)?)
    };
    Ok(ConvertedFacts { properties, quote })
}

#[path = "home_conversion_tests.rs"]
#[cfg(test)]
mod home_conversion_tests;
