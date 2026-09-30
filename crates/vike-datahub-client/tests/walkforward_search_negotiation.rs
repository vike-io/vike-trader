//! The STUDIO walk-forward SEARCH capability's negotiation, driven end-to-end against a fake peer
//! — `search_method_negotiation.rs`'s sibling, deliberately its shape, and for the same reason.
//!
//! What is being prevented is not an unanswerable verb, it is a **silently different ANSWER**.
//! `Request` has no `deny_unknown_fields`, so a daemon predating `WireWalkforward::search` decodes
//! the frame, drops the field, runs the FIXED walk and replies with a perfectly well-formed
//! `WalkforwardResult`. Nothing downstream can tell an optimized walk from a fixed one by looking
//! at one report — and the fixed walk is not a coarser answer to the question asked, it is an
//! answer to a DIFFERENT question (were these parameters stable out of sample, rather than does
//! fit-then-trade survive out of sample). So the assertion that carries the weight is not "an error
//! came back" — it is that **NOTHING WAS SENT**.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for the same reason its siblings are: a
//! proto-only PR does not fire the heavier jobs, and this must run every time `-p
//! vike-datahub-client` does. Loopback only, no store, no server crate.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
    DatahubClient, FEATURE_WALKFORWARD_SEARCH, NO_WINDOW_SEARCH, PROTO_VERSION, Request, Response,
    WireParamscan, WireSlice, WireSliceKind, WireSpec, WireWalkforward, WireWindowSearch,
    read_frame, write_frame,
};

/// Answer ONE `Hello` with `features`, then count every frame the client sends afterwards and
/// report the total when the connection closes.
///
/// Its own copy rather than a shared helper, exactly as `search_method_negotiation.rs` and
/// `coverage_negotiation.rs` each keep theirs: proving "nothing was sent" needs a server that
/// COUNTS, and a real one would answer the frame either way.
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
        // It COUNTS and then ANSWERS: these tests drive the SENT direction too, and a fake that
        // stayed silent would park the client in `read_frame` until nextest's timeout killed it.
        // `Response::Error` is what a peer that cannot serve the verb would send anyway.
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

/// The features a compute daemon with the Studio runners mounted advertised BEFORE this capability
/// existed — the real strings `vike_backtest::compute_server`'s `served_features` pushed then.
/// Note it DOES advertise `run_walkforward`: this peer serves the verb, it just cannot search.
fn an_old_compute_daemon_with_studio() -> Vec<String> {
    [
        "backtest",
        "list_strategies",
        "run_sweep_profile",
        "run_walkforward_profile",
        "search_method",
        "run_slice",
        "run_sweep",
        "run_walkforward",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn a_slice() -> WireSlice {
    WireSlice {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: WireSliceKind::Bars,
    }
}

fn a_spec() -> WireSpec {
    WireSpec::Rhai("fn on_bar() {}".to_string())
}

fn a_sweep_search() -> WireWindowSearch {
    WireWindowSearch {
        method: "sweep".to_string(),
        grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
        rank_by: None,
    }
}

/// **The one that matters.** A daemon that would DROP the search must never be sent one, because
/// the report it answers with looks exactly like a successful optimized walk.
#[test]
fn a_search_an_old_daemon_would_drop_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon_with_studio());
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");

    let err = client
        .run_walkforward(a_spec(), a_slice(), WireWalkforward::searching(4, a_sweep_search()), None)
        .expect_err("a search an old daemon cannot honour must be refused client-side");

    assert!(err.contains(FEATURE_WALKFORWARD_SEARCH), "the refusal names the capability: {err}");
    assert!(
        err.contains("nothing was sent"),
        "the refusal says it did not reach the server, so a reader knows NO walk ran: {err}"
    );
    drop(client); // EOF ends the fake server's counting loop

    assert_eq!(
        rx.recv().expect("count"),
        0,
        "the client sent NOTHING after the refused negotiation"
    );
    handle.join().expect("fake server thread joins cleanly");
}

