//! The cpapi market lane: ONE thread and ONE gateway WS for every subscription the Client Portal
//! backend (`super::DataClientBusSpecBackendIbkrCpapi`) holds, multiplexed by conId, on the shared
//! market pump. Each `smd+<conid>` row is folded by that conId's [`SmdAccumulator`] and handed to
//! every stream on the conId. REST is used for two seeds: the first quote of a new conId
//! (`/iserver/marketdata/snapshot`) and a bar stream's history (`/iserver/marketdata/history`), both
//! before the stream's first live row; a bar stream that saw the socket drop is re-seeded before its
//! `Live`.
//!
//! The pump is `vike_bridge_core::websocket_session::market`'s `run_market_on`, its knobs the Client
//! Portal's pump row (`vike_bridge_core::websocket_session::spec::IBKR_CPAPI`, which reads the market
//! lane of `vike_venues::venue_timings::IBKR_CPAPI`): it redials on the row's fixed 2 s after a failed
//! dial or a dropped session, ends a session silent past the row's 60 s idle limit (the gateway's
//! documented `system` heartbeat comes every 10 s on every connected socket), and says when a session
//! is live. The lane keeps what is its own:
//! - the dial: the SAME `connect_websocket` the exec side's `sor` pump uses
//!   (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `connect_websocket`);
//! - the session's stream ([`Session`]): at each open it subscribes every held conId and primes it;
//!   before each read it applies the commands (add or remove a stream) that arrived on the channel,
//!   then renews the gateway session by the REST `tickle` on the row's 30 s cadence, as the lane's own
//!   loop did. A command that arrives while the pump waits to redial is applied at the next dial;
//! - the fold ([`Book`]) and the status words, written through `DataClientContext::set_status`; a
//!   session's end opens every held stream's gap.
//!
//! ⚠ UNMEASURED: whether a CP Gateway serves this second WS beside the exec's own without dropping
//! either. One WS per data client (not per subscription) keeps the count at two for a box running both.
//!
//! Bars are aggregated from the stream by the shared `BarAggregator`
//! (`crates/bridges/vike-ibkr/src/feed/bar.rs`, the folder the socket backend runs its
//! 5-second bars through), fed one single-print bar per tick: the last-price print, or for a
//! `CASH` pair, which prints nothing, the quote's mid.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::live_status;
use vike_bridge_core::pump_clock;
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionMarketStream,
    WebsocketSessionMarketStreamTungstenite, WebsocketSessionStatus, run_market_on,
};
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserError;
use vike_data::{DataClientSink, DataClientSubscriptionId};
use vike_marketdata::Bar;
use vike_model::feed_status::DataClientConnectionState;
use vike_model::{StreamDelay, StreamLabel, StreamStatus};
use vike_transport::websocket::configure_websocket_stream;
use vike_venues::venue_timings;

use super::decode::{
    SmdAccumulator, SmdUpdate, decode_history, history_bar_param, history_period, smd_conid,
    smd_subscribe, smd_unsubscribe, snapshot_fields, snapshot_row,
};
use crate::feed::VENUE;
use crate::feed::bar::BarAggregator;
use crate::transport::CpapiRest;
use crate::transport::cpapi::connect_websocket;

/// The REST renewal cadence (the exec pump's): the market lane's `session_renew_every` in
/// `vike_venues::venue_timings::IBKR_CPAPI`. A tickle renews the gateway session and produces no
/// inbound frame, which is why the table keeps it apart from a socket keepalive.
const TICKLE_EVERY: Duration = venue_timings::IBKR_CPAPI.market().session_renew_every();

/// What the status line names.
const DISPLAY: &str = "IBKR Client Portal market data";

/// One stream a subscriber asked for.
pub(crate) enum FeedLane {
    Quotes,
    Trades,
    Bars {
        interval: String,
        interval_ms: i64,
        agg: BarAggregator,
        /// Aggregate the quote MID instead of prints (a `CASH` pair prints no last price).
        from_mid: bool,
    },
}

impl FeedLane {
    /// The `stream` word the sink's `stream_status` takes: the interval for a bar lane.
    fn name(&self) -> &str {
        match self {
            FeedLane::Quotes => StreamLabel::Quotes.word(),
            FeedLane::Trades => StreamLabel::Trades.word(),
            FeedLane::Bars { interval, .. } => interval,
        }
    }
}

/// A command from the client to the pump.
pub(crate) enum FeedCpapiCommand {
    Add {
        id: DataClientSubscriptionId,
        conid: i64,
        symbol: String,
        lane: FeedLane,
    },
    /// Drop a stream; the pump answers on `done` once nothing more is delivered for it.
    Remove {
        id: DataClientSubscriptionId,
        done: Sender<()>,
    },
}

