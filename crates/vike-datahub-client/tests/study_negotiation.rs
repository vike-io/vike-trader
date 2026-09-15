//! `run_study`'s negotiation, against a fake peer. The sibling of `coverage_negotiation.rs`.
//!
//! ⚠ What this proves that `crates/vike-cli/tests/study_report_refusal_cli.rs` cannot: that half
//! drives the shipped binary against a REAL data server, which advertises no `study` and never
//! will, so it can only ever exercise the REFUSAL. The direction that matters now is the other
//! one — a peer that DOES advertise the capability must be SENT the request — and it is the
//! direction that did not exist to test until the verb did.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
    DatahubClient, FEATURE_STUDY, PROTO_VERSION, Request, Response, WireStudy, read_frame,
    write_frame,
};

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

fn a_recipe() -> WireStudy {
    WireStudy {
        study: "cohort".to_string(),
        recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
        from: "2026-04-07T05".to_string(),
        to: "2026-08-05T05".to_string(),
    }
}

/// A daemon that does not mount a study runner is refused CLIENT-side, with nothing sent.
#[test]
fn a_daemon_without_the_capability_is_refused_without_sending() {
    let (addr, rx, handle) = spawn_counting_fake(
        ["backtest", "list_strategies"].iter().map(|s| s.to_string()).collect(),
    );
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
    let err = client.run_study(&a_recipe()).expect_err("no capability, no run");
    assert!(err.contains(FEATURE_STUDY), "the refusal names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    assert_eq!(rx.recv().expect("count"), 0, "nothing after the handshake");
    handle.join().expect("join");
}

/// …and a daemon that DOES advertise it is SENT the request. The fake answers a `Response::Error`,
/// so the call fails on the ANSWER — which is the point: reaching an answer proves the write.
#[test]
fn a_daemon_that_advertises_the_capability_receives_the_request() {
    let (addr, rx, handle) =
        spawn_counting_fake(vec!["backtest".to_string(), FEATURE_STUDY.to_string()]);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds");
    let _ = client.run_study(&a_recipe());
    drop(client);
    assert_eq!(rx.recv().expect("count"), 1, "an advertised capability must be USED");
    handle.join().expect("join");
}
