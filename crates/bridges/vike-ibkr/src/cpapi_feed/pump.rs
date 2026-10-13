//! The cpapi market-data pump: ONE thread and ONE gateway WS for every subscription a
//! `CpapiFeeds` holds, multiplexed by conId. Commands (add/remove a stream) arrive on a channel and
//! are applied between reads; each `smd+<conid>` row is folded by that conId's
//! [`SmdAccumulator`] and handed to every stream on the conId. REST is used for two seeds: the
//! first quote of a new conId (`/iserver/marketdata/snapshot`) and a bar stream's history
//! (`/iserver/marketdata/history`), both before the stream's first live row; a bar stream that saw
//! the socket drop is re-seeded before its `Live`.
//!
//! The WS is dialled by the SAME `connect_ws` the exec side's `sor` pump uses
//! (`crates/bridges/vike-ibkr/src/transport/cpapi/mod.rs`'s `connect_ws`), and the session is kept
//! alive by the same REST `tickle` every 30 s. ⚠ UNMEASURED: whether a CP Gateway serves this second
//! WS beside the exec's own without dropping either. One WS per data client (not per subscription)
//! keeps the count at two for a box running both.
//!
//! Bars are aggregated from the stream by the shared `BarAggregator`
//! (`crates/bridges/vike-ibkr/src/agg.rs`, the folder the socket backend runs its
//! 5-second bars through), fed one single-print bar per tick: the last-price print, or for a
//! `CASH` pair, which prints nothing, the quote's mid.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::user_data::{StreamError, StreamMsg, UserStream};
use vike_bridge_core::ws::{TungsteniteStream, configure_ws_stream};
use vike_data::{LiveDataSink, SubscriptionId};
use vike_marketdata::Bar;
use vike_model::{DataDelay, StreamStatus};

use super::decode::{
    SmdAccumulator, SmdUpdate, decode_history, history_bar_param, history_period, smd_conid,
    smd_subscribe, smd_unsubscribe, snapshot_fields, snapshot_row,
};
use crate::agg::BarAggregator;
use crate::data_door::VENUE;
use crate::transport::CpapiRest;
use crate::transport::cpapi::connect_ws;

/// The socket read timeout, so commands and `stop` are looked at even when the market is quiet.
const READ_TIMEOUT: Duration = Duration::from_millis(200);
/// The REST keepalive cadence (the exec pump's).
const TICKLE_EVERY: Duration = Duration::from_secs(30);
/// The wait between failed dials.
const REDIAL: Duration = Duration::from_secs(2);

/// One stream a subscriber asked for.
pub(crate) enum Lane {
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

impl Lane {
    /// The `stream` word the sink's `stream_status` takes: the interval for a bar lane.
    fn name(&self) -> &str {
        match self {
            Lane::Quotes => "quotes",
            Lane::Trades => "trades",
            Lane::Bars { interval, .. } => interval,
        }
    }
}

/// A command from the client to the pump.
pub(crate) enum Cmd {
    Add {
        id: SubscriptionId,
        conid: i64,
        symbol: String,
        lane: Lane,
    },
    /// Drop a stream; the pump answers on `done` once nothing more is delivered for it.
    Remove {
        id: SubscriptionId,
        done: Sender<()>,
    },
}

struct Stream {
    id: SubscriptionId,
    symbol: String,
    lane: Lane,
    /// The delay label last noted for this stream (see [`note_delay`]).
    delay: Option<DataDelay>,
    /// When this stream's gap opened, if the socket dropped under it.
    gap_since: Option<i64>,
    /// A bar stream's history seed has been attempted.
    seeded: bool,
}

struct ConidState {
    acc: SmdAccumulator,
    streams: Vec<Stream>,
    /// The first-quote snapshot has been attempted.
    primed: bool,
}

/// Everything the pump folds, separated from the socket so the tests drive it offline.
pub(crate) struct Book {
    sink: Arc<dyn LiveDataSink>,
    conids: HashMap<i64, ConidState>,
}

impl Book {
    pub(crate) fn new(sink: Arc<dyn LiveDataSink>) -> Self {
        Book { sink, conids: HashMap::new() }
    }

