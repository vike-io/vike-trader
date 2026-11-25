//! The reconcile clients' shared ADMITTED-symbol half: the list a client reads beyond its mounted symbol
//! ([`ReconcileAdmittedSymbols`]) with the logged forwarding of `ReconcileClient::admit_symbol` /
//! `forget_symbol`, and the judgement of ONE admitted symbol's read
//! ([`ReconcileAdmittedReadSpec::admitted_rows`]) with the pass-wide quiet period after a rate limit
//! ([`RATE_LIMIT_BACKOFF`], [`not_backing_off`]; every venue takes it, the owner's 2026-11-11 ruling).
//!
//! **The list.** An account's engine is admitted symbols on demand (decision 0173): the engine holds the
//! mounted symbol plus up to [`MAX_EXTRA_SYMBOLS`] admitted ones, and a restart restores positions and
//! resting orders for all of them. The core's reconcile driver asks a `vike_execution::recon::ReconcileClient`
//! three questions with no symbol in them, and the restore planner reads an order the report does not
//! carry as GONE, so a client that read only the mounted symbol made every resting order on an admitted
//! one invisible, then retired it. `ReconcileClient::admit_symbol` is how the client hears which symbols the
//! engine admitted, and [`ReconcileAdmittedSymbols`] is the list it keeps:
//!
//! * **Bounded, and it shrinks only when the engine GIVES a symbol back** (decision 0205:
//!   [`ReconcileAdmittedSymbols::forget`], reached through `ReconcileClient::forget_symbol`). The cap is the
//!   engine's own [`MAX_EXTRA_SYMBOLS`], so a pass costs at most that many extra reads per report kind and
//!   the client never reads a symbol the engine could not hold. Without the forget a released slot would
//!   be free in the engine and still taken here, and the next symbol would be admitted into an engine
//!   whose reads never reach it.
//! * **A symbol is added when the admission is REAL, never on a look.** The caller tells the client after
//!   `Command::AdmitSymbol` was sent, not when the node merely asked for the symbol's facts. There is no
//!   eviction: a symbol leaves only because the engine said so.
//! * **The mounted symbol is not in it**: the client always reads that one, with the request it has
//!   always made.
//! * **Spelling is the caller's** (the engine's: binance and aster `.P` for the perpetual lane, bybit the
//!   bare symbol); the list only upper-cases, de-duplicates and bounds. A venue's own spelling step (the
//!   binance family's lane filter, bybit's `.P` strip) runs in its bridge BEFORE it calls here. A venue
//!   whose symbols are case-sensitive keeps them as the engine spelled them
//!   ([`ReconcileAdmittedSymbols::case_kept`]: hyperliquid's `dex:COIN` and `BASE/QUOTE`), compared exactly.
//!
//! **The read judgement.** A venue that no longer lists an admitted symbol holds nothing on it: that read
//! is no rows with a warning, so one delisted symbol cannot blind the pass for good. A rate limit starts
//! the pass-wide quiet period. Anything else fails the whole fetch: the restore planner reads an order a
//! successful report lacks as gone. Which answer means "not listed" or "rate limited", and the TEXT a
//! venue error becomes, are the venue's ([`ReconcileAdmittedReadSpec`]): binance gives its error's whole
//! `Display`, bybit the request with the venue's code and message.
//!
//! The log lines speak one wording for every venue, under this module's target, with the venue a field;
//! the noun a forwarding line names (`a symbol`, `a pair`, `an epic`, `an instrument`) stays the venue's.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use vike_execution::MAX_EXTRA_SYMBOLS;

use crate::ratelimit::RateGatePause;
use crate::transport::VenueApiError;

/// How long a reconcile pass stays silent after an admitted read was answered a rate limit. The
/// transport keeps no `Retry-After`, so the pass-wide [`RateGatePause`] is fixed; it is never started with
/// nothing admitted. (A scheduling clock, kept out of `vike-execution`, which reads none.)
pub const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

