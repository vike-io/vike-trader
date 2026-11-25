//! The `stream` class: one session is one venue client, one bounded queue, one writer thread and the
//! reader loop below.
//!
//! `DataOpen` is the MODE SWITCH. After the `DataOpened` reply (the last positional frame this socket
//! carries) every client frame is a `StreamUp` and every server frame a `StreamDown`; a refusal BEFORE
//! the reply (an unserved venue, a full plane, the lane's own error) is a positional `Response::Error`
//! and the connection stays positional; the daemon's CAP of sessions is the refusal frame
//! (`vike_gatewayhub_client::refusals::GatewayhubRefusal::StreamSessionsFull`), so a reader decides on its id. A
//! subscription's refusal keeps its CLASS (`StreamSubscriptionErrorKind::NotNow`/`Cap`, through
//! `from_live_error`) toward a client whose connection said it reads the refusal frame.
//!
//! # Two threads, one socket
//!
//! The connection thread becomes the session's READER: it reads `StreamUp` frames, drives the venue
//! client (`subscribe_*`, `unsubscribe`, `begin_shutdown`, `shutdown`) and is the only thread of this
//! file that calls it (a guarded credentialed venue's reaper and wind-down threads call it too, under
//! `crate::data::session_guard`'s client lock). A named WRITER thread (`vike_market_stream::writer`) drains the session's queue to the
//! socket. They share nothing but the `DataClientSinkStream` and the socket, so a venue call that blocks the
//! reader (a connect inside `subscribe_*`) never stops frames flowing, and a consumer that stops
//! reading stops only the writer, never a venue pump.
//!
//! # The class of a subscription
//!
//! A `Sub` may carry a delivery class floor and a `StreamUp::Delivery` changes it on a live
//! subscription (both gated by the `stream_class` feature this daemon advertises). The floor is queue
//! policy and nothing more: `handle_sub` applies it to the session's queue BEFORE the venue client is
//! asked, so a frame emitted from inside `subscribe_*` is already under it, and gives a joined key's
//! floor back when the venue refuses; the venue client is never told. The rules are
//! `vike_market_stream::queue`'s module doc.
//!
//! # The strategy lane's additions (wave 2, W10)
//!
//! * **A tick pump is a subscription, not a client verb.** `SubscriptionOperation::Ticks` asks the data lane to host the
//!   CEX pump of the symbol on the caller's price grid (`DataLane::open_feed_tick`, shared across sessions by
//!   the lane); the session holds a lease per pump and drops them AFTER the writer is joined, and an
//!   `Unsubscribe` drops one at once. The grid, the venue and a repeat of the symbol are refused before a key is
//!   bound (`refuse_pump`), all permanent.
//! * **A credentialed venue (`cs_venue=`) is served to `Scope::Write` only**, through
//!   `DataLane::open_credentialed`; the daemon reads the account, nothing of it crosses.
//! * **A `network` is never ignored**: it is refused for a venue that takes none, and the lane refuses a
//!   word it does not know.
//!
//! # Liveness
//!
//! The stream is NOT bounded by the idle read timeout: its client is silent by design. The daemon proves
//! liveness the other way, with a `Heartbeat` after the heartbeat period it STATED in `DataOpened`
//! (the client's deadline is three of them), and notices a vanished client by EOF, a write fault or the
//! write timeout. A session that holds no subscription for `session_idle` is ended with
//! `StreamByeReason::SessionIdle`: a venue client kept for nobody.
//!
//! # Draining
//!
//! `BeginShutdown` forwards `DataClient::begin_shutdown` and nothing more: the in-process contract
//! lets a client subscribe again after it, and the broker's drain may end and do so. `Draining` is
//! answered only once the session is ENDING (`Close`, the daemon's stop).
//!
//! # Teardown order
//!
//! The session ends in three steps whose order matters: end the queue (the writer writes its closing
//! `Bye` at once), join the writer (so the `Bye` is out before anything slow), then `begin_shutdown` and
//! `shutdown` the venue client, which may take a socket read timeout. A client that sent `Close` is
//! therefore answered `Bye(ClientClosed)` in milliseconds, whatever the venue takes to stop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::delivery::DataClientSinkDelivery;
use vike_data::{DataClient, DataClientError, DataClientSink, DataClientSubscriptionId};
use vike_gatewayhub_client::proto::{GATEWAY_DOWN_FRAME_CEILING_BYTES, Response};
use vike_gatewayhub_client::refusals::GatewayhubRefusal;
use vike_hub_protocol::auth::{VerbScope, scope_admits};
use vike_market_stream::frame::{StreamFrameReady, wait_readable};
use vike_market_stream::queue::StreamQueueLimits;
use vike_market_stream::sink::{
    DataClientSinkStream, StreamSinkLimits, StreamSinkOpen, StreamSinkOpenKind,
};
use vike_market_stream::wire::{
    StreamByeReason, StreamSubscriptionErrorKind, StreamUp, SubscriptionOperation,
    validate_tick_size,
};
use vike_market_stream::writer::{StreamWriterConfig, run_writer};

