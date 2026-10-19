//! `ctrader_rate` — the ONE place the cTrader bridge's `QuoteRate` becomes the rate a facts answer carries
//! (decision 0183, phase 1), the twin of `oanda_rate`. The orientation is decided in the bridge
//! (`vike_ctrader::quote_rate`: the mid of the conversion pair, turned the right way round, with USDJPY and
//! GBPUSD goldens); this function only reads it out and refuses what cannot be priced.
//!
//! The rate is **account-currency units per ONE unit of the instrument's QUOTE currency** (the engine's
//! orientation, `SymbolAdmission::quote_rate`): a pound is worth more than a dollar (`1.35`), a yen a fraction
//! of one (`1/150`, never `150`; the flipped rate is a 22,500x error).
//!
//! **Fail closed**: a row that names no quote currency, a read that failed and a currency the account lists no
//! single conversion pair for are each a refusal. A read that failed or returned an unusable quote is worded
//! as a failure of the RATE ([`FACTS_RATE_UNAVAILABLE`]), so the node does not remember it as a verdict on the
//! symbol; "this account lists no forex pair for the currency" IS a verdict about the symbol and is worded as
//! one (the node remembers it, so a repeated order does not cost a venue session each time). There is no path
//! that answers `1.0` for "I do not know": the only `None` is the quote being the deposit currency, which
//! needs no conversion. A rate priced by a session for ANOTHER account currency than the listing's is a
//! failure of the rate too.
//!
//! **Each ask is a whole authenticated cTrader session**, so [`RateMemory`] stands between the facts and the
//! venue: a rate read is handed out again for [`RATE_REUSE_FOR`] (the order's look and its commit are one read),
//! a failed ask is not dialled again for [`RATE_FAILED_FOR`], and at most one ask per account and
//! [`HUB_RATE_ASKS`] in the hub run at once (the rest answer `FACTS_BUSY`, which is transient).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_ctrader::quote_rate::{QuoteRate, RateError};
use vike_gatewayhub_client::proto::{FACTS_BUSY, FACTS_RATE_UNAVAILABLE};

use super::symbol_facts::QuotedRate;

/// How long a rate that was read is handed out AGAIN to the next ask of the same account and currency. A
/// foreign-quoted order asks twice within a second (the LOOK, then the COMMIT that tells the reconcile client,
/// whose reply carries a rate nobody reads), and each ask is a whole authenticated cTrader session. The rate
/// keeps the moment it was ASKED (`QuotedRate::as_of_ms`), so reusing it hides no age: the node judges that
/// itself, and keeps what it is handed for its own `RATE_FRESH_FOR` (30 s), so a rate is at most this plus that
/// old when it is last used, well inside the engine's 120 s window.
pub(crate) const RATE_REUSE_FOR: Duration = Duration::from_secs(15);

/// How long a FAILED ask is remembered, per account and currency, before the next ask dials again: the
/// listing's `FAILED_TTL`. A dead or mute host costs one deadline's wait, not one per order or per timer tick.
pub(crate) const RATE_FAILED_FOR: Duration = Duration::from_secs(30);

/// The most cTrader rate asks running at once in the whole hub. The hub allows four symbol-facts lookups in
/// flight for EVERY venue (`session.rs`'s `MAX_SYMBOL_FACTS_INFLIGHT`, not changed here), and a rate ask is the
/// longest of them (a whole authenticated session), so cTrader may hold at most half of them: the rest answer
/// `FACTS_BUSY` and the other venues' lookups still find a slot.
pub(crate) const HUB_RATE_ASKS: usize = 2;

/// The cTrader rate asks running now, across every account of the hub ([`HUB_RATE_ASKS`]).
#[derive(Default)]
pub struct HubRateAsks(AtomicUsize);

/// What one ask of an account kept: when it ended and how.
struct Kept {
    at: Instant,
    answer: Result<QuoteRate, RateError>,
}

/// **One account's memory of its rate asks**: the answer to the last ask of each currency (a rate for
/// [`RATE_REUSE_FOR`], a failure for [`RATE_FAILED_FOR`]), and at most ONE ask running at a time. Kept per
/// account AND per credential fingerprint (`FactsByAccount`), so credentials swapped under one label start a
/// fresh memory. An ask that is refused for being one too many is not remembered: it asked nothing.
pub struct RateMemory {
    kept: Mutex<HashMap<String, Kept>>,
    asking: AtomicBool,
    reuse: Duration,
    failed: Duration,
}

impl Default for RateMemory {
    fn default() -> Self {
        Self::with_windows(RATE_REUSE_FOR, RATE_FAILED_FOR)
    }
}

/// Holds the account's single place and one of the hub's, given back when dropped (a panic included).
struct Turn<'a> {
    asking: &'a AtomicBool,
    hub: &'a AtomicUsize,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.hub.fetch_sub(1, Ordering::SeqCst);
        self.asking.store(false, Ordering::SeqCst);
    }
}

