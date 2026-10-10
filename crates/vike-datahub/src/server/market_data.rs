//! The market-data verb family: the `MdUpdate` registry mutation (`md_update_verb`), the
//! whole-request length bound both spec-list verbs share (`refuse_an_oversized_spec_list`), and the
//! stream writer the connection hands its socket to after the `MdSubscribe` MODE SWITCH
//! (`run_market_writer`). The mode switch itself — `dispatch`, `Step::ModeSwitch` and the
//! `NO_MARKET_DATA_PLANE` refusal a hub-less build answers with — stays in the parent module with
//! the connection loop it is a decision of.

use super::*;

/// The WHOLE-REQUEST length bound on a spec list, the market-data plane's twin of
/// `delete_series_verb`'s door check — refused before anything is opened, planned or cloned.
///
/// # ⚠ What was unbounded, and why the per-spec cap did not bound it
///
/// [`crate::md::MD_MAX_SPECS_PER_SESSION`] is a cap on the keys a session HOLDS:
/// [`MdHub::acquire`] refuses the 65th and the caller keeps going. Nothing bounded the LENGTH of
/// the `Vec<MdSpec>` a client sent, and both consumers of one are loops that CLONE every refusal
/// into a reply — `run_market_writer` over `specs`, [`MdHub::update`] over `add` and `remove`.
/// A post-auth body is read at [`vike_datahub_client::proto::MAX_FRAME_LEN`] (64 MiB) against a
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
/// a flake (`crates/vike-core/CLAUDE.md`'s latency section, and `MD_MAX_SYMBOL_BYTES`'s own
/// derivation next door). The cost is that a
/// list padded with DUPLICATES past the count is refused although re-`acquire` would have no-op'd
/// them; that is accepted, and the message names both numbers so the sender sees the rule it met.
///
/// `Some(message)` is the refusal; `None` means the length is fine.
pub(super) fn refuse_an_oversized_spec_list(verb: &str, field: &str, len: usize) -> Option<String> {
    let cap = crate::md::MD_MAX_SPECS_PER_SESSION;
    (len > cap as usize).then(|| {
        format!(
            "{verb}: `{field}` carries {len} specs, over MD_MAX_SPECS_PER_SESSION = {cap} — the \
             most keys ONE session may hold. Nothing was opened and nothing was changed. A list \
             this long cannot be satisfied even if every spec were valid: the surplus would be \
             refused one by one and every refusal CLONED into the reply, so the request is refused \
             whole instead. Send at most {cap}, and drop any duplicate spec — a key named twice \
             asks for nothing the first mention did not."
        )
    })
}

/// The `MdUpdate` verb: mutate one session's subscription set and report what changed.
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
pub(super) fn md_update_verb(
    session: vike_datahub_client::market::MdSessionId,
    add: &[MdSpec],
    remove: &[MdSpec],
    md: Option<&MdHub>,
) -> Response {
    let Some(hub) = md else {
        return Response::Error(NO_MARKET_DATA_PLANE.to_string());
    };
    // AT THE DOOR, before the session lookup takes a lock: BOTH lists, each naming its own field,
    // because `update` loops and clones over both. `remove` is bounded by the same number for the
    // same reason — a session holds at most that many keys, so a longer removal list names keys it
    // cannot be holding.
    for (field, len) in [("add", add.len()), ("remove", remove.len())] {
        if let Some(why) = refuse_an_oversized_spec_list("MdUpdate", field, len) {
            return Response::Error(why);
        }
    }
    if !hub.has_session(session) {
        return Response::Error(format!(
            "market data: no such session `{session}` on this server — it was never opened here, or \n             its stream connection has ended. Re-open a stream with MdSubscribe; nothing was changed."
        ));
    }
    let (accepted, refused, released) = hub.update(session, add, remove, vike_model::now_ms());
    Response::MdUpdated { accepted, refused, released }
}

