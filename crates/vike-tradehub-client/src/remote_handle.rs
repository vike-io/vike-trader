//! `RemoteCoreHandle` — the read-only, push-fed thin-client twin of `vike_core::CoreHandle`
//! (headless two-layer plan, Layer 2, PR-11).
//!
//! A laptop GUI connects this handle to a live headless node's observe server, authenticates under
//! [`Scope::Observe`], subscribes, and thereafter reads the latest pushed [`WireSnapshot`] off a
//! LOCAL arc-swap cell — exactly the lossy, never-blocking read the in-process GUI already does over
//! `CoreHandle::snapshot()`. A background receive thread decodes [`Response::SnapshotFrame`]s and
//! stores each into the cell (latest-wins); [`RemoteCoreHandle::snapshot`] loads it. Dropped frames
//! on a slow link are fine — the observer is a lossy downstream reader by contract (steal S6/S9).
//!
//! # RESOLVED DESIGN — `snapshot()` returns `Arc<WireSnapshot>`, NOT `Arc<CoreSnapshot>`
//!
//! Returning `Arc<vike_core::CoreSnapshot>` would force this LIGHT client crate to depend on
//! `vike-core` (the whole live-runtime tree: tokio, the execution engines), defeating the entire
//! reason PR-10 made this crate DataFusion-free and dependency-light. So the client stays light and
//! exposes the wire type [`WireSnapshot`] directly. The "interface-identical to `CoreHandle` for the
//! GUI panels" adapter — the shim that presents a `RemoteCoreHandle` as if it were a `CoreHandle` —
//! is part of the DEFERRED `vike-app --observe` follow-up (that crate already links `vike-core`, so
//! it can bridge the two snapshot shapes there), not this crate.
//!
//! # Liveness: a quiet stream and a dead one are no longer the same thing
//!
//! A subscribed observe stream is ONE-WAY — this handle sends nothing after its `Subscribe` — so
//! until the node grew a heartbeat there was no packet whose absence meant anything, and
//! [`RemoteCoreHandle::is_connected`] could only ever answer for a close the socket REPORTED. The
//! node now writes an idle `Response::Pong` every [`crate::liveness::OBSERVE_HEARTBEAT`] (advertised
//! as [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`]) and this handle deadlines its read at
//! [`crate::liveness::OBSERVE_READ_TIMEOUT`] against a node that advertises it — three missed beats
//! and the receive loop's EXISTING error arm ends the loop. See `connect_with_read_timeout` for why
//! the deadline is negotiated rather than always armed.
//!
//! # No control path here (PR-11 is read-only)
//!
//! This handle only observes. A remote `try_command`/`send_command` (the write half, pre-minting the
//! coid client-side) lands in PR-13 on top of PR-12's authenticated control channel.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::handshake::node_handshake;
use crate::liveness::OBSERVE_READ_TIMEOUT;
use crate::proto::{
    FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_STRATEGY_VERBS, Request, Response,
    Scope, Topic, read_frame, write_frame,
};
use crate::wire::{WireSettingsShow, WireSnapshot, WireStrategyStatus};

/// A connected, authenticated, read-only observer of a remote headless node.
///
/// Construct with [`RemoteCoreHandle::connect`]. It owns a background receive thread that keeps a
/// local arc-swap cell current from the node's pushed frames; [`snapshot`](Self::snapshot) reads that
/// cell without ever blocking or back-pressuring the node. Drop it to tear down the connection and
/// join the receive thread.
pub struct RemoteCoreHandle {
    /// The latest snapshot the node pushed (latest-wins). Read by [`Self::snapshot`], written by the
    /// receive thread — a lossy hand-off, the wire twin of the core's own publish cell.
    cell: Arc<ArcSwap<WireSnapshot>>,
    /// `false` once the receive thread has exited (EOF / transport fault / local shutdown).
    connected: Arc<AtomicBool>,
    /// A second handle to the same socket, kept ONLY so [`Drop`] can `shutdown` it and thereby unblock
    /// the receive thread's blocking `read_frame`, so the thread exits promptly.
    shutdown_stream: TcpStream,
    /// The receive thread's join handle, taken and joined in [`Drop`].
    recv: Option<JoinHandle<()>>,
    /// The datahub dial address the node's `Welcome.features` advertised at connect
    /// (`datahub=<addr>`, split-plane REQ-2), parsed once by
    /// [`crate::proto::advertised_datahub`]. `None` from a node with no
    /// `datahub_advertise_addr` configured (or one that predates the advertisement). Fixed for
    /// this connection's lifetime — the handshake happens once.
    advertised_datahub: Option<String>,
}