impl RateMemory {
    /// A memory reusing a rate for `reuse` and a failure for `failed` (the tests').
    #[must_use]
    pub fn with_windows(reuse: Duration, failed: Duration) -> Self {
        Self { kept: Mutex::new(HashMap::new()), asking: AtomicBool::new(false), reuse, failed }
    }

    /// **The rate of `quote` for this account**: the kept answer while it is inside its window, else `ask`'s,
    /// run under the account's one place and the hub's ([`HUB_RATE_ASKS`]). A place that is taken answers
    /// `Unavailable` in the `FACTS_BUSY` words without asking the venue (transient: the node asks again), so
    /// a slow venue can hold one lookup thread per account and two in the hub, never the four. `ask` is
    /// `CtraderCatalog::quote_rate` in production, itself bounded by one overall deadline.
    pub fn read(
        &self,
        hub: &HubRateAsks,
        quote: &str,
        ask: impl FnOnce() -> Result<QuoteRate, RateError>,
    ) -> Result<QuoteRate, RateError> {
        let quote = quote.trim().to_ascii_uppercase();
        if let Some(kept) = self.kept_answer(&quote) {
            return kept;
        }
        let busy =
            |why: &str| RateError::Unavailable(format!("{FACTS_BUSY} ({why}; retry shortly)"));
        if self.asking.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err(busy("a rate of this account is already being read"));
        }
        if hub.0.fetch_add(1, Ordering::SeqCst) >= HUB_RATE_ASKS {
            hub.0.fetch_sub(1, Ordering::SeqCst);
            self.asking.store(false, Ordering::SeqCst);
            return Err(busy("the hub is already reading its most cTrader rates at once"));
        }
        let _turn = Turn { asking: &self.asking, hub: &hub.0 };
        // Another ask of this account may have finished between the look above and the place just taken.
        if let Some(kept) = self.kept_answer(&quote) {
            return kept;
        }
        let answer = ask();
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(quote, Kept { at: Instant::now(), answer: answer.clone() });
        answer
    }

    /// The kept answer of `quote` while it is inside its window.
    fn kept_answer(&self, quote: &str) -> Option<Result<QuoteRate, RateError>> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let held = kept.get(quote)?;
        let window = if held.answer.is_ok() { self.reuse } else { self.failed };
        (held.at.elapsed() < window).then(|| held.answer.clone())
    }
}

/// The rate for an instrument quoted in `quote` on an account whose deposit currency is `deposit`:
/// `Ok(None)` when it is quoted in `deposit`, `Ok(Some(rate))` when it is quoted in another currency and the
/// venue priced it, `Err` otherwise. `read` is the venue read for the quote currency (production:
/// `CtraderCatalog::quote_rate`), called only when a conversion is needed.
pub(crate) fn quoted_rate(
    quote: &str,
    deposit: &str,
    read: impl FnOnce(&str) -> Result<QuoteRate, RateError>,
) -> Result<Option<QuotedRate>, String> {
    let quote = quote.trim().to_ascii_uppercase();
    if quote.is_empty() {
        return Err(format!(
            "ctrader: the instrument names no quote currency, so it cannot be converted into the \
             account's currency ({deposit})"
        ));
    }
    if quote.eq_ignore_ascii_case(deposit) {
        return Ok(None);
    }
    let unavailable = |why: &dyn std::fmt::Display| {
        format!("ctrader: {FACTS_RATE_UNAVAILABLE}: no rate for {quote} in {deposit} ({why})")
    };
    let rate = match read(&quote) {
        Ok(rate) => rate,
        // A verdict about the symbol, not a failed moment: this account lists no (single) pair for it.
        Err(no_pair @ RateError::NoPair(_)) => {
            return Err(format!(
                "{no_pair}; the symbol is NOT admitted (no rate, no admission: it is never valued at par)"
            ));
        }
        Err(e) => return Err(unavailable(&e)),
    };
    // The read must answer for the currency asked: a rate for another one is the flipped-rate class of bug.
    if !rate.quote_ccy.eq_ignore_ascii_case(&quote) {
        return Err(unavailable(&format!("the read answered for {}", rate.quote_ccy)));
    }
    // ... and for the ACCOUNT the listing was read for: the listing (kept minutes) states a deposit currency, the
    // session that priced this stated its own. Credentials swapped under one label, a listing not yet replaced,
    // would otherwise book one account's rate on another's ledger (a EUR rate of par on a USD engine).
    if !rate.account_ccy.eq_ignore_ascii_case(deposit) {
        return Err(unavailable(&format!(
            "the session that priced it is for an account in {}, and this account's listing says {deposit}",
            rate.account_ccy
        )));
    }
    Ok(Some(QuotedRate {
        quote_ccy: rate.quote_ccy,
        account_per_quote: rate.account_per_quote.get(),
        as_of_ms: rate.as_of_ms,
    }))
}

#[path = "ctrader_rate_tests.rs"]
#[cfg(test)]
mod ctrader_rate_tests;
