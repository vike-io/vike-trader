//! The market-data verb family: the `MarketDataUpdate` registry mutation (`market_data_update_verb`), the
//! whole-request length bound both spec-list verbs share (`refuse_an_oversized_spec_list`), and the
//! stream writer the connection hands its socket to after the `MarketDataSubscribe` MODE SWITCH
//! (`run_market_writer`). The mode switch itself — `dispatch`, `Step::ModeSwitch` and the
//! `NO_MARKET_DATA_PLANE` refusal a hub-less build answers with — stays in the parent module with
//! the connection loop it is a decision of.

use std::time::Duration;

use super::*;
use vike_hub_protocol::frame::{BodyCodec, write_reply_as};

/// The WHOLE-REQUEST length bound on a spec list, the market-data plane's twin of
/// `delete_series_verb`'s door check — refused before anything is opened, planned or cloned.
///
/// # ⚠ What was unbounded, and why the per-spec cap did not bound it
///
/// [`crate::market_data::MARKET_DATA_MAX_SPECS_PER_SESSION`] is a cap on the keys a session HOLDS:
/// [`MarketDataHub::acquire`] refuses the 65th and the caller keeps going. Nothing bounded the LENGTH of
/// the `Vec<MarketDataSpec>` a client sent, and both consumers of one are loops that CLONE every refusal
/// into a reply — `run_market_writer` over `specs`, [`MarketDataHub::update`] over `add` and `remove`.
/// A post-auth body is read at [`vike_hub_protocol::frame::MAX_FRAME_LEN`] (64 MiB) against a
/// spec that costs tens of bytes, so one legal frame carries on the order of a million of them, and
/// `write_frame` materialises the whole reply before it compares it to that same ceiling. Refusing
/// cost more than accepting, which is backwards, and `docs/decisions/0052`'s decision 2 — *"a
/// subscription's cost is bounded by server constants … never by the request"* — was false on
/// exactly this term while every OTHER term of it was true.
///
/// # Why it reuses the session cap rather than deriving a new constant
///
/// A request may not usefully name more keys than a session could hold even if every one were
/// accepted, so the already-derived number is the honest ceiling — and an UNCALIBRATED constant is
/// a flake (`crates/vike-core/CLAUDE.md`'s latency section, and `MARKET_DATA_MAX_SYMBOL_BYTES`'s own
/// derivation next door). The cost is that a
/// list padded with DUPLICATES past the count is refused although re-`acquire` would have no-op'd
/// them; that is accepted, and the message names both numbers so the sender sees the rule it met.
///
/// `Some(message)` is the refusal; `None` means the length is fine.
pub(super) fn refuse_an_oversized_spec_list(verb: &str, field: &str, len: usize) -> Option<String> {
    let cap = crate::market_data::MARKET_DATA_MAX_SPECS_PER_SESSION;
    (len > cap as usize).then(|| {
        format!(
            "{verb}: `{field}` carries {len} specs, over MARKET_DATA_MAX_SPECS_PER_SESSION = {cap} — the \
             most keys ONE session may hold. Nothing was opened and nothing was changed. A list \
             this long cannot be satisfied even if every spec were valid: the surplus would be \
             refused one by one and every refusal CLONED into the reply, so the request is refused \
             whole instead. Send at most {cap}, and drop any duplicate spec — a key named twice \
             asks for nothing the first mention did not."
        )
    })
}

/// The `MarketDataUpdate` verb: mutate one session's subscription set and report what changed.
///
/// ⚠ **An unknown or expired `session` is a [`Response::Error`], not a refusal list** — the same
/// per-spec-versus-whole-request rule [`NO_MARKET_DATA_PLANE`] carries. It is also the check that
/// stops one desktop mutating another's subscription set on a shared authenticated channel: the id
/// names a set, it authorizes nothing, and the connection's `Scope` remains the ceiling.
///
/// ⚠ **[`refuse_an_oversized_spec_list`] runs BEFORE that session lookup**, on `add` and on
/// `remove` alike: the length is refusable without knowing whose session it is, and the lookup
/// takes a lock. A message naming the session rather than the cap is the shape that says the check
/// ran too late, which is what `crates/vike-datahub-client/tests/market_data_negotiation.rs`'s
/// `an_over_length_update_list_is_refused_before_the_session_is_looked_up` sends a FICTIONAL
/// session to prove.
pub(super) fn market_data_update_verb(
    session: vike_datahub_client::market::MarketDataSessionId,
    add: &[MarketDataSpec],
    remove: &[MarketDataSpec],
    market_data: Option<&MarketDataHub>,
) -> Response {
    let Some(hub) = market_data else {
        return Response::Error(NO_MARKET_DATA_PLANE.to_string());
    };
    // AT THE DOOR, before the session lookup takes a lock: BOTH lists, each naming its own field,
    // because `update` loops and clones over both. `remove` is bounded by the same number for the
    // same reason — a session holds at most that many keys, so a longer removal list names keys it
    // cannot be holding.
    for (field, len) in [("add", add.len()), ("remove", remove.len())] {
        if let Some(why) = refuse_an_oversized_spec_list("MarketDataUpdate", field, len) {
            return Response::Error(why);
        }
    }
    if !hub.has_session(session) {
        return Response::Error(format!(
            "market data: no such session `{session}` on this server — it was never opened here, or \n             its stream connection has ended. Re-open a stream with MarketDataSubscribe; nothing was changed."
        ));
    }
    let (accepted, refused, released) = hub.update(session, add, remove, vike_model::now_ms());
    Response::MarketDataUpdated { accepted, refused, released }
}