    /// Register a stream. Returns `true` when `conid` is new (the caller subscribes it on the WS).
    /// A quote stream joining a known conId is handed the held quote at once.
    pub(crate) fn add(
        &mut self,
        id: SubscriptionId,
        conid: i64,
        symbol: String,
        lane: Lane,
    ) -> bool {
        let fresh = !self.conids.contains_key(&conid);
        let state = self.conids.entry(conid).or_insert_with(|| ConidState {
            acc: SmdAccumulator::default(),
            streams: Vec::new(),
            primed: false,
        });
        let mut stream = Stream { id, symbol, lane, delay: None, gap_since: None, seeded: false };
        let held = matches!(stream.lane, Lane::Quotes)
            .then(|| state.acc.current_quote(vike_model::now_ms()))
            .flatten();
        if let Some(q) = held {
            note_delay(self.sink.as_ref(), &mut stream, q.delay);
            self.sink.quote(VENUE, &stream.symbol, q.quote);
        }
        state.streams.push(stream);
        fresh
    }

    /// Drop stream `id`. Returns the conId when it was that conId's last stream (the caller then
    /// unsubscribes it on the WS).
    pub(crate) fn remove(&mut self, id: SubscriptionId) -> Option<i64> {
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
        let update = state.acc.apply(row, vike_model::now_ms());
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
                    StreamStatus::GapStart { at_ts_ms },
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
            let is_bars = matches!(stream.lane, Lane::Bars { .. });
            if is_bars && (!stream.seeded || gap.is_some()) {
                seed_history(rest, self.sink.as_ref(), conid, stream);
                stream.seeded = true;
            }
            if let Some(gap) = gap {
                self.sink.stream_status(
                    VENUE,
                    &stream.symbol,
                    stream.lane.name(),
                    StreamStatus::Live { gap_started_ts_ms: Some(gap) },
                );
            }
        }
    }
}