use super::frame::read_client_body;
use super::shared::{Connection, ServerAfter, ServerShared, refuse};
use crate::data::{DataLane, DataLaneHolder, FeedTickSink, FeedTickSubscription};
use vike_model::StreamLabel;

/// How often the reader wakes to look at the session's idle clock when no frame arrives.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// **The open CREDENTIALED stream sessions** (`cs_venue=`), by session id, so a change of the credentials one logged in
/// with ends it (decision 0287): its sink is finished, the reader leaves at its next tick, and its client (the
/// datahub's broker) redials, so the new open reads the store afresh and logs in on the new credentials. A session
/// leaves the table when it ends; nothing else is kept and nothing polls it.
#[derive(Default)]
pub struct CredentialedStreamSessions(Mutex<HashMap<u64, (String, DataClientSinkStream)>>);

impl CredentialedStreamSessions {
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<u64, (String, DataClientSinkStream)>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Hold session `session` of `venue` until the returned entry is dropped (the session's end, by any path).
    fn enter(
        &self,
        session: u64,
        venue: &str,
        sink: &DataClientSinkStream,
    ) -> CredentialedStreamEntry<'_> {
        self.table().insert(session, (venue.to_string(), sink.clone()));
        CredentialedStreamEntry { sessions: self, session }
    }

    /// **End every open session whose venue's stream logs in as `account` row NOW** (`lane`'s fresh answer, asked once
    /// per venue), and name each one ended (`<venue> price stream`). No other session is touched.
    pub fn end_logged_in_as(&self, lane: &dyn DataLane, account: i64) -> Vec<String> {
        let open: Vec<(u64, String, DataClientSinkStream)> = self
            .table()
            .iter()
            .map(|(id, (venue, sink))| (*id, venue.clone(), sink.clone()))
            .collect();
        let mut asked: HashMap<String, Option<i64>> = HashMap::new();
        let mut ended = Vec::new();
        for (session, venue, sink) in open {
            let logs_in_as = *asked
                .entry(venue.clone())
                .or_insert_with(|| lane.credentialed_feed_account(&venue));
            if logs_in_as != Some(account) {
                continue;
            }
            // shortcut: the wire has no Bye word for "reopen on new credentials"; `ServerStopping` is what the reconnect
            // is to the client (a lost session it redials). A word of its own is a stream-protocol change.
            sink.finish(Some(StreamByeReason::ServerStopping));
            tracing::warn!(
                session,
                venue,
                "vike-gatewayhub stream: the account's credentials changed: this credentialed session is ended, and its \
                 client reopens it on the new ones"
            );
            ended.push(format!("{venue} price stream"));
        }
        ended
    }
}

/// One session's place in [`CredentialedStreamSessions`], given back when the session ends.
struct CredentialedStreamEntry<'a> {
    sessions: &'a CredentialedStreamSessions,
    session: u64,
}

impl Drop for CredentialedStreamEntry<'_> {
    fn drop(&mut self) {
        self.sessions.table().remove(&self.session);
    }
}

struct Sub {
    /// The venue client's id for the subscription; `None` for a tick PUMP, which is not a client verb.
    id: Option<DataClientSubscriptionId>,
    symbol: String,
    k: u32,
    /// The tick lane the subscription is on, `None` for a bar lane: where a class change applies.
    lane: Option<StreamLabel>,
    /// The share of a hosted tick pump this subscription holds (`SubscriptionOperation::Ticks`); dropping it leaves.
    lease: Option<FeedTickSubscription>,
}