impl RemoteCoreHandle {
    /// Connect to a node's observe server at `addr`, run the full handshake under [`Scope::Observe`]
    /// using `observe_key` (the raw bytes of `VIKE_TRADEHUB_OBSERVE_KEY`), subscribe to every block,
    /// and spawn the receive thread.
    ///
    /// The handshake is the shared `crate::handshake` sequence run under [`Scope::Observe`]: send
    /// [`Request::Hello`], read the [`Response::Welcome`] challenge nonce (failing — naming both
    /// versions — on a protocol-version mismatch), sign the nonce, send [`Request::Auth`], and
    /// require [`Response::AuthOk`]. A [`Response::AuthDenied`] (wrong key, or the server refusing the
    /// scope) surfaces as [`io::ErrorKind::PermissionDenied`] carrying the server's reason. On success
    /// a [`Request::Subscribe`] for [`Topic::All`] opens the push stream.
    pub fn connect<A: ToSocketAddrs>(addr: A, observe_key: &[u8]) -> io::Result<Self> {
        Self::connect_with_read_timeout(addr, observe_key, OBSERVE_READ_TIMEOUT)
    }

    /// [`Self::connect`] with the silent-link deadline as a PARAMETER — the seam the unit tests
    /// below drive, so "a link that dies without a FIN flips `is_connected`" is proven in
    /// milliseconds rather than [`OBSERVE_READ_TIMEOUT`]'s 45 s. Crate-private and single-caller in
    /// production ([`Self::connect`], which passes the constant): the window is a protocol fact
    /// paired with the node's heartbeat cadence, not something a caller may choose.
    ///
    /// ⚠ The deadline is armed ONLY when the node advertises
    /// [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`], whatever is passed here. A node that does not
    /// heartbeat has no cadence for a deadline to be a multiple OF, so deadlining it would end a
    /// perfectly healthy stream to a quiet node every window and reconnect — a reconnect loop
    /// traded for a silent death. Against such a node this handle behaves exactly as it did before
    /// the heartbeat existed: a blocking read that ends only on a reported close.
    pub(crate) fn connect_with_read_timeout<A: ToSocketAddrs>(
        addr: A,
        observe_key: &[u8],
        read_timeout: Duration,
    ) -> io::Result<Self> {
        // 1-2. The shared node handshake (Hello -> Welcome version guard -> sign -> Auth -> AuthOk)
        //      under Observe — one sequence for every connection path (see `crate::handshake`).
        //      Every verb this handle SENDS (Subscribe) predates feature negotiation and every
        //      server speaks it; the features are kept for the two values they CARRY — the
        //      `datahub=<addr>` advertisement (split-plane REQ-2), parsed here once and exposed
        //      via [`Self::advertised_datahub`], and the heartbeat capability below.
        let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
        let advertised_datahub = crate::proto::advertised_datahub(&features);

        // 3. Subscribe to every block — the node pushes whole coalesced snapshots.
        write_frame(&mut stream, &Request::Subscribe { topics: vec![Topic::All] })?;

        // 3b. THE SILENT-DEATH DEADLINE. `is_connected` flips when the receive loop's read ERRORS,
        //     and until this existed the only thing that could make it error was a close the
        //     socket REPORTED (a FIN, an RST, the tunnel process exiting). A link that dies
        //     without a packet — a sleeping laptop, a Wi-Fi change, a VPN re-key under an `ssh -L`
        //     that keeps its forwarded socket open — reported nothing at all, so this handle
        //     stayed "connected" for hours and `vike-cli mcp`'s `node_snapshot` kept answering the
        //     pre-drop frame as live. A read deadline turns that silence into the error arm the
        //     loop already has; NOTHING new decides anything, the read merely stops blocking
        //     forever. Armed only against a node that heartbeats (see the doc above), so an
        //     ordinary idle stream is never mistaken for a dead one.
        let heartbeats = features.iter().any(|f| f == FEATURE_OBSERVE_HEARTBEAT);
        if heartbeats {
            stream.set_read_timeout(Some(read_timeout))?;
        }

        // 4. Spawn the receive thread over one clone; keep the other for a Drop-time shutdown.
        let cell = Arc::new(ArcSwap::from_pointee(WireSnapshot::empty()));
        let connected = Arc::new(AtomicBool::new(true));
        let shutdown_stream = stream.try_clone()?;

        let recv = {
            let cell = Arc::clone(&cell);
            let connected = Arc::clone(&connected);
            let mut read_stream = stream;
            std::thread::Builder::new()
                .name("vt-remote-recv".into())
                .spawn(move || {
                    // Blocking decode loop: each SnapshotFrame overwrites the cell (latest-wins).
                    // Any other reply is ignored — and the node's idle HEARTBEAT is exactly that,
                    // an unsolicited `Response::Pong` (`crate::proto::FEATURE_OBSERVE_HEARTBEAT`).
                    // It lands in this arm and is DROPPED, which is the point: a heartbeat proves
                    // the link, never a change. Its whole effect is that this read returned at all,
                    // so the deadline armed above does not fire.
                    // A read error / EOF / that deadline expiring / a Drop-triggered socket
                    // shutdown ends the loop.
                    loop {
                        match read_frame::<_, Response>(&mut read_stream) {
                            Ok(Response::SnapshotFrame(wire)) => cell.store(Arc::new(*wire)),
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    connected.store(false, Ordering::Release);
                })
                .expect("spawn vt-remote-recv thread")
        };

        Ok(RemoteCoreHandle {
            cell,
            connected,
            shutdown_stream,
            recv: Some(recv),
            advertised_datahub,
        })
    }

    /// The datahub dial address this node's `Welcome.features` advertised at connect
    /// (split-plane REQ-2: the backend fronts a `vike-datahub` and names where a client should
    /// dial it — advertisement, never proxying). `None` from a node with no
    /// `datahub_advertise_addr` configured or one that predates the advertisement. Captured once
    /// at the handshake, so it is only as live as this connection: a reconnect (a fresh
    /// `connect`) re-reads it from the next node's Welcome.
    pub fn advertised_datahub(&self) -> Option<&str> {
        self.advertised_datahub.as_deref()
    }

    /// The latest snapshot the node pushed — a lossy, never-blocking read off the local arc-swap
    /// cell, exactly like the in-process GUI's `CoreHandle::snapshot()`. Before the first frame
    /// arrives (or after a disconnect with no newer frame) this is [`WireSnapshot::empty`].
    pub fn snapshot(&self) -> Arc<WireSnapshot> {
        self.cell.load_full()
    }

    /// `true` while the receive thread is alive (the connection is delivering frames). Flips to
    /// `false` once the node closes the stream or a transport fault ends the receive loop — a
    /// supervisor probe for the GUI, mirroring `CoreHandle::is_alive`.
    ///
    /// ⚠ It used to answer ONLY for a close the socket REPORTED, and every caller's safety gate
    /// inherited that width: a link that died in silence left this `true` for hours. Against a node
    /// that advertises [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`] it now also flips when nothing
    /// at all arrives for [`OBSERVE_READ_TIMEOUT`] (three missed heartbeats), so a silent death is
    /// bounded by that window instead of NOT BEING BOUNDED AT ALL. (This sentence used to end "by
    /// the OS's two-hour keepalive"; `crate::liveness`'s module doc corrects that — nothing in this
    /// workspace enables `SO_KEEPALIVE`, so an undeadlined read on a silently-dead socket never
    /// returns.) Against a node that does NOT advertise it, the old width is exactly what this
    /// still has — say so when reporting on a mixed-version deployment.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

impl Drop for RemoteCoreHandle {
    fn drop(&mut self) {
        // Unblock the receive thread's blocking `read_frame` by shutting the shared socket, then join
        // it so no thread outlives the handle. A best-effort shutdown — the socket may already be
        // closed by the peer.
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(join) = self.recv.take() {
            let _ = join.join();
        }
    }
}

/// Ask a node WHAT IT IS RUNNING — the STRATEGY-level read verb (split-plane B4), the read-only
/// sibling of [`crate::remote_control::preview_command`]'s per-call shape: open a fresh short-lived
/// [`Scope::Observe`] connection (the same handshake [`RemoteCoreHandle::connect`] runs, minus the
/// `Subscribe`), send [`Request::StrategyStatus`], read the [`Response::StrategyStatus`] payload,
/// and drop the connection. A per-call connection is correct for the same reason it is for a
/// preview: a single request/response with no ordering concern.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: the request is sent ONLY when the server's
/// `Welcome.features` advertises [`FEATURE_STRATEGY_VERBS`]. Against an older node the verb does
/// not exist — its serde cannot decode the frame — so this fails with
/// [`io::ErrorKind::Unsupported`] naming the missing capability, and NOTHING is sent after the
/// handshake. A handshake failure (wrong/absent observe key, version skew, transport fault)
/// surfaces as its usual [`io::Error`]; a server-side [`Response::Error`] (e.g. a node publishing
/// no identity block) surfaces as [`io::ErrorKind::InvalidData`] carrying the server's text.
pub fn strategy_status<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireStrategyStatus> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
    if !features.iter().any(|f| f == FEATURE_STRATEGY_VERBS) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{FEATURE_STRATEGY_VERBS}\" capability \
                 (an older vike-tradehub) — StrategyStatus refused client-side, nothing was sent"
            ),
        ));
    }
    write_frame(&mut stream, &Request::StrategyStatus)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::StrategyStatus(status) => Ok(*status),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub strategy status error: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tradehub strategy status: expected StrategyStatus, got {}",
                crate::handshake::resp_kind(&other)
            ),
        )),
    }
}