/// The symbols a reconcile client reads BEYOND the one it was built on. See the module doc.
#[derive(Debug, Default)]
pub struct ReconcileAdmittedSymbols {
    symbols: Mutex<Vec<String>>,
    /// [`Self::case_kept`]: symbols are stored and compared as given, never upper-cased.
    case_kept: bool,
    /// The admitted symbols already said to be no longer listed ([`Self::not_listed_logged`]).
    said_not_listed: Mutex<Vec<String>>,
}

impl ReconcileAdmittedSymbols {
    /// A list that keeps each symbol's case and compares spellings exactly (hyperliquid, whose `dex:coin` and pair
    /// names are case-sensitive). [`Self::default`] upper-cases.
    pub fn case_kept() -> Self {
        ReconcileAdmittedSymbols { case_kept: true, ..Self::default() }
    }

    /// `symbol` as this list spells it.
    fn spelled(&self, symbol: &str) -> String {
        if self.case_kept { symbol.to_string() } else { symbol.to_ascii_uppercase() }
    }

    /// Add `symbol`, unless it is empty, is the `mounted` symbol, is already held or the list is full.
    /// `true` = it was added. Both spellings are compared and stored upper-case (as given, compared
    /// exactly, in a [`Self::case_kept`] list).
    pub fn admit(&self, symbol: &str, mounted: &str) -> bool {
        let symbol = self.spelled(symbol);
        if symbol.is_empty() || symbol == self.spelled(mounted) {
            return false;
        }
        let mut held = self.symbols.lock().unwrap_or_else(PoisonError::into_inner);
        if held.len() >= MAX_EXTRA_SYMBOLS || held.contains(&symbol) {
            return false;
        }
        held.push(symbol);
        true
    }

    /// Take `symbol` out of the list (any case; exactly in a [`Self::case_kept`] list): the engine GAVE
    /// IT BACK (decision 0205), so the next pass stops reading it and its place is free for the next
    /// admission. `true` = it was held. A symbol that was never held (a look, a refused admission, a
    /// repeat) is a no-op.
    pub fn forget(&self, symbol: &str) -> bool {
        let symbol = self.spelled(symbol);
        self.said_not_listed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| *s != symbol);
        let mut held = self.symbols.lock().unwrap_or_else(PoisonError::into_inner);
        let before = held.len();
        held.retain(|s| *s != symbol);
        held.len() != before
    }

    /// **The venue no longer lists the admitted `symbol`: said ONCE, loudly.** Every pass reads such a symbol as
    /// holding nothing (the not-listed verdict of [`ReconcileAdmittedReadSpec::admitted_rows`], or the venue's own
    /// listing check); the operator is told once per symbol, at ERROR (what `trade health`'s `journal` row has an
    /// operator grep), naming the venue, the `account` and the symbol. Nothing is synthesized: an order the engine
    /// still holds there stays as it is, and the pass reports it as an orphan local order. Said again only after the
    /// engine gave the symbol back ([`Self::forget`]). `noun` is the venue's word with its article (`"an
    /// instrument"`), `why` the venue's answer. `true` the first time.
    pub fn not_listed_logged(
        &self,
        symbol: &str,
        venue: &str,
        account: &str,
        noun: &str,
        why: &str,
    ) -> bool {
        let symbol = self.spelled(symbol);
        let mut said = self.said_not_listed.lock().unwrap_or_else(PoisonError::into_inner);
        if said.contains(&symbol) {
            return false;
        }
        tracing::error!(
            venue, account, symbol = %symbol,
            "reconcile: the venue no longer lists {noun} the engine admitted ({why}); every pass reads it as \
             holding nothing from now on, and an order the engine still holds there is left as it is (the pass \
             reports it as an orphan local order)"
        );
        said.push(symbol);
        true
    }

    /// The admitted symbols, in admission order.
    pub fn symbols(&self) -> Vec<String> {
        self.symbols.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// `ReconcileClient::admit_symbol`'s body: [`Self::admit`], and one `info` line when the symbol was
    /// added. `noun` is the venue's word with its article (`"a symbol"`, `"an epic"`).
    pub fn admit_logged(&self, symbol: &str, mounted: &str, venue: &str, noun: &str) {
        if self.admit(symbol, mounted) {
            tracing::info!(
                venue, symbol = %self.spelled(symbol),
                "reconcile: the engine admitted {noun}; every pass now reads it too"
            );
        }
    }

    /// `ReconcileClient::forget_symbol`'s body: [`Self::forget`], and one `info` line when it was held.
    pub fn forget_logged(&self, symbol: &str, venue: &str, noun: &str) {
        if self.forget(symbol) {
            tracing::info!(
                venue, symbol = %self.spelled(symbol),
                "reconcile: the engine gave {noun} back; no pass reads it any more"
            );
        }
    }
}

