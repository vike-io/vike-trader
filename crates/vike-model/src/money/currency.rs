//! The currency facts every rate source and the engine share: the ONE reading of a currency code
//! ([`currency_code`], [`same_currency`]), the conversion factor's TYPE ([`AccountPerQuote`],
//! decision 0183's one scalar), the ORIENTATION of a conversion pair's mid
//! ([`AccountPerQuoteLeg`], [`AccountPerQuote::of_mid`]), the ONE pick of the pair or pairs that
//! convert a quote currency ([`ConversionRoute::pick`]), and what a rate source answers with: the
//! one record ([`QuoteRate`]) or the one refusal ([`QuoteRateError`]).
//!
//! The currency vocabulary beside them: the USD the owner's ruling fills in when nothing says
//! otherwise ([`DEFAULT_CURRENCY`]), how an account's currency is known ([`AccountCurrency`]), the
//! dollar words ([`DOLLAR_LABELS`], [`is_dollar_group`], [`ledger_label`]), the fiat codes a broker
//! quotes in ([`fiat_code`]), the minor units some venues quote in ([`MinorUnit`]) and what a
//! symbol's fiat quote is ([`FiatQuote`]).
//!
//! A currency code is any non-empty word, trimmed and upper-cased: `usd` and ` USD ` are the
//! currency `USD`, and a settlement coin (`USDT`, `USDC`) is a currency too. The engine keeps every
//! code it is handed in this spelling and compares codes a venue wrote with [`same_currency`].
//! Currencies stay STRINGS (a coin ledger is any coin, decision 0258): no enum of codes.
//!
//! Each bridge keeps its own rate SOURCE (cTrader's spot, Dukascopy's tick mid, OANDA's
//! `homeConversions`, IBKR's quote, FXCM's offers), its own choice of the pair, its own mid checks and
//! its own refusals; the type a rate is carried in and the one orientation rule (a pair written
//! `QUOTE/ACCOUNT` is read as it stands, one written `ACCOUNT/QUOTE` is inverted: flipped, it is a
//! 22,500x error on a yen) are written once here, below every bridge and below the simulator.

/// A currency code as the engine keeps it: trimmed and upper-cased, so `usd` and ` USD ` are the
/// currency `USD`. `None` for a code with nothing in it. Applied where a code ENTERS the engine (an
/// admission's `quote_currency`, the account currency the node states); the comparisons against codes a
/// venue wrote use [`same_currency`].
#[inline]
#[must_use]
pub fn currency_code(raw: &str) -> Option<String> {
    let code = raw.trim();
    (!code.is_empty()).then(|| code.to_ascii_uppercase())
}

/// Whether two currency codes name one currency (trim + ASCII case folded).
#[inline]
#[must_use]
pub fn same_currency(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// **The currency when nothing says otherwise** (the owner's ruling of 2026-10-19: "the API's
/// currency; if it cannot say, USD"): the one spelling of that USD. Each reader keeps its own rule of
/// WHEN it falls back; only the word is here.
pub const DEFAULT_CURRENCY: &str = "USD";

/// The USD Coin dollar label, as every venue writes it.
pub const USDC: &str = "USDC";

/// The Tether dollar label, as every venue writes it.
pub const USDT: &str = "USDT";

/// **The three dollar labels a venue states its money in**, compared as written (every venue writes
/// them upper-case): a balance, a fee or a premium in one of them is in dollars.
pub const DOLLAR_LABELS: [&str; 3] = [DEFAULT_CURRENCY, USDC, USDT];

/// **A ledger group key as text**: the empty key is the dollar group and reads [`DEFAULT_CURRENCY`];
/// any other key is itself.
#[inline]
#[must_use]
pub fn ledger_label(key: &str) -> &str {
    if key.is_empty() { DEFAULT_CURRENCY } else { key }
}

/// **Whether a ledger currency folds into the DOLLAR group** (the empty group key): no code,
/// [`DEFAULT_CURRENCY`] or `USDT`, trimmed and in any case. Every other code, `USDC` included, is a
/// group of its own.
#[inline]
#[must_use]
pub fn is_dollar_group(code: &str) -> bool {
    code.trim().is_empty() || same_currency(code, DEFAULT_CURRENCY) || same_currency(code, USDT)
}

/// The fiat currencies a roster FX or CFD venue quotes an instrument in: the closed set
/// `vike_catalog::fiat_quote_of` recognises. Anything outside it (a crypto asset, a stablecoin, a
/// metal, a minor unit) is NOT a fiat quote, and a name ending in one is unrecognised rather than
/// guessed at. `CLP`, `COP` and `TWD` are fxcm's committed forex rows
/// (`crates/bridges/fxcm/tests/fixtures/eur_demo_offers.json`: `USD/CLP`, `USD/COP`, `USD/TWD`),
/// which were read as stating no quote and so booked at par. The list stays whole: no second
/// "majors" list.
const FIAT_CODES: &[&str] = &[
    "AUD", "CAD", "CHF", "CLP", "CNH", "CNY", "COP", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF",
    "ILS", "INR", "JPY", "KRW", "MXN", "NOK", "NZD", "PLN", "RUB", "SEK", "SGD", "THB", "TRY",
    "TWD", "USD", "ZAR",
];

/// `token` (trimmed, any case) as a `FIAT_CODES` entry, upper-cased, or `None`: the one door a
/// ledger currency a person TYPED (`engine.account_currency = "jpy"`) goes through.
#[must_use]
pub fn fiat_code(token: &str) -> Option<&'static str> {
    let token = token.trim();
    FIAT_CODES.iter().copied().find(|code| code.eq_ignore_ascii_case(token))
}