struct Stream {
    id: DataClientSubscriptionId,
    symbol: String,
    lane: FeedLane,
    /// The delay label last noted for this stream (see [`note_delay`]).
    delay: Option<StreamDelay>,
    /// When this stream's gap opened, if the socket dropped under it.
    gap_since: Option<i64>,
    /// A bar stream's history seed has been attempted.
    seeded: bool,
    /// The log was told this quote stream's quotes wait for a label (once, see [`deliver`]).
    said_unlabelled: bool,
}

struct ConidState {
    accumulator: SmdAccumulator,
    streams: Vec<Stream>,
    /// The first-quote snapshot has been attempted.
    primed: bool,
}

/// Everything the pump folds, separated from the socket so the tests drive it offline.
pub(crate) struct Book {
    sink: Arc<dyn DataClientSink>,
    conids: HashMap<i64, ConidState>,
}

impl Book {
    pub(crate) fn new(sink: Arc<dyn DataClientSink>) -> Self {
        Book { sink, conids: HashMap::new() }
    }

    /// Register a stream. Returns `true` when `conid` is new (the caller subscribes it on the WS).
    /// A quote stream joining a known conId is handed the held quote at once, stamped with the venue
    /// time it first had; the receipt time is its `local_ts`.
    pub(crate) fn add(
        &mut self,
        id: DataClientSubscriptionId,
        conid: i64,
        symbol: String,
        lane: FeedLane,
    ) -> bool {
        let fresh = !self.conids.contains_key(&conid);
        let state = self.conids.entry(conid).or_insert_with(|| ConidState {
            accumulator: SmdAccumulator::default(),
            streams: Vec::new(),
            primed: false,
        });
        let mut stream = Stream {
            id,
            symbol,
            lane,
            delay: None,
            gap_since: None,
            seeded: false,
            said_unlabelled: false,
        };
        let held = matches!(stream.lane, FeedLane::Quotes)
            .then(|| state.accumulator.current_quote(vike_model::now_ms()))
            .flatten();
        if let Some(q) = held {
            note_delay(self.sink.as_ref(), &mut stream, q.delay);
            // The rule of `deliver`: a quote with no label in force waits for a row that states one.
            if q.delay.is_some() {
                crate::execution::delayed_order::note_quote(
                    &stream.symbol,
                    q.delay,
                    q.quote.bid,
                    q.quote.ask,
                );
                self.sink.quote(VENUE, &stream.symbol, q.quote);
            }
        }
        state.streams.push(stream);
        fresh
    }

    /// Drop stream `id`. Returns the conId when it was that conId's last stream (the caller then
    /// unsubscribes it on the WS).
    pub(crate) fn remove(&mut self, id: DataClientSubscriptionId) -> Option<i64> {
        let conid = self
            .conids
            .iter()
            .find(|(_, s)| s.streams.iter().any(|st| st.id == id))
            .map(|(c, _)| *c)?;
        let state = self.conids.get_mut(&conid)?;
        state.streams.retain(|st| st.id != id);
        if state.streams.is_empty() {
            self.conids.remove(&conid);
            return Some(conid);
        }
        None
    }