/// What `SubscriptionOperation::Ticks` needs beyond the venue client: the lane that hosts the pump, the session's
/// venue and holder, and the session's sink (the pump delivers into it).
struct FeedTickContext<'a> {
    lane: &'a dyn DataLane,
    venue: &'a str,
    holder: &'a DataLaneHolder,
    sink: Arc<dyn DataClientSink>,
    /// The same sink, concretely: where the session's lost book frames are counted.
    wire: DataClientSinkStream,
}

impl FeedTickContext<'_> {
    /// What a pump of `symbol` delivers into: the session's sink and its book-loss figure for the symbol.
    fn pump_sink(&self, symbol: &str) -> FeedTickSink {
        let (wire, symbol) = (self.wire.clone(), symbol.to_string());
        FeedTickSink::tracked(Arc::clone(&self.sink), Arc::new(move || wire.book_losses(&symbol)))
    }
}

/// `DataOpen { venue, network }`: refuse, or open the session and run it to its end.
///
/// A CREDENTIALED venue (`cs_venue=`) is served to a `Scope::Write` connection only and takes no
/// network; a keyless one is served to either scope. A `network` is refused for a venue that takes none,
/// never ignored.
pub fn serve(conn: &mut Connection, venue: &str, network: Option<&str>) -> ServerAfter {
    let shared = Arc::clone(&conn.shared);
    let credentialed = shared.lanes.data.credentialed_venues().iter().any(|v| v == venue);
    if credentialed {
        if !scope_admits(conn.scope, VerbScope::Write) {
            return refuse(
                conn,
                format!(
                    "gatewayhub: the credentialed stream of `{venue}` needs a control-scope \
                     connection (this one is {}); nothing was opened",
                    conn.scope.name()
                ),
            );
        }
        if let Some(n) = network {
            return refuse(
                conn,
                format!(
                    "gatewayhub: venue `{venue}` takes no network (asked for `{n}`); nothing was \
                     opened"
                ),
            );
        }
    } else if !shared.data_venues.iter().any(|v| v == venue) {
        return refuse(
            conn,
            format!(
                "gatewayhub: no live stream is served for `{venue}` (its Welcome advertises \
                 market_data_venue= for: [{}]); nothing was opened",
                shared.data_venues.join(", ")
            ),
        );
    }
    if let Some(n) = network
        && !shared.lanes.data.network_venues().iter().any(|v| v == venue)
    {
        return refuse(
            conn,
            format!(
                "gatewayhub: venue `{venue}` takes no network (asked for `{n}`); nothing was opened"
            ),
        );
    }
    let cap = shared.config.limits.max_stream_sessions;
    let Some(slot) = shared.stats.stream_sessions.try_enter(cap) else {
        // A CAP, by its id (the refusal frame; an older client is answered the words it classes a cap by).
        let full = Response::Refused(GatewayhubRefusal::StreamSessionsFull { cap }.into());
        return match conn.reply(&full) {
            Ok(()) => ServerAfter::Continue,
            Err(_) => ServerAfter::Close,
        };
    };
    let session = shared.stats.next_session();
    let holder =
        DataLaneHolder { session, peer: conn.peer, scope: conn.scope, key_id: conn.key_id.clone() };
    let limits = &shared.config.limits;
    let sink = DataClientSinkStream::new(
        venue,
        StreamSinkLimits {
            queue: StreamQueueLimits {
                bytes: limits.queue_bytes,
                frames: limits.queue_frames,
                control_frames: limits.queue_control_frames,
            },
            max_keys: limits.max_subs_per_session,
            max_unsolicited: limits.max_unsolicited_keys,
        },
    );
    // The session's queue ends when this reader leaves by ANY path, a panic inside a lane included
    // (`DataClientSinkStream::end_on_drop`), or a writer would heartbeat on for a session nobody serves.
    let _end = sink.end_on_drop();
    // A refusal's class (not now, a cap) goes to a subscriber whose connection said it reads it.
    if conn.reads_refusal_frame {
        sink.carry_refusal_kinds();
    }
    #[cfg(feature = "data")]
    let _price_links = crate::link_writer::PriceLinks::over(&sink, venue, network, session);
    let wire_sink: Arc<dyn DataClientSink> = Arc::new(sink.clone());
    let opened_client = if credentialed {
        shared.lanes.data.open_credentialed(venue, &holder, Arc::clone(&wire_sink))
    } else if let Some(n) = network {
        shared.lanes.data.open_on(venue, n, &holder, Arc::clone(&wire_sink))
    } else {
        shared.lanes.data.open(venue, &holder, Arc::clone(&wire_sink))
    };
    let mut client = match opened_client {
        Ok(client) => client,
        Err(text) => return refuse(conn, text),
    };
    let writer_socket = match conn.stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            client.shutdown();
            return refuse(conn, format!("gatewayhub: could not open the session: {e}"));
        }
    };
    let heartbeat = shared.config.timings.stream_heartbeat;
    let opened = Response::DataOpened {
        session,
        epoch: shared.stats.epoch,
        heartbeat_ms: u64::try_from(heartbeat.as_millis()).unwrap_or(u64::MAX),
    };
    if conn.reply(&opened).is_err() {
        client.shutdown();
        return ServerAfter::Close;
    }
    tracing::info!(
        session,
        venue,
        peer = ?conn.peer,
        key_id = conn.key_id.as_deref(),
        "vike-gatewayhub stream: session opened"
    );

    let writer_sink = sink.clone();
    let config = StreamWriterConfig {
        heartbeat,
        lapse_budget: limits.lapse_budget,
        lapse_window: limits.lapse_window,
        lapse_episode: limits.lapse_episode,
        down_frame_ceiling_bytes: GATEWAY_DOWN_FRAME_CEILING_BYTES,
        // The form the `DataOpen` came in: an older datahub, which writes JSON, is written JSON.
        codec: conn.codec,
    };
    let writer = std::thread::Builder::new()
        .name(vike_market_stream::market::venue_thread_name("gw-wr-", venue))
        .spawn(move || run_writer(writer_socket, &writer_sink, config, session));
    let writer = match writer {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!(session, error = %e, "vike-gatewayhub stream: could not start the writer");
            client.shutdown();
            return ServerAfter::Close;
        }
    };
    let stop_sink = sink.clone();
    conn.control.set_wake(move || stop_sink.finish(Some(StreamByeReason::ServerStopping)));
    // A credentialed session is held where a change of its account's credentials can end it (decision 0287).
    let _credentialed =
        credentialed.then(|| shared.credentialed_sessions.enter(session, venue, &sink));

    let pump = FeedTickContext {
        lane: shared.lanes.data.as_ref(),
        venue,
        holder: &holder,
        sink: wire_sink,
        wire: sink.clone(),
    };
    let (subs, leases) = read_loop(conn, &shared, &sink, client.as_mut(), &pump);

    // Teardown, in the order the module doc gives. The pump leases go last of all: a lease never blocks
    // (the lane hands the join to a thread of its own), but nothing of the session is waiting on them.
    sink.finish(None);
    let _ = writer.join();
    client.begin_shutdown();
    client.shutdown();
    drop(leases);
    let summary = sink.summary();
    tracing::info!(
        session,
        venue,
        subscriptions_at_end = subs,
        superseded = summary.queue.superseded,
        lossless_evicted = summary.queue.lossless_evicted,
        conflatable_evicted = summary.queue.conflatable_evicted,
        nonfinite = summary.queue.nonfinite,
        unsendable = summary.queue.unsendable,
        unbound_dropped = summary.unbound_dropped,
        "vike-gatewayhub stream: session closed"
    );
    drop(slot);
    ServerAfter::Close
}