/// **A currency some venues quote in its HUNDREDTH**: London in pence (`GBX` of `GBP`), Tel Aviv in
/// agorot (`ILA` of `ILS`), Johannesburg in cents (`ZAC` of `ZAR`). Currency knowledge, not one
/// venue's fact (`GBX` is pence of `GBP` on every venue); which venue may QUOTE in a minor unit stays
/// that venue's rule, and a price in one is 100 times the major's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinorUnit {
    /// The major currency's code (`GBP`).
    pub major: &'static str,
    /// The code a price in hundredths of it is written in (`GBX`), read in any case (`ZAc` is
    /// `ZAC`). ⚠ `GBp`, pence in some feeds, folds to the MAJOR `GBP`: a venue that writes it reads
    /// its own case.
    pub minor: &'static str,
}

/// Every [`MinorUnit`] this workspace knows.
pub const MINOR_UNITS: [MinorUnit; 3] = [
    MinorUnit { major: "GBP", minor: "GBX" },
    MinorUnit { major: "ILS", minor: "ILA" },
    MinorUnit { major: "ZAR", minor: "ZAC" },
];

impl MinorUnit {
    /// The unit whose MINOR code is `code` (trimmed, any case: `ZAc` is `ZAC`), or `None`.
    #[must_use]
    pub fn of_minor(code: &str) -> Option<Self> {
        MINOR_UNITS.iter().copied().find(|unit| same_currency(unit.minor, code))
    }

    /// The unit whose MAJOR code is `code` (trimmed, any case), or `None`: a currency a venue may
    /// quote in hundredths while it names the major.
    #[must_use]
    pub fn of_major(code: &str) -> Option<Self> {
        MINOR_UNITS.iter().copied().find(|unit| same_currency(unit.major, code))
    }
}

/// **The fiat money a symbol is quoted in**, as the kept list or its name states it
/// (`vike_catalog::kept_fiat_quote`'s answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiatQuote {
    /// A fiat currency ([`fiat_code`]'s answer, `JPY`).
    Code(&'static str),
    /// A broker's symbol whose currency must be known and is not: never a currency, so every ledger
    /// refuses it and no conversion pair books it.
    Unknown,
    /// Quoted in the minor unit of a fiat currency (`GBX`): never the major itself.
    Minor(MinorUnit),
}

/// **The account's currency and how it is known** (the owner's ruling of 2026-10-19: the API's
/// word, else [`DEFAULT_CURRENCY`]). Wire fields keep their own shape; this is the one in-process
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCurrency {
    /// The venue stated it: a code in [`currency_code`]'s spelling.
    Stated(String),
    /// The venue stated none, so [`DEFAULT_CURRENCY`] is ASSUMED; `why` is why it could not say.
    Assumed {
        /// Why the venue's word was not available, in words.
        why: String,
    },
}

