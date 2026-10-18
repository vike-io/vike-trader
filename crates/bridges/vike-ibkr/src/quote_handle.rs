//! **A price for a symbol the engine was not mounted on** (any-symbol, decision 0174): the handle the control
//! door asks `price(symbol, subscribe)` of, over the ONE data client of the account's gateway session.
//!
//! The door prices a foreign symbol before it admits it (an opening order needs a mark: the collar and the
//! exposure caps judge it). [`QuoteHandle`] is that handle for IBKR, built on the seam the data lane already
//! has - [`DataClient`] (`crate::connect_data_client`: `IbkrFeeds` on the socket, `CpapiFeeds` on the Client
//! Portal) - and on nothing new at the wire:
//!
//! * **ONE data client for every symbol, dialled once.** The first priced symbol dials the client through the
//!   gate's data lane (`GatewayGate::dial_data`: it never delays an order-lane dial, and spends the data
//!   lane's own hourly budget); every later symbol is a `subscribe_quotes` on the same client, so N symbols
//!   are one dial and N subscriptions, never N dials. A dial that failed is REMEMBERED for [`REDIAL_AFTER`]:
//!   every ask inside it answers the same sentence without touching the gateway (the gate's own backoff sits
//!   under this one).
//! * **At most `vike_exec::MAX_EXTRA_SYMBOLS` symbols** (the engine's own cap on admitted symbols): IBKR's
//!   market-data line limit is UNMEASURED, so the handle never asks for more lines than the engine could hold.
//! * **A price is the MID of a two-sided quote that the venue labelled REAL-TIME, and no older than
//!   [`MAX_QUOTE_AGE`].** A delayed or frozen stream, an unlabelled one and a one-sided quote are no price
//!   (named in the answer), because a mark is what the exposure caps are judged at. Whether the paper account
//!   is entitled to real-time stocks is UNMEASURED (decision 0140 records delayed stocks): on a box without the
//!   entitlement this handle answers "delayed" for every stock and the door refuses the opening order, which
//!   is the safe direction.
//! * A read (`subscribe` false) never dials or subscribes anything.
//!
//! ⚠ **Nothing in the tree calls this yet.** The tradehub has no ibkr feed arm and the gatewayhub's data lane
//! has no ibkr stream, so the door still answers `this node runs no price feed for ibkr` (the plan's "ibkr"
//! section lists the stream lane and the arm as the open step). When it is wired, the lookup
//! (`crate::any_symbol_socket`) must run over this handle's client: both use `data_client_id`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::DataDelay;

use crate::config::IbkrConfig;
use crate::connect_data_client;
use crate::error::IbkrError;

/// How long a failed dial is remembered, so a burst of orders does not become a burst of dials.
pub const REDIAL_AFTER: Duration = Duration::from_secs(30);

/// The oldest a quote may be and still be a price. A quiet stock sends no quote while its market does not
/// move, so this is generous; a quote older than this is "none yet".
pub const MAX_QUOTE_AGE: Duration = Duration::from_secs(300);

/// What the sink holds for one symbol.
#[derive(Default)]
struct Quoted {
    /// The mid of the last two-sided quote, and when it arrived.
    mid: Option<(f64, Instant)>,
    /// The venue's label of the stream, as last sent.
    delay: Option<DataDelay>,
}

/// The latest quote and delay label per symbol: the sink the data client delivers into.
#[derive(Default)]
pub struct QuoteBoard {
    symbols: Mutex<HashMap<String, Quoted>>,
}

impl QuoteBoard {
    /// Write into the entry of `symbol` (made on first write: only the sink writes, and the data client only
    /// delivers for symbols it was asked to stream).
    fn write(&self, symbol: &str, f: impl FnOnce(&mut Quoted)) {
        let mut held = self.symbols.lock().unwrap_or_else(PoisonError::into_inner);
        f(held.entry(symbol.to_ascii_uppercase()).or_default());
    }

    /// The price of `symbol`, or why there is none (`Ok(None)` = nothing has arrived yet). A read: it adds
    /// no entry, so asking about a thousand made-up symbols grows nothing.
    ///
    /// # Errors
    /// The words of the refusal: the stream is not labelled real-time (delayed, frozen, unlabelled).
    pub fn price(&self, symbol: &str) -> Result<Option<f64>, String> {
        let held = self.symbols.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(&Quoted { mid: Some((mid, at)), delay }) = held.get(&symbol.to_ascii_uppercase())
        else {
            return Ok(None);
        };
        match delay {
            Some(DataDelay::RealTime) => {}
            Some(other) => {
                return Err(format!(
                    "ibkr: the {symbol} quote stream is {} and a delayed price is not a mark; the \
                     account needs a real-time market-data entitlement for it",
                    other.word()
                ));
            }
            None => {
                return Err(format!(
                    "ibkr: the {symbol} quote stream carries no real-time label, so its price is not \
                     trusted as a mark"
                ));
            }
        }
        Ok((at.elapsed() < MAX_QUOTE_AGE).then_some(mid))
    }
}