/// Why one report read failed: the venue answered an error (kept whole, so a read of an admitted
/// symbol can judge it) or its answer could not be used (a body that did not parse, a page chain that
/// never ended). `E` is the venue's error type.
#[derive(Debug)]
pub enum ReconcileReadError<E = VenueApiError> {
    Venue(E),
    Parse(String),
}

impl<E> From<E> for ReconcileReadError<E> {
    fn from(e: E) -> Self {
        ReconcileReadError::Venue(e)
    }
}

impl<E> ReconcileReadError<E> {
    /// The pass's error text: a venue error through the venue's `text` (its
    /// [`ReconcileAdmittedReadSpec::text`]), a parse failure as it was written.
    pub fn into_text(self, text: fn(E) -> String) -> String {
        match self {
            ReconcileReadError::Venue(e) => text(e),
            ReconcileReadError::Parse(why) => why,
        }
    }
}

/// A venue's facts for judging an admitted symbol's read. Each is the venue's own; nothing here
/// guesses one.
pub struct ReconcileAdmittedReadSpec<E = VenueApiError> {
    /// The venue answered that it does not list the symbol (binance `-1121`, bybit `10001` naming
    /// the symbol): it holds nothing there.
    pub not_listed: fn(&E) -> bool,
    /// The venue answered a rate limit (bybit also counts the `403` its Cloudflare front answers a
    /// throttled IP with): the pass goes quiet for [`RATE_LIMIT_BACKOFF`].
    pub rate_limited: fn(&E) -> bool,
    /// The text a venue error becomes in the pass error (binance `e.to_string()`, bybit the request, code and
    /// message).
    pub text: fn(E) -> String,
}

impl<E> ReconcileAdmittedReadSpec<E> {
    /// Judge the read of ONE admitted `symbol` (the engine's spelling) on `venue`'s client. Not listed:
    /// no rows, with a warning. Rate limited: `pause` starts for [`RATE_LIMIT_BACKOFF`], with a warning,
    /// and the read fails. Anything else fails the read in its own text.
    pub fn admitted_rows<R>(
        &self,
        venue: &str,
        symbol: &str,
        pause: &RateGatePause,
        read: Result<Vec<R>, ReconcileReadError<E>>,
    ) -> Result<Vec<R>, String> {
        match read {
            Err(ReconcileReadError::Venue(e)) if (self.not_listed)(&e) => {
                let why = (self.text)(e);
                tracing::warn!(
                    venue, symbol = %symbol,
                    "reconcile: the venue does not list this admitted symbol ({why}); reading it as \
                     holding nothing"
                );
                Ok(Vec::new())
            }
            Err(ReconcileReadError::Venue(e)) if (self.rate_limited)(&e) => {
                pause.start(RATE_LIMIT_BACKOFF);
                let why = (self.text)(e);
                tracing::warn!(
                    venue, symbol = %symbol,
                    "reconcile: rate limited reading an admitted symbol ({why}); the pass is silent \
                     for {}s",
                    RATE_LIMIT_BACKOFF.as_secs()
                );
                Err(why)
            }
            other => other.map_err(|e| e.into_text(self.text)),
        }
    }
}

/// `Err` while `venue`'s pass is quiet after a rate limit on an admitted symbol. Only a client with
/// admitted symbols ever starts the pause, so with none this is always `Ok`.
pub fn not_backing_off(venue: &str, pause: &RateGatePause) -> Result<(), String> {
    match pause.left() {
        Some(left) => Err(format!(
            "{venue} reconcile is backing off for {}s after a rate-limit answer on an admitted symbol",
            left.as_secs() + 1
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "reconcile_admitted_tests.rs"]
mod reconcile_admitted_tests;