impl AccountCurrency {
    /// The ruling applied to what the venue said: [`Self::Stated`] when `stated` is a code
    /// ([`currency_code`]), else [`Self::Assumed`] with `why`'s words.
    #[must_use]
    pub fn of_venue(stated: Option<&str>, why: impl FnOnce() -> String) -> Self {
        match stated.and_then(currency_code) {
            Some(code) => Self::Stated(code),
            None => Self::Assumed { why: why() },
        }
    }

    /// The code: the stated one, or [`DEFAULT_CURRENCY`].
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Stated(code) => code,
            Self::Assumed { .. } => DEFAULT_CURRENCY,
        }
    }

    /// `true` when the code is the ruling's assumption and not the venue's word.
    #[must_use]
    pub fn is_assumed(&self) -> bool {
        matches!(self, Self::Assumed { .. })
    }
}

/// **Account-currency (ledger-currency) units per ONE unit of a quote currency.** Multiply a
/// quote-currency amount by it ([`Self::to_account`]); never divide. Finite and strictly positive,
/// or it does not exist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountPerQuote(f64);

impl AccountPerQuote {
    /// The account currency converting into itself. The only literal `1.0` a bridge's rate path
    /// needs: cTrader's and OANDA's rate for the account's own currency.
    pub const IDENTITY: Self = Self(1.0);

    /// `None` unless `account_units_per_quote_unit` is finite and greater than zero.
    #[must_use]
    pub fn new(account_units_per_quote_unit: f64) -> Option<Self> {
        (account_units_per_quote_unit.is_finite() && account_units_per_quote_unit > 0.0)
            .then_some(Self(account_units_per_quote_unit))
    }

    /// The factor: account-currency units per one quote-currency unit.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// A quote-currency amount in account currency (`quote_amount * factor`).
    #[must_use]
    pub fn to_account(self, quote_amount: f64) -> f64 {
        quote_amount * self.0
    }

    /// **The rate a conversion pair's `mid` states, turned the right way round for `leg`**: the mid
    /// itself for a `QUOTE/ACCOUNT` pair, its inverse for an `ACCOUNT/QUOTE` one (USDJPY at 150 on a
    /// dollar account is `1/150` dollar per yen, never `150`). The ONE orientation rule every bridge
    /// that reads a pair calls; the mid's own checks (a side, a spread, an age) stay the venue's.
    ///
    /// # Errors
    /// The oriented figure when it is no rate ([`Self::new`] refuses it), for the caller's words.
    pub fn of_mid(leg: AccountPerQuoteLeg, mid: f64) -> Result<Self, f64> {
        let rate = match leg {
            AccountPerQuoteLeg::QuoteIsBase => mid,
            AccountPerQuoteLeg::AccountIsBase => 1.0 / mid,
        };
        Self::new(rate).ok_or(rate)
    }
}

/// Which side of a conversion pair the ACCOUNT's currency is on, which fixes how its price is read
/// ([`AccountPerQuote::of_mid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPerQuoteLeg {
    /// The pair is `QUOTE/ACCOUNT` (GBPUSD for a pound quote on a dollar account): its price is
    /// account units per quote unit already, and the rate is the mid.
    QuoteIsBase,
    /// The pair is `ACCOUNT/QUOTE` (USDJPY for a yen quote on a dollar account): its price is quote
    /// units per account unit, and the rate is the INVERSE of the mid.
    AccountIsBase,
}

impl AccountPerQuoteLeg {
    /// **Which way a conversion pair's NAME points**, read against the two codes ([`currency_code`]):
    /// `QUOTE` then `ACCOUNT` (`GBPUSD` for a pound quote, `USDT-USD` for a USDT quote on a dollar
    /// ledger) is [`Self::QuoteIsBase`], `ACCOUNT` then `QUOTE` (`USDJPY` for a yen quote) is
    /// [`Self::AccountIsBase`]. The name is read in any case with its separators (`-`, `/`, `_`, `.`)
    /// dropped, and must be the two codes and nothing else: a suffix, a third code, a code it does not
    /// name, two codes that are one currency, or a name that reads both ways is `None`, never a guess
    /// (a guess at a pair is a guess at money: decision 0243 item 3).
    #[must_use]
    pub fn of_pair_name(pair: &str, quote: &str, account: &str) -> Option<Self> {
        let (quote, account) = (currency_code(quote)?, currency_code(account)?);
        let name: String = pair
            .trim()
            .chars()
            .filter(|c| !matches!(c, '-' | '/' | '_' | '.'))
            .collect::<String>()
            .to_ascii_uppercase();
        let quote_first = name == format!("{quote}{account}");
        let account_first = name == format!("{account}{quote}");
        match (quote_first, account_first) {
            (true, false) => Some(Self::QuoteIsBase),
            (false, true) => Some(Self::AccountIsBase),
            _ => None,
        }
    }
}

