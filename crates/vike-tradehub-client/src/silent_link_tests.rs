use super::reads::directory_with_deadline;
use super::*;
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::Instant;

use crate::auth;
use crate::proto::{FEATURE_DIRECTORY, NODE_PROTO_VERSION, write_frame};
use crate::wire::WireSnapshot;

const KEY: &[u8] = b"observe-key-for-the-silent-link-tests";
/// The scripted deadline every test here drives through the seam. Production is
/// [`OBSERVE_READ_TIMEOUT`] (45 s = three heartbeats); the property is the same at any scale.
const TEST_READ_TIMEOUT: Duration = Duration::from_millis(300);

/// A one-connection node that speaks the REAL handshake, answers `Subscribe` with ONE snapshot
/// frame, then goes silent while HOLDING the socket open for `hold` (dropping it would send a FIN,
/// the reported close this handle always detected).
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

/// How much EARLIER than its armed deadline a socket read may return before the lower bound below
/// calls it a violation. Slack for the socket's timer, not scheduling: `SO_RCVTIMEO` is quantised
/// to the platform tick (≈15.6 ms on Windows) while [`Instant`] is high-resolution, and a bare
/// stream armed at 300 ms was MEASURED returning at 291.1 ms (8.9 ms short). One order of
/// magnitude over that and a sixth of [`TEST_READ_TIMEOUT`]: a flag that flips instantly, with no
/// deadline waited out, still fails.
const SOCKET_DEADLINE_SLACK: Duration = Duration::from_millis(50);

// Equal to `crates/vike-tradehub/src/server/tests/server_link_liveness.rs`'s `wait_until`; the
// `secs: u64` copies (`crates/vike-mount/tests/common/mod.rs`'s) take seconds, not a `Duration`.
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

/// **THE DEFECT.** A link that dies in SILENCE (socket still open, nothing on the wire) must flip
/// `is_connected` within the deadline; without it `vike-cli mcp`'s `node_snapshot` gate served the
/// pre-drop frame as live for the life of the process.
///
/// ⚠ KILL PROOF: remove the `set_read_timeout` in `connect_with_read_timeout` and this fails on
/// the 5 s assertion.
///
/// ⚠ The LOWER bound (the flag may not flip before the window was waited out, which a bare 5 s
/// upper bound cannot express) is anchored at the CONNECT, never later than the socket's own
/// deadline start (an anchor after the 10 ms-polled frame wait was late), and carries
/// [`SOCKET_DEADLINE_SLACK`]. Either flaw alone failed about one run in three.
#[test]
fn a_silently_dead_observe_link_flips_is_connected() {
    let addr = scripted_silent_node(
        vec!["observe".to_string(), FEATURE_OBSERVE_HEARTBEAT.to_string()],
        Duration::from_secs(30),
    );
    let handle = RemoteCoreHandle::connect_with_read_timeout(addr, KEY, TEST_READ_TIMEOUT)
        .expect("the scripted node completes the real handshake");
    // Provably NOT LATER than the socket's own deadline start (see the doc above).
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

/// The COMPATIBILITY half: a node that does not advertise the heartbeat is never deadlined, so
/// its idle stream stays connected instead of reconnecting every window.
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

/// A node that completes the REAL handshake advertising `features`, reads the one request a
/// per-call verb sends, then answers NOTHING while holding the socket open.
fn scripted_mute_node(features: Vec<String>, hold: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let nonce = [13u8; 32];
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
        )
        .expect("welcome");
        let Ok(Request::Auth { scope, .. }) = read_frame::<_, Request>(&mut stream) else {
            return;
        };
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("authok");
        let _ = read_frame::<_, Request>(&mut stream);
        // …and now NOTHING, with the socket still open.
        thread::sleep(hold);
    });
    addr
}

/// `directory`'s reply read has a DEADLINE: a node that accepts the handshake and never answers
/// must end the read, as an error, within it (it used to park the desktop's directory slot).
#[test]
fn a_node_that_never_answers_the_directory_ends_the_read_within_its_deadline() {
    let addr = scripted_mute_node(vec![FEATURE_DIRECTORY.to_string()], Duration::from_secs(30));
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(directory_with_deadline(addr, KEY, TEST_READ_TIMEOUT));
    });
    let got = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the directory read must end within its deadline, not wait on a mute node");
    assert!(got.is_err(), "a mute node's directory is an error, never a list: {got:?}");
}
