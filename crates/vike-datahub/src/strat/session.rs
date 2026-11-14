//! One strategy-lane SESSION: the tube's sending half behind a `StrategyOpen`, in the shape of the
//! gatewayhub's stream session (`crates/vike-gatewayhub/src/server/stream.rs`) - one bounded queue, one
//! writer thread, and the reader loop below on the connection's own thread.
//!
//! `StrategyOpen` is the MODE SWITCH. After the `StrategyOpened` reply (the last positional frame this socket
//! carries) every client frame is a `StreamUp` and every server frame a `StreamDown`; a refusal BEFORE
//! the reply is a positional `Response::Error` and the connection stays positional
//! (`crate::server`'s `strategy_open_verb`).
//!
//! # Two threads, one socket
//!
//! The connection thread becomes the session's READER: it reads `StreamUp` frames, subscribes through
//! the session's [`DataClientBrokerHandle`] (and, for the pump, its own client) and is the only thread that calls
//! either. A named WRITER thread (`vike_market_stream::writer`) drains the queue to the socket. They
//! share nothing but the `DataClientSinkStream` and the socket, so a venue call that blocks the reader (a connect
//! inside a `subscribe_*`) never stops frames flowing, and a tradehub that stops reading stops only the
//! writer, never a venue pump and never the market-data plane.
//!
//! # A frame reaches a session by its key set
//!
//! Before the broker is asked to subscribe, the session publishes what it will hold for the symbol to
//! `DataClientSinkStrategyLane` (`session.rs`'s `publish`), so a frame a venue client emits from INSIDE `subscribe_*`
//! (a seed) is already routed. A failed subscribe takes the claim back; the last unsubscribe of a symbol
//! withdraws it and gives the key back to the lane's caps.
//!
//! # Teardown order
//!
//! End the queue (the writer writes its closing `Bye` at once), join the writer (so the `Bye` is out
//! before anything slow), then `begin_shutdown` and drop the broker handle and the pump client, which
//! may take a socket read timeout. A client that sent `Close` is therefore answered `Bye(ClientClosed)`
//! in milliseconds, whatever the venue takes to stop. `Session`'s `Drop` withdraws every route and every
//! key even when the reader unwinds.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::delivery::DataClientSinkDelivery;
use vike_data::{DataClient, DataClientSink, DataClientSubscriptionId};
use vike_datahub_client::proto::{
    Response, STRATEGY_LANE_MAX_SYMBOL_BYTES, STRATEGY_LANE_UP_FRAME_CEILING_BYTES,
    read_frame_raw_capped, write_frame,
};
use vike_gatewayhub_client::history::FEED_TICK_VENUES;
use vike_market_stream::frame::{StreamFrameReady, wait_readable};
use vike_market_stream::market::DataClientStream;
use vike_market_stream::queue::StreamQueueLimits;
use vike_market_stream::sink::{
    DataClientSinkStream, StreamSinkLimits, StreamSinkOpen, StreamSinkOpenKind,
};
use vike_market_stream::wire::{
    StreamByeReason, StreamSubscriptionErrorKind, StreamUp, SubscriptionOperation,
    validate_tick_size,
};
use vike_market_stream::writer::{StreamWriterConfig, run_writer};
use vike_model::StreamLabel;

use super::history::{PRELOAD_KEY_BUDGET, PRELOAD_SESSION_BUDGET};
use super::{StrategyLane, StrategyLaneGuard};
use crate::feeds::routing::DataClientBrokerStrategyKeys;
use crate::feeds::{DataClientBrokerHandle, DataClientBrokerHolder, DataClientBrokerLane};

/// How often the reader wakes to look at the session's idle clock when no frame arrives.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// The writer thread's name: fixed, ASCII, inside the 15 bytes Linux keeps.
const WRITER_THREAD: &str = "dh-strat-wr";

/// Which client holds a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// The session's [`DataClientBrokerHandle`] on the broker.
    Broker,
    /// The session's own tick-pump client.
    Pump,
}