/// Ask a node for its EFFECTIVE SETTINGS — the REQ-7 read verb, [`strategy_status`]'s exact
/// per-call shape: open a fresh short-lived [`Scope::Observe`] connection (the same handshake
/// [`RemoteCoreHandle::connect`] runs, minus the `Subscribe`), send [`Request::SettingsShow`],
/// read the [`Response::SettingsShow`] payload, and drop the connection. A per-call connection is
/// correct for the same reason it is there: a single request/response with no ordering concern.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: the request is sent ONLY when the server's
/// `Welcome.features` advertises [`FEATURE_SETTINGS_SHOW`]. Against an older node the verb does
/// not exist — its serde cannot decode the frame — so this fails with
/// [`io::ErrorKind::Unsupported`] naming the missing capability, and NOTHING is sent after the
/// handshake (the GUI renders that as "server predates settings-show"). A handshake failure
/// surfaces as its usual [`io::Error`]; a server-side [`Response::Error`] (a node started without
/// a settings source, or settings that no longer load) surfaces as
/// [`io::ErrorKind::InvalidData`] carrying the server's text.
pub fn settings_show<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireSettingsShow> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
    if !features.iter().any(|f| f == FEATURE_SETTINGS_SHOW) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{FEATURE_SETTINGS_SHOW}\" capability \
                 (an older vike-tradehub) — SettingsShow refused client-side, nothing was sent"
            ),
        ));
    }
    write_frame(&mut stream, &Request::SettingsShow)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SettingsShow(show) => Ok(*show),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub settings show error: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tradehub settings show: expected SettingsShow, got {}",
                crate::handshake::resp_kind(&other)
            ),
        )),
    }
}