/// Hand one row's update to one stream: its label first (only when the row carries data for this
/// lane, and only on first knowledge or a change — [`note_delay`]), then the data.
fn deliver(sink: &dyn LiveDataSink, stream: &mut Stream, update: &SmdUpdate) {
    let label = match &stream.lane {
        Lane::Quotes => update.quote.as_ref().and_then(|q| q.delay),
        Lane::Trades => update.trade.as_ref().and_then(|t| t.delay),
        Lane::Bars { from_mid: true, .. } => update.quote.as_ref().and(update.delay),
        Lane::Bars { from_mid: false, .. } => update.trade.as_ref().and(update.delay),
    };
    note_delay(sink, stream, label);
    let symbol = stream.symbol.as_str();
    match &mut stream.lane {
        Lane::Quotes => {
            if let Some(q) = &update.quote {
                sink.quote(VENUE, symbol, q.quote.clone());
            }
        }
        Lane::Trades => {
            if let Some(t) = &update.trade {
                sink.trade(VENUE, symbol, t.trade.clone());
            }
        }
        Lane::Bars { interval, agg, from_mid, .. } => {
            let tick = if *from_mid {
                update.quote.as_ref().map(|q| ((q.quote.bid + q.quote.ask) / 2.0, 0.0, q.quote.ts))
            } else {
                update.trade.as_ref().map(|t| (t.trade.price, t.trade.size, t.trade.ts))
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
        ts,
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
fn seed_history(rest: &CpapiRest, sink: &dyn LiveDataSink, conid: i64, stream: &mut Stream) {
    let (interval, interval_ms) = match &stream.lane {
        Lane::Bars { interval, interval_ms, .. } => (interval.clone(), *interval_ms),
        Lane::Quotes | Lane::Trades => return,
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

/// THE ONE site the cpapi feed sends a stream's delay label: `LiveDataSink::data_delay`, when the
/// label is first known and again whenever it CHANGES — never per message (decision 0140: "sent at
/// subscribe and again whenever the label of a stream changes"; the socket feed's twin is
/// `crates/bridges/vike-ibkr/src/market_feed/map.rs`'s `forward_quote_tick`). `None` (no marker
/// this decoder reads) sends nothing and keeps the last label sent.
fn note_delay(sink: &dyn LiveDataSink, stream: &mut Stream, delay: Option<DataDelay>) {
    let Some(delay) = delay else { return };
    if stream.delay == Some(delay) {
        return;
    }
    stream.delay = Some(delay);
    sink.data_delay(VENUE, &stream.symbol, stream.lane.name(), delay);
}

/// What the pump thread owns.
pub(crate) struct PumpCtx {
    pub(crate) ws_url: String,
    pub(crate) rest: CpapiRest,
    pub(crate) book: Book,
    pub(crate) cmds: Receiver<Cmd>,
    pub(crate) stop: Arc<AtomicBool>,
}

/// The pump thread's body: dial, subscribe and prime every held conId, read and fold until the
/// socket drops or `stop` is raised, and redial. Commands are applied between reads and while
/// waiting to redial, so a `Remove` is always answered.
pub(crate) fn run(mut ctx: PumpCtx) {
    let mut last_tickle = Instant::now();
    while !ctx.stop.load(Ordering::Relaxed) {
        let (socket, _resp) = match connect_ws(&ctx.ws_url) {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!(error = %e, "ibkr cpapi market-data ws connect failed; retrying");
                ctx.book.open_gaps(vike_model::now_ms());
                if wait_applying_cmds(&mut ctx) {
                    return;
                }
                continue;
            }
        };
        configure_ws_stream(&socket, READ_TIMEOUT);
        let mut ws = TungsteniteStream(socket);
        for conid in ctx.book.conid_list() {
            let _ = ws.send_text(&smd_subscribe(conid));
            ctx.book.prime(conid, &ctx.rest);
        }
        loop {
            if ctx.stop.load(Ordering::Relaxed) || apply_cmds(&mut ctx, Some(&mut ws)) {
                return;
            }
            if last_tickle.elapsed() >= TICKLE_EVERY {
                let _ = ctx.rest.tickle();
                last_tickle = Instant::now();
            }
            match ws.recv() {
                Ok(StreamMsg::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        ctx.book.on_frame(&v);
                    }
                }
                Ok(StreamMsg::Ping(p)) => {
                    let _ = ws.pong(p);
                }
                Ok(_) | Err(StreamError::Timeout) => {}
                Err(StreamError::Closed(why)) => {
                    tracing::warn!(reason = %why, "ibkr cpapi market-data ws closed; redialling");
                    ctx.book.open_gaps(vike_model::now_ms());
                    break;
                }
            }
        }
        if wait_applying_cmds(&mut ctx) {
            return;
        }
    }
}

/// Wait [`REDIAL`] in read-timeout slices with no socket, applying commands. `true` = stop
/// (raised, or the client is gone).
fn wait_applying_cmds(ctx: &mut PumpCtx) -> bool {
    let deadline = Instant::now() + REDIAL;
    while Instant::now() < deadline {
        if ctx.stop.load(Ordering::Relaxed) || apply_cmds(ctx, None) {
            return true;
        }
        thread::sleep(READ_TIMEOUT);
    }
    false
}

/// Drain the command channel. With a socket, a new conId is subscribed on it and primed at once;
/// without one (redialling) the next dial's per-conId loop does both. `true` when the client is
/// gone (the channel closed).
fn apply_cmds(ctx: &mut PumpCtx, mut ws: Option<&mut TungsteniteStream>) -> bool {
    loop {
        match ctx.cmds.try_recv() {
            Ok(Cmd::Add { id, conid, symbol, lane }) => {
                let fresh = ctx.book.add(id, conid, symbol, lane);
                if let Some(ws) = ws.as_deref_mut() {
                    if fresh {
                        let _ = ws.send_text(&smd_subscribe(conid));
                    }
                    ctx.book.prime(conid, &ctx.rest);
                }
            }
            Ok(Cmd::Remove { id, done }) => {
                if let (Some(conid), Some(ws)) = (ctx.book.remove(id), ws.as_deref_mut()) {
                    let _ = ws.send_text(&smd_unsubscribe(conid));
                }
                let _ = done.send(());
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod pump_tests;