struct Sub {
    id: DataClientSubscriptionId,
    via: Via,
    symbol: String,
    k: u32,
    /// The tick lane the subscription is on, `None` for a bar lane and for the pump: where a class
    /// change applies.
    lane: Option<StreamLabel>,
    /// The broker lane, for a broker subscription.
    broker_lane: Option<DataClientBrokerLane>,
}

/// What the session holds for one symbol.
#[derive(Default)]
struct SymbolState {
    subs: usize,
    /// The view `DataClientSinkStrategyLane` routes by.
    keys: DataClientBrokerStrategyKeys,
    /// How many subscriptions hold each broker lane (a lane's bit goes when the last leaves).
    lane_holds: HashMap<DataClientBrokerLane, usize>,
}

struct Session {
    hub: Arc<StrategyLane>,
    id: u64,
    venue: String,
    /// The name the broker and the lane's sink know this session's client by: the venue, and its
    /// network when it is not the main one (`StrategyLaneGuard::label`).
    route: String,
    sink: DataClientSinkStream,
    /// The wire sink as the trait object a route holds.
    wire: Arc<dyn DataClientSink>,
    handle: DataClientBrokerHandle,
    ticks: Option<DataClientStream>,
    subs: HashMap<u64, Sub>,
    symbols: HashMap<String, SymbolState>,
    /// How much of `PRELOAD_SESSION_BUDGET` this session's bar subscriptions have spent on the start
    /// history. Zero for ever unless `flags.start_history_from_store` is on.
    preload_spent: Duration,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Withdraw every claim and give every key back, whatever ended the reader.
        self.hub.sink.forget(self.id);
        for symbol in self.symbols.keys() {
            self.hub.release_key(&self.route, symbol);
        }
    }
}

/// `StrategyOpen`, after the door let it through: write the reply, run the session to its end.
pub(crate) fn serve(
    mut stream: TcpStream,
    hub: &Arc<StrategyLane>,
    guard: StrategyLaneGuard,
    peer: Option<SocketAddr>,
) {
    let limits = hub.limits;
    let id = guard.id();
    let venue = guard.venue().to_string();
    let route = guard.label();
    let sink = DataClientSinkStream::new(
        &venue,
        StreamSinkLimits {
            queue: StreamQueueLimits {
                bytes: limits.queue_bytes,
                frames: limits.queue_frames,
                control_frames: limits.control_frames,
            },
            max_keys: limits.max_specs_per_session,
            max_unsolicited: limits.max_unsolicited,
        },
    );
    // The session's queue ends when this reader leaves by ANY path (`DataClientSinkStream::end_on_drop`), or a
    // writer would heartbeat on for a session nobody serves: a zombie the tradehub would believe in.
    let _end = sink.end_on_drop();
    let reply = Response::StrategyOpened {
        session: id,
        heartbeat_ms: u64::try_from(limits.heartbeat.as_millis()).unwrap_or(u64::MAX),
    };
    // THE LAST POSITIONAL FRAME.
    if write_frame(&mut stream, &reply).is_err() {
        return;
    }
    // The bound on a write to a tradehub that stopped reading. The option belongs to the socket, so the
    // writer's clone carries it.
    if stream.set_write_timeout(Some(limits.write_timeout)).is_err() {
        return;
    }
    let writer_socket = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(session = id, error = %e, "vike-datahub strat: could not open the session");
            return;
        }
    };
    tracing::info!(session = id, venue = %venue, ?peer, "vike-datahub strat: session opened");
    let config = StreamWriterConfig {
        heartbeat: limits.heartbeat,
        lapse_budget: limits.lapse_budget,
        lapse_window: limits.lapse_window,
        lapse_episode: limits.lapse_episode,
        down_frame_ceiling_bytes: vike_datahub_client::proto::STRATEGY_LANE_FRAME_CEILING_BYTES,
    };
    let writer_sink = sink.clone();
    let writer = std::thread::Builder::new()
        .name(WRITER_THREAD.to_string())
        .spawn(move || run_writer(writer_socket, &writer_sink, config, id));
    let writer = match writer {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!(session = id, error = %e, "vike-datahub strat: could not start the writer");
            return;
        }
    };

    let mut session = Session {
        hub: Arc::clone(hub),
        id,
        venue: venue.clone(),
        route: route.clone(),
        sink: sink.clone(),
        wire: Arc::new(sink.clone()),
        handle: hub.broker.handle(DataClientBrokerHolder::Strategy, &route),
        ticks: None,
        subs: HashMap::new(),
        symbols: HashMap::new(),
        preload_spent: Duration::ZERO,
    };
    session.read_loop(&mut stream);
    let held = session.subs.len();

    // Teardown, in the order the module doc gives.
    sink.finish(None);
    let _ = writer.join();
    if hub.sole_on_route(&route) {
        session.handle.begin_shutdown();
    }
    session.handle.shutdown();
    if let Some(pump) = session.ticks.as_mut() {
        pump.begin_shutdown();
        pump.shutdown();
    }
    let summary = sink.summary();
    drop(session);
    tracing::info!(
        session = id,
        venue = %venue,
        subscriptions_at_end = held,
        superseded = summary.queue.superseded,
        lossless_evicted = summary.queue.lossless_evicted,
        conflatable_evicted = summary.queue.conflatable_evicted,
        nonfinite = summary.queue.nonfinite,
        unsendable = summary.queue.unsendable,
        unbound_dropped = summary.unbound_dropped,
        "vike-datahub strat: session closed"
    );
    drop(guard);
}

