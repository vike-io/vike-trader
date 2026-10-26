//! **fxcm's PRICE STREAM** (any-symbol, decision 0174): the credentialed stream the strategy lane opens for
//! `cs_venue=fxcm`, so the tradehub's control door can price a pair its engine was not started on.
//!
//! The same kind of stream as dukascopy's ([`super::dukascopy_quotes`]) and for the same reason: nothing is mounted
//! on fxcm (a profile cannot name it: the venue has no market-data seam) and this stream **logs in to nothing**.
//! FXCM sessions are scarce, so the quotes come off the Offers table of the account's RUNNING reconcile session,
//! which the trade lane's reconcile client registered (`vike_fxcm::reads`) and `vike_fxcm::quotes::PriceDesk` polls:
//!
//! * **LAZY, end to end.** Building it dials nothing and asks the venue nothing; the first `subscribe_quotes` (the
//!   door asking a price for a pair) starts the desk's poller, which reads the table every couple of seconds for
//!   the pairs wanted and for no others. No reconcile session (reconciliation off, a failed login, a box whose
//!   only FXCM account is labelled) means no price, said in words (`Listening::want`'s refusals): a second login
//!   is never opened.
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the desk has already refused a row whose market is closed, whose quote
//!   is one-sided, crossed, untimed, from the future or older than 120 s by its own time
//!   (`vike_fxcm::quotes::judge`), so every quote this stream emits is a live one, and the stream's label is sent as
//!   real-time on a symbol's first quote ([`DataDelay::RealTime`]).
//! * **NO link state is disclosed**: no `GapStart`, no `Live`. A closed FX market and a wedged session are both
//!   silence, and the `fxcm` row of `vike_venues::link_deadman_default` is `Inert` for that reason; the tradehub's
//!   price tap ages the last quote out (fail-closed) and the two hub links speak for the venue.
//! * **Bounded** by the desk (`vike_fxcm::quotes::MAX_PRICED_SYMBOLS`), one request every couple of seconds at
//!   most, and one stream at a time owns the desk (a newer one replaces the older, which the desk warns about and
//!   whose asks then refuse, the symbols it already streams too).
//!
//! ⚠ **What this needs from the box**: a shim that exports `fc_offers` (an older one is refused in words by the
//! read, once a minute), and an FXCM `demo` account running with reconciliation ON.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_fxcm::quotes::{Listening, PriceDesk, PriceTick};
use vike_fxcm::recon_client::VENUE;
use vike_marketdata::QuoteTick;
use vike_model::DataDelay;

/// See the module doc.
pub struct FxcmQuotes {
    listening: Option<Listening>,
    /// The symbols a quote stream is open for, with the id handed out.
    subscribed: HashMap<String, SubscriptionId>,
    /// The symbols whose real-time label already went out (cleared with the subscription, so a re-subscribe
    /// labels again).
    labelled: Arc<Mutex<HashSet<String>>>,
    next_id: u64,
}

impl FxcmQuotes {
    /// A stream on `desk` delivering into `sink`. It becomes the desk's consumer at once (nothing is subscribed):
    /// an older stream on the same desk is replaced.
    #[must_use]
    pub fn on(desk: &Arc<PriceDesk>, sink: Arc<dyn LiveDataSink>) -> Self {
        let labelled: Arc<Mutex<HashSet<String>>> = Arc::default();
        let seen = Arc::clone(&labelled);
        let listening = desk.listen(Arc::new(move |tick: &PriceTick| {
            // The label is sent when the first quote of a symbol arrives, never per tick.
            let first =
                seen.lock().unwrap_or_else(PoisonError::into_inner).insert(tick.symbol.clone());
            if first {
                sink.data_delay(
                    VENUE,
                    &tick.symbol,
                    StreamLabel::Quotes.as_str(),
                    DataDelay::RealTime,
                );
            }
            sink.quote(
                VENUE,
                &tick.symbol,
                QuoteTick {
                    ts: tick.ts,
                    local_ts: vike_model::now_ms(),
                    bid: tick.bid,
                    ask: tick.ask,
                    // The Offers table carries no size.
                    bid_size: 0.0,
                    ask_size: 0.0,
                    symbol: tick.symbol.clone(),
                },
            );
        }));
        Self { listening: Some(listening), subscribed: HashMap::new(), labelled, next_id: 0 }
    }

    /// The process's stream: on the desk the trade lane's reconcile session is polled for.
    #[must_use]
    pub fn on_process_desk(sink: Arc<dyn LiveDataSink>) -> Self {
        Self::on(&vike_fxcm::quotes::process_desk(), sink)
    }

    fn forget(&mut self, symbol: &str) {
        self.labelled.lock().unwrap_or_else(PoisonError::into_inner).remove(symbol);
        if let Some(listening) = &self.listening {
            listening.unwant(symbol);
        }
    }

    fn close(&mut self) {
        let symbols: Vec<String> = self.subscribed.drain().map(|(symbol, _)| symbol).collect();
        for symbol in symbols {
            self.forget(&symbol);
        }
        // Dropping the hold stops whatever else it asked and removes the listener.
        self.listening = None;
    }
}

impl DataClient for FxcmQuotes {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fxcm's price stream carries quotes only (no bars)"))
    }

    /// Start the quote stream of `symbol` (six letters, the engine's spelling); a symbol already streaming answers
    /// its own id and takes no second place, but only while this stream is still the desk's consumer: once a newer
    /// stream replaced it (or a fault in its consumer detached it) every ask is refused in words, the already-streaming
    /// symbols included, so the tradehub is never told a stopped feed still prices. A repeat ask still goes to the
    /// desk's `want` (idempotent there): a poller that died on a panic is started again by the next ask for ANY
    /// pair it was serving, not only by a first one.
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let symbol = symbol.trim().to_ascii_uppercase();
        let Some(listening) = &self.listening else {
            return Err(LiveDataError::Subscribe("fxcm: the price stream is closed".to_string()));
        };
        listening.ensure_current().map_err(LiveDataError::Subscribe)?;
        listening.want(&symbol).map_err(LiveDataError::Subscribe)?;
        if let Some(id) = self.subscribed.get(&symbol) {
            return Ok(*id);
        }
        self.next_id += 1;
        let id = SubscriptionId(self.next_id);
        self.subscribed.insert(symbol, id);
        Ok(id)
    }

    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fxcm's price stream carries quotes only (no trades)"))
    }

    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("fxcm's price stream carries quotes only (no book)"))
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        let symbols: Vec<String> = self
            .subscribed
            .iter()
            .filter(|(_, held)| **held == id)
            .map(|(s, _)| s.clone())
            .collect();
        for symbol in symbols {
            self.subscribed.remove(&symbol);
            self.forget(&symbol);
        }
    }

    fn shutdown(&mut self) {
        self.close();
    }
}

impl Drop for FxcmQuotes {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "fxcm_quotes_tests.rs"]
mod fxcm_quotes_tests;
