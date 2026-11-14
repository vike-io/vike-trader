//! **A reply is bounded by IDLENESS, per verb, and a timed-out client sends nothing more.**
//!
//! `crates/vike-datahub-client/src/client.rs`'s `exchange` arms each verb's idle read bound before
//! writing it. These tests drive that over loopback against a peer that completes the handshake and
//! then behaves badly, with bounds of a few hundred milliseconds set through
//! `DatahubClient::set_reply_timeout` so no test sleeps for seconds:
//!
//! 1. **A silent peer** (the half-open shape: handshake done, then nothing) fails the verb with
//!    `TimedOut` within the bound, naming the verb and the bound, and the client is then UNUSABLE:
//!    the next verb is refused locally with `NotConnected` and writes no byte.
//! 2. **A slow peer** that answers inside the bound is answered.
//! 3. **A long answer whose gaps are each shorter than the bound** succeeds although the whole
//!    answer takes longer than the bound: idle, not total.

use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use vike_datahub_client::DatahubClient;
use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};

/// Accept ONE client, complete a KEY-LESS handshake, and hand the accepted stream to `then`.
fn spawn_peer<T: Send + 'static>(
    then: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> (String, thread::JoinHandle<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr").to_string();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        stream.set_nodelay(true).expect("nodelay");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features: Vec::new(), nonce: None },
        )
        .expect("write Welcome");
        then(stream)
    });
    (addr, handle)
}

#[test]
fn a_silent_peer_times_out_within_the_bound_and_the_client_then_sends_nothing() {
    let bound = Duration::from_millis(200);
    let (got_first, first_seen) = mpsc::channel::<()>();
    let (client_done, wait_client) = mpsc::channel::<()>();
    let (addr, peer) = spawn_peer(move |mut stream| {
        let first: Request = read_frame(&mut stream).expect("the first Ping arrives");
        assert!(matches!(first, Request::Ping), "{first:?}");
        got_first.send(()).expect("report the first frame");
        // Say NOTHING — the half-open shape — until the client has tried a second verb.
        wait_client.recv().expect("the client finishes");
        stream.set_read_timeout(Some(Duration::from_millis(300))).expect("bound the probe read");
        // Whatever arrives now was written after the timeout: there must be nothing.
        read_frame::<_, Request>(&mut stream).map(|r| format!("{r:?}"))
    });

    let mut client = DatahubClient::connect(&addr).expect("handshake");
    client.set_reply_timeout(Some(bound));
    let t0 = Instant::now();
    let err = client.ping().expect_err("a silent peer cannot answer");
    let waited = t0.elapsed();
    first_seen.recv().expect("the Ping was written");

    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "one kind on every platform: {err}");
    let msg = err.to_string();
    assert!(msg.contains("Ping"), "names the verb: {msg}");
    assert!(msg.contains("200ms"), "names the bound: {msg}");
    assert!(waited >= bound, "fired early: {waited:?}");
    assert!(waited < Duration::from_secs(10), "it must not wait anywhere near forever: {waited:?}");
    assert!(!client.is_usable(), "a stream that timed out mid-exchange is out of step");

    let again = client.ping().expect_err("an unusable client sends nothing");
    assert_eq!(again.kind(), io::ErrorKind::NotConnected, "{again}");
    assert!(again.to_string().contains("not sent"), "{again}");
    assert!(again.to_string().contains("Ping failed mid-exchange"), "names the first: {again}");
    let listed = client.list_series().expect_err("every verb is refused, not only the one");
    assert!(listed.contains("ListSeries: not sent"), "{listed}");

    client_done.send(()).expect("release the peer");
    let late = peer.join().expect("the peer thread does not panic");
    assert!(late.is_err(), "no frame may follow the timed-out one, got {late:?}");
}

#[test]
fn a_slow_answer_inside_the_bound_is_answered() {
    let (addr, peer) = spawn_peer(|mut stream| {
        let _: Request = read_frame(&mut stream).expect("the Ping arrives");
        thread::sleep(Duration::from_millis(150));
        write_frame(&mut stream, &Response::Pong).expect("answer late, inside the bound");
        stream
    });
    let mut client = DatahubClient::connect(&addr).expect("handshake");
    client.set_reply_timeout(Some(Duration::from_secs(2)));
    client.ping().expect("an answer inside the bound is an answer");
    assert!(client.is_usable());
    let _stream = peer.join().expect("peer");
}

/// The answer arrives in ten pieces 150 ms apart: about 1.5 s in total against a 1 s bound, with
/// no single silence longer than 150 ms. A total deadline would clip it; an idle bound must not.
#[test]
fn a_long_answer_with_short_gaps_is_never_clipped() {
    let bound = Duration::from_secs(1);
    let gap = Duration::from_millis(150);
    let (addr, peer) = spawn_peer(move |mut stream| {
        let _: Request = read_frame(&mut stream).expect("the ListSeries arrives");
        let mut frame = Vec::new();
        write_frame(&mut frame, &Response::SeriesList(Vec::new())).expect("encode the answer");
        // At least ten pieces, so the gaps alone outlast the bound.
        let piece = (frame.len() / 10).max(1);
        for chunk in frame.chunks(piece) {
            thread::sleep(gap);
            stream.write_all(chunk).expect("write one piece");
            stream.flush().expect("flush");
        }
        stream
    });
    let mut client = DatahubClient::connect(&addr).expect("handshake");
    client.set_reply_timeout(Some(bound));
    let t0 = Instant::now();
    let listed = client.list_series().expect("a steadily arriving answer is not a dead link");
    assert!(listed.is_empty());
    assert!(
        t0.elapsed() > bound,
        "the whole answer took longer than the bound: {:?}",
        t0.elapsed()
    );
    assert!(client.is_usable());
    let _stream = peer.join().expect("peer");
}