/// The broker lane a non-pump op subscribes, `None` for the pump.
fn broker_lane_of(op: &SubscriptionOperation) -> Option<DataClientBrokerLane> {
    match op {
        SubscriptionOperation::Bars { interval, .. } => {
            Some(DataClientBrokerLane::Bars(interval.clone()))
        }
        SubscriptionOperation::Quotes { .. } => {
            Some(DataClientBrokerLane::Tick(StreamLabel::Quotes))
        }
        SubscriptionOperation::Trades { .. } => {
            Some(DataClientBrokerLane::Tick(StreamLabel::Trades))
        }
        SubscriptionOperation::Book { .. } => Some(DataClientBrokerLane::Tick(StreamLabel::Book)),
        SubscriptionOperation::Depth { .. } => Some(DataClientBrokerLane::Tick(StreamLabel::Depth)),
        SubscriptionOperation::Ticks { .. } => None,
    }
}

impl Session {
    /// The reader loop: runs until the session ends.
    fn read_loop(&mut self, stream: &mut TcpStream) {
        let limits = self.hub.limits;
        let tick = IDLE_TICK.min(limits.session_idle);
        let mut idle_since = Some(Instant::now());
        loop {
            if self.sink.is_ended() {
                break;
            }
            match wait_readable(stream, tick) {
                Ok(StreamFrameReady::Data) => {}
                Ok(StreamFrameReady::Timeout) => {
                    if self.subs.is_empty()
                        && idle_since.is_some_and(|t| t.elapsed() >= limits.session_idle)
                    {
                        self.sink.finish(Some(StreamByeReason::SessionIdle));
                        break;
                    }
                    continue;
                }
                Ok(StreamFrameReady::Eof) | Err(_) => break,
            }
            if let Err(e) = stream
                .set_read_timeout(Some(limits.frame_read_timeout.max(Duration::from_millis(1))))
            {
                tracing::info!(session = self.id, error = %e, "vike-datahub strat: read fault; closing");
                break;
            }
            let body = match read_frame_raw_capped(stream, STRATEGY_LANE_UP_FRAME_CEILING_BYTES) {
                Ok(body) => body,
                Err(e) => {
                    tracing::info!(session = self.id, error = %e, "vike-datahub strat: read fault; closing");
                    break;
                }
            };
            let frame = match serde_json::from_slice::<StreamUp>(&body) {
                Ok(frame) => frame,
                Err(e) => {
                    // The lane's grammar is closed (`StreamUp`), so an undecodable frame is a desync,
                    // not a bad request: there is no positional reply to answer it with.
                    tracing::warn!(
                        session = self.id,
                        error = %e,
                        "vike-datahub strat: undecodable client frame; closing"
                    );
                    break;
                }
            };
            match frame {
                StreamUp::Subscribe { sid, op, class } => self.handle_sub(sid, op, class),
                StreamUp::Delivery { sid, class } => self.handle_delivery(sid, class),
                StreamUp::Unsubscribe { sid } => self.handle_unsubscribe(sid),
                StreamUp::BeginShutdown => {
                    if self.hub.sole_on_route(&self.route) {
                        self.handle.begin_shutdown();
                    }
                    if let Some(pump) = self.ticks.as_mut() {
                        pump.begin_shutdown();
                    }
                }
                StreamUp::Ping => self.sink.request_heartbeat(),
                // The tradehub decodes funding frames (it saw `stream_funding`): carry them from now on.
                StreamUp::Funding => self.sink.carry_funding(),
                StreamUp::Close => {
                    self.sink.finish(Some(StreamByeReason::ClientClosed));
                    break;
                }
            }
            idle_since = if self.subs.is_empty() {
                idle_since.or_else(|| Some(Instant::now()))
            } else {
                None
            };
        }
    }

