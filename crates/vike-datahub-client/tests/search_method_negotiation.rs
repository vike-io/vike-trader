//! The SEARCH-METHOD capability's negotiation, driven end-to-end against a fake peer — the sibling
//! of `coverage_negotiation.rs` and `backfill_negotiation.rs`, and deliberately their shape.
//!
//! What makes this one different from every sibling: the thing being prevented is not an
//! unanswerable verb, it is a SILENTLY DIFFERENT ANSWER. `Request` has no `deny_unknown_fields`, so
//! a daemon predating the `search` field decodes the frame, drops the field, runs the exhaustive
//! grid and replies with a perfectly well-formed `ParamscanReport`. Nothing downstream can tell a
//! Bayesian search from a grid by looking at one. So the assertion that carries the weight is not
//! "an error came back" — it is that NOTHING WAS SENT.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for the same reason its siblings are: a
//! proto-only PR does not fire the heavier jobs, and this must run every time `-p
//! vike-datahub-client` does. Loopback only, no store, no server crate.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
    DatahubClient, FEATURE_SEARCH_METHOD, PROTO_VERSION, Request, Response, WireSearch, read_frame,
    write_frame,
};

/// Answer ONE `Hello` with `features`, then count every frame the client sends afterwards and
/// report the total when the connection closes.
///
/// Hand-rolled rather than driven through a real `serve`, and its own copy rather than a shared
/// helper, exactly as `coverage_negotiation.rs` keeps its own: proving "nothing was sent" needs a
/// server that COUNTS, and a real one would answer the frame either way.
fn spawn_counting_fake(
    features: Vec<String>,
) -> (SocketAddr, mpsc::Receiver<usize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<usize>();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            Ok(other) => panic!("fake server expected Hello, got {other:?}"),
            Err(e) => panic!("fake server failed to read Hello: {e}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake server writes Welcome");
        // ⚠ It COUNTS and then ANSWERS, where `coverage_negotiation.rs`'s twin only counts. That
        // sibling never drives the SENT direction, so its client always refuses before a write and
        // never blocks on a read; these tests do drive it, and a fake that stayed silent would
        // park the client in `read_frame` until nextest's timeout killed it. The answer is a
        // `Response::Error`, which is what a peer that cannot serve the verb would send anyway.
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
            if write_frame(&mut stream, &Response::Error("fake peer".to_string())).is_err() {
                break;
            }
        }
        tx.send(frames_after_hello).expect("report the count");
    });
    (addr, rx, handle)
}

/// The features a daemon predating this capability advertises — the real strings
/// `vike_backtest::compute_server`'s `served_features` pushed before it grew one.
fn an_old_compute_daemon() -> Vec<String> {
    ["backtest", "list_strategies", "run_sweep_profile", "run_walkforward_profile"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn a_method_an_old_daemon_would_drop_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");

    let search = WireSearch { optimizer: Some("tpe".to_string()), ..WireSearch::default() };
    let err = client
        .run_paramscan_profile("[paramscan]\nfast = [5, 10]\n", None, Some(&search))
        .expect_err("a method an old daemon cannot honour must be refused client-side");

    assert!(err.contains(FEATURE_SEARCH_METHOD), "the refusal names the capability: {err}");
    assert!(
        err.contains("nothing was sent"),
        "the refusal says it did not reach the server, so a reader knows the GRID did not run: \
         {err}"
    );
    drop(client); // EOF ends the fake server's counting loop

    assert_eq!(
        rx.recv().expect("count"),
        0,
        "the client sent NOTHING after the refused negotiation"
    );
    handle.join().expect("fake server thread joins cleanly");
}

/// `--rank-by multi` needs the SAME capability and for a different reason: an old daemon resolves
/// `rank_by` through `RankMetric::from_str_ci`, whose four arms have no `multi`, so it answers a
/// server-side error naming a set this client advertises five of. That is not a downgrade, but it
/// is a worse message than "your daemon is older", and one refusal for one capability beats two.
#[test]
fn rank_by_multi_is_refused_against_a_daemon_without_the_capability() {
    let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");

    let err = client
        .run_paramscan_profile("[paramscan]\nfast = [5, 10]\n", Some("multi"), None)
        .expect_err("multi must be refused against a daemon that cannot rank by it");
    assert!(err.contains(FEATURE_SEARCH_METHOD), "{err}");
    drop(client);

    assert_eq!(rx.recv().expect("count"), 0, "nothing was sent");
    handle.join().expect("join");
}

/// ⚠ THE OTHER DIRECTION, and the one a capability check gets wrong most easily. An ORDINARY grid
/// search — no selector, or an explicit `grid` — must still work against a daemon that advertises
/// nothing, because that is every deployment in the field today. The fake never answers the
/// request, which is fine: what is asserted is that the frame WAS SENT.
#[test]
fn an_ordinary_grid_search_is_still_sent_to_a_daemon_without_the_capability() {
    for (label, search) in [
        ("no selector at all", None),
        (
            "an explicit grid",
            Some(WireSearch { optimizer: Some("grid".to_string()), ..WireSearch::default() }),
        ),
    ] {
        let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon());
        let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
        // The fake answers with a `Response::Error`, so the call fails on the ANSWER — which is
        // what we want: reaching an answer at all proves the write happened.
        let _ =
            client.run_paramscan_profile("[paramscan]\nfast = [5, 10]\n", None, search.as_ref());
        drop(client);
        assert_eq!(rx.recv().expect("count"), 1, "{label} must be SENT, not refused");
        handle.join().expect("join");
    }
}

/// …and a daemon that DOES advertise it takes the selector. The capability check is the only thing
/// this test exercises — the fake runs no search — so it fails on the fake's error, after the write.
#[test]
fn a_daemon_that_advertises_the_capability_receives_the_selector() {
    let mut features = an_old_compute_daemon();
    features.push(FEATURE_SEARCH_METHOD.to_string());
    let (addr, rx, handle) = spawn_counting_fake(features);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");

    let search = WireSearch {
        optimizer: Some("genetic".to_string()),
        seed: Some("7".to_string()),
        ..WireSearch::default()
    };
    let _ = client.run_paramscan_profile("[paramscan]\nfast = [5, 10]\n", None, Some(&search));
    drop(client);

    assert_eq!(
        rx.recv().expect("count"),
        1,
        "an advertised capability must be used, not re-checked"
    );
    handle.join().expect("join");
}
