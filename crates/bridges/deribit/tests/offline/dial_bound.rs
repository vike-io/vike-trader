//! Both Deribit authed sockets CONNECT bounded all the way through the handshake (known-limitations
//! "deribit: the order WebSocket's TLS and WebSocket handshake has no read timeout", A49, and its
//! sibling for the fill stream), and an edge that ANSWERS the upgrade with a 502 is a prompt, named
//! connect failure (the testnet's 15-minute window of 2026-10-27, 13:11 to 13:26 UTC).
//!
//! `DeribitOrderTransport::connect` dials through `vike_bridge_core::ws_proxy::connect_ws` with the exec
//! lane's `dial_timeout`. That bound used to end where the TLS + WebSocket handshake began, so a peer
//! that ACCEPTED the TCP connection and then said nothing (a half-open edge) left `connect`, and the
//! exec thread or the recon / resync client that made the call, blocked with no error logged. The
//! shared dial now puts the same window on every read of the handshake
//! (`crates/vike-bridge-core/src/ws_proxy_tests.rs` pins the mechanism for `ws://` and `wss://`); this
//! pins the consumers: the transport hands its window down and reports the failure as a connect
//! failure, and the fill pump's `open_deribit_user_data_ws` (a bare `tungstenite::connect` until
//! 0.1.97, which also read its `stop` flag only after the dial returned) ends the open as a
//! reconnectable transport failure.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use vike_bridge_core::user_data::OpenOutcome;
use vike_deribit::ratelimit::ws_rate_gate;
use vike_deribit::rpc::JsonRpcBuilder;
use vike_deribit::transport::DeribitOrderTransport;
use vike_deribit::user_data::open_deribit_user_data_ws;

/// A silent peer, a 300 ms window, and a `connect` that must come back with an error naming it.
#[test]
fn a_peer_that_accepts_and_says_nothing_cannot_hold_the_order_socket_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("ws://{}", listener.local_addr().expect("local addr"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut transport = DeribitOrderTransport::new(&url, "client-id", "client-secret", None)
            .with_dial_timeout(Duration::from_millis(300));
        let _ = tx.send(transport.connect().map_err(|e| e.to_string()));
    });
    // accept and HOLD the connection, saying nothing, until the verdict is in
    let (_held, _) = listener.accept().expect("the transport dials the listener");

    let verdict = rx.recv_timeout(Duration::from_secs(10)).expect(
        "connect is still blocked 10 s after a 300 ms dial bound: the handshake read is unbounded",
    );
    let err =
        verdict.expect_err("a silent peer cannot complete the handshake, let alone public/auth");
    assert!(err.contains("order-WS connect failed"), "a connect failure, got: {err}");
    assert!(err.contains("no answer within"), "the silent handshake is named, got: {err}");
}

/// The fill pump's open, against the same silent peer, under the exec lane's own 10 s window (the pump
/// takes the row's value, with no override): it must come back as `OpenOutcome::Transport` (the pump
/// reconnects on it) naming the silent handshake, not hang its thread.
#[test]
fn a_peer_that_accepts_and_says_nothing_cannot_hold_the_fill_pump_open() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("ws://{}", listener.local_addr().expect("local addr"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = open_deribit_user_data_ws(
            &url,
            "client-id",
            "client-secret",
            &["user.trades.any.any.raw".to_string()],
            &JsonRpcBuilder::new(),
            &Mutex::new(String::new()),
            &AtomicBool::new(false),
            &ws_rate_gate(),
        );
        let _ = tx.send(match outcome {
            OpenOutcome::Transport(why) => Ok(why),
            OpenOutcome::Ready(_) => Err("Ready"),
            OpenOutcome::Auth(_) => Err("Auth"),
            OpenOutcome::Stopped => Err("Stopped"),
        });
    });
    let (_held, _) = listener.accept().expect("the pump dials the listener");

    let verdict = rx.recv_timeout(Duration::from_secs(40)).expect(
        "the open is still blocked 40 s after the 10 s dial window: the pump's dial is unbounded",
    );
    let why = verdict.unwrap_or_else(|outcome| {
        panic!("a silent peer can only be a transport failure, got OpenOutcome::{outcome}")
    });
    assert!(why.contains("no answer within"), "the silent handshake is named, got: {why}");
}

/// An edge that answers the WebSocket upgrade with a 502 (what the testnet did for 15 minutes on
/// 2026-10-27): `connect` fails at once, names the status, leaves the transport able to dial again
/// (the next pass, the next order), and the dial window is never the thing that bounded it.
#[test]
fn a_502_to_the_upgrade_is_a_prompt_named_connect_failure_and_the_next_dial_is_allowed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("ws://{}", listener.local_addr().expect("local addr"));
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let Ok(mut stream) = stream else { return };
            let mut request = BufReader::new(stream.try_clone().expect("clone the stream"));
            let mut line = String::new();
            // read the upgrade request up to its blank line, then answer 502 and hang up
            while request.read_line(&mut line).is_ok_and(|n| n > 0) {
                if line == "\r\n" {
                    break;
                }
                line.clear();
            }
            let _ = stream.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });

    let mut transport = DeribitOrderTransport::new(&url, "client-id", "client-secret", None)
        .with_dial_timeout(Duration::from_secs(30));
    for dial in 1..=2 {
        let began = Instant::now();
        let err = transport.connect().expect_err("a 502 is not a socket").to_string();
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "dial {dial} took {:?}",
            began.elapsed()
        );
        assert!(err.contains("order-WS connect failed"), "a connect failure, got: {err}");
        assert!(err.contains("502"), "the status is named, got: {err}");
    }
}
