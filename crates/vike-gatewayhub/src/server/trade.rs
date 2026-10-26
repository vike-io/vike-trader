//! The `trade` class: the WIRE ADAPTER of `crate::trade`. It owns the socket and the two threads of a
//! lease; everything else is `crate::trade`'s (and `crate::trade` names nothing in here).
//!
//! `TradeOpen` is the MODE SWITCH. After the `TradeOpened` reply (the last positional frame this socket
//! carries) every client frame is a `TradeUp` and every server frame a `TradeDown`; a refusal BEFORE the
//! reply (a held lease, a tier that does not match, a venue this build carries no adapter for) is a
//! positional `LeaseHeld`, `TradeRefused` or `Error` and the connection stays positional.
//!
//! # Two threads, one socket
//!
//! The connection thread becomes the lease's READER: it waits for a frame to begin in short slices (so
//! it notices a fence, a `Bye` or the daemon's stop between them), reads it under the trade up ceiling,
//! decodes it and hands it to `Lease::handle`. A named WRITER thread (`gwh-trade-write`) is the only one
//! that writes the socket after the reply: it asks `Lease::next` for the next frame (control frames, then
//! the account's event ring in `seq` order), writes it under its ceiling, and writes a `Heartbeat` when a
//! full heartbeat period passed with nothing to say. They share nothing but the lease and the socket, so
//! an `AdapterStart` that blocks the reader in a venue login never stops the heartbeat, and a client that
//! stops reading stops only its writer (a write timeout ends the lease; the ring keeps the events).
//!
//! # Liveness, both ways
//!
//! A trade connection is exempt from the idle read timeout (`SERVER_IDLE_READ_TIMEOUT`): a client that has
//! nothing to send is silent by design, and what proves it alive is its `Ping` at the heartbeat period
//! the daemon stated in `TradeOpened`. The daemon holds the other end of the same bargain: a lease silent
//! for `max(read_floor, 3 x heartbeat)` is released (`TRADE-LINK:` line), so a ghost client cannot hold a
//! lease, and the daemon's `Heartbeat` is what lets the client call a silent daemon dead.
//!
//! # The end
//!
//! However the reader leaves (EOF, a fence, a `Bye`, a protocol violation, the stop, a panic), the guard
//! closes the lease, joins the writer (after forcing the socket shut if it will not end) and calls
//! `Lease::finish`, which releases the session's hold, lets a fenced open proceed and, after a final
//! detach, tears the adapter down. The client's final `Detach` is answered `Bye(ClientClosed)` and the
//! client closes first.

use std::io::{self, ErrorKind};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    FEATURE_GW_TRADE, GW_DOWN_FRAME_CEILING_BYTES, GW_TRADE_EVENT_FRAME_CEILING_BYTES,
    GW_TRADE_UP_FRAME_CEILING_BYTES, Response, TradeDown, TradeUp, WireCursor,
    read_frame_raw_capped,
};
use vike_market_stream::frame::write_capped;
use vike_model::AccountTier;
use vike_model::events::Event;

use super::features::not_served;
use super::frame::{Ready, wait_readable};
use super::limits::ServerConfig;
use super::shared::{After, Conn};
use crate::budget::ledger::Budget;
use crate::trade::TradeConfig;
use crate::trade::lane::TradeLane;
use crate::trade::lease::{Flow, Lease, Next};
use crate::trade::session::{OpenRefusal, OpenRequest, Opened, TradeHub};

/// How long the reader waits for a frame to begin before it looks at its flags again: the longest a
/// fence, a `Bye` or the daemon's stop can go unnoticed by a reader parked in a read.
const READER_SLICE: Duration = Duration::from_millis(100);

/// What the daemon is handed to mount the trade lane: the venue half, the paths the sessions need and
/// the shared request budget. Built by the composition root.
pub struct TradeSetup {
    /// The venue half (`BridgeTrade` in production, `ScriptedTrade` in the testkit).
    pub lane: Arc<dyn TradeLane>,
    /// The project's state directory (`LIVE-*.lock`, `gateway-id`): `Booted::state_dir`.
    pub state_dir: Option<PathBuf>,
    /// The HALT sentinel's path: `vike_boot::halt::halt_path_from_env()` after the boot declared the
    /// project. `None` = no gate at the sessions' seam (the bridges keep their own).
    pub halt_path: Option<PathBuf>,
    /// The request budget (`Ready::budget`): every order-class command is counted against it.
    pub budget: Arc<Budget>,
}