    /// The number of streams held, over every conId.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.conids.values().map(|s| s.streams.len()).sum()
    }

    fn conid_list(&self) -> Vec<i64> {
        self.conids.keys().copied().collect()
    }

    /// Fold one WS frame. A frame on any topic but `smd`, or for a conId nothing holds, is ignored.
    pub(crate) fn on_frame(&mut self, frame: &Value) {
        let Some(conid) = smd_conid(frame) else { return };
        self.apply_row(conid, frame);
    }

    /// Fold one row (a stream row or a snapshot row — they share the field codes) for `conid`.
    pub(crate) fn apply_row(&mut self, conid: i64, row: &Value) {
        let Some(state) = self.conids.get_mut(&conid) else { return };
        let update = state.accumulator.apply(row, vike_model::now_ms());
        for stream in &mut state.streams {
            deliver(self.sink.as_ref(), stream, &update);
        }
    }

    /// The socket dropped (or never came up): every stream's gap opens now, once — a redial loop
    /// does not re-open it.
    fn open_gaps(&mut self, at_ts_ms: i64) {
        for stream in self.conids.values_mut().flat_map(|s| s.streams.iter_mut()) {
            if stream.gap_since.is_none() {
                stream.gap_since = Some(at_ts_ms);
                self.sink.stream_status(
                    VENUE,
                    &stream.symbol,
                    stream.lane.name(),
                    StreamStatus::GapStart { at_timestamp_ms: at_ts_ms },
                );
            }
        }
    }

    /// Bring `conid` up on a live socket: the first-quote snapshot when it was never taken, a
    /// history seed for every bar stream not yet seeded, a RE-seed for every gapped bar stream,
    /// then `Live` for every gapped stream.
    fn prime(&mut self, conid: i64, rest: &CpapiRest) {
        let needs_snapshot = self.conids.get(&conid).is_some_and(|s| !s.primed);
        if needs_snapshot {
            match rest.marketdata_snapshot(conid, &snapshot_fields()) {
                Ok(body) => {
                    if let Some(row) = snapshot_row(&body, conid).cloned() {
                        self.apply_row(conid, &row);
                    }
                }
                Err(e) => tracing::warn!(conid, error = %e.msg, "ibkr cpapi snapshot failed"),
            }
        }
        let Some(state) = self.conids.get_mut(&conid) else { return };
        state.primed = true;
        for stream in &mut state.streams {
            let gap = stream.gap_since.take();
            let is_bars = matches!(stream.lane, FeedLane::Bars { .. });
            if is_bars && (!stream.seeded || gap.is_some()) {
                seed_history(rest, self.sink.as_ref(), conid, stream);
                stream.seeded = true;
            }
            if let Some(gap) = gap {
                self.sink.stream_status(
                    VENUE,
                    &stream.symbol,
                    stream.lane.name(),
                    StreamStatus::Live { gap_started_timestamp_ms: Some(gap) },
                );
            }
        }
    }
}

/// Hand one row's update to one stream: its label first (only when the row carries data for this
/// lane, and only on first knowledge or a change — [`note_delay`]), then the data.
///
/// ⚠ **A quote with no label in force is not delivered.** The label in force is the row's marker, sticky across rows
/// without one, and none before the first marker or after one this decoder does not read (`N`, not subscribed). The
/// tap already reads a stream with no label as "none yet" (`crates/vike-tradehub/src/feeds/price_tap.rs`'s
/// `labelled_quotes`), but the datahub's recorder stores whatever is not labelled delayed or frozen (decision 0193
/// verdict 7), and on an account with no real-time entitlement an unlabelled row is as likely delayed as not. On this
/// backend the label is a field of the row (`6509`, IBKR's documented shape, never captured), so a row can arrive
/// without it; on the socket the label is read off the very tick that makes the quote
/// (`crate::feed::socket::map`'s `forward_quote_tick`), so the case does not arise there.
fn deliver(sink: &dyn DataClientSink, stream: &mut Stream, update: &SmdUpdate) {
    let label = match &stream.lane {
        FeedLane::Quotes => update.quote.as_ref().and_then(|q| q.delay),
        FeedLane::Trades => update.trade.as_ref().and_then(|t| t.delay),
        FeedLane::Bars { from_mid: true, .. } => update.quote.as_ref().and(update.delay),
        FeedLane::Bars { from_mid: false, .. } => update.trade.as_ref().and(update.delay),
    };
    note_delay(sink, stream, label);
    let symbol = stream.symbol.as_str();
    match &mut stream.lane {
        FeedLane::Quotes => {
            if let Some(q) = &update.quote {
                if q.delay.is_some() {
                    crate::execution::delayed_order::note_quote(
                        symbol,
                        q.delay,
                        q.quote.bid,
                        q.quote.ask,
                    );
                    sink.quote(VENUE, symbol, q.quote.clone());
                } else if !stream.said_unlabelled {
                    stream.said_unlabelled = true;
                    tracing::warn!(
                        %symbol,
                        "ibkr cpapi quotes: a row of {symbol} states no market-data availability (field 6509), so its \
                         quotes are held back until one does: neither a price nor a recorded row can be told real time \
                         from delayed without it"
                    );
                }
            }
        }
        FeedLane::Trades => {
            if let Some(t) = &update.trade {
                sink.trade(VENUE, symbol, t.trade.clone());
            }
        }
        FeedLane::Bars { interval, agg, from_mid, .. } => {
            let tick = if *from_mid {
                update
                    .quote
                    .as_ref()
                    .map(|q| ((q.quote.bid + q.quote.ask) / 2.0, 0.0, q.quote.timestamp))
            } else {
                update.trade.as_ref().map(|t| (t.trade.price, t.trade.size, t.trade.timestamp))
            };
            if let Some((px, size, ts)) = tick {
                let (forming, closed) = agg.fold(&tick_bar(px, size, ts));
                // `bar_close_tick`, NOT `mark_tick`: a print or a mid is not the venue's mark.
                sink.bar_close_tick(VENUE, symbol, px, ts);
                if let Some(c) = closed {
                    sink.close_bar(VENUE, symbol, interval, c);
                }
                sink.forming_bar(VENUE, symbol, interval, forming);
            }
        }
    }
}