/// The SILENT-DEATH property of the observe stream, against a scripted node.
///
/// A relay that CUTS a link (the `vike-cli` MCP drop harness) proves the reported-close path, which
/// this handle has always handled. What no relay can plant is the failure these tests are for: a
/// link that stops delivering and never says so — no FIN, no RST, nothing — which is what a
/// sleeping laptop, a Wi-Fi change or a VPN re-key looks like through an `ssh -L` whose forwarded
/// socket stays open locally. The scripted node here does exactly that: it completes the real
/// handshake, pushes one real frame, and then holds the socket open and SILENT forever.
///
/// Both directions are pinned, because the fix is a NEGOTIATION and half of it is the compatibility
/// half: a node that advertises the heartbeat gets a deadlined read (the silence is caught), a node
/// that does not gets the blocking read it always had (the silence is not caught, and must not be —
/// deadlining a heartbeat-less node would tear down every healthy idle stream).
#[cfg(test)]
mod silent_link_tests {
    use super::*;
    use std::io::Write as _;
    use std::net::{SocketAddr, TcpListener};
    use std::thread;
    use std::time::Instant;

    use crate::auth;
    use crate::proto::{NODE_PROTO_VERSION, write_frame};
    use crate::wire::WireSnapshot;

    const KEY: &[u8] = b"observe-key-for-the-silent-link-tests";
    /// The scripted deadline every test here drives through the seam. Production is
    /// [`OBSERVE_READ_TIMEOUT`] (45 s = three heartbeats); the property is the same at any scale.
    const TEST_READ_TIMEOUT: Duration = Duration::from_millis(300);

