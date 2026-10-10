//! What one request does to the connection (`Step`), and the stop probe it runs under.

use std::cell::Cell;
use std::io;
use std::net::{SocketAddr, TcpStream};

use vike_datahub_client::market::MdSpec;
use vike_datahub_client::proto::{Plane, Response, wrong_plane_message};

use crate::md::SessionGuard;

/// The refusal a COMPUTE verb gets here — one call per moved verb, so the seven arms in
/// [`handle_request`] stay explicit while the TEXT has one spelling
/// (`vike_datahub_client::proto`'s `wrong_plane_message`, which the compute daemon's mirror-image
/// refusal also calls).
///
/// ⚠ It takes the verb name as a `&'static str` rather than the `Request` because the arms that
/// call it have already consumed the request by pattern-matching it, and re-deriving the name
/// through `request_kind` would mean either matching twice or borrowing what was moved. The names
/// cannot drift: an arm naming the wrong verb would be a wrong string in a `Response::Error` on a
/// path `crates/vike-datahub/tests/plane_split.rs` drives verb by verb.
pub(super) fn compute_verb_moved(verb: &'static str) -> Response {
    Response::Error(wrong_plane_message(verb, Plane::Data, Plane::Compute))
}

/// What one decoded request does to the CONNECTION, not just what it answers.
///
/// Exists because [`Request::MdSubscribe`] is this wire's one MODE SWITCH and a plain `Response`
/// cannot express it: the reply must be written and then the socket handed to a writer that never
/// reads again. §8 item 10 calls this "ONE arm before `handle_request`", and it is not — the whole
/// `match` has to yield a `ControlFlow`-shaped value or the unconditional write at the bottom of the
/// loop would write a second positional frame onto a push stream.
pub(super) enum Step {
    /// Write this and keep reading.
    ///
    /// ⚠ **BOXED, and the reason is a property of the wire rather than of this loop.**
    /// [`Response`] carries the STUDIO answers, and those grew a cost-model stamp
    /// (`vike_datahub_client::wire_studio`'s `WireCostModel` — ruling 0063), which pushed this
    /// enum's largest variant far past its second. `Step` is built once per decoded request and
    /// `ModeSwitch` is the common-sized arm, so an unboxed `Response` makes every mode switch pay
    /// the reply arm's width. The indirection is the same move
    /// `Request::RunWalkforward`'s own `slice` field already makes for the same reason, and it is
    /// invisible outside this file: `Step` is a control-flow value, never serialized, so nothing
    /// about the frame changes.
    ///
    /// [`Step::reply`] is the constructor — prefer it to spelling the `Box` at each site.
    Reply(Box<Response>),
    /// Hand the socket to `run_market_writer` and RETURN.
    ///
    /// ⚠ **The [`SessionGuard`] is TAKEN HERE, before the switch, and that placement is the whole
    /// point of it.** `run_market_writer` used to call `hub.open_session()` itself and, on the
    /// [`crate::md::MD_MAX_STREAM_CONNS`] refusal, write a `Response::Error` and then DROP the
    /// stream — closing a connection whose refusal every doc in this change describes as leaving it
    /// POSITIONAL (`vike_datahub_client::market`'s module doc: *"A server that answers
    /// `Response::Error` … has NOT switched"*; `DatahubClient::md_subscribe` promises the caller a
    /// client "returned intact"). Opening the session in `dispatch` makes that refusal a
    /// [`Step::Reply`] like the hub-less one, on the loop that is still reading.
    ///
    /// It cannot be a check followed by an open — two connections would both pass a check — so the
    /// RESERVATION itself moves, and the guard rides the switch. If it is dropped without being
    /// used, its own `Drop` frees the slot.
    ModeSwitch(SessionGuard, Vec<MdSpec>),
    /// Close the connection WITHOUT writing anything — the request ran for a client that has gone.
    ///
    /// Produced by [`dispatch`] exactly when the request's [`StopProbe`] saw the peer gone, which
    /// only `backfill_verb` and the archive import (`crate::import::import_archive_verb`, between
    /// days) ever ask — so no other verb can reach this arm.
    /// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §3 is the
    /// argument: a reply would be read by nobody, and writing it would only put a misleading
    /// "write fault" into the log. The thread then ends and its [`ConnSlot`] is released, as on any
    /// close.
    Close,
}