/// A `TradeOpen`'s fields, as the connection loop decoded them.
pub struct OpenArgs {
    /// The caller's id for the lease.
    pub client_id: String,
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text.
    pub account: Option<String>,
    /// The tier the caller believes the account runs at.
    pub tier: AccountTier,
    /// The last event the caller folded.
    pub resume: Option<WireCursor>,
    /// Take the lease over.
    pub takeover: bool,
}

/// The hub for `setup`, with the server's numbers.
pub(super) fn build_hub(setup: TradeSetup, config: &ServerConfig) -> Arc<TradeHub> {
    let (t, l) = (&config.timings, &config.limits);
    let cfg = TradeConfig {
        heartbeat: t.trade_heartbeat,
        read_floor: t.trade_read_floor,
        ring_events: l.trade_ring_events,
        dedup_coids: l.trade_dedup_coids,
        command_queue: l.trade_command_queue,
        control_queue: l.trade_control_queue,
        event_channel: l.trade_event_channel,
        max_leases: l.max_trade_leases,
        orphan_ttl: t.trade_orphan_ttl,
        recon_progress: t.fetch_progress_every,
        recon_page_bytes: l.fetch_page_ceiling,
        fence_drain: t.trade_fence_drain,
        account_ccy_retry_after: crate::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: setup.state_dir,
        halt_path: setup.halt_path,
    };
    TradeHub::new(setup.lane, cfg, setup.budget)
}

/// `TradeOpen`: refuse, or open the lease and run it to its end.
pub fn serve(conn: &mut Conn, args: OpenArgs) -> After {
    let shared = Arc::clone(&conn.shared);
    let Some(hub) = shared.trade.clone() else {
        let text = not_served("TradeOpen", FEATURE_GW_TRADE);
        return match conn.reply_error(text) {
            Ok(()) => After::Continue,
            Err(_) => After::Close,
        };
    };
    let socket = match conn.stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            let _ = conn.reply_error(format!("gatewayhub: could not open the lease: {e}"));
            return After::Close;
        }
    };
    let closer: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = socket.shutdown(Shutdown::Both);
    });
    let request = OpenRequest {
        client_id: args.client_id,
        venue: args.venue,
        account: args.account,
        tier: args.tier,
        resume: args.resume,
        takeover: args.takeover,
        key_id: conn.key_id.clone(),
        closer,
    };
    let response = match hub.open(request) {
        Ok(Opened { reply, lease }) => {
            if conn.reply(&Response::TradeOpened(reply)).is_err() {
                lease.close();
                lease.finish();
                return After::Close;
            }
            run(conn, lease);
            return After::Close;
        }
        Err(OpenRefusal::Error(text)) => Response::Error(text),
        Err(OpenRefusal::LeaseHeld { holder_key_id, since_ms }) => {
            Response::LeaseHeld { holder_key_id, since_ms }
        }
        Err(OpenRefusal::Refused { block_word, text }) => {
            Response::TradeRefused { block_word, text }
        }
    };
    match conn.reply(&response) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

/// Ends the lease however the reader leaves: close, join the writer, finish.
struct EndLease {
    lease: Arc<Lease>,
    writer: Option<JoinHandle<()>>,
    write_timeout: Duration,
}

impl Drop for EndLease {
    fn drop(&mut self) {
        self.lease.close();
        if let Some(writer) = self.writer.take() {
            let mut waited = join_for(&writer, Duration::from_secs(1));
            if !waited {
                // A write to a peer that stopped reading: cut the socket so the writer wakes.
                self.lease.force_close();
                waited = join_for(&writer, self.write_timeout + Duration::from_secs(1));
            }
            if waited {
                let _ = writer.join();
            }
        }
        self.lease.finish();
    }
}

