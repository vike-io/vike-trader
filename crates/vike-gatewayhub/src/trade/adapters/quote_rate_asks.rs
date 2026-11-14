//! `quote_rate_asks` — **the guard in front of a COSTLY quote-rate ask** (decision 0183): a venue whose every rate ask
//! is a whole authenticated session (cTrader's: `vike_ctrader::symbol_properties::QuoteRateAskSpecCtrader`, the one
//! spec today) has [`QuoteRateAsks`] stand between its facts and the venue. A rate read is handed out again for the
//! spec's `reuse` (the order's look and its commit are one read), a failed ask is not dialled again for its `failed`,
//! and at most one ask per account and the spec's `HUB_ASKS` in the hub ([`QuoteRateAsksHub`]) run at once; the rest
//! answer `PROPERTIES_BUSY`, which is transient, without asking the venue. The numbers and the words are the bridge's
//! spec; the keeping is the shared `vike_bridge_core::quote_rate::QuoteRateMemory`.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use vike_bridge_core::quote_rate::{QuoteRateAskSpec, QuoteRateMemory, QuoteRateMemoryConfig};
use vike_gatewayhub_client::proto::PROPERTIES_BUSY;
use vike_model::{QuoteRate, QuoteRateError};

/// The rate asks of one spec running now, across every account of the hub (the spec's `HUB_ASKS`).
#[derive(Default)]
pub struct QuoteRateAsksHub(AtomicUsize);

/// **One account's memory of its rate asks**: the answer to the last ask of each currency (a rate for the spec's
/// `reuse`, a failure for its `failed`), and at most ONE ask running at a time. Kept per account AND per credential
/// fingerprint (`PerAccount`), so credentials swapped under one label start a fresh memory. An ask that is refused
/// for being one too many is not remembered: it asked nothing.
pub struct QuoteRateAsks<S> {
    /// The rate and, beside it, the account currency the session that priced it read.
    kept: QuoteRateMemory<(QuoteRate, String)>,
    asking: AtomicBool,
    spec: PhantomData<fn() -> S>,
}

impl<S: QuoteRateAskSpec> Default for QuoteRateAsks<S> {
    fn default() -> Self {
        Self::with_windows(S::MEMORY.reuse, S::MEMORY.failed)
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

impl<S: QuoteRateAskSpec> QuoteRateAsks<S> {
    /// A memory reusing a rate for `reuse` and a failure for `failed` (the tests').
    #[must_use]
    pub fn with_windows(reuse: Duration, failed: Duration) -> Self {
        let config = QuoteRateMemoryConfig { reuse, failed, ..S::MEMORY };
        Self {
            kept: QuoteRateMemory::new(config),
            asking: AtomicBool::new(false),
            spec: PhantomData,
        }
    }

    /// **The rate of `quote` for this account**: the kept answer while it is inside its window, else `ask`'s,
    /// run under the account's one place and the hub's (the spec's `HUB_ASKS`). A place that is taken answers
    /// `Unavailable` in the `PROPERTIES_BUSY` words without asking the venue (transient: the node asks again), so
    /// a slow venue can hold one lookup thread per account and `HUB_ASKS` in the hub, never the four. `ask` is
    /// the venue's read in production (cTrader's `CatalogProviderCtrader::quote_rate`), itself bounded by one deadline.
    pub fn read(
        &self,
        hub: &QuoteRateAsksHub,
        quote: &str,
        ask: impl FnOnce() -> Result<(QuoteRate, String), QuoteRateError>,
    ) -> Result<(QuoteRate, String), QuoteRateError> {
        let quote = quote.trim().to_ascii_uppercase();
        if let Some(kept) = self.kept.answer(&quote) {
            return kept;
        }
        let busy = |why: &str| {
            QuoteRateError::Unavailable(format!(
                "{}: {PROPERTIES_BUSY} ({why}; retry shortly)",
                S::VENUE
            ))
        };
        if self.asking.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err(busy("a rate of this account is already being read"));
        }
        if hub.0.fetch_add(1, Ordering::SeqCst) >= S::HUB_ASKS {
            hub.0.fetch_sub(1, Ordering::SeqCst);
            self.asking.store(false, Ordering::SeqCst);
            return Err(busy(&format!(
                "the hub is already reading its most {} rates at once",
                S::VENUE_NAME
            )));
        }
        let _turn = Turn { asking: &self.asking, hub: &hub.0 };
        // Another ask of this account may have finished between the look above and the place just taken.
        if let Some(kept) = self.kept.answer(&quote) {
            return kept;
        }
        let answer = ask();
        self.kept.note(&quote, &answer);
        answer
    }
}

#[path = "quote_rate_asks_tests.rs"]
#[cfg(test)]
mod quote_rate_asks_tests;
