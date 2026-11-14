//! `cheap_np`'s token grammar: the Polymarket BTC 5-minute up/down token symbol and its parser,
//! [`TokenId`]. The strategy itself is the shipped file
//! `strategies/prediction_markets/cheap_catch_updown_fair_value.rs`; the backtest's resolution
//! source and the research runners parse the same symbols, so the grammar lives here, below all of
//! them.
//!
//! # Symbol convention
//!
//! The model layer has no instrument expiry, so the window's `sts` — hence `t` and the token
//! pairing — comes from the slug, as in the Python; one instance spans thousands of windows:
//!
//! ```text
//! btc-updown-5m-1772323200#0     <slug>#<outcome_index>
//!                ^^^^^^^^^^ ^    sts (must be a multiple of 300 — the Python's own guard)
//! ```
//!
//! A symbol that does not parse is never traded, so the spot series can be fed as a symbol safely.

use vike_model::fair::UPDOWN_WINDOW_SECS;

/// One market's identity as parsed out of a token symbol `"<slug>#<outcome_index>"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenId {
    /// Window open, epoch SECONDS — the trailing `-<sts>` of the slug.
    pub sts: i64,
    /// 0 = Up, 1 = Down.
    pub oidx: u8,
}

impl TokenId {
    /// Parse `"btc-updown-5m-1772323200#0"`. Checks ONLY the `#<0|1>` suffix, a trailing
    /// `-<integer>` slug segment, and `sts` a whole multiple of [`UPDOWN_WINDOW_SECS`] (the
    /// Python's `sts % 300 == 0` grid guard). ⚠ ANY prefix passes, and `sport_taker`'s
    /// `<condition_id>#<outcome_index>` shares the `#` suffix.
    pub fn parse(symbol: &str) -> Option<TokenId> {
        let (slug, index) = symbol.rsplit_once('#')?;
        let oidx: u8 = index.parse().ok()?;
        if oidx > 1 {
            return None;
        }
        let (_, sts_str) = slug.rsplit_once('-')?;
        let sts: i64 = sts_str.parse().ok()?;
        (sts > 0 && sts % UPDOWN_WINDOW_SECS == 0).then_some(TokenId { sts, oidx })
    }
}
