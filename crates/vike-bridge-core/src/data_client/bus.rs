//! **The market-data client over a bridge's SHARED source** ([`DataClientBus`] over a venue's
//! [`DataClientBusSpec`]): one connection or one process-wide source carries every symbol, so a subscribe is
//! only a WANT on that source and an unsubscribe an UNWANT. It serves the gatewayhub's credentialed
//! streams `cs_venue=dukascopy` (over `vike_dukascopy::feed::QuoteBus`) and `cs_venue=fxcm` (over
//! `vike_fxcm::feed::PriceDesk`), whose stream contracts are their bridges' module docs
//! (`crates/bridges/dukascopy/src/feed.rs`, `crates/bridges/fxcm/src/feed.rs`), and the
//! market data of cTrader (`vike_ctrader::feed`), polymarket
//! (`vike_polymarket::DataClientBusSpecPolymarket`) and ibkr's price stream (`vike_ibkr::feed::price`).
//!
//! What the engine holds, once for every source:
//!
//! * **Four lanes**, keyed by [`DataClientBusSpecKey`]: quotes, bars, trades, book. A lane the client does not
//!   carry is refused in the client's own words by its `BARS`/`TRADES`/`BOOK` constant, before anything else.
//! * **Holders counted per key.** Every subscribe is a holder with an id of its own. The source is asked on EVERY
//!   subscribe ([`DataClientBusSpecHold::ask`], told whether the key already streams, so the VENUE decides whether
//!   a repeat ask goes back to it) and told `unwant` only when the key's LAST holder leaves.
//! * **One symbol cap** (`MAX_SYMBOLS`): past it, a symbol no lane holds yet is refused in words.
//! * **One label rule**: a [`DataClientBusSpecEvent::Delay`] reaches the sink when a stream's label is first known
//!   and again when it changes, never per message. A key whose last holder leaves forgets its label, so a fresh
//!   subscription is labelled afresh.
//! * **One outage voice**: [`DataClientBusSpecEvent::Down`] opens ONE `GapStart` on every held key that has none
//!   open, [`DataClientBusSpecEvent::Up`] closes each with a `Live` echoing it, and a key whose last holder leaves
//!   (or a shutdown) closes its open gap. A source that discloses no link state (dukascopy, fxcm: their silence is
//!   the venue's) sends neither.
//! * **The receive time** (`local_timestamp`) and the symbol of every quote and trade, written here. With `PRICE_CHECK`,
//!   also the one price check: a quote must be two-sided, finite, positive and uncrossed, a trade must have a
//!   finite, positive price and size; what fails is dropped.
//! * **The teardown**: `begin_shutdown` reaches the source's hold (and, with `RELEASE_AT_BEGIN_SHUTDOWN`, lets every
//!   key go first); `shutdown` and a drop let every key go once (telling the source, unless `UNWANT_AT_SHUTDOWN` is
//!   off and the hold's drop tears the source down whole), then drop the hold.
//!
//! A source the engine does not hand a deliver to, because it takes a sink at its own construction (cTrader's actor
//! at its handshake), is built over a [`DataClientBusSink`]: the fixed sink its spec's `consume` attaches the
//! deliver to, so its ticks meet the same rules.
//!
//! Everything a venue decides (its words, whether a repeat ask goes back to the source, how its bars are
//! seeded) is its [`DataClientBusSpec`] and [`DataClientBusSpecHold`] impl, in its own bridge beside its source.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use vike_data::{DataClient, DataClientError, DataClientSink, DataClientSubscriptionId};
use vike_marketdata::{Bar, L2Book, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};
use vike_utility::mutex::lock;

/// One subscription's identity on a shared source: its lane, and what it names (the symbol in the engine's
/// spelling, the interval of a bar lane).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataClientBusSpecKey {
    Quotes(String),
    Trades(String),
    Book(String),
    Bars { symbol: String, interval: String },
}

impl DataClientBusSpecKey {
    /// The symbol the key names.
    #[must_use]
    pub fn symbol(&self) -> &str {
        match self {
            Self::Quotes(symbol)
            | Self::Trades(symbol)
            | Self::Book(symbol)
            | Self::Bars { symbol, .. } => symbol,
        }
    }

    /// The sink's word for the key's stream: the interval of a bar lane, a tick lane's [`StreamLabel`] word.
    fn stream(&self) -> &str {
        match self {
            Self::Quotes(_) => StreamLabel::Quotes.word(),
            Self::Trades(_) => StreamLabel::Trades.word(),
            Self::Book(_) => StreamLabel::Book.word(),
            Self::Bars { interval, .. } => interval,
        }
    }
}

