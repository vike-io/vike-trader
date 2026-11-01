//! The currency facts the broker bridges share: the conversion factor's TYPE ([`AccountPerQuote`],
//! decision 0183's one scalar) and the reading of a currency code ([`iso_code`]).
//!
//! Each bridge keeps its own rate SOURCE (cTrader's spot, Dukascopy's tick mid, OANDA's
//! `homeConversions`, IBKR's quote, FXCM's offers), its own orientation rules and its own refusals;
//! only the type a rate is carried in and the three-letter check are written once here.

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
}

/// `code` as an upper-case ISO currency code, or `None` unless it is exactly three ASCII letters
/// once trimmed.
#[must_use]
pub fn iso_code(code: &str) -> Option<String> {
    let code = code.trim();
    (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin of cTrader's, Dukascopy's, OANDA's and IBKR's former copies (each crate's
    /// `a_rate_cannot_be_built_from_a_non_rate`). Red on a mutant that drops `is_finite` (inf
    /// passes) or turns `> 0.0` into `>= 0.0` (zero passes).
    #[test]
    fn a_rate_cannot_be_built_from_a_non_rate() {
        for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
        }
        assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
        assert_eq!(AccountPerQuote::new(0.5).map(|r| r.to_account(10_000.0)), Some(5_000.0));
        assert_eq!(AccountPerQuote::IDENTITY.get(), 1.0);
    }

    /// The pin of FXCM's `iso`, IBKR's and Dukascopy's `code`: trimmed, upper-cased, exactly three
    /// ASCII letters. Red on a mutant that skips the trim, the length or the letter check.
    #[test]
    fn a_currency_code_is_three_ascii_letters_upper_cased() {
        assert_eq!(iso_code(" usd ").as_deref(), Some("USD"));
        assert_eq!(iso_code("JPY").as_deref(), Some("JPY"));
        for bad in ["", "US", "USDT", "U$D", "12A", "ÉUR", "us d"] {
            assert_eq!(iso_code(bad), None, "{bad:?}");
        }
    }
}
