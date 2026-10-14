//! The `stream` class: one session is one venue client, one bounded queue, one writer thread and the
//! reader loop below.
//!
//! `DataOpen` is the MODE SWITCH. After the `DataOpened` reply (the last positional frame this socket
//! carries) every client frame is a `StreamUp` and every server frame a `StreamDown`; a refusal BEFORE
//! the reply (an unserved venue, a full plane, the lane's own error) is a positional `Response::Error`
//! and the connection stays positional.
//!
//! # Two threads, one socket
//!
//! The connection thread becomes the session's READER: it reads `StreamUp` frames, drives the venue
//! client (`subscribe_*`, `unsubscribe`, `begin_shutdown`, `shutdown`) and is the only thread that
//! ever calls it. A named WRITER thread (`vike_market_stream::writer`) drains the session's queue to the
//! socket. They share nothing but the `WireSink` and the socket, so a venue call that blocks the
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
//! # Liveness
//!
//! The stream is NOT bounded by the idle read timeout: its client is silent by design. The daemon proves
//! liveness the other way, with a `Heartbeat` after the heartbeat period it STATED in `DataOpened`
//! (the client's deadline is three of them), and notices a vanished client by EOF, a write fault or the
//! write timeout. A session that holds no subscription for `session_idle` is ended with
//! `ByeReason::SessionIdle`: a venue client kept for nobody.
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
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, SubscriptionId};
use vike_gatewayhub_client::proto::{GW_DOWN_FRAME_CEILING_BYTES, GW_MAX_SYMBOL_BYTES, Response};
use vike_market_stream::queue::QueueLimits;
use vike_market_stream::sink::{OpenKind, Opened, SinkLimits, WireSink};
use vike_market_stream::wire::{ByeReason, StreamUp, SubErrKind, SubOp, WireDelivery, WireLane};
use vike_market_stream::writer::{WriterConfig, run_writer};

use super::frame::{Ready, read_client_body, wait_readable};
use super::shared::{After, Conn, Shared};
use crate::data::Holder;

/// How often the reader wakes to look at the session's idle clock when no frame arrives.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// Ends the session's queue when the reader leaves by ANY path, a panic inside a lane included. The
/// writer thread holds a clone of the socket, so without this a reader that unwound would leave a writer
/// heartbeating on an open socket for a session nobody serves: a zombie the client would believe in.
struct EndOnDrop(WireSink);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        self.0.finish(None);
    }
}

struct Sub {
    id: SubscriptionId,
    symbol: String,
    k: u32,
    /// The tick lane the subscription is on, `None` for a bar lane: where a class change applies.
    lane: Option<WireLane>,
}

/// `DataOpen { venue }`: refuse, or open the session and run it to its end.
pub fn serve(conn: &mut Conn, venue: &str) -> After {
    let shared = Arc::clone(&conn.shared);
    if !shared.data_venues.iter().any(|v| v == venue) {
        return refuse(
            conn,
            format!(
                "gatewayhub: no live stream is served for `{venue}` (its Welcome advertises \
                 md_venue= for: [{}]); nothing was opened",
                shared.data_venues.join(", ")
            ),
        );
    }
    let cap = shared.config.limits.max_stream_sessions;
    let Some(slot) = shared.stats.stream_sessions.try_enter(cap) else {
        return refuse(
            conn,
            format!(
                "gatewayhub: already serving {cap} stream sessions (the daemon's cap); transient, \
                 retried on the caller's next pass"
            ),
        );
    };
    let session = shared.stats.next_session();
    let holder =
        Holder { session, peer: conn.peer, scope: conn.scope, key_id: conn.key_id.clone() };
    let limits = &shared.config.limits;
    let sink = WireSink::new(
        venue,
        SinkLimits {
            queue: QueueLimits {
                bytes: limits.queue_bytes,
                frames: limits.queue_frames,
                ctrl_frames: limits.queue_ctrl_frames,
            },
            max_keys: limits.max_subs_per_session,
            max_unsolicited: limits.max_unsolicited_keys,
        },
    );
    let _end = EndOnDrop(sink.clone());
    let mut client = match shared.lanes.data.open(venue, &holder, Arc::new(sink.clone())) {
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
        return After::Close;
    }
    tracing::info!(
        session,
        venue,
        peer = ?conn.peer,
        key_id = conn.key_id.as_deref(),
        "vike-gatewayhub stream: session opened"
    );

    let writer_sink = sink.clone();
    let cfg = WriterConfig {
        heartbeat,
        lapse_budget: limits.lapse_budget,
        lapse_window: limits.lapse_window,
        down_frame_ceiling_bytes: GW_DOWN_FRAME_CEILING_BYTES,
    };
    let writer = std::thread::Builder::new()
        .name(writer_thread_name(venue))
        .spawn(move || run_writer(writer_socket, &writer_sink, cfg, session));
    let writer = match writer {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!(session, error = %e, "vike-gatewayhub stream: could not start the writer");
            client.shutdown();
            return After::Close;
        }
    };
    let stop_sink = sink.clone();
    conn.ctl.set_wake(move || stop_sink.finish(Some(ByeReason::ServerStopping)));

    let subs = read_loop(conn, &shared, &sink, client.as_mut());

    // Teardown, in the order the module doc gives.
    sink.finish(None);
    let _ = writer.join();
    client.begin_shutdown();
    client.shutdown();
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
    After::Close
}

