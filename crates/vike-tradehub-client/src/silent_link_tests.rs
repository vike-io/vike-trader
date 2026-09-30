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