impl Step {
    /// [`Step::Reply`] with the boxing done once rather than at each of its construction sites —
    /// see that variant's doc for why it is boxed at all.
    pub(super) fn reply(response: Response) -> Step {
        Step::Reply(Box::new(response))
    }
}

/// **The stop probe one request runs under** — whether the client that sent it is still there,
/// asked without reading anything the protocol needs. The connection's half of
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §2.
///
/// # Why the connection has to be ASKED
///
/// `handle_connection` runs one request at a time on the connection's own thread, and
/// `backfill_verb` runs its collector inline, so while a long backfill runs nothing reads this
/// socket: a client that went away left a FIN unread in the kernel, and the daemon fetched on for
/// nobody — the incident the design was written from (four OANDA pairs over 21 years, stopped only
/// by a restart that took the recorder down with it). The market-data writer learns a peer is gone
/// from a failed heartbeat write; this wire is strictly request/response and the verb writes nothing
/// until it answers, so it cannot learn it that way. A heartbeat or progress frame is refused by the
/// design: an old client would read an unsolicited frame as its reply.
///
/// # What one ask does — a NON-BLOCKING PEEK, and nothing else
///
/// [`StopProbe::should_stop`] puts the socket into non-blocking mode, `peek`s one byte, and puts it
/// back (`peek_peer`). The verdicts, per std's `TcpStream::peek` and the platform's `recv(MSG_PEEK)`:
///
/// - `Ok(0)` — the peer sent FIN: **gone**.
/// - `WouldBlock` — nothing to read: **present**.
/// - `Ok(n > 0)` — the client sent bytes, a PIPELINED frame: **present**. `peek` leaves them in the
///   kernel, so the loop reads them as the next request once this one is answered.
/// - `Interrupted` — asked again. Any other error (a reset, an abort) — **gone**.
///
/// It is asked BETWEEN chunks of a chunked collector and never inside one, so it costs three
/// syscalls beside a chunk that costs at least a venue round trip. The archive import
/// (`crate::import::import_archive_verb`) asks the SAME probe between DAYS, which is its unit —
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §8 named that verb
/// as one that should take it when it was built. A watcher thread (one more thread
/// per backfill, which must be stopped and would hold the socket with the loop) and TCP keepalive (a
/// new dependency, and blind on the deployed shape, where the peer is sshd on loopback) were weighed
/// and refused for v1 — the design's §2 table.
///
/// # What it LATCHES, and what it cannot see
///
/// - **Latched**: once a peek has seen the peer gone, every later ask answers `true` without
///   touching the socket — a gone peer does not come back.
/// - ⚠ **A half-close cancels.** A client that shuts down its WRITE side and still waits for the
///   answer reads as gone, because its FIN is all a peek can see. No vike client does that; one that
///   did would get no reply.
/// - ⚠ **A half-OPEN peer is invisible** — power off, a cable pulled — because it sends nothing.
///   Through the deployed `ssh -L` tunnel the datahub's peer is sshd on loopback, so what closes the
///   socket for a vanished laptop is sshd's own keepalive, and the peek then sees that FIN.
/// - ⚠ **If blocking mode cannot be restored**, `mode_lost` is set and the connection CLOSES after
///   the request's reply rather than serving on a socket in an unknown mode.
///
/// # Platforms
///
/// `set_nonblocking` and `peek` are std on every target, and so is the mapping above: unix `recv`
/// with `MSG_PEEK | O_NONBLOCK` answers `EAGAIN`/`EWOULDBLOCK`, Windows `recv(MSG_PEEK)` on a socket
/// put into non-blocking mode by `ioctlsocket(FIONBIO)` answers `WSAEWOULDBLOCK`, and std maps both
/// to `ErrorKind::WouldBlock`; a graceful close answers `0` on both. The receive TIMEOUT
/// (`SO_RCVTIMEO`) is a separate socket option that neither mode switch touches, so
/// [`IDLE_READ_TIMEOUT`] still holds once blocking mode is back. The tests in this file's `tests`
/// module run on Linux in CI; on Windows the same calls are compiled, and the behaviour above is the
/// platform's documented one rather than a run.
///
/// ⚠ The `Cell`s make it `!Sync`, which is right: it lives on the connection's thread and is handed
/// to the collector as a borrowed `&dyn Fn() -> bool` on that same thread.
pub(super) struct StopProbe<'s> {
    stream: &'s TcpStream,
    pub(super) peer: Option<SocketAddr>,
    /// LATCHED `true` once a peek has seen the peer gone.
    peer_gone: Cell<bool>,
    /// `true` once a peek could not put the socket back into blocking mode.
    mode_lost: Cell<bool>,
}