/// A positional refusal of `DataOpen`: the socket stays positional.
fn refuse(conn: &mut Conn, text: String) -> After {
    match conn.reply_error(text) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

/// `gwh-wr-<venue>`, ASCII and at most the 15 bytes Linux keeps.
fn writer_thread_name(venue: &str) -> String {
    let mut name = String::from("gwh-wr-");
    name.extend(venue.chars().filter(char::is_ascii_graphic));
    name.truncate(15);
    name
}

/// Why a subscription's symbol or interval is refused before the venue is asked, or `None`.
fn refuse_spelling(op: &SubOp) -> Option<String> {
    let mut fields: Vec<(&str, &str)> = vec![("symbol", op.symbol())];
    if let SubOp::Bars { interval, .. } = op {
        fields.push(("interval", interval));
    }
    for (what, value) in fields {
        if value.trim().is_empty() {
            return Some(format!("the {what} is blank"));
        }
        if value.len() > GW_MAX_SYMBOL_BYTES {
            return Some(format!(
                "the {what} is {} bytes, over the {GW_MAX_SYMBOL_BYTES}-byte limit",
                value.len()
            ));
        }
        if value.chars().any(char::is_control) {
            return Some(format!("the {what} carries a control character"));
        }
    }
    None
}

/// The reader loop: runs until the session ends, returns how many subscriptions it held.
fn read_loop(
    conn: &mut Conn,
    shared: &Shared,
    sink: &WireSink,
    client: &mut dyn DataClient,
) -> usize {
    let mut subs: HashMap<u64, Sub> = HashMap::new();
    let max_subs = shared.config.limits.max_subs_per_session;
    let session_idle = shared.config.timings.session_idle;
    let frame_timeout = shared.config.timings.frame_read_timeout;
    let tick = IDLE_TICK.min(session_idle);
    let mut idle_since = Some(Instant::now());
    loop {
        if conn.ctl.is_stopping() || sink.is_ended() {
            break;
        }
        match wait_readable(&conn.stream, tick) {
            Ok(Ready::Data) => {}
            Ok(Ready::Timeout) => {
                if subs.is_empty() && idle_since.is_some_and(|t| t.elapsed() >= session_idle) {
                    sink.finish(Some(ByeReason::SessionIdle));
                    break;
                }
                continue;
            }
            Ok(Ready::Eof) | Err(_) => break,
        }
        let body = match read_client_body(&mut conn.stream, frame_timeout) {
            Ok(body) => body,
            Err(e) => {
                tracing::info!(error = %e, "vike-gatewayhub stream: read fault; closing");
                break;
            }
        };
        let frame = match serde_json::from_slice::<StreamUp>(&body) {
            Ok(frame) => frame,
            Err(e) => {
                // The stream's grammar is closed (`StreamUp`), so an undecodable frame is a desync, not
                // a bad request: there is no positional reply to answer it with.
                tracing::warn!(error = %e, "vike-gatewayhub stream: undecodable client frame; closing");
                break;
            }
        };
        match frame {
            StreamUp::Sub { sid, op, class } => {
                handle_sub(sink, client, &mut subs, max_subs, sid, &op, class);
            }
            StreamUp::Delivery { sid, class } => {
                // The class is queue policy: the venue client is not told, and an unknown `sid` or a
                // bar lane is a no-op (no reply, like `Unsub`).
                if let Some(sub) = subs.get(&sid)
                    && let Some(lane) = sub.lane
                {
                    sink.set_delivery(sub.k, lane, class.delivery());
                }
            }
            StreamUp::Unsub { sid } => {
                if let Some(sub) = subs.remove(&sid) {
                    client.unsubscribe(sub.id);
                    // The lane's floor was this subscription's: with the symbol's other subscriptions
                    // staying, nothing on the lane holds it any more. (With none left, `sub_ended`
                    // releases the key and its floors with it.)
                    if let Some(lane) = sub.lane
                        && subs.values().any(|s| s.k == sub.k)
                        && !subs.values().any(|s| s.k == sub.k && s.lane == Some(lane))
                    {
                        sink.set_delivery(sub.k, lane, Delivery::Conflatable);
                    }
                    sink.sub_ended(&sub.symbol);
                }
            }
            StreamUp::BeginShutdown => client.begin_shutdown(),
            StreamUp::Ping => sink.request_heartbeat(),
            StreamUp::Close => {
                sink.finish(Some(ByeReason::ClientClosed));
                break;
            }
        }
        idle_since =
            if subs.is_empty() { idle_since.or_else(|| Some(Instant::now())) } else { None };
    }
    subs.len()
}

/// One `Sub`: validate, bind a key, apply the class it asks for, ask the venue client, answer.
fn handle_sub(
    sink: &WireSink,
    client: &mut dyn DataClient,
    subs: &mut HashMap<u64, Sub>,
    max_subs: usize,
    sid: u64,
    op: &SubOp,
    class: Option<WireDelivery>,
) {
    // The client allocates `sid`, so a repeat is the same subscription: answer it again, count nothing.
    if let Some(existing) = subs.get(&sid) {
        sink.re_ack(sid, existing.k);
        return;
    }
    if let Some(why) = refuse_spelling(op) {
        sink.refuse(sid, SubErrKind::SymbolRejected(format!("gatewayhub: {why}")));
        return;
    }
    if subs.len() >= max_subs {
        sink.refuse(
            sid,
            SubErrKind::Budget {
                scope: "subscriptions per session".to_string(),
                held: u32::try_from(subs.len()).unwrap_or(u32::MAX),
                cap: u32::try_from(max_subs).unwrap_or(u32::MAX),
            },
        );
        return;
    }
    let symbol = op.symbol();
    let opened: Opened = match sink.open_sub(symbol) {
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
        (Some(class), Some(lane)) => {
            Some((lane, sink.set_delivery(opened.k, lane, class.delivery())))
        }
        _ => None,
    };
    let started = match op {
        SubOp::Bars { symbol, interval } => client.subscribe_bars(symbol, interval),
        SubOp::Quotes { symbol } => client.subscribe_quotes(symbol),
        SubOp::Trades { symbol } => client.subscribe_trades(symbol),
        SubOp::Book { symbol } => client.subscribe_book(symbol),
        SubOp::Depth { symbol } => client.subscribe_depth(symbol),
    };
    match started {
        Ok(id) => {
            subs.insert(sid, Sub { id, symbol: symbol.to_string(), k: opened.k, lane });
            sink.sub_ok(sid, symbol, opened);
        }
        Err(e) => {
            // A key this call created is purged with its floor; one it joined gets the lane's old floor back.
            if let Some((lane, previous)) = raised
                && opened.kind != OpenKind::Fresh
            {
                sink.set_delivery(opened.k, lane, previous);
            }
            let adopted = opened.kind == OpenKind::Adopted;
            tracing::debug!(error = %e, adopted, "vike-gatewayhub stream: the venue client refused a subscription");
            sink.sub_failed(sid, symbol, opened, SubErrKind::from_live_error(&e));
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