/// One print as a bar, for the aggregator.
fn tick_bar(px: f64, size: f64, ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: size,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Seed one bar stream from `/iserver/marketdata/history`. Best effort, as the socket backend's
/// seed is: a failed or empty fetch, or an interval the endpoint has no exact bar for, leaves the
/// live stream running unseeded (with a warning).
fn seed_history(rest: &CpapiRest, sink: &dyn DataClientSink, conid: i64, stream: &mut Stream) {
    let (interval, interval_ms) = match &stream.lane {
        FeedLane::Bars { interval, interval_ms, .. } => (interval.clone(), *interval_ms),
        FeedLane::Quotes | FeedLane::Trades => return,
    };
    let Some(bar) = history_bar_param(interval_ms) else {
        tracing::warn!(
            symbol = %stream.symbol,
            %interval,
            "ibkr cpapi bars: the history endpoint has no bar of this length — no seed, live \
             bars only"
        );
        return;
    };
    match rest.marketdata_history(conid, &history_period(interval_ms), bar) {
        Ok(body) => {
            let history = decode_history(&body);
            if !history.bars.is_empty() {
                note_delay(sink, stream, history.delay);
                sink.seed_bars(VENUE, &stream.symbol, &interval, history.bars);
            }
        }
        Err(e) => {
            tracing::warn!(symbol = %stream.symbol, error = %e.msg, "ibkr cpapi history failed");
        }
    }
}

/// THE ONE site the cpapi feed sends a stream's delay label: `DataClientSink::data_delay`, when the
/// label is first known and again whenever it CHANGES — never per message (decision 0140: "sent at
/// subscribe and again whenever the label of a stream changes"; the socket feed's twin is
/// `crates/bridges/vike-ibkr/src/feed/socket/map.rs`'s `forward_quote_tick`). `None` (no marker
/// this decoder reads) sends nothing and keeps the last label sent.
fn note_delay(sink: &dyn DataClientSink, stream: &mut Stream, delay: Option<StreamDelay>) {
    let Some(delay) = delay else { return };
    if stream.delay == Some(delay) {
        return;
    }
    stream.delay = Some(delay);
    sink.data_delay(VENUE, &stream.symbol, stream.lane.name(), delay);
}

/// The pump thread's body over what it owns (the WS address, its REST lane, the [`Book`], the command lane and the
/// thread's context, whose `stop` ends it): [`run_on`] with the real dial and the real tickle.
pub(crate) fn run(
    websocket_url: &str,
    rest: &CpapiRest,
    book: Book,
    commands: &Receiver<FeedCpapiCommand>,
    ctx: &DataClientContext,
) {
    let tickle = || {
        let _ = rest.tickle();
    };
    run_on(|| dial(websocket_url), rest, book, commands, ctx, &tickle);
}

/// Dial the gateway's socket and set the pump row's read timeout on it, the lane's command and stop poll.
fn dial(websocket_url: &str) -> Result<WebsocketSessionMarketStreamTungstenite, String> {
    let socket = connect_websocket(websocket_url)
        .map_err(|e| format!("cpapi market-data ws connect: {e}"))?;
    configure_websocket_stream(&socket, spec::IBKR_CPAPI.read_timeout);
    Ok(WebsocketSessionMarketStreamTungstenite::new(WebsocketSessionUserStreamTungstenite(socket)))
}

/// [`run`] over any stream, its two network steps handed in (`dial`, and `tickle`, the REST renewal) so a test runs
/// it over scripted sessions on a virtual clock: the shared market pump with the Client Portal's row, each session's
/// stream a [`Session`], each frame folded into the [`Book`], and each session's end every held stream's gap.
fn run_on<M: WebsocketSessionMarketStream>(
    mut dial: impl FnMut() -> Result<M, String>,
    rest: &CpapiRest,
    book: Book,
    commands: &Receiver<FeedCpapiCommand>,
    ctx: &DataClientContext,
    tickle: &dyn Fn(),
) {
    let lane = &Lane {
        book: RefCell::new(book),
        rest,
        commands,
        stop: &ctx.stop,
        tickle,
        last_tickle: Cell::new(pump_clock::now()),
    };
    ctx.set_status(DataClientConnectionState::Connecting, format!("connecting · {DISPLAY}"));
    run_market_on(
        || {
            // A command asked while the pump waited to redial is applied first, so the dial subscribes what is held now.
            if lane.apply_commands(None) {
                lane.stop.store(true, Ordering::Relaxed);
                return Err("the market-data backend is gone".to_string());
            }
            let mut session = Session { socket: dial()?, lane };
            let held = lane.book.borrow().conid_list();
            for conid in held {
                let _ = session.socket.send_text(&smd_subscribe(conid));
                lane.book.borrow_mut().prime(conid, rest);
            }
            Ok(session)
        },
        &spec::IBKR_CPAPI.opts(None, None),
        &ctx.stop,
        &vike_model::now_ms,
        |text| match serde_json::from_str::<Value>(text) {
            Ok(frame) => {
                lane.book.borrow_mut().on_frame(&frame);
                // Every frame (the `system` greeting and heartbeat, an `smd` row) is the gateway speaking.
                WebsocketSessionFrameOutcome::Confirm
            }
            Err(_) => WebsocketSessionFrameOutcome::Ignore,
        },
        || {},
        |status| match status {
            WebsocketSessionStatus::Live => {
                ctx.set_status(DataClientConnectionState::Connected, live_status(DISPLAY));
            }
            // A failed dial, a dropped socket and a silent one alike: every held stream's gap opens (once).
            WebsocketSessionStatus::Error(e) => {
                lane.book.borrow_mut().open_gaps(vike_model::now_ms());
                ctx.set_status(
                    DataClientConnectionState::Error,
                    format!("{DISPLAY}: {e}; reconnecting"),
                );
            }
            // The row's opts arm no notice hook (the gateway announces no close): no session ends announced.
            WebsocketSessionStatus::Announced(_) => {}
        },
    );
}

/// What the lane holds across its sessions, on the pump's one thread: the fold, the command lane, the REST lane its
/// primes read, the tickle and when it last ran. The session's stream and the pump's hooks reach it in turn, never at
/// once.
struct Lane<'a> {
    book: RefCell<Book>,
    rest: &'a CpapiRest,
    commands: &'a Receiver<FeedCpapiCommand>,
    stop: &'a AtomicBool,
    tickle: &'a dyn Fn(),
    /// When the last tickle ran, on the pump clock (`pump_clock`); the lane's start before the first.
    last_tickle: Cell<Instant>,
}