/// The reader loop: runs until the session ends, returns how many subscriptions it held and the tick
/// pump leases they held (released by the caller, after the writer is joined).
fn read_loop(
    conn: &mut Connection,
    shared: &ServerShared,
    sink: &DataClientSinkStream,
    client: &mut dyn DataClient,
    pump: &FeedTickContext<'_>,
) -> (usize, Vec<FeedTickSubscription>) {
    let mut subs: HashMap<u64, Sub> = HashMap::new();
    let max_subs = shared.config.limits.max_subs_per_session;
    let session_idle = shared.config.timings.session_idle;
    let frame_timeout = shared.config.timings.frame_read_timeout;
    let tick = IDLE_TICK.min(session_idle);
    let mut idle_since = Some(Instant::now());
    loop {
        if conn.control.is_stopping() || sink.is_ended() {
            break;
        }
        match wait_readable(&conn.stream, tick) {
            Ok(StreamFrameReady::Data) => {}
            Ok(StreamFrameReady::Timeout) => {
                if subs.is_empty() && idle_since.is_some_and(|t| t.elapsed() >= session_idle) {
                    sink.finish(Some(StreamByeReason::SessionIdle));
                    break;
                }
                continue;
            }
            Ok(StreamFrameReady::Eof) | Err(_) => break,
        }
        let body = match read_client_body(&mut conn.stream, frame_timeout) {
            Ok(body) => body,
            Err(e) => {
                tracing::info!(error = %e, "vike-gatewayhub stream: read fault; closing");
                break;
            }
        };
        let frame = match vike_hub_protocol::frame::decode_body::<StreamUp>(&body) {
            Ok(frame) => frame,
            Err(e) => {
                // The stream's grammar is closed (`StreamUp`), so an undecodable frame is a desync, not
                // a bad request: there is no positional reply to answer it with.
                tracing::warn!(error = %e, "vike-gatewayhub stream: undecodable client frame; closing");
                break;
            }
        };
        match frame {
            StreamUp::Subscribe { sid, op, class } => {
                handle_sub(sink, client, pump, &mut subs, max_subs, sid, &op, class);
            }
            StreamUp::Delivery { sid, class } => {
                // The class is queue policy: the venue client is not told, and an unknown `sid` or a
                // bar lane is a no-op (no reply, like `Unsubscribe`).
                if let Some(sub) = subs.get(&sid)
                    && let Some(lane) = sub.lane
                {
                    sink.set_delivery(sub.k, lane, class);
                }
            }
            StreamUp::Unsubscribe { sid } => {
                if let Some(sub) = subs.remove(&sid) {
                    if let Some(id) = sub.id {
                        client.unsubscribe(id);
                    }
                    // A tick pump lease goes BEFORE the key: leaving the pump hub takes its lock, so
                    // after the drop no frame of the pump is in flight for this symbol. Given back the
                    // other way round, such a frame finds the symbol unbound and binds an UNSOLICITED
                    // key (a stray `Key` frame, one of the session's few unsolicited slots burned).
                    drop(sub.lease);
                    // The lane's floor was this subscription's: with the symbol's other subscriptions
                    // staying, nothing on the lane holds it any more. (With none left, `sub_ended`
                    // releases the key and its floors with it.)
                    if let Some(lane) = sub.lane
                        && subs.values().any(|s| s.k == sub.k)
                        && !subs.values().any(|s| s.k == sub.k && s.lane == Some(lane))
                    {
                        sink.set_delivery(sub.k, lane, DataClientSinkDelivery::Conflatable);
                    }
                    sink.sub_ended(&sub.symbol);
                }
            }
            StreamUp::BeginShutdown => client.begin_shutdown(),
            StreamUp::Ping => sink.request_heartbeat(),
            // The subscriber decodes funding frames (it saw `stream_funding`): carry them from now on.
            StreamUp::Funding => sink.carry_funding(),
            // The subscriber reads binary tick bodies (it saw `stream_binary_ticks`): write the ticks so from now on,
            // the one spelling a tick has on this hop and on the strategy lane after it.
            StreamUp::BinaryTicks => sink.carry_binary_ticks(),
            StreamUp::Close => {
                sink.finish(Some(StreamByeReason::ClientClosed));
                break;
            }
        }
        idle_since =
            if subs.is_empty() { idle_since.or_else(|| Some(Instant::now())) } else { None };
    }
    let held = subs.len();
    (held, subs.into_values().filter_map(|s| s.lease).collect())
}