impl<'s> StopProbe<'s> {
    /// A probe over `stream` that has asked nothing yet — building one costs no syscall.
    pub(super) fn new(stream: &'s TcpStream, peer: Option<SocketAddr>) -> Self {
        StopProbe { stream, peer, peer_gone: Cell::new(false), mode_lost: Cell::new(false) }
    }

    /// `true` means "stop now": the client is gone. Peeks once per call until it has said `true`,
    /// and from then on answers `true` without touching the socket.
    pub(super) fn should_stop(&self) -> bool {
        if self.peer_gone.get() {
            return true;
        }
        let peek = peek_peer(self.stream);
        if !peek.blocking_restored {
            self.mode_lost.set(true);
        }
        if peek.gone {
            self.peer_gone.set(true);
        }
        peek.gone
    }

    /// Whether a peek has seen the peer gone — read WITHOUT peeking, which is what lets
    /// [`dispatch`] ask it after every request without touching the socket of one that never ran a
    /// backfill.
    pub(super) fn peer_gone(&self) -> bool {
        self.peer_gone.get()
    }

    /// Whether a peek left the socket in a mode it could not restore.
    pub(super) fn mode_lost(&self) -> bool {
        self.mode_lost.get()
    }
}

/// What one [`peek_peer`] saw.
struct Peek {
    /// The peer sent FIN, or the socket errored: nobody is there.
    gone: bool,
    /// Blocking mode is back on — `false` only when switching it back FAILED.
    blocking_restored: bool,
}