impl Lane<'_> {
    /// Drain the command channel. With a socket, a new conId is subscribed on it and primed at once; without one
    /// (between sessions) the next dial subscribes and primes every held conId. `true` when the backend is gone (the
    /// channel closed).
    fn apply_commands(&self, mut ws: Option<&mut dyn WebsocketSessionMarketStream>) -> bool {
        loop {
            match self.commands.try_recv() {
                Ok(FeedCpapiCommand::Add { id, conid, symbol, lane }) => {
                    let fresh = self.book.borrow_mut().add(id, conid, symbol, lane);
                    if let Some(ws) = ws.as_deref_mut() {
                        if fresh {
                            let _ = ws.send_text(&smd_subscribe(conid));
                        }
                        self.book.borrow_mut().prime(conid, self.rest);
                    }
                }
                Ok(FeedCpapiCommand::Remove { id, done }) => {
                    let last = self.book.borrow_mut().remove(id);
                    if let (Some(conid), Some(ws)) = (last, ws.as_deref_mut()) {
                        let _ = ws.send_text(&smd_unsubscribe(conid));
                    }
                    let _ = done.send(());
                }
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
    }

    /// The REST tickle, when [`TICKLE_EVERY`] has passed since the last one (since the lane started, before the
    /// first): checked before each read of an open session, as the lane's own loop did, never between sessions.
    fn tickle_when_due(&self) {
        if pump_clock::since(self.last_tickle.get()) >= TICKLE_EVERY {
            (self.tickle)();
            self.last_tickle.set(pump_clock::now());
        }
    }
}

/// One gateway session's stream as the pump reads it: before each read the lane's commands and its tickle, then one
/// frame of the socket. Sending and the idle clock are the socket's.
struct Session<'l, 'a, M> {
    socket: M,
    lane: &'l Lane<'a>,
}

impl<M: WebsocketSessionMarketStream> WebsocketSessionMarketStream for Session<'_, '_, M> {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        if self.lane.apply_commands(Some(&mut self.socket)) {
            // The backend is gone (its channel closed): raise the stop, which the pump reads after this read.
            self.lane.stop.store(true, Ordering::Relaxed);
        } else {
            self.lane.tickle_when_due();
        }
        self.socket.read_frame()
    }

    fn send_text(&mut self, text: &str) -> Result<(), WebsocketSessionUserError> {
        self.socket.send_text(text)
    }

    fn since_last_frame(&self) -> Duration {
        self.socket.since_last_frame()
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod pump_tests;