    /// A one-connection node that speaks the REAL handshake (so the client's own auth path runs
    /// unmodified), answers `Subscribe` with ONE real snapshot frame, and then goes silent while
    /// HOLDING the socket open — the whole point: dropping it would send a FIN, which is the
    /// reported close this handle has always detected. Returns the address; the thread parks until
    /// `hold`, long past every assertion.
    fn scripted_silent_node(features: Vec<String>, hold: Duration) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let nonce = [11u8; 32];
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Hello { .. }) => {}
                other => panic!("expected Hello, got {other:?}"),
            }
            write_frame(
                &mut stream,
                &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
            )
            .expect("welcome");
            let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
                return;
            };
            assert!(
                auth::verify(KEY, &nonce, NODE_PROTO_VERSION, scope, &mac),
                "the client must present a valid observe mac"
            );
            write_frame(&mut stream, &Response::AuthOk { scope }).expect("authok");
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Subscribe { .. }) => {}
                other => panic!("expected Subscribe, got {other:?}"),
            }
            let mut snap = WireSnapshot::empty();
            snap.seq = 7;
            write_frame(&mut stream, &Response::SnapshotFrame(Box::new(snap))).expect("one frame");
            stream.flush().expect("flush the one frame");
            // …and now NOTHING, with the socket still open. No FIN, no RST, no data.
            thread::sleep(hold);
        });
        addr
    }

    /// How much EARLIER than its own armed deadline a socket read is allowed to return before the
    /// lower bound below calls it a violation.
    ///
    /// It is not slack for scheduling — it is slack for the socket's own timer, which is coarser
    /// than the clock the test measures with. `set_read_timeout` is `SO_RCVTIMEO`, quantised to the
    /// platform's tick (≈15.6 ms on Windows), while [`Instant`] is the high-resolution counter;
    /// MEASURED on the dev box with a bare `TcpStream` armed at 300 ms and nothing to read, six
    /// consecutive reads returned at 291.1, 302.1, 301.4, 307.5, 301.4 and 300.6 ms — the first one
    /// 8.9 ms SHORT. So `elapsed >= TEST_READ_TIMEOUT` is not a sound assertion on this platform for
    /// ANY anchor, and this test failed roughly one run in three until it had this.
    ///
    /// One order of magnitude over the observed shortfall, and still a sixth of
    /// [`TEST_READ_TIMEOUT`] — comfortably tight enough that the thing the bound exists to exclude,
    /// a flag that flips instantly because no deadline was waited out at all, still fails it.
    const SOCKET_DEADLINE_SLACK: Duration = Duration::from_millis(50);

    fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// **THE DEFECT.** A link that dies in SILENCE — nothing on the wire in either direction, the
    /// socket still open — must flip `is_connected` within the deadline. Before the read deadline
    /// existed this handle reported "connected" for the life of the process (no FIN, no RST, and no
    /// keepalive to fall back on — `crate::liveness`'s module doc corrects the "two hours" this
    /// comment used to claim), and `vike-cli mcp`'s `node_snapshot` gate (which reads exactly this
    /// bit) served the pre-drop frame as live for that whole time.
    ///
    /// ⚠ KILL PROOF: remove the `set_read_timeout` in `connect_with_read_timeout` (or make the
    /// heartbeat-advertised branch unconditional-false) and this must fail on the 5 s assertion —
    /// the receive loop's read never returns, so nothing ever flips the flag. Run both ways; the
    /// commit message carries the outcomes.
    ///
    /// ⚠ THE LOWER BOUND HAD TWO INDEPENDENT UNSOUNDNESSES, and it failed about one run in three
    /// (4 in 12, measured on an idle dev box) until both were fixed. Neither was load.
    ///
    /// **1. The anchor was later than the clock it was compared against.** `started` used to be
    /// taken AFTER the `wait_until(… seq == 7)` frame wait. That wait polls every 10 ms, so it
    /// returns up to a full tick after the receive thread already stored the frame and re-entered
    /// its DEADLINED read: the socket's deadline had started, `started` had not, and the bound was
    /// measuring the window from strictly the wrong end. It is taken at the CONNECT now — the final
    /// read's deadline provably cannot have begun before the connection existed, so it is an anchor
    /// this test can see that is never later than the one the socket uses.
    ///
    /// **2. The socket's own timer is coarser than [`Instant`], and undershoots.** Fixing the
    /// anchor alone left it failing, at 291.3 ms and 292.2 ms against a 300 ms window — impossible
    /// unless the READ returned early, which it does: see [`SOCKET_DEADLINE_SLACK`] for the
    /// standalone measurement. Hence the bound is stated with that slack rather than exactly.
    ///
    /// What the bound still buys with both fixes in: the flag may not flip WITHOUT the window
    /// having been waited out. That is the half a bare 5 s upper bound cannot express — a handle
    /// that reported dead the moment it connected would satisfy "detected within 5 s" perfectly.
    #[test]
    fn a_silently_dead_observe_link_flips_is_connected() {
        let addr = scripted_silent_node(
            vec!["observe".to_string(), FEATURE_OBSERVE_HEARTBEAT.to_string()],
            Duration::from_secs(30),
        );
        let handle = RemoteCoreHandle::connect_with_read_timeout(addr, KEY, TEST_READ_TIMEOUT)
            .expect("the scripted node completes the real handshake");
        // The only anchor this test can observe that is provably NOT LATER than the socket's own
        // (see the doc above). Every read this connection makes happens after this line.
        let started = Instant::now();
        assert!(
            wait_until(Duration::from_secs(2), || handle.snapshot().seq == 7),
            "the one real frame must arrive before the silence begins"
        );
        assert!(handle.is_connected(), "…and the link is live while frames are arriving");

        // Nothing further is ever sent, and the socket is never closed.
        assert!(
            wait_until(Duration::from_secs(5), || !handle.is_connected()),
            "a silently dead link must be detected within the read deadline, not never"
        );
        let took = started.elapsed();
        assert!(
            took + SOCKET_DEADLINE_SLACK >= TEST_READ_TIMEOUT,
            "…and not BEFORE it: a link is not dead until the window has actually been waited out \
             ({took:?} + {SOCKET_DEADLINE_SLACK:?} slack < {TEST_READ_TIMEOUT:?}). The anchor is \
             the CONNECT, which is never later than the socket's own, and the slack is for a \
             SO_RCVTIMEO tick that is coarser than Instant — see both docs above"
        );
        assert_eq!(
            handle.snapshot().seq,
            7,
            "the last frame is still readable — the CALLER's gate decides what to do with it, this only stops claiming the link is live"
        );
    }

    /// The COMPATIBILITY half. A node that does not advertise the heartbeat is not deadlined at
    /// all: its idle stream stays connected exactly as it did before this change. Without the
    /// negotiation, this configuration would drop and reconnect every window forever — a node with
    /// nothing to say is the ordinary state of an idle daemon, not a fault.
    #[test]
    fn a_node_that_does_not_heartbeat_is_never_deadlined() {
        let addr = scripted_silent_node(vec!["observe".to_string()], Duration::from_secs(5));
        let handle = RemoteCoreHandle::connect_with_read_timeout(addr, KEY, TEST_READ_TIMEOUT)
            .expect("the scripted node completes the real handshake");
        assert!(
            wait_until(Duration::from_secs(2), || handle.snapshot().seq == 7),
            "the one real frame must arrive"
        );
        thread::sleep(TEST_READ_TIMEOUT * 4);
        assert!(
            handle.is_connected(),
            "a heartbeat-less node's quiet stream must NOT be torn down — the deadline is armed \
             only against a node that promised to fill the silence"
        );
    }
}