/// **The market-data stream writer** — everything this socket does after
/// [`Response::MarketDataSubscribed`] is written.
///
/// It resolves or refuses each spec, writes the ONE positional reply, arms the socket for pushing,
/// hands each accepted key its status-then-snapshot (§5.2 step 5), and then drains its own mailbox
/// until the peer goes away. **It never reads this connection again**, which is the invariant
/// `vike_datahub_client::market`'s module doc states and the reason no correlation id is needed.
///
/// # ⚠ What it arms, and why the ACCOUNT plane could not
///
/// - **`set_write_timeout`([`MARKET_DATA_WRITE_TIMEOUT`]).** `crates/vike-tradehub/src/server.rs`'s
///   `run_push_writer` tolerates an indefinite park because its writer holds nothing; here a parked
///   writer holds a REFCOUNT on a venue socket for a client that is gone. A timed-out `write_all`
///   can leave the stream desynced mid-frame, which is harmless precisely because the only response
///   to any write fault is to CLOSE. This is the explicit disconnect-the-laggard policy.
/// - **`TCP_NODELAY` is already on, and NOT armed here.** This bullet used to arm it, because the
///   request/response plane then set only `set_read_timeout` and this was the one socket that
///   needed it most. Since 2026-10-03 [`handle_connection`] arms it on every accepted socket
///   through `vike_hub_protocol::frame::configure_hub_stream`, before the first frame, so the stream
///   this function inherits already carries it; a second `set_nodelay` here would be a second
///   spelling of one option.
///
/// # ⚠ The `TapeGap` is SYNTHESIZED HERE, not dequeued
///
/// `crate::market_data::mailbox` records the owed range and hands it back beside the frame it precedes, so
/// the "a gap is written BEFORE the next `Trades` batch, never after" contract is STRUCTURAL rather
/// than a rule somebody has to keep. The one cost, stated rather than left for a reviewer to find:
/// this thread does a serialization `run_push_writer` never does. It is bounded — it happens only
/// when a drop actually occurred — and a client accumulating enough gaps for it to matter is already
/// inside [`MARKET_DATA_LAPSE_BUDGET`]'s disconnect.
///
/// Logging stays at CONNECTION BOUNDARIES: no per-frame line, and no payload in any line.
///
/// # ⚠ The heartbeat period is a PARAMETER, and it is the one the client is told
///
/// `heartbeat` is `ServerTimings::market_data_heartbeat`: `vike_datahub_client::market::MARKET_DATA_HEARTBEAT` unless
/// the box wrote `config.md_heartbeat_ms`. The SAME value paces the quiet-stream wake (`recv_timeout`),
/// gates the heartbeat write, and is sent in `MarketDataSubscribed.heartbeat_ms` — one number three times, so
/// the period a client deadlines against is the period this loop keeps.
///
/// # The form is the subscribe's
///
/// `codec` is the form the `MarketDataSubscribe` came in (`vike_hub_protocol::frame::BodyCodec`): the reply, every pushed
/// frame and every frame this writer makes itself are written in it. The session's mailbox is told it BEFORE the
/// first key is acquired, so the publisher frames nothing for this session in another form.
pub(super) fn run_market_writer(
    mut stream: TcpStream,
    hub: Arc<MarketDataHub>,
    guard: MarketDataSessionGuard,
    specs: Vec<MarketDataSpec>,
    peer: Option<SocketAddr>,
    heartbeat: Duration,
    codec: BodyCodec,
) {
    // ⚠ THE GUARD IS OWNED HERE AND DROPPED LAST. Its `Drop` releases every key this session held —
    // `Drop` rather than an explicit call at the end of this function so that a PANIC here also
    // releases, mirroring `crates/vike-tradehub/src/publish.rs`'s `PublisherSubscription`. A release written
    // as the last statement is correct on every ordinary return path and leaks a venue refcount
    // forever on the panic path, and a leaked nonzero refcount is never reaped.
    //
    // ⚠ It is taken by `dispatch` rather than here, and that is not a tidy-up: opening it here made
    // the MARKET_DATA_MAX_STREAM_CONNECTIONS refusal a write-then-drop of the socket, on the one path whose whole
    // contract is that a `Response::Error` leaves the connection positional. See `Step::ModeSwitch`.
    let session = guard.id();
    let mailbox: Arc<MarketDataMailbox> = Arc::clone(guard.mailbox());
    mailbox.set_body_codec(codec);

    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for spec in &specs {
        match hub.acquire(session, spec) {
            Ok(s) => accepted.push(s),
            Err(r) => refused.push((spec.clone(), r)),
        }
    }
    tracing::info!(
        ?peer,
        %session,
        accepted = accepted.len(),
        refused = refused.len(),
        "vike-datahub market data: stream opened"
    );

    // THE LAST POSITIONAL FRAME.
    let reply = Response::MarketDataSubscribed {
        session,
        accepted: accepted.clone(),
        refused,
        heartbeat_ms: heartbeat.as_millis() as u64,
    };
    if write_reply_as(&mut stream, codec, &reply).is_err() {
        return;
    }
    if stream.set_write_timeout(Some(MARKET_DATA_WRITE_TIMEOUT)).is_err() {
        return;
    }
    // `TCP_NODELAY` is NOT armed here: `handle_connection` armed it on this socket before its first
    // frame (this function's doc).

    // §5.2 step 5: per accepted key, its current Status and — only if that status is Live — its
    // current book. `attach_frames` enforces the order; this loop only delivers it.
    let now = vike_model::now_ms();
    for spec in &accepted {
        let key = MarketDataKey::of(spec);
        for frame in hub.attach_frames(&key, now) {
            push_attach_frame(&mailbox, &key, frame);
        }
    }

    let mut lapses: u64 = 0;
    let mut window_start = Instant::now();
    let mut last_write = Instant::now();
    loop {
        if mailbox.must_close() {
            // The CTRL lane overflowed: a client that cannot absorb the status frames of its own
            // keys is dead, and pretending otherwise hands it a frozen ladder it believes is live.
            //
            // ⚠ The reason used to be `MarketDataBye::SessionIdle`, whose own doc is "the session held no
            // specs for long enough that keeping the socket bought nothing" — the OPPOSITE
            // diagnosis, and the only thing anyone has to go on once the socket is gone.
            let _ = write_market_data(
                &mut stream,
                codec,
                MarketDataFrame::Bye(MarketDataBye::ControlLaneOverflow),
            );
            tracing::info!(?peer, %session, "vike-datahub market data: stream closed (control lane overflow)");
            break;
        }
        if window_start.elapsed() >= MARKET_DATA_LAPSE_WINDOW {
            lapses = 0;
            window_start = Instant::now();
        }
        match mailbox.recv_timeout(heartbeat) {
            MarketDataMailboxReceive::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    lapses = lapses.saturating_add(1);
                    let gap = MarketDataFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_sequence: from_seq,
                        to_sequence: to_seq,
                    };
                    if write_market_data(&mut stream, codec, gap).is_err() {
                        break;
                    }
                    if lapses > MARKET_DATA_LAPSE_BUDGET {
                        let _ = write_market_data(
                            &mut stream,
                            codec,
                            MarketDataFrame::Bye(MarketDataBye::TooSlow { lapses }),
                        );
                        tracing::info!(
                            ?peer,
                            %session,
                            lapses,
                            "vike-datahub market data: stream closed (too slow — tape lapses over budget)"
                        );
                        break;
                    }
                }
                if write_all_flush(&mut stream, &bytes).is_err() {
                    tracing::info!(?peer, %session, "vike-datahub market data: stream closed (write fault)");
                    break;
                }
                last_write = Instant::now();
            }
            MarketDataMailboxReceive::Timeout => {
                if last_write.elapsed() >= heartbeat {
                    if write_market_data(&mut stream, codec, MarketDataFrame::Heartbeat).is_err() {
                        break;
                    }
                    last_write = Instant::now();
                }
            }
            MarketDataMailboxReceive::Closed => break,
        }
    }
    drop(guard);
    tracing::info!(?peer, %session, "vike-datahub market data: stream ended");
}

/// Frame one [`MarketDataFrame`] and write it — the per-SUBSCRIBER path (a heartbeat, a `Bye`, a
/// writer-synthesized `TapeGap`), as against the publisher's serialize-once fan-out.
fn write_market_data(
    stream: &mut TcpStream,
    codec: BodyCodec,
    frame: MarketDataFrame,
) -> io::Result<()> {
    write_reply_as(stream, codec, &Response::MarketData(Box::new(frame)))
}

/// Write pre-framed bytes and flush — `crates/vike-tradehub/src/server.rs`'s `write_all_flush`
/// shape. The bytes already carry their length prefix and already passed `MAX_FRAME_LEN`, because
/// the publisher framed them through the same `write_frame` every other frame goes through.
fn write_all_flush(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    stream.write_all(bytes)?;
    stream.flush()
}