/// The currency a cross is composed through when a venue lists no pair of the quote currency and
/// the account's (owner, 2026-10-26: "a user must be able to trade ANY pair of every venue, a real
/// account is not always USD").
pub const CONVERSION_HUB: &str = "USD";

/// One pair of a [`ConversionRoute`]: the venue's own record of it (`pair`, what its lookup
/// returned), the currency it converts (`from`), the one it converts into (`to`), and which way
/// round it is listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionLeg<T> {
    /// The venue's pair, as its lookup returned it.
    pub pair: T,
    /// The currency converted: the quote currency, or [`CONVERSION_HUB`] on a route's second leg.
    pub from: String,
    /// The currency converted into: the account's, or [`CONVERSION_HUB`] on a route's first leg.
    pub to: String,
    /// `FROM/TO` is [`AccountPerQuoteLeg::QuoteIsBase`], `TO/FROM` is
    /// [`AccountPerQuoteLeg::AccountIsBase`] ([`AccountPerQuote::of_mid`] reads its mid by it).
    pub orientation: AccountPerQuoteLeg,
}

/// **The pair or pairs that convert a quote currency into the account's** ([`Self::pick`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionRoute<T> {
    /// The venue lists a pair of the two currencies.
    Direct(ConversionLeg<T>),
    /// It lists none: the quote currency into [`CONVERSION_HUB`], then the hub into the account's.
    /// The rate is the product of the two legs' rates.
    ThroughHub([ConversionLeg<T>; 2]),
}

/// **Why no route converts a quote currency into the account's**: every variant is a refusal, and
/// each bridge words it in its own sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionRefusal {
    /// A code is empty ([`currency_code`]).
    NotACurrency,
    /// The two codes are one currency: there is nothing to convert.
    SameCurrency,
    /// The venue lists the pair of `from` and `to` BOTH ways round, so which one the rate is read
    /// from would be a guess (decision 0243 item 3: a guess at a pair is a guess at money).
    BothOrientations {
        /// The leg's `from` currency.
        from: String,
        /// The leg's `to` currency.
        to: String,
    },
    /// Neither a pair of the two currencies nor both legs through [`CONVERSION_HUB`] are listed.
    NoPair,
}

impl<T> ConversionRoute<T> {
    /// **THE pick of every bridge that converts** (owner, 2026-11-22): the DIRECT pair of `quote`
    /// and `account` in either orientation; else TWO legs through [`CONVERSION_HUB`], each picked by
    /// the same rule; a pair listed BOTH ways round is refused, never chosen between. `listed(base,
    /// counter)` is the venue's only part: its pair written `BASE/COUNTER` (the two codes
    /// upper-cased), or `None`. Each bridge keeps its own list, its own reading of "listed" and its
    /// own words; the route is the same for all.
    ///
    /// # Errors
    /// [`ConversionRefusal`]: a code that is empty, one currency twice, a pair listed both ways, or
    /// no route.
    pub fn pick(
        quote: &str,
        account: &str,
        mut listed: impl FnMut(&str, &str) -> Option<T>,
    ) -> Result<Self, ConversionRefusal> {
        let (Some(quote), Some(account)) = (currency_code(quote), currency_code(account)) else {
            return Err(ConversionRefusal::NotACurrency);
        };
        if quote == account {
            return Err(ConversionRefusal::SameCurrency);
        }
        if let Some(direct) = conversion_leg(&quote, &account, &mut listed)? {
            return Ok(Self::Direct(direct));
        }
        if quote == CONVERSION_HUB || account == CONVERSION_HUB {
            return Err(ConversionRefusal::NoPair);
        }
        let first = conversion_leg(&quote, CONVERSION_HUB, &mut listed)?;
        let second = conversion_leg(CONVERSION_HUB, &account, &mut listed)?;
        match (first, second) {
            (Some(first), Some(second)) => Ok(Self::ThroughHub([first, second])),
            _ => Err(ConversionRefusal::NoPair),
        }
    }

