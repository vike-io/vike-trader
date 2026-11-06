//! **The price stream over a bridge's process-wide quote source** ([`PriceBus`]): the one body of the price
//! streams that log in to nothing, the gatewayhub's credentialed streams `cs_venue=dukascopy` (over
//! `vike_dukascopy::quotes::QuoteBus`) and `cs_venue=fxcm` (over `vike_fxcm::quotes::PriceDesk`). Everything a
//! venue decides (its name, its words, whether a repeat ask goes back to the source) is that venue's
//! [`PriceBus`] and [`BusHold`] impl, in its own bridge beside its source; each stream's contract is its
//! gatewayhub module's doc (`crates/vike-gatewayhub/src/data/dukascopy_quotes.rs`,
//! `crates/vike-gatewayhub/src/data/fxcm_quotes.rs`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_marketdata::QuoteTick;
use vike_model::DataDelay;

/// What a [`PriceBus`] hands each tick it accepts to: `(symbol, ts, bid, ask)`.
pub type Deliver = Box<dyn Fn(&str, i64, f64, f64) + Send + Sync>;

/// A bridge's process-wide quote source, as a [`PriceStream`] consumes it.
pub trait PriceBus: Send + Sync + 'static {
    /// The source's hold on its one consumer place.
    type Hold: BusHold;
    /// The venue the stream quotes and labels.
    const VENUE: &'static str;
    /// The refusal of a subscribe on a closed stream.
    const CLOSED: &'static str;
    /// The refusals of the three lanes the stream does not carry.
    const NO_BARS: &'static str;
    const NO_TRADES: &'static str;
    const NO_BOOK: &'static str;

    fn process() -> Arc<Self>;

    /// Become the source's consumer: `deliver(symbol, ts, bid, ask)` for each tick the source accepts.
    fn consume(self: &Arc<Self>, deliver: Deliver) -> Self::Hold;
}

/// What a [`PriceBus`] consumer holds; dropping it stops whatever it asked and removes the listener.
pub trait BusHold: Send {
    /// `Err` (in words) while a newer consumer holds the source, or once a fault detached this one. Whether a
    /// replaced consumer may take a free source back is the venue's.
    fn ensure_current(&self) -> Result<(), String>;
    /// Ask the source for `symbol`'s quotes, `streaming` when the stream already carries it: the VENUE decides
    /// whether a repeat ask goes back to the source. `Err` is the source's refusal, in words.
    fn ask(&self, symbol: &str, streaming: bool) -> Result<(), String>;
    fn unwant(&self, symbol: &str);
}

/// A price stream over `B` (see the module doc).
pub struct PriceStream<B: PriceBus> {
    listening: Option<B::Hold>,
    /// The symbols a quote stream is open for, with the id handed out.
    subscribed: HashMap<String, SubscriptionId>,
    /// The symbols whose real-time label already went out (cleared with the subscription, so a re-subscribe
    /// labels again).
    labelled: Arc<Mutex<HashSet<String>>>,
    next_id: u64,
}

impl<B: PriceBus> PriceStream<B> {
    /// A stream on `bus` delivering into `sink`. It becomes the source's consumer at once (nothing is
    /// subscribed): an older stream on the same source is replaced.
    #[must_use]
    pub fn on(bus: &Arc<B>, sink: Arc<dyn LiveDataSink>) -> Self {
        let labelled: Arc<Mutex<HashSet<String>>> = Arc::default();
        let seen = Arc::clone(&labelled);
        let listening = bus.consume(Box::new(move |symbol: &str, ts: i64, bid: f64, ask: f64| {
            // The label is sent when the first quote of a symbol arrives, never per tick.
            let first =
                seen.lock().unwrap_or_else(PoisonError::into_inner).insert(symbol.to_string());
            if first {
                sink.data_delay(
                    B::VENUE,
                    symbol,
                    StreamLabel::Quotes.as_str(),
                    DataDelay::RealTime,
                );
            }
            sink.quote(
                B::VENUE,
                symbol,
                QuoteTick {
                    ts,
                    local_ts: vike_model::now_ms(),
                    bid,
                    ask,
                    // No source carries a size (each impl's `consume` says why).
                    bid_size: 0.0,
                    ask_size: 0.0,
                    symbol: symbol.to_string(),
                },
            );
        }));
        Self { listening: Some(listening), subscribed: HashMap::new(), labelled, next_id: 0 }
    }

    /// The process's stream: on the source the venue's trade lane feeds.
    #[must_use]
    pub fn on_process(sink: Arc<dyn LiveDataSink>) -> Self {
        Self::on(&B::process(), sink)
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

impl<B: PriceBus> DataClient for PriceStream<B> {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported(B::NO_BARS))
    }

    /// Start the quote stream of `symbol` (the engine's spelling); a symbol already streaming answers its own
    /// id, but only while this stream is the source's consumer: while a newer stream holds the source (or once a
    /// fault in its consumer detached it) every ask is refused in words, the already-streaming symbols
    /// included, so the tradehub is never told a stopped feed still prices. Whether a repeat ask also goes to
    /// the source is the venue's ([`BusHold::ask`]).
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let symbol = symbol.trim().to_ascii_uppercase();
        let Some(listening) = &self.listening else {
            return Err(LiveDataError::Subscribe(B::CLOSED.to_string()));
        };
        listening.ensure_current().map_err(LiveDataError::Subscribe)?;
        let streaming = self.subscribed.get(&symbol).copied();
        listening.ask(&symbol, streaming.is_some()).map_err(LiveDataError::Subscribe)?;
        if let Some(id) = streaming {
            return Ok(id);
        }
        self.next_id += 1;
        let id = SubscriptionId(self.next_id);
        self.subscribed.insert(symbol, id);
        Ok(id)
    }

    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported(B::NO_TRADES))
    }

    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported(B::NO_BOOK))
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

impl<B: PriceBus> Drop for PriceStream<B> {
    fn drop(&mut self) {
        self.close();
    }
}