    /// Publish what this session will hold of `symbol` to the lane's sink, so a frame emitted from
    /// inside the broker's `subscribe_*` is already routed here.
    fn publish(&self, symbol: &str, keys: &DataClientBrokerStrategyKeys) {
        self.hub.sink.set(&self.route, symbol, self.id, &self.wire, keys.clone());
    }

    /// The session's pump client, built on first use.
    fn pump(&mut self) -> Result<&mut DataClientStream, StreamSubscriptionErrorKind> {
        if self.ticks.is_none() {
            let Some(builder) = self.hub.ticks.as_ref() else {
                return Err(StreamSubscriptionErrorKind::Unsupported(
                    "the strategy lane serves no tick pump in this build".to_string(),
                ));
            };
            match builder(&self.venue, Arc::clone(&self.wire)) {
                Ok(client) => self.ticks = Some(client),
                Err(text) => return Err(StreamSubscriptionErrorKind::Subscribe(text)),
            }
        }
        self.ticks.as_mut().ok_or_else(|| {
            StreamSubscriptionErrorKind::Subscribe("the pump client vanished".to_string())
        })
    }

    /// One `Sub`: validate, count, bind a key, apply the class it asks for, route, ask the venue's
    /// client, answer.
    fn handle_sub(
        &mut self,
        sid: u64,
        op: SubscriptionOperation,
        class: Option<DataClientSinkDelivery>,
    ) {
        // The client allocates `sid`, so a repeat is the same subscription: answer it again.
        if let Some(existing) = self.subs.get(&sid) {
            self.sink.re_ack(sid, existing.k);
            return;
        }
        if let Some(why) = op.refuse_spelling(STRATEGY_LANE_MAX_SYMBOL_BYTES) {
            self.sink.refuse(
                sid,
                StreamSubscriptionErrorKind::SymbolRejected(format!("strategy lane: {why}")),
            );
            return;
        }
        if let SubscriptionOperation::Ticks { tick_size, .. } = &op {
            if let Err(why) = validate_tick_size(*tick_size) {
                self.sink.refuse(
                    sid,
                    StreamSubscriptionErrorKind::SymbolRejected(format!("strategy lane: {why}")),
                );
                return;
            }
            if !self.hub.serves_ticks() {
                self.sink.refuse(
                    sid,
                    StreamSubscriptionErrorKind::Unsupported(
                        "the strategy lane serves no tick pump in this build".to_string(),
                    ),
                );
                return;
            }
            if !FEED_TICK_VENUES.contains(&self.venue.as_str()) {
                self.sink.refuse(
                    sid,
                    StreamSubscriptionErrorKind::Unsupported(format!(
                        "venue `{}` has no tick pump (the pump is the CEX feed of [{}])",
                        self.venue,
                        FEED_TICK_VENUES.join(", ")
                    )),
                );
                return;
            }
        }
        let cap = self.hub.limits.max_specs_per_session;
        if self.subs.len() >= cap {
            self.sink.refuse(sid, super::budget("subscriptions per session", self.subs.len(), cap));
            return;
        }
        let symbol = op.symbol().to_string();
        let first_on_symbol = !self.symbols.contains_key(&symbol);
        if first_on_symbol && let Err(refusal) = self.hub.reserve_key(&self.route, &symbol) {
            self.sink.refuse(sid, refusal);
            return;
        }
        let opened: StreamSinkOpen = match self.sink.open_sub(&symbol) {
            Ok(opened) => opened,
            Err(refusal) => {
                if first_on_symbol {
                    self.hub.release_key(&self.route, &symbol);
                }
                self.sink.refuse(sid, refusal);
                return;
            }
        };

        // The class goes in BEFORE the venue is asked, so a frame the venue emits from inside the
        // subscribe is already queued under it: on this hop's queue, and (a depth lane wanting every
        // snapshot) on the broker's, which is the hop the conflation would otherwise happen on.
        let lane = op.lane();
        let floor = class.filter(|c| *c > DataClientSinkDelivery::Conflatable);
        let raised = match (class, lane) {
            (Some(class), Some(lane)) => {
                Some((lane, self.sink.set_delivery(opened.k, lane, class)))
            }
            _ => None,
        };
        let hop_one_floor = match (floor, lane) {
            (Some(floor), Some(lane)) if !matches!(op, SubscriptionOperation::Ticks { .. }) => {
                let _ = self.handle.set_delivery(&symbol, lane, floor);
                Some(lane)
            }
            _ => None,
        };

        // The START HISTORY of a bar key, ABOVE the publish below and before the venue is asked: with
        // `flags.start_history_from_store` on, the store's last bars (and the span the store lacks, fetched) go
        // to this session as a bar seed now. The order is the point: a seed REPLACES the core's series, so it
        // must be the first thing this session's key sees; published first, a session that JOINS a live key
        // would be handed the venue's closes before its own seed and the seed would swallow them. A refused
        // subscription purges the seed with the key's other held frames. With the flag off `preload` does
        // nothing at all.
        if let SubscriptionOperation::Bars { symbol, interval } = &op
            && let Some(history) = self.hub.history()
        {
            let left = PRELOAD_SESSION_BUDGET.saturating_sub(self.preload_spent);
            if !left.is_zero() {
                let spent = history.preload(
                    self.id,
                    &self.route,
                    symbol,
                    interval,
                    left.min(PRELOAD_KEY_BUDGET),
                    self.wire.as_ref(),
                );
                self.preload_spent += spent;
            }
        }

        // Route BEFORE the broker is asked (the module doc).
        let broker_lane = broker_lane_of(&op);
        let before = self.symbols.get(&symbol).map(|s| s.keys.clone()).unwrap_or_default();
        let mut after = before.clone();
        if let Some(bl) = &broker_lane {
            after.set(bl, true);
            self.publish(&symbol, &after);
        }

        let started = match &op {
            SubscriptionOperation::Bars { symbol, interval } => {
                self.handle.subscribe_bars(symbol, interval)
            }
            SubscriptionOperation::Quotes { symbol } => self.handle.subscribe_quotes(symbol),
            SubscriptionOperation::Trades { symbol } => self.handle.subscribe_trades(symbol),
            SubscriptionOperation::Book { symbol } => self.handle.subscribe_book(symbol),
            SubscriptionOperation::Depth { symbol } => self.handle.subscribe_depth(symbol),
            SubscriptionOperation::Ticks { symbol, tick_size } => match self.pump() {
                Ok(pump) => pump.subscribe_ticks(symbol, *tick_size),
                Err(refusal) => {
                    self.undo_sub(symbol, &before, first_on_symbol, raised, hop_one_floor, &opened);
                    self.sink.sub_failed(sid, symbol, opened, refusal);
                    return;
                }
            },
        };
        match started {
            Ok(id) => {
                let state = self.symbols.entry(symbol.clone()).or_default();
                state.subs += 1;
                state.keys = after;
                if let Some(bl) = &broker_lane {
                    *state.lane_holds.entry(bl.clone()).or_default() += 1;
                }
                let via = if broker_lane.is_some() { Via::Broker } else { Via::Pump };
                // A session that joins a broker lane is told where the lane stands (the last status its
                // venue client disclosed, or `Live`): the client discloses transitions only, and a window
                // the session's tradehub opened before a datahub restart would otherwise never close.
                let joined_word = broker_lane
                    .as_ref()
                    .map(|bl| match bl {
                        DataClientBrokerLane::Tick(label) => label.as_str().to_string(),
                        DataClientBrokerLane::Bars(interval) => interval.clone(),
                    })
                    .filter(|word| !before.admits_stream_word(word));
                self.subs.insert(
                    sid,
                    Sub { id, via, symbol: symbol.clone(), k: opened.k, lane, broker_lane },
                );
                self.sink.sub_ok(sid, &symbol, opened);
                if let Some(word) = joined_word {
                    self.hub.sink.restate(&self.route, &symbol, &word, self.wire.as_ref());
                }
            }
            Err(e) => {
                let adopted = opened.kind == StreamSinkOpenKind::Adopted;
                tracing::debug!(
                    session = self.id,
                    error = %e,
                    adopted,
                    "vike-datahub strat: the venue client refused a subscription"
                );
                self.undo_sub(&symbol, &before, first_on_symbol, raised, hop_one_floor, &opened);
                self.sink.sub_failed(
                    sid,
                    &symbol,
                    opened,
                    StreamSubscriptionErrorKind::from_live_error(&e),
                );
                // The slot the preload claimed for this bar key goes back to the saver (a no-op for a key
                // that has since been seen live).
                if let SubscriptionOperation::Bars { symbol, interval } = &op
                    && let Some(history) = self.hub.history()
                {
                    history.release_unproven(&self.route, symbol, interval);
                }
            }
        }
    }

