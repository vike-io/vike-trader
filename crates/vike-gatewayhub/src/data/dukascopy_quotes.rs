//! **dukascopy's PRICE STREAM** (any-symbol, decision 0174): the credentialed stream the strategy lane opens for
//! `cs_venue=dukascopy`, so the tradehub's control door can price a pair its engine was not started on.
//!
//! A different kind of credentialed stream from the other four: nothing is mounted on dukascopy (a profile
//! cannot name it), and this stream **logs in to nothing**. The JForex sidecar is ONE child process per daemon,
//! started by the TRADE lane for the one dukascopy account that is armed at `demo` (`vike_dukascopy::adapter`);
//! its quotes reach this stream over the process's price bus (`vike_dukascopy::quotes`), which the sidecar's
//! exec client attaches itself to. So:
//!
//! * **LAZY, end to end.** Building it dials nothing and subscribes nothing; the first `subscribe_quotes` (the
//!   door asking a price for a pair) writes the `subscribe` command to the sidecar that is already up. No
//!   armed account and no sidecar means no price, said in words (`Listening::want`'s refusals), never a second
//!   login: that would put two JVMs on one platform cache (`crates/bridges/dukascopy/src/adapter.rs`'s module doc).
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the bus has already dropped any tick that is one-sided, crossed, untimed
//!   or not current (`vike_dukascopy::quotes::accept`), so every quote this stream emits is one, and the stream's
//!   label is sent as real-time on a symbol's first quote ([`DataDelay::RealTime`], read off that tick's own time).
//! * **NO link state is disclosed**: no `GapStart`, no `Live`. A dead sidecar and a closed FX market are both
//!   silence, and the `dukascopy` row of `vike_venues::link_deadman_default` is `Inert` for that reason; the
//!   tradehub's price tap ages the last quote out (fail-closed) and the two hub links speak for the venue.
//! * **Bounded** by the bus (`vike_dukascopy::quotes::MAX_PRICED_SYMBOLS`), and one stream at a time owns the
//!   bus (a newer one replaces the older, which the bus warns about, naming both, and whose asks then refuse: the
//!   symbols it already streams too, `DukascopyQuotes::subscribe_quotes`).
//! * **The sink is fed from the bus's own thread**, never the sidecar's reader (`vike_dukascopy::quotes`'s module
//!   doc): a slow or panicking sink costs this stream its quotes and nothing else.
//!
//! ⚠ **What this needs from the box**: a `jforex-bridge.jar` of the release that announces `ticks` (an older jar
//! is refused in words), and a dukascopy account armed at `demo` in THIS gatewayhub process.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_dukascopy::quotes::{Listening, PriceTick, QuoteBus};
use vike_dukascopy::recon_client::VENUE;
use vike_marketdata::QuoteTick;
use vike_model::DataDelay;

/// See the module doc.
pub struct DukascopyQuotes {
    listening: Option<Listening>,
    /// The symbols a quote stream is open for, with the id handed out.
    subscribed: HashMap<String, SubscriptionId>,
    /// The symbols whose real-time label already went out (cleared with the subscription, so a re-subscribe
    /// labels again).
    labelled: Arc<Mutex<HashSet<String>>>,
    next_id: u64,
}

impl DukascopyQuotes {
    /// A stream on `bus` delivering into `sink`. It becomes the bus's consumer at once (nothing is subscribed):
    /// an older stream on the same bus is replaced.
    #[must_use]
    pub fn on(bus: &Arc<QuoteBus>, sink: Arc<dyn LiveDataSink>) -> Self {
        let labelled: Arc<Mutex<HashSet<String>>> = Arc::default();
        let seen = Arc::clone(&labelled);
        let listening = bus.listen(Arc::new(move |tick: &PriceTick| {
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
                    // JForex tick volumes are not carried: "the feed does not carry it".
                    bid_size: 0.0,
                    ask_size: 0.0,
                    symbol: tick.symbol.clone(),
                },
            );
        }));
        Self { listening: Some(listening), subscribed: HashMap::new(), labelled, next_id: 0 }
    }

    /// The process's stream: on the bus the sidecar's exec client attaches to.
    #[must_use]
    pub fn on_process_bus(sink: Arc<dyn LiveDataSink>) -> Self {
        Self::on(&vike_dukascopy::quotes::process_bus(), sink)
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

impl DataClient for DukascopyQuotes {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("dukascopy's price stream carries quotes only (no bars)"))
    }

    /// Start the quote stream of `symbol` (six letters, the engine's spelling); a symbol already streaming answers
    /// its own id and subscribes nothing, but only while this stream is still the bus's consumer: once a newer
    /// stream replaced it (or a fault in its consumer detached it) every ask is refused in words, the already-streaming
    /// symbols included, so the tradehub is never told a stopped feed still prices.
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let symbol = symbol.trim().to_ascii_uppercase();
        let Some(listening) = &self.listening else {
            return Err(LiveDataError::Subscribe(
                "dukascopy: the price stream is closed".to_string(),
            ));
        };
        listening.ensure_current().map_err(LiveDataError::Subscribe)?;
        if let Some(id) = self.subscribed.get(&symbol) {
            return Ok(*id);
        }
        listening.want(&symbol).map_err(LiveDataError::Subscribe)?;
        self.next_id += 1;
        let id = SubscriptionId(self.next_id);
        self.subscribed.insert(symbol, id);
        Ok(id)
    }

    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("dukascopy's price stream carries quotes only (no trades)"))
    }

    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("dukascopy's price stream carries quotes only (no book)"))
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

impl Drop for DukascopyQuotes {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "dukascopy_quotes_tests.rs"]
mod dukascopy_quotes_tests;