/// One non-blocking peek at `stream` — `StopProbe`'s whole I/O, and its doc carries the verdicts.
///
/// If non-blocking mode cannot be SET, nothing was changed and nothing can be asked without
/// blocking, so the peer reads as present: a probe that cannot see is not evidence of absence.
fn peek_peer(stream: &TcpStream) -> Peek {
    if stream.set_nonblocking(true).is_err() {
        return Peek { gone: false, blocking_restored: true };
    }
    let mut byte = [0u8; 1];
    let gone = loop {
        match stream.peek(&mut byte) {
            Ok(0) => break true,
            Ok(_) => break false,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break false,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
        }
    };
    let blocking_restored = stream.set_nonblocking(false).is_ok();
    Peek { gone, blocking_restored }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use vike_data::HistStore;
    use vike_datahub_client::proto::{Request, write_frame};

    use crate::server::connection::handle_connection;
    use crate::server::limits::{HANDSHAKE_DEADLINE, ReadCeilings};

    /// The accepted socket carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`)
    /// — the market-data stream included, since it is this socket after the mode switch. Read off
    /// the socket itself rather than timed, so it holds on any OS: a clone of the served stream is
    /// the same socket, and once a `Pong` has come back `handle_connection` is past the line that
    /// arms it. (`tests/wire_latency.rs` pins what it is FOR, on Linux.)
    #[test]
    fn the_accepted_socket_has_nagle_off() {
        let (mut client, served) = socket_pair();
        let clone = served.try_clone().expect("clone the served socket");
        assert!(!clone.nodelay().expect("read TCP_NODELAY"), "guard: a fresh socket has Nagle on");
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(vike_data::MemHistStore::new());
        thread::spawn(move || {
            handle_connection(
                served,
                store,
                None,
                None,
                None,
                None,
                None,
                None,
                ReadCeilings::PRODUCTION,
                HANDSHAKE_DEADLINE,
            )
        });
        write_frame(&mut client, &Request::Ping).expect("ping");
        let answer = vike_node_proto::frame::read_frame::<_, Response>(&mut client).expect("pong");
        assert_matches!(answer, Response::Pong, "expected Pong, got {answer:?}");
        assert!(clone.nodelay().expect("read TCP_NODELAY"), "the accepted socket has Nagle on");
    }

    // ── T4: THE PEEK PROBE ON A REAL SOCKET ─────────────────────────────────────────────────────
    //
    // `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §6, T4: `false`
    // while the peer is connected and silent, `false` once it has sent bytes (which stay unread),
    // `true` once it has closed — and the socket BLOCKS again afterwards. Real loopback sockets, the
    // kind the server serves; this runs on Linux in CI, and `StopProbe`'s doc carries what that does
    // and does not say about Windows.

    use std::io::{Read, Write};

    /// A connected loopback pair: the client's end, and the end a server would serve.
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (served, _) = listener.accept().expect("accept");
        (client, served)
    }

    /// Wait — blocking, bounded — until `served` has something to report: bytes, or the FIN. A
    /// BLOCKING peek returns as soon as either has arrived, so the probe below is asked about a
    /// state the kernel already holds rather than one still in flight.
    fn wait_for_the_peer(served: &TcpStream) -> usize {
        served.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        let mut byte = [0u8; 1];
        served.peek(&mut byte).expect("the peer's bytes or FIN arrive on loopback")
    }

    /// **T4, the half K4 kills.** A connected, silent peer is PRESENT, and the probe leaves the
    /// socket BLOCKING: a read under a short timeout waits that timeout out. A probe that left the
    /// socket non-blocking would make the read answer `WouldBlock` at once — and the connection
    /// loop's next `read_frame_raw` would then close a live connection as "idle".
    #[test]
    fn a_silent_connected_peer_is_present_and_the_socket_blocks_again_afterwards() {
        let (_client, served) = socket_pair();
        let probe = StopProbe::new(&served, None);

        assert!(!probe.should_stop(), "a connected, silent peer must read as present");
        assert!(!probe.peer_gone(), "nothing latched");
        assert!(!probe.mode_lost(), "blocking mode was restored");

        let timeout = Duration::from_millis(400);
        served.set_read_timeout(Some(timeout)).expect("timeout");
        let started = Instant::now();
        let mut byte = [0u8; 1];
        let read = (&served).read(&mut byte);
        let waited = started.elapsed();
        assert_matches!(
            &read, Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
            "a silent peer's read must time out: {read:?}"
        );
        assert!(
            waited >= timeout - Duration::from_millis(100),
            "the read returned after {waited:?}, not after its {timeout:?} timeout: the probe left \
             the socket NON-blocking"
        );
    }

    /// **T4, the half K5 kills.** A peer that has SENT bytes — a pipelined frame — is present, and
    /// the bytes are still there afterwards: a peek consumes nothing the protocol needs.
    #[test]
    fn a_peer_that_sent_bytes_is_present_and_the_bytes_stay_unread() {
        let (mut client, served) = socket_pair();
        client.write_all(b"frame").expect("write");
        assert!(wait_for_the_peer(&served) > 0, "guard: the bytes arrived");
        let probe = StopProbe::new(&served, None);

        assert!(!probe.should_stop(), "pipelined bytes are a live client, never a disconnect");
        assert!(!probe.peer_gone());

        let mut got = [0u8; 5];
        (&served).read_exact(&mut got).expect("the bytes are still there");
        assert_eq!(&got, b"frame", "the probe consumed nothing");
    }

    /// **T4, the half K3 kills.** A peer that has CLOSED is gone — and so is one that only shut its
    /// WRITE side, which is the half-close the probe's doc says cancels — and the answer LATCHES.
    #[test]
    fn a_closed_peer_is_gone_and_a_half_close_reads_the_same() {
        for half_close in [false, true] {
            let (client, served) = socket_pair();
            let _kept = if half_close {
                client.shutdown(std::net::Shutdown::Write).expect("half-close");
                Some(client)
            } else {
                drop(client);
                None
            };
            assert_eq!(wait_for_the_peer(&served), 0, "guard: the FIN arrived");
            let probe = StopProbe::new(&served, None);

            assert!(probe.should_stop(), "half_close = {half_close}: a closed peer is gone");
            assert!(probe.peer_gone(), "half_close = {half_close}");
            assert!(probe.should_stop(), "half_close = {half_close}: and it stays gone");
            assert!(!probe.mode_lost(), "half_close = {half_close}");
        }
    }

    /// The LATCH, white-box: once the probe has said "stop" it never peeks again, so a peer that
    /// would read as present cannot un-say it. Planted on a live, silent connection — the one state
    /// in which a fresh peek would answer `false`.
    #[test]
    fn once_the_probe_has_said_stop_it_keeps_saying_it_without_peeking() {
        let (_client, served) = socket_pair();
        let probe = StopProbe::new(&served, None);
        assert!(!probe.should_stop(), "guard: a fresh peek of this peer answers present");

        probe.peer_gone.set(true);

        assert!(probe.should_stop(), "a latched probe must not be un-latched by a peek");
    }
}
