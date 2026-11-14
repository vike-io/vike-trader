//! The currency facts every rate source and the engine share: the ONE reading of a currency code
//! ([`currency_code`], [`same_currency`]), the conversion factor's TYPE ([`AccountPerQuote`],
//! decision 0183's one scalar), the ORIENTATION of a conversion pair's mid
//! ([`AccountPerQuoteLeg`], [`AccountPerQuote::of_mid`]), and what a rate source answers with: the
//! one record ([`QuoteRate`]) or the one refusal ([`QuoteRateError`]).
//!
//! A currency code is any non-empty word, trimmed and upper-cased: `usd` and ` USD ` are the
//! currency `USD`, and a settlement coin (`USDT`, `USDC`) is a currency too. The engine keeps every
//! code it is handed in this spelling and compares codes a venue wrote with [`same_currency`].
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