/// What a source hands the engine, through its [`DataClientBusSpecDeliver`].
#[derive(Debug)]
pub enum DataClientBusSpecEvent<'a> {
    /// A quote of `symbol`; the engine writes its `local_timestamp` and `symbol`.
    Quote { symbol: &'a str, quote: TickQuote },
    /// A trade print of `symbol`; the engine writes its `local_timestamp` and `symbol`.
    Trade { symbol: &'a str, trade: TickTrade },
    /// `symbol`'s whole book.
    Book { symbol: &'a str, book: Arc<L2Book> },
    /// One raw book update of `symbol` (`DataClientSink::book_update`, the recordable twin of `Book`), as it came.
    BookUpdate { symbol: &'a str, update: L2BookUpdate },
    /// The recent history a (re)subscribed bar series starts from, ts-ascending.
    BarSeed { symbol: &'a str, interval: &'a str, bars: Vec<Bar> },
    /// One bar of the series: `closed` once it is final, else the bar still forming. With `BAR_CLOSE_TICK` its close
    /// is also the series' last-price snapshot.
    Bar { symbol: &'a str, interval: &'a str, bar: Bar, closed: bool },
    /// The delay label the venue states for the data it sent on `stream` (the sink's word for the lane).
    Delay { symbol: &'a str, stream: &'a str, delay: StreamDelay },
    /// One stream's own status, as the source judged it (a per-key gap, a stale stream, a tape hole under its own
    /// word), handed to the sink as it is: `stream` is the sink's word ([`StreamLabel::word`] for a tick lane, the
    /// interval for a bar lane, or a word of the source's own). The engine's own outage voice is `Down`/`Up`.
    Status { symbol: &'a str, stream: &'a str, status: StreamStatus },
    /// The shared connection is gone, from `at_ms`.
    Down { at_ms: i64 },
    /// It is back, and every held key streams again.
    Up,
}

/// Where a [`DataClientBusSpec`] hands each event it accepts.
pub type DataClientBusSpecDeliver = Box<dyn Fn(DataClientBusSpecEvent<'_>) + Send + Sync>;

/// A bridge's shared source, as a [`DataClientBus`] consumes it.
pub trait DataClientBusSpec: Send + Sync + 'static {
    /// The source's hold on its consumer place.
    type Hold: DataClientBusSpecHold;
    /// The venue the client streams and labels.
    const VENUE: &'static str;
    /// The refusal of a subscribe on a closed client.
    const CLOSED: &'static str;
    /// The bars lane: `Ok` carries it, `Err` refuses every bars subscribe in these words.
    const BARS: Result<(), &'static str>;
    /// The trades lane, as [`DataClientBusSpec::BARS`].
    const TRADES: Result<(), &'static str>;
    /// The book lane, as [`DataClientBusSpec::BARS`].
    const BOOK: Result<(), &'static str>;
    /// `(cap, why)`: at most `cap` symbols at once across every lane, and why, in the venue's words. `None`: the
    /// engine counts nothing (the source caps itself, in its own words).
    const MAX_SYMBOLS: Option<(usize, &'static str)>;
    /// Whether the engine applies its one price check (module doc). `false` for a source that judged every tick.
    const PRICE_CHECK: bool;
    /// `true`: a symbol reaches the source as the caller spelled it. `false` (the default): trimmed and upper-cased,
    /// the engine's spelling. A venue whose names are case-significant free text keeps them verbatim.
    const SYMBOL_VERBATIM: bool = false;
    /// `true`: every [`DataClientBusSpecEvent::Bar`] also files its close as the series' last-price snapshot
    /// (`DataClientSink::bar_close_tick`), ahead of the bar: a kline-driven source with no mark of its own. `false`
    /// (the default) keeps the source's bars off that slot.
    const BAR_CLOSE_TICK: bool = false;
    /// `true`: `begin_shutdown` lets every key go (each `unwant` in the usual way, its gap closed, its holders
    /// forgotten, so a later `unsubscribe` of one is the no-op of an unknown id) and keeps the source for a later
    /// subscribe. `false` (the default): it only tells the hold ([`DataClientBusSpecHold::begin_shutdown`]).
    const RELEASE_AT_BEGIN_SHUTDOWN: bool = false;
    /// `true` (the default): a shutdown (or a drop) tells the source `unwant` for every key before it drops the hold,
    /// a source that outlives the client (a process-wide bus, a shared socket) stops each stream. `false`: the hold's
    /// drop tears the whole source down at once (its threads stopped together, not key by key), and the engine only
    /// closes each key's open gap.
    const UNWANT_AT_SHUTDOWN: bool = true;

    /// Become the source's consumer: `deliver` takes every event the source accepts.
    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> Self::Hold;
}

/// What a [`DataClientBusSpec`] consumer holds; dropping it stops whatever it asked and removes the listener.
pub trait DataClientBusSpecHold: Send {
    /// `Err` (in words) while a newer consumer holds the source, or once a fault detached this one. Whether a
    /// replaced consumer may take a free source back is the venue's. The default: a source with one consumer place
    /// never refuses.
    fn ensure_current(&self) -> Result<(), String> {
        Ok(())
    }
    /// Ask the source for `key`, on EVERY subscribe, `streaming` when a holder of this client already holds it:
    /// the VENUE decides whether a repeat ask goes back to the source. `Err` is the source's refusal:
    /// `DataClientError::Unsupported` for what the source never serves (an interval, a lane its venue's caps row
    /// refuses, a name it does not list), `DataClientError::Subscribe` for one that failed this time.
    fn ask(&self, key: &DataClientBusSpecKey, streaming: bool) -> Result<(), DataClientError>;
    /// The key's last holder left.
    fn unwant(&self, key: &DataClientBusSpecKey);
    /// Phase one of a teardown (`DataClient::begin_shutdown`): raise the source's stop flags and RETURN AT ONCE,
    /// joining nothing; the client stays open for a later subscribe. The default does nothing.
    fn begin_shutdown(&self) {}
}

/// **The fixed sink** (module doc): a [`DataClientSink`] a source takes at its own construction, before its bus client
/// exists. The source's spec hands the consumer's deliver to [`DataClientBusSink::attach`] in its `consume`, and from
/// then on every verb reaches the engine as the event it names: a quote, a trade, a book and its raw update, a bar
/// seed, a closed or forming bar, a delay label, a stream status. Until a consumer attaches, and after
/// [`DataClientBusSink::detach`], what arrives is dropped. The bus carries no mark, depth, bar-close or funding lane:
/// `mark_tick` is dropped here and the others keep the trait's no-op, so a source that speaks them does not ride
/// this sink.
#[derive(Default)]
pub struct DataClientBusSink {
    deliver: RwLock<Option<DataClientBusSpecDeliver>>,
}

impl DataClientBusSink {
    /// Deliver into `deliver` from now on (a newer consumer replaces the older one).
    pub fn attach(&self, deliver: DataClientBusSpecDeliver) {
        *self.deliver.write().unwrap_or_else(PoisonError::into_inner) = Some(deliver);
    }

    /// Deliver nowhere from now on.
    pub fn detach(&self) {
        *self.deliver.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Hand `event` to the attached consumer: every verb below, and a source's own events beside them (IBKR's
    /// supervisor says its connection's `Down` and `Up` here).
    pub fn send(&self, event: DataClientBusSpecEvent<'_>) {
        if let Some(deliver) = self.deliver.read().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            deliver(event);
        }
    }
}

impl DataClientSink for DataClientBusSink {
    fn seed_bars(&self, _venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.send(DataClientBusSpecEvent::BarSeed { symbol, interval, bars });
    }
    fn close_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.send(DataClientBusSpecEvent::Bar { symbol, interval, bar, closed: true });
    }
    fn forming_bar(&self, _venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.send(DataClientBusSpecEvent::Bar { symbol, interval, bar, closed: false });
    }
    /// No mark lane on the bus (the type's doc).
    fn mark_tick(&self, _venue: &str, _symbol: &str, _price: f64, _ts: i64) {}
    fn quote(&self, _venue: &str, symbol: &str, quote: TickQuote) {
        self.send(DataClientBusSpecEvent::Quote { symbol, quote });
    }
    fn trade(&self, _venue: &str, symbol: &str, trade: TickTrade) {
        self.send(DataClientBusSpecEvent::Trade { symbol, trade });
    }
    fn book(&self, _venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.send(DataClientBusSpecEvent::Book { symbol, book });
    }
    fn book_update(&self, _venue: &str, symbol: &str, update: L2BookUpdate) {
        self.send(DataClientBusSpecEvent::BookUpdate { symbol, update });
    }
    fn stream_status(&self, _venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.send(DataClientBusSpecEvent::Status { symbol, stream, status });
    }
    fn data_delay(&self, _venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.send(DataClientBusSpecEvent::Delay { symbol, stream, delay });
    }
}

/// Hand on one two-sided quote that carries no sizes, after its label `delay`: the shape of a source that judges
/// its own ticks (dukascopy's bus, fxcm's desk), each of which says in its `consume` why it has no size.
pub fn deliver_quote(
    deliver: &DataClientBusSpecDeliver,
    symbol: &str,
    ts: i64,
    bid: f64,
    ask: f64,
    delay: StreamDelay,
) {
    deliver(DataClientBusSpecEvent::Delay { symbol, stream: StreamLabel::Quotes.word(), delay });
    let quote = TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    };
    deliver(DataClientBusSpecEvent::Quote { symbol, quote });
}

/// A market-data client over `B` (see the module doc).
pub struct DataClientBus<B: DataClientBusSpec> {
    hold: Option<B::Hold>,
    sink: Arc<dyn DataClientSink>,
    /// Every holder's id, with the key it holds: one id per subscribe.
    ids: HashMap<DataClientSubscriptionId, DataClientBusSpecKey>,
    /// Shared with the source's thread, which labels and discloses outages through it. ⚠ The hold is never
    /// called under this lock: a source may wait on its own thread, which delivers through it.
    held: Arc<Mutex<Held>>,
    next_id: u64,
}

#[derive(Default)]
struct Held {
    /// Each held key: its holder count, and when its open gap began.
    keys: HashMap<DataClientBusSpecKey, KeyHeld>,
    /// The label last sent, by stream word, then by the symbol the source named.
    labels: HashMap<String, HashMap<String, StreamDelay>>,
}

#[derive(Default)]
struct KeyHeld {
    holds: u32,
    gap: Option<i64>,
}

impl Held {
    /// Whether `delay` is news for `symbol`'s `stream` (first known, or changed); it is remembered.
    fn label(&mut self, symbol: &str, stream: &str, delay: StreamDelay) -> bool {
        if self.labels.get(stream).and_then(|by_symbol| by_symbol.get(symbol)) == Some(&delay) {
            return false;
        }
        self.labels.entry(stream.to_string()).or_default().insert(symbol.to_string(), delay);
        true
    }

    /// The key's last holder left: it is no longer held, its label is forgotten, and its open gap is handed back.
    fn release(&mut self, key: &DataClientBusSpecKey) -> Option<i64> {
        if let Some(by_symbol) = self.labels.get_mut(key.stream()) {
            by_symbol.remove(key.symbol());
        }
        self.keys.remove(key).and_then(|held| held.gap)
    }
}

/// The `Live` that closes `key`'s gap opened at `at`.
fn close_gap(sink: &dyn DataClientSink, venue: &str, key: &DataClientBusSpecKey, at: i64) {
    let live = StreamStatus::Live { gap_started_timestamp_ms: Some(at) };
    sink.stream_status(venue, key.symbol(), key.stream(), live);
}

/// The one price check: a quote two-sided, finite, positive and uncrossed. Public for a venue whose lanes write the
/// sink themselves under the same rule (alpaca's `SessionOn` lanes, owner ruling B-11).
#[must_use]
pub fn quote_prices_hold(quote: &TickQuote) -> bool {
    quote.bid.is_finite() && quote.ask.is_finite() && quote.bid > 0.0 && quote.bid <= quote.ask
}

/// The one price check: a print with a finite, positive price and size.
fn trade_prices_hold(trade: &TickTrade) -> bool {
    trade.price.is_finite() && trade.price > 0.0 && trade.size.is_finite() && trade.size > 0.0
}

/// One event from the source, on the source's thread.
fn deliver<B: DataClientBusSpec>(
    held: &Mutex<Held>,
    sink: &dyn DataClientSink,
    event: DataClientBusSpecEvent<'_>,
) {
    match event {
        DataClientBusSpecEvent::Quote { symbol, mut quote } => {
            if B::PRICE_CHECK && !quote_prices_hold(&quote) {
                return;
            }
            quote.local_timestamp = vike_model::now_ms();
            quote.symbol = symbol.into();
            sink.quote(B::VENUE, symbol, quote);
        }
        DataClientBusSpecEvent::Trade { symbol, mut trade } => {
            if B::PRICE_CHECK && !trade_prices_hold(&trade) {
                return;
            }
            trade.local_timestamp = vike_model::now_ms();
            trade.symbol = symbol.into();
            sink.trade(B::VENUE, symbol, trade);
        }
        DataClientBusSpecEvent::Book { symbol, book } => sink.book(B::VENUE, symbol, book),
        DataClientBusSpecEvent::BookUpdate { symbol, update } => {
            sink.book_update(B::VENUE, symbol, update);
        }
        DataClientBusSpecEvent::BarSeed { symbol, interval, bars } => {
            sink.seed_bars(B::VENUE, symbol, interval, bars);
        }
        DataClientBusSpecEvent::Bar { symbol, interval, bar, closed } => {
            if B::BAR_CLOSE_TICK {
                sink.bar_close_tick(B::VENUE, symbol, bar.close, bar.timestamp);
            }
            if closed {
                sink.close_bar(B::VENUE, symbol, interval, bar);
            } else {
                sink.forming_bar(B::VENUE, symbol, interval, bar);
            }
        }
        DataClientBusSpecEvent::Status { symbol, stream, status } => {
            sink.stream_status(B::VENUE, symbol, stream, status);
        }
        DataClientBusSpecEvent::Delay { symbol, stream, delay } => {
            let news = lock(held).label(symbol, stream, delay);
            if news {
                sink.data_delay(B::VENUE, symbol, stream, delay);
            }
        }
        DataClientBusSpecEvent::Down { at_ms } => {
            let opened: Vec<DataClientBusSpecKey> = lock(held)
                .keys
                .iter_mut()
                .filter(|(_, entry)| entry.gap.is_none())
                .map(|(key, entry)| {
                    entry.gap = Some(at_ms);
                    key.clone()
                })
                .collect();
            for key in opened {
                let gap = StreamStatus::GapStart { at_timestamp_ms: at_ms };
                sink.stream_status(B::VENUE, key.symbol(), key.stream(), gap);
            }
        }
        DataClientBusSpecEvent::Up => {
            let closed: Vec<(DataClientBusSpecKey, i64)> = lock(held)
                .keys
                .iter_mut()
                .filter_map(|(key, entry)| entry.gap.take().map(|at| (key.clone(), at)))
                .collect();
            for (key, at) in closed {
                close_gap(sink, B::VENUE, &key, at);
            }
        }
    }
}

/// The engine's spelling of a symbol, or the caller's with `SYMBOL_VERBATIM`.
fn spelled<B: DataClientBusSpec>(symbol: &str) -> String {
    if B::SYMBOL_VERBATIM { symbol.to_string() } else { symbol.trim().to_ascii_uppercase() }
}

impl<B: DataClientBusSpec> DataClientBus<B> {
    /// A client on `bus` delivering into `sink`. It becomes the source's consumer at once (nothing is
    /// subscribed): an older client on the same source is replaced.
    #[must_use]
    pub fn on(bus: &Arc<B>, sink: Arc<dyn DataClientSink>) -> Self {
        let held: Arc<Mutex<Held>> = Arc::default();
        let (shared, to) = (Arc::clone(&held), Arc::clone(&sink));
        let hold = bus.consume(Box::new(move |event: DataClientBusSpecEvent<'_>| {
            deliver::<B>(&shared, to.as_ref(), event);
        }));
        Self { hold: Some(hold), sink, ids: HashMap::new(), held, next_id: 0 }
    }

    /// One more holder of `key`, with an id of its own (see [`DataClient::subscribe_quotes`]'s doc).
    fn subscribe(
        &mut self,
        key: DataClientBusSpecKey,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let Some(hold) = &self.hold else {
            return Err(DataClientError::Subscribe(B::CLOSED.to_string()));
        };
        let streaming = {
            let held = lock(&self.held);
            let streaming = held.keys.contains_key(&key);
            if let Some((cap, why)) = B::MAX_SYMBOLS {
                let symbols: HashSet<&str> =
                    held.keys.keys().map(DataClientBusSpecKey::symbol).collect();
                let symbol = key.symbol();
                if !symbols.contains(symbol) && symbols.len() >= cap {
                    return Err(DataClientError::Subscribe(format!(
                        "{venue}: at most {cap} symbols are priced at once ({why}); {symbol} was not \
                         subscribed",
                        venue = B::VENUE,
                    )));
                }
            }
            streaming
        };
        hold.ensure_current().map_err(DataClientError::Subscribe)?;
        hold.ask(&key, streaming)?;
        self.next_id += 1;
        let id = DataClientSubscriptionId(self.next_id);
        lock(&self.held).keys.entry(key.clone()).or_default().holds += 1;
        self.ids.insert(id, key);
        Ok(id)
    }

    /// `key`'s last holder left: the source is told, and an open gap closes.
    fn let_go(&self, key: &DataClientBusSpecKey, gap: Option<i64>) {
        self.let_go_as(key, gap, true);
    }

    /// [`Self::let_go`], telling the source only when `unwant`.
    fn let_go_as(&self, key: &DataClientBusSpecKey, gap: Option<i64>, unwant: bool) {
        if unwant && let Some(hold) = &self.hold {
            hold.unwant(key);
        }
        if let Some(at) = gap {
            close_gap(self.sink.as_ref(), B::VENUE, key, at);
        }
    }

    /// Every key goes once (each `unwant`, unless `UNWANT_AT_SHUTDOWN` is off), then the hold is dropped.
    fn close(&mut self) {
        self.release_every_key(B::UNWANT_AT_SHUTDOWN);
        // Dropping the hold stops whatever else it asked and removes the listener.
        self.hold = None;
    }

    /// Let every key go once, whatever its holder count, its open gap closed and the source told when `unwant`;
    /// every holder is forgotten. The hold stays.
    fn release_every_key(&mut self, unwant: bool) {
        self.ids.clear();
        let released: Vec<(DataClientBusSpecKey, Option<i64>)> = {
            let mut held = lock(&self.held);
            let keys: Vec<DataClientBusSpecKey> = held.keys.keys().cloned().collect();
            keys.into_iter()
                .map(|key| {
                    let gap = held.release(&key);
                    (key, gap)
                })
                .collect()
        };
        for (key, gap) in released {
            self.let_go_as(&key, gap, unwant);
        }
    }
}

impl<B: DataClientBusSpec> DataClient for DataClientBus<B> {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        B::BARS.map_err(DataClientError::Unsupported)?;
        self.subscribe(DataClientBusSpecKey::Bars {
            symbol: spelled::<B>(symbol),
            interval: interval.to_string(),
        })
    }

    /// Start the quote stream of `symbol` (the engine's spelling) for one more holder, with an id of its own: the
    /// symbol stops only when its LAST holder leaves. A symbol already streaming is admitted only while this
    /// client is the source's consumer: while a newer client holds the source (or once a fault in its consumer
    /// detached it) every ask is refused in words, the already-streaming symbols included, so the tradehub is
    /// never told a stopped feed still prices. Whether a repeat ask also goes to the source is the venue's
    /// ([`DataClientBusSpecHold::ask`]). Every other lane's subscribe follows the same rules.
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.subscribe(DataClientBusSpecKey::Quotes(spelled::<B>(symbol)))
    }

    fn subscribe_trades(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        B::TRADES.map_err(DataClientError::Unsupported)?;
        self.subscribe(DataClientBusSpecKey::Trades(spelled::<B>(symbol)))
    }

    fn subscribe_book(
        &mut self,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        B::BOOK.map_err(DataClientError::Unsupported)?;
        self.subscribe(DataClientBusSpecKey::Book(spelled::<B>(symbol)))
    }

    /// One holder leaves; the key stops when it was the last. An unknown id is a no-op.
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        let Some(key) = self.ids.remove(&id) else { return };
        let gap = {
            let mut held = lock(&self.held);
            let Some(entry) = held.keys.get_mut(&key) else { return };
            entry.holds = entry.holds.saturating_sub(1);
            if entry.holds > 0 {
                return;
            }
            held.release(&key)
        };
        self.let_go(&key, gap);
    }

    /// Phase one of a teardown (module doc): with `RELEASE_AT_BEGIN_SHUTDOWN` every key goes first, then the hold
    /// is told ([`DataClientBusSpecHold::begin_shutdown`]). The client stays open for a later subscribe.
    fn begin_shutdown(&mut self) {
        if B::RELEASE_AT_BEGIN_SHUTDOWN {
            self.release_every_key(true);
        }
        if let Some(hold) = &self.hold {
            hold.begin_shutdown();
        }
    }

    fn shutdown(&mut self) {
        self.close();
    }
}

impl<B: DataClientBusSpec> Drop for DataClientBus<B> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "bus_tests.rs"]
mod bus_tests;