/// A knob written UNDER the no-search control still needs the capability, and for the reason the
/// predicate's own doc gives: the refusal it deserves ("a grid is a sweep knob") is one an old
/// daemon cannot produce — it drops the whole field and reports success.
#[test]
fn a_grid_or_a_rank_written_under_none_still_needs_the_capability() {
    for (label, search) in [
        (
            "a grid under none",
            WireWindowSearch {
                method: NO_WINDOW_SEARCH.to_string(),
                grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0])] },
                rank_by: None,
            },
        ),
        (
            "a rank_by under none",
            WireWindowSearch {
                method: NO_WINDOW_SEARCH.to_string(),
                grid: WireParamscan::default(),
                rank_by: Some("equity".to_string()),
            },
        ),
    ] {
        let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon_with_studio());
        let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
        let err = client
            .run_walkforward(a_spec(), a_slice(), WireWalkforward::searching(4, search), None)
            .expect_err("a knob under the control still needs the capability");
        assert!(err.contains(FEATURE_WALKFORWARD_SEARCH), "{label}: {err}");
        drop(client);
        assert_eq!(rx.recv().expect("count"), 0, "{label} must send NOTHING");
        handle.join().expect("join");
    }
}

/// ⚠ **THE OTHER DIRECTION, and the one a capability check gets wrong most easily.** A FIXED walk —
/// no selector, or an explicit `none` carrying nothing — must still work against a daemon that
/// advertises nothing, because that is every deployment in the field today. The fake never runs the
/// walk, which is fine: what is asserted is that the frame WAS SENT.
#[test]
fn a_fixed_walk_is_still_sent_to_a_daemon_without_the_capability() {
    for (label, wf) in [
        ("no selector at all", WireWalkforward::fixed(4)),
        (
            "an explicit none",
            WireWalkforward::searching(
                4,
                WireWindowSearch {
                    method: NO_WINDOW_SEARCH.to_string(),
                    grid: WireParamscan::default(),
                    rank_by: None,
                },
            ),
        ),
    ] {
        let (addr, rx, handle) = spawn_counting_fake(an_old_compute_daemon_with_studio());
        let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
        // The fake answers `Response::Error`, so the call fails on the ANSWER — which is what we
        // want: reaching an answer at all proves the write happened.
        let _ = client.run_walkforward(a_spec(), a_slice(), wf, None);
        drop(client);
        assert_eq!(rx.recv().expect("count"), 1, "{label} must be SENT, not refused");
        handle.join().expect("join");
    }
}

/// …and a daemon that DOES advertise it takes the selector. The capability check is the only thing
/// this test exercises — the fake runs no walk — so it fails on the fake's error, after the write.
#[test]
fn a_daemon_that_advertises_the_capability_receives_the_search() {
    let mut features = an_old_compute_daemon_with_studio();
    features.push(FEATURE_WALKFORWARD_SEARCH.to_string());
    let (addr, rx, handle) = spawn_counting_fake(features);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");

    let _ = client.run_walkforward(
        a_spec(),
        a_slice(),
        WireWalkforward::searching(4, a_sweep_search()),
        None,
    );
    drop(client);

    assert_eq!(
        rx.recv().expect("count"),
        1,
        "an advertised capability must be used, not re-checked"
    );
    handle.join().expect("join");
}

/// The FIXED walk's frame is byte-identical to the one this verb carried before the field existed —
/// which is what `skip_serializing_if` buys and why no `PROTO_VERSION` bump was needed.
#[test]
fn a_fixed_walk_serializes_without_the_search_key_at_all() {
    let json = serde_json::to_string(&WireWalkforward::fixed(4)).expect("serialize");
    assert!(!json.contains("search"), "an absent selector must not appear on the wire: {json}");
    // …and an OLD frame (no key) decodes for a NEW client as the fixed walk.
    let back: WireWalkforward = serde_json::from_str(r#"{"n_splits":4}"#).expect("decode");
    assert_eq!(back, WireWalkforward::fixed(4));
}