fn join_for(handle: &JoinHandle<()>, max: Duration) -> bool {
    let deadline = Instant::now() + max;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// The lease after `TradeOpened`: start the writer, read until the end.
fn run(conn: &mut Conn, lease: Arc<Lease>) {
    let timings = conn.shared.config.timings;
    let stop_lease = Arc::clone(&lease);
    conn.ctl.set_wake(move || stop_lease.stop());
    let writer_socket = match conn.stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "TRADE-LINK: could not clone the lease socket; closing");
            let _guard = EndLease { lease, writer: None, write_timeout: timings.write_timeout };
            return;
        }
    };
    let writer_lease = Arc::clone(&lease);
    let (heartbeat, write_timeout) = (timings.trade_heartbeat, timings.write_timeout);
    let writer = std::thread::Builder::new()
        .name("gwh-trade-write".into())
        .spawn(move || write_loop(writer_socket, &writer_lease, heartbeat, write_timeout));
    let writer = match writer {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, "TRADE-LINK: could not start the lease writer; closing");
            None
        }
    };
    let have_writer = writer.is_some();
    let _guard = EndLease { lease: Arc::clone(&lease), writer, write_timeout };
    if !have_writer {
        return;
    }
    let deadline = timings.trade_read_floor.max(timings.trade_heartbeat.saturating_mul(3));
    read_loop(conn, &lease, deadline);
}

fn read_loop(conn: &mut Conn, lease: &Arc<Lease>, deadline: Duration) {
    let frame_timeout = conn.shared.config.timings.frame_read_timeout;
    let mut idle = Duration::ZERO;
    loop {
        if lease.is_ended() {
            return;
        }
        match wait_readable(&conn.stream, READER_SLICE) {
            Ok(Ready::Eof) => {
                tracing::info!(client = %lease.client_id, "TRADE-LINK: the client closed the connection");
                return;
            }
            Ok(Ready::Timeout) => {
                if lease.is_fenced() {
                    // Quiet for a whole slice after the fence: everything the client had sent is read.
                    return;
                }
                idle += READER_SLICE;
                if idle >= deadline {
                    tracing::warn!(
                        client = %lease.client_id,
                        silent_ms = u64::try_from(idle.as_millis()).unwrap_or(u64::MAX),
                        "TRADE-LINK: the client is silent past the heartbeat deadline; the lease is released"
                    );
                    return;
                }
            }
            Ok(Ready::Data) => {
                idle = Duration::ZERO;
                let body = match read_up_frame(&mut conn.stream, frame_timeout) {
                    Ok(body) => body,
                    Err(e) => {
                        tracing::warn!(client = %lease.client_id, error = %e, "TRADE-LINK: read fault; the lease is released");
                        return;
                    }
                };
                let frame = match serde_json::from_slice::<TradeUp>(&body) {
                    Ok(frame) => frame,
                    Err(e) => {
                        // A well-framed body that is not a known `TradeUp` is a desynced or newer
                        // client: this class has no positional recovery, so it ends, saying why.
                        lease.bye(vike_gatewayhub_client::proto::TradeBye::Protocol(format!(
                            "gatewayhub: an undecodable trade frame: {e}"
                        )));
                        return;
                    }
                };
                if lease.handle(frame) == Flow::End {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(client = %lease.client_id, error = %e, "TRADE-LINK: read fault; the lease is released");
                return;
            }
        }
    }
}

fn read_up_frame(stream: &mut TcpStream, timeout: Duration) -> io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    read_frame_raw_capped(stream, GW_TRADE_UP_FRAME_CEILING_BYTES)
}