impl LiveDataSink for QuoteBoard {
    fn seed_bars(&self, _venue: &str, _symbol: &str, _interval: &str, _bars: Vec<Bar>) {}
    fn close_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn forming_bar(&self, _venue: &str, _symbol: &str, _interval: &str, _bar: Bar) {}
    fn mark_tick(&self, _venue: &str, _symbol: &str, _px: f64, _ts: i64) {}
    fn trade(&self, _venue: &str, _symbol: &str, _trade: TradeTick) {}
    fn book(&self, _venue: &str, _symbol: &str, _book: Arc<L2Book>) {}

    fn quote(&self, _venue: &str, symbol: &str, quote: QuoteTick) {
        let (bid, ask) = (quote.bid, quote.ask);
        if bid.is_finite() && ask.is_finite() && bid > 0.0 && ask >= bid {
            self.write(symbol, |q| q.mid = Some(((bid + ask) / 2.0, Instant::now())));
        }
    }

    fn data_delay(&self, _venue: &str, symbol: &str, _stream: &str, delay: DataDelay) {
        self.write(symbol, |q| q.delay = Some(delay));
    }
}

/// How the handle opens its data client over the board (the production dial is
/// [`QuoteHandle::for_config`]; a test passes a scripted one).
pub type Dial = Box<
    dyn Fn(Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, IbkrError> + Send + Sync,
>;

struct State {
    client: Option<Box<dyn DataClient + Send>>,
    subscribed: HashSet<String>,
    failed: Option<(Instant, String)>,
}

/// See the module doc.
pub struct QuoteHandle {
    dial: Dial,
    board: Arc<QuoteBoard>,
    state: Mutex<State>,
}

impl QuoteHandle {
    /// A handle that opens its client with `dial` on the first priced symbol.
    #[must_use]
    pub fn new(dial: Dial) -> Self {
        Self {
            dial,
            board: Arc::new(QuoteBoard::default()),
            state: Mutex::new(State { client: None, subscribed: HashSet::new(), failed: None }),
        }
    }

    /// The production handle: `cfg`'s own product through the one door (`connect_data_client`).
    #[must_use]
    pub fn for_config(cfg: IbkrConfig) -> Self {
        Self::new(Box::new(move |sink| connect_data_client(&cfg, sink)))
    }

    /// The latest price of `symbol` (the engine's spelling, `TICKER.SMART.CCY`). With `subscribe` the first ask
    /// dials the client (once) and starts the symbol's quote stream; WITHOUT it the ask is a read that starts
    /// nothing. `Ok(None)` is "none yet"; `Err` is the reason no price can come.
    ///
    /// # Errors
    /// The failed dial (kept for [`REDIAL_AFTER`]), a refused subscription, the cap of
    /// `vike_exec::MAX_EXTRA_SYMBOLS` symbols, or a stream that is not labelled real-time.
    pub fn price(&self, symbol: &str, subscribe: bool) -> Result<Option<f64>, String> {
        let symbol = symbol.to_ascii_uppercase();
        if subscribe {
            self.ensure_subscribed(&symbol)?;
        }
        self.board.price(&symbol)
    }

    /// The symbols a quote stream was started for, in no order.
    #[must_use]
    pub fn priced(&self) -> Vec<String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.subscribed.iter().cloned().collect()
    }

    /// Dial the client if there is none and subscribe `symbol` if it is not. The state lock is held across
    /// the dial ON PURPOSE: a second asker waits for the first dial and then reads its client, which is what
    /// keeps N concurrent first orders at one dial. The dial is bounded by the gate (`dial_data`'s deadline).
    fn ensure_subscribed(&self, symbol: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.subscribed.contains(symbol) {
            return Ok(());
        }
        let cap = vike_exec::MAX_EXTRA_SYMBOLS;
        if state.subscribed.len() >= cap {
            return Err(format!(
                "ibkr: at most {cap} symbols are priced at once (the market-data line limit is \
                 unmeasured); {symbol} was not subscribed"
            ));
        }
        if state.client.is_none() {
            if let Some((at, why)) = &state.failed
                && at.elapsed() < REDIAL_AFTER
            {
                return Err(why.clone());
            }
            let sink: Arc<dyn LiveDataSink> = Arc::<QuoteBoard>::clone(&self.board);
            match (self.dial)(sink) {
                Ok(client) => {
                    state.client = Some(client);
                    state.failed = None;
                }
                Err(why) => {
                    let why = format!("ibkr: no market-data connection for {symbol}: {why}");
                    state.failed = Some((Instant::now(), why.clone()));
                    return Err(why);
                }
            }
        }
        let Some(client) = state.client.as_mut() else {
            return Err("ibkr: no market-data connection".to_string());
        };
        client
            .subscribe_quotes(symbol)
            .map_err(|e| format!("ibkr: the quote stream of {symbol} was refused: {e:?}"))?;
        state.subscribed.insert(symbol.to_string());
        Ok(())
    }
}

impl Drop for QuoteHandle {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(mut client) = state.client.take() {
            client.shutdown();
        }
    }
}

#[cfg(test)]
#[path = "quote_handle_tests.rs"]
mod quote_handle_tests;