    /// Take back everything a refused `Sub` put in place before the venue was asked.
    fn undo_sub(
        &mut self,
        symbol: &str,
        before: &DataClientBrokerStrategyKeys,
        first_on_symbol: bool,
        raised: Option<(StreamLabel, DataClientSinkDelivery)>,
        hop_one_floor: Option<StreamLabel>,
        opened: &StreamSinkOpen,
    ) {
        self.publish(symbol, before);
        // A key this call created is purged with its floor; one it joined gets the lane's old floor back.
        if let Some((lane, previous)) = raised
            && opened.kind != StreamSinkOpenKind::Fresh
        {
            self.sink.set_delivery(opened.k, lane, previous);
        }
        if let Some(lane) = hop_one_floor {
            let _ = self.handle.set_delivery(symbol, lane, DataClientSinkDelivery::Conflatable);
        }
        if first_on_symbol {
            self.hub.release_key(&self.route, symbol);
        }
    }

    /// `StreamUp::Delivery`: the class is queue policy on this hop, and on the broker's for a broker
    /// subscription. An unknown `sid`, a bar lane and the pump are no-ops (no reply, like `Unsubscribe`).
    fn handle_delivery(&mut self, sid: u64, class: DataClientSinkDelivery) {
        let Some(sub) = self.subs.get(&sid) else { return };
        let Some(lane) = sub.lane else { return };
        self.sink.set_delivery(sub.k, lane, class);
        if sub.via == Via::Broker
            && let Err(e) = self.handle.set_delivery(&sub.symbol, lane, class)
        {
            tracing::warn!(
                session = self.id,
                symbol = %sub.symbol,
                error = %e,
                "vike-datahub strat: the venue client could not change a lane's delivery class; the \
                 key keeps the class it runs at"
            );
        }
    }