/// The writer: the only thread that writes the socket after `TradeOpened`.
fn write_loop(
    mut socket: TcpStream,
    lease: &Arc<Lease>,
    heartbeat: Duration,
    write_timeout: Duration,
) {
    let _ = socket.set_write_timeout(Some(write_timeout));
    loop {
        let frame = match lease.next(heartbeat) {
            Next::Frame(frame) => frame,
            Next::Idle => TradeDown::Heartbeat,
            Next::Bye(bye) => {
                let _ = write_down(&mut socket, &TradeDown::Bye(bye));
                break;
            }
            Next::Closed => break,
        };
        match write_down(&mut socket, &frame) {
            Ok(()) => {}
            // Nothing was written: the frame could not be encoded under its ceiling. The lease goes on,
            // and an event is never lost in silence (see `write_oversize_event`).
            Err(e) if e.kind() == ErrorKind::InvalidData => {
                if let Err(e) = write_oversize(&mut socket, lease, &frame, &e) {
                    tracing::warn!(client = %lease.client_id, error = %e, "TRADE-LINK: write fault; the lease is released");
                    lease.close();
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(client = %lease.client_id, error = %e, "TRADE-LINK: write fault; the lease is released");
                lease.close();
                break;
            }
        }
    }
    let _ = socket.shutdown(Shutdown::Both);
}

fn write_down(socket: &mut TcpStream, frame: &TradeDown) -> io::Result<()> {
    let ceiling = match frame {
        TradeDown::Event { .. } => GW_TRADE_EVENT_FRAME_CEILING_BYTES,
        _ => GW_DOWN_FRAME_CEILING_BYTES,
    };
    write_capped(socket, frame, ceiling)
}

/// The longest `reason` a rejected-class event keeps when its frame does not fit the event ceiling.
const MAX_SHRUNK_REASON_BYTES: usize = 4096;

/// A frame did not fit its ceiling and nothing was written. **An event is never skipped in silence**: its
/// `seq` is already spent, and the client cannot otherwise tell a skipped event from a quiet ring.
///
/// 1. A reject-class event (a venue's error body can be any size, and the reject is an order's terminal
///    event) is sent again with its `reason` cut to [`MAX_SHRUNK_REASON_BYTES`].
/// 2. Anything else, or a frame that is still too large, is replaced by `ReplayUnavailable{floor}` with
///    `floor` just past the skipped `seq`: the client reconciles (the venue's own record has what the
///    frame would have said), and the next event follows as usual.
///
/// A frame that is not an event keeps the old answer: logged, the lease goes on (the requester times out).
fn write_oversize(
    socket: &mut TcpStream,
    lease: &Arc<Lease>,
    frame: &TradeDown,
    cause: &io::Error,
) -> io::Result<()> {
    let TradeDown::Event { seq, .. } = frame else {
        tracing::error!(
            client = %lease.client_id,
            error = %cause,
            "TRADE-LINK: a frame could not be encoded under its ceiling and was NOT sent"
        );
        return Ok(());
    };
    if let Some(small) = shrunk(frame) {
        match write_down(socket, &small) {
            Ok(()) => {
                tracing::warn!(
                    client = %lease.client_id,
                    seq,
                    "TRADE-LINK: an event over the frame ceiling was sent with its reason cut"
                );
                return Ok(());
            }
            Err(e) if e.kind() == ErrorKind::InvalidData => {}
            Err(e) => return Err(e),
        }
    }
    tracing::error!(
        client = %lease.client_id,
        seq,
        error = %cause,
        "TRADE-LINK: an event could not be encoded under its ceiling and was NOT sent; the client is told to reconcile"
    );
    write_down(socket, &TradeDown::ReplayUnavailable { floor: seq.saturating_add(1) })
}

/// The event of `frame` with an over-long `reason` cut, if it is a reject-class event.
fn shrunk(frame: &TradeDown) -> Option<TradeDown> {
    let TradeDown::Event { account, gw_epoch, seq, ts_ms, replayed, event } = frame else {
        return None;
    };
    let mut event = event.clone();
    match &mut event {
        Event::OrderRejected(r) => r.reason = cut(&r.reason).into(),
        Event::OrderCancelRejected(r) => r.reason = cut(&r.reason).into(),
        Event::OrderModifyRejected(r) => r.reason = cut(&r.reason).into(),
        _ => return None,
    }
    Some(TradeDown::Event {
        account: account.clone(),
        gw_epoch: *gw_epoch,
        seq: *seq,
        ts_ms: *ts_ms,
        replayed: *replayed,
        event,
    })
}

/// `text` cut to [`MAX_SHRUNK_REASON_BYTES`] on a character boundary, with a marker.
fn cut(text: &str) -> String {
    if text.len() <= MAX_SHRUNK_REASON_BYTES {
        return text.to_string();
    }
    let mut end = MAX_SHRUNK_REASON_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} [cut: the venue's text was {} bytes]",
        text.get(..end).unwrap_or_default(),
        text.len()
    )
}