    /// The route's legs, in the order their rates multiply.
    #[must_use]
    pub fn legs(&self) -> &[ConversionLeg<T>] {
        match self {
            Self::Direct(leg) => std::slice::from_ref(leg),
            Self::ThroughHub(legs) => legs,
        }
    }
}

/// The one listed pair of `from` and `to`, `None` when neither orientation is listed.
fn conversion_leg<T>(
    from: &str,
    to: &str,
    listed: &mut impl FnMut(&str, &str) -> Option<T>,
) -> Result<Option<ConversionLeg<T>>, ConversionRefusal> {
    let leg = |pair, orientation| ConversionLeg {
        pair,
        from: from.to_string(),
        to: to.to_string(),
        orientation,
    };
    match (listed(from, to), listed(to, from)) {
        (Some(_), Some(_)) => {
            Err(ConversionRefusal::BothOrientations { from: from.to_string(), to: to.to_string() })
        }
        (Some(pair), None) => Ok(Some(leg(pair, AccountPerQuoteLeg::QuoteIsBase))),
        (None, Some(pair)) => Ok(Some(leg(pair, AccountPerQuoteLeg::AccountIsBase))),
        (None, None) => Ok(None),
    }
}

/// **What a rate source hands the node for ONE quote currency**: the factor and the moment it is
/// of. The one quote-rate record (B15c); a source with more to say (the pair it read, the session's
/// account currency) carries that beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRate {
    /// The quote currency (upper-case).
    pub quote_currency: String,
    /// Account-currency units per one `quote_currency` unit.
    pub account_per_quote: AccountPerQuote,
    /// The moment the rate is of, epoch ms: never newer than the price it was made of. Each source
    /// says which moment it stamps (the ask that read it, or the quote's own time).
    pub as_of_ms: i64,
}

impl QuoteRate {
    /// The record of `account_per_quote` for `quote_currency` at `as_of_ms`, the code normalised by
    /// [`currency_code`] (`" jpy"` is `JPY`); `None` for a code with nothing in it.
    #[must_use]
    pub fn new(
        quote_currency: &str,
        account_per_quote: AccountPerQuote,
        as_of_ms: i64,
    ) -> Option<Self> {
        Some(Self { quote_currency: currency_code(quote_currency)?, account_per_quote, as_of_ms })
    }
}

/// **Why a quote currency has no rate in the account's currency**: every variant is a refusal, none
/// carries a number to use. The ONE such error of every source that converts (B18 (2)).
///
/// Each variant carries the source's WHOLE sentence, its own prefix included (`ctrader: …`,
/// `dukascopy: …`, `oanda home conversion unavailable: …`), and [`Display`](std::fmt::Display)
/// writes it as it stands, so a bridge's error text stays byte for byte what it was. The KIND is
/// what a caller may act on: only [`Self::NoPair`] is a verdict about the instrument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteRateError {
    /// No rate could be had NOW: no session or credentials, a transport or venue error, no quote in
    /// time, no account currency known. About the moment: the next ask may find one.
    Unavailable(String),
    /// The venue converts the two currencies through no single pair (none, or two to choose
    /// between), or a code is no currency or both are the same one. About the INSTRUMENT: the one
    /// kind a caller may remember as a refusal of the symbol.
    NoPair(String),
    /// The venue answered, but not with a usable rate: a side absent or zero, crossed, non-finite,
    /// untimed or too old, a suspended pair, a conversion entry missing, unusable or listed twice, a
    /// reply that carries no conversions at all. About the answer, not the instrument.
    BadQuote(String),
}

impl std::fmt::Display for QuoteRateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(words) | Self::NoPair(words) | Self::BadQuote(words) => {
                f.write_str(words)
            }
        }
    }
}

impl std::error::Error for QuoteRateError {}

#[path = "currency_tests.rs"]
#[cfg(test)]
mod currency_tests;