    fn handle_unsubscribe(&mut self, sid: u64) {
        let Some(sub) = self.subs.remove(&sid) else { return };
        match sub.via {
            Via::Broker => self.handle.unsubscribe(sub.id),
            Via::Pump => {
                if let Some(pump) = self.ticks.as_mut() {
                    pump.unsubscribe(sub.id);
                }
            }
        }
        // The lane's floor was this subscription's: with the symbol's other subscriptions staying,
        // nothing on the lane holds it any more. (With none left, `sub_ended` releases the key and its
        // floors with it.)
        if let Some(lane) = sub.lane
            && self.subs.values().any(|s| s.symbol == sub.symbol)
            && !self.subs.values().any(|s| s.symbol == sub.symbol && s.lane == Some(lane))
        {
            self.sink.set_delivery(sub.k, lane, DataClientSinkDelivery::Conflatable);
            if sub.via == Via::Broker {
                let _ = self.handle.set_delivery(
                    &sub.symbol,
                    lane,
                    DataClientSinkDelivery::Conflatable,
                );
            }
        }
        let mut gone = false;
        if let Some(state) = self.symbols.get_mut(&sub.symbol) {
            state.subs = state.subs.saturating_sub(1);
            if let Some(bl) = &sub.broker_lane {
                let holds = state.lane_holds.entry(bl.clone()).or_default();
                *holds = holds.saturating_sub(1);
                if *holds == 0 {
                    state.lane_holds.remove(bl);
                    state.keys.set(bl, false);
                }
            }
            gone = state.subs == 0;
        }
        if gone {
            self.symbols.remove(&sub.symbol);
            self.hub.sink.set(
                &self.route,
                &sub.symbol,
                self.id,
                &self.wire,
                DataClientBrokerStrategyKeys::default(),
            );
            self.hub.release_key(&self.route, &sub.symbol);
        } else if let Some(state) = self.symbols.get(&sub.symbol) {
            let keys = state.keys.clone();
            self.publish(&sub.symbol, &keys);
        }
        self.sink.sub_ended(&sub.symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spelling_is_refused_blank_long_or_with_a_control_character() {
        let quotes = |s: &str| SubscriptionOperation::Quotes { symbol: s.to_string() };
        let refuse_spelling =
            |op: &SubscriptionOperation| op.refuse_spelling(STRATEGY_LANE_MAX_SYMBOL_BYTES);
        assert!(refuse_spelling(&quotes("BTCUSDT")).is_none());
        assert!(refuse_spelling(&quotes("  ")).expect("blank").contains("blank"));
        assert!(refuse_spelling(&quotes("A\nB")).expect("control").contains("control"));
        let long = "x".repeat(STRATEGY_LANE_MAX_SYMBOL_BYTES + 1);
        assert!(refuse_spelling(&quotes(&long)).expect("long").contains("limit"));
        let bars = SubscriptionOperation::Bars {
            symbol: "BTCUSDT".to_string(),
            interval: " ".to_string(),
        };
        assert!(refuse_spelling(&bars).expect("interval").contains("interval"));
    }

    #[test]
    fn only_the_pump_has_no_broker_lane() {
        assert!(
            broker_lane_of(&SubscriptionOperation::Ticks {
                symbol: "X".to_string(),
                tick_size: 0.5
            })
            .is_none()
        );
        for op in [
            SubscriptionOperation::Bars { symbol: "X".to_string(), interval: "1m".to_string() },
            SubscriptionOperation::Quotes { symbol: "X".to_string() },
            SubscriptionOperation::Trades { symbol: "X".to_string() },
            SubscriptionOperation::Book { symbol: "X".to_string() },
            SubscriptionOperation::Depth { symbol: "X".to_string() },
        ] {
            assert!(broker_lane_of(&op).is_some(), "{op:?}");
        }
    }
}