/// Why a `Ticks` subscription is refused before any key is bound, or `None`: a grid the pump cannot be
/// sized to, a venue with no pump, a symbol this session already pumps. All three are PERMANENT for the
/// same subscription (retrying cannot help), so none is a `Subscribe` failure.
fn refuse_pump(
    pump: &FeedTickContext<'_>,
    subs: &HashMap<u64, Sub>,
    symbol: &str,
    tick_size: f64,
) -> Option<StreamSubscriptionErrorKind> {
    if let Err(why) = validate_tick_size(tick_size) {
        return Some(StreamSubscriptionErrorKind::SymbolRejected(format!("gatewayhub: {why}")));
    }
    if !pump.lane.feed_tick_venues().iter().any(|v| v == pump.venue) {
        return Some(StreamSubscriptionErrorKind::Unsupported(
            "the gatewayhub hosts no tick pump for this venue".to_string(),
        ));
    }
    if subs.values().any(|s| s.lease.is_some() && s.symbol == symbol) {
        return Some(StreamSubscriptionErrorKind::SymbolRejected(format!(
            "gatewayhub: this session already pumps `{symbol}` (one tick pump per symbol per session)"
        )));
    }
    None
}

/// One `Sub`: validate, bind a key, apply the class it asks for, ask the venue client, answer.
fn handle_sub(
    sink: &DataClientSinkStream,
    client: &mut dyn DataClient,
    pump: &FeedTickContext<'_>,
    subs: &mut HashMap<u64, Sub>,
    max_subs: usize,
    sid: u64,
    op: &SubscriptionOperation,
    class: Option<DataClientSinkDelivery>,
) {
    // The client allocates `sid`, so a repeat is the same subscription: answer it again, count nothing.
    if let Some(existing) = subs.get(&sid) {
        sink.re_ack(sid, existing.k);
        return;
    }
    if let Some(why) = op.refuse_spelling(vike_marketdata::Symbol::MAX_READ_LEN) {
        sink.refuse(sid, StreamSubscriptionErrorKind::SymbolRejected(format!("gatewayhub: {why}")));
        return;
    }
    if let SubscriptionOperation::Ticks { symbol, tick_size } = op
        && let Some(kind) = refuse_pump(pump, subs, symbol, *tick_size)
    {
        sink.refuse(sid, kind);
        return;
    }
    if subs.len() >= max_subs {
        sink.refuse(
            sid,
            StreamSubscriptionErrorKind::Budget {
                scope: "subscriptions per session".to_string(),
                held: u32::try_from(subs.len()).unwrap_or(u32::MAX),
                cap: u32::try_from(max_subs).unwrap_or(u32::MAX),
            },
        );
        return;
    }
    let symbol = op.symbol();
    let opened: StreamSinkOpen = match sink.open_sub(symbol) {
        Ok(opened) => opened,
        Err(err) => {
            sink.refuse(sid, err);
            return;
        }
    };
    // The class goes in BEFORE the venue is asked, so a frame the venue client emits from inside
    // `subscribe_*` is already queued under it.
    let lane = op.lane();
    let raised = match (class, lane) {
        (Some(class), Some(lane)) => Some((lane, sink.set_delivery(opened.k, lane, class))),
        _ => None,
    };
    let mut lease: Option<FeedTickSubscription> = None;
    let started: Result<Option<DataClientSubscriptionId>, DataClientError> = match op {
        SubscriptionOperation::Bars { symbol, interval } => {
            client.subscribe_bars(symbol, interval).map(Some)
        }
        SubscriptionOperation::Quotes { symbol } => client.subscribe_quotes(symbol).map(Some),
        SubscriptionOperation::Trades { symbol } => client.subscribe_trades(symbol).map(Some),
        SubscriptionOperation::Book { symbol } => client.subscribe_book(symbol).map(Some),
        SubscriptionOperation::Depth { symbol } => client.subscribe_depth(symbol).map(Some),
        // The tube carries the tick pump as an op, not a client verb: the lane hosts the pump (shared
        // with every session naming the same grid) and the lease is what this subscription holds.
        SubscriptionOperation::Ticks { symbol, tick_size } => pump
            .lane
            .open_feed_tick(pump.venue, pump.holder, symbol, *tick_size, pump.pump_sink(symbol))
            .map(|held| {
                lease = Some(held);
                None
            }),
    };
    match started {
        Ok(id) => {
            subs.insert(sid, Sub { id, symbol: symbol.to_string(), k: opened.k, lane, lease });
            sink.sub_ok(sid, symbol, opened);
        }
        Err(e) => {
            // A key this call created is purged with its floor; one it joined gets the lane's old floor back.
            if let Some((lane, previous)) = raised
                && opened.kind != StreamSinkOpenKind::Fresh
            {
                sink.set_delivery(opened.k, lane, previous);
            }
            let adopted = opened.kind == StreamSinkOpenKind::Adopted;
            tracing::debug!(error = %e, adopted, "vike-gatewayhub stream: the venue client refused a subscription");
            sink.sub_failed(sid, symbol, opened, StreamSubscriptionErrorKind::from_live_error(&e));
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