/// **The market-data stream writer** — everything this socket does after
/// [`Response::MdSubscribed`] is written.
///
/// It resolves or refuses each spec, writes the ONE positional reply, arms the socket for pushing,
/// hands each accepted key its status-then-snapshot (§5.2 step 5), and then drains its own mailbox
/// until the peer goes away. **It never reads this connection again**, which is the invariant
/// `vike_datahub_client::market`'s module doc states and the reason no correlation id is needed.
///
/// # ⚠ What it arms, and why the ACCOUNT plane could not
///
/// - **`set_write_timeout`([`MD_WRITE_TIMEOUT`]).** `crates/vike-tradehub/src/server.rs`'s
///   `run_push_writer` tolerates an indefinite park because its writer holds nothing; here a parked
///   writer holds a REFCOUNT on a venue socket for a client that is gone. A timed-out `write_all`
///   can leave the stream desynced mid-frame, which is harmless precisely because the only response
///   to any write fault is to CLOSE. This is the explicit disconnect-the-laggard policy.
/// - **`TCP_NODELAY` is already on, and NOT armed here.** This bullet used to arm it, because the
///   request/response plane then set only `set_read_timeout` and this was the one socket that
///   needed it most. Since 2026-10-03 [`handle_connection`] arms it on every accepted socket
///   through `vike_node_proto::frame::configure_node_stream`, before the first frame, so the stream
///   this function inherits already carries it; a second `set_nodelay` here would be a second
///   spelling of one option.
///
/// # ⚠ The `TapeGap` is SYNTHESIZED HERE, not dequeued
///
/// `crate::md::mailbox` records the owed range and hands it back beside the frame it precedes, so
/// the "a gap is written BEFORE the next `Trades` batch, never after" contract is STRUCTURAL rather
/// than a rule somebody has to keep. The one cost, stated rather than left for a reviewer to find:
/// this thread does a serialization `run_push_writer` never does. It is bounded — it happens only
/// when a drop actually occurred — and a client accumulating enough gaps for it to matter is already
/// inside [`MD_LAPSE_BUDGET`]'s disconnect.
///
/// Logging stays at CONNECTION BOUNDARIES: no per-frame line, and no payload in any line.
pub(super) fn run_market_writer(
    mut stream: TcpStream,
    hub: Arc<MdHub>,
    guard: SessionGuard,
    specs: Vec<MdSpec>,
    peer: Option<SocketAddr>,
) {
    // ⚠ THE GUARD IS OWNED HERE AND DROPPED LAST. Its `Drop` releases every key this session held —
    // `Drop` rather than an explicit call at the end of this function so that a PANIC here also
    // releases, mirroring `crates/vike-tradehub/src/publish.rs`'s `Subscription`. A release written
    // as the last statement is correct on every ordinary return path and leaks a venue refcount
    // forever on the panic path, and a leaked nonzero refcount is never reaped.
    //
    // ⚠ It is taken by `dispatch` rather than here, and that is not a tidy-up: opening it here made
    // the MD_MAX_STREAM_CONNS refusal a write-then-drop of the socket, on the one path whose whole
    // contract is that a `Response::Error` leaves the connection positional. See `Step::ModeSwitch`.
    let session = guard.id();
    let mailbox: Arc<Mailbox> = Arc::clone(guard.mailbox());

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
        "vike-datahub md: stream opened"
    );

    // THE LAST POSITIONAL FRAME.
    let reply = Response::MdSubscribed {
        session,
        accepted: accepted.clone(),
        refused,
        heartbeat_ms: MD_HEARTBEAT.as_millis() as u64,
    };
    if write_frame(&mut stream, &reply).is_err() {
        return;
    }
    if stream.set_write_timeout(Some(MD_WRITE_TIMEOUT)).is_err() {
        return;
    }
    // `TCP_NODELAY` is NOT armed here: `handle_connection` armed it on this socket before its first
    // frame (this function's doc).

    // §5.2 step 5: per accepted key, its current Status and — only if that status is Live — its
    // current book. `attach_frames` enforces the order; this loop only delivers it.
    let now = vike_model::now_ms();
    for spec in &accepted {
        let key = MdKey::of(spec);
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
            // ⚠ The reason used to be `MdBye::SessionIdle`, whose own doc is "the session held no
            // specs for long enough that keeping the socket bought nothing" — the OPPOSITE
            // diagnosis, and the only thing anyone has to go on once the socket is gone.
            let _ = write_md(&mut stream, MdFrame::Bye(MdBye::ControlLaneOverflow));
            tracing::info!(?peer, %session, "vike-datahub md: stream closed (control lane overflow)");
            break;
        }
        if window_start.elapsed() >= MD_LAPSE_WINDOW {
            lapses = 0;
            window_start = Instant::now();
        }
        match mailbox.recv_timeout(MD_HEARTBEAT) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    lapses = lapses.saturating_add(1);
                    let gap = MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    };
                    if write_md(&mut stream, gap).is_err() {
                        break;
                    }
                    if lapses > MD_LAPSE_BUDGET {
                        let _ = write_md(&mut stream, MdFrame::Bye(MdBye::TooSlow { lapses }));
                        tracing::info!(
                            ?peer,
                            %session,
                            lapses,
                            "vike-datahub md: stream closed (too slow — tape lapses over budget)"
                        );
                        break;
                    }
                }
                if write_all_flush(&mut stream, &bytes).is_err() {
                    tracing::info!(?peer, %session, "vike-datahub md: stream closed (write fault)");
                    break;
                }
                last_write = Instant::now();
            }
            Recv::Timeout => {
                if last_write.elapsed() >= MD_HEARTBEAT {
                    if write_md(&mut stream, MdFrame::Heartbeat).is_err() {
                        break;
                    }
                    last_write = Instant::now();
                }
            }
            Recv::Closed => break,
        }
    }
    drop(guard);
    tracing::info!(?peer, %session, "vike-datahub md: stream ended");
}

/// Frame one [`MdFrame`] and write it — the per-SUBSCRIBER path (a heartbeat, a `Bye`, a
/// writer-synthesized `TapeGap`), as against the publisher's serialize-once fan-out.
fn write_md(stream: &mut TcpStream, frame: MdFrame) -> io::Result<()> {
    write_frame(stream, &Response::Md(Box::new(frame)))
}

/// Write pre-framed bytes and flush — `crates/vike-tradehub/src/server.rs`'s `write_all_flush`
/// shape. The bytes already carry their length prefix and already passed `MAX_FRAME_LEN`, because
/// the publisher framed them through the same `write_frame` every other frame goes through.
fn write_all_flush(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    stream.write_all(bytes)?;
    stream.flush()
}
