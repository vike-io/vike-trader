//! **A peer that accepts a dial and then says nothing** (`test-support` only): the half-open edge
//! every bounded venue WS dial is held to, one double for every bridge's `dial_bound` test.
//! `vike_transport::websocket_proxy`'s module doc has the bound; each venue's test proves its own dial took it, and
//! that the failure is the one its pump reconnects on.
//!
//! The dial runs on a thread of its own, so a dial with no bound fails the test instead of hanging it.
//! That thread is left to end with the process.

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::websocket_session::user::WebsocketSessionUserOpenOutcome;

/// The longest a dial may take to fail: a `crate::websocket_session::spec::CONNECT_10S` window, with room for a
/// loaded box.
const FAILS_WITHIN: Duration = Duration::from_secs(40);

/// What one dial of the silent peer left behind.
pub struct SilentDial {
    /// The upgrade request as the peer read it.
    pub request: String,
    /// The dial's words for the failure.
    pub failure: String,
}

/// Run `dial` against a listener bound on `bind` (it is handed `ws://<the listener>`), hold the
/// connection without a word, and panic unless the dial fails inside [`FAILS_WITHIN`] naming the
/// silent handshake.
pub fn dial_silent_peer(
    bind: &str,
    dial: impl FnOnce(String) -> Result<(), String> + Send + 'static,
) -> SilentDial {
    let listener = TcpListener::bind(bind).expect("bind the silent peer");
    let url = format!("ws://{}", listener.local_addr().expect("the peer's address"));
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("silent-dial".into())
        .spawn(move || {
            let _ = tx.send(dial(url));
        })
        .expect("spawn the dial thread");
    let began = Instant::now();
    let mut held = accept_or_panic(&listener, &rx, began);
    let request = read_request(&mut held);
    let left = FAILS_WITHIN.saturating_sub(began.elapsed());
    let verdict = rx.recv_timeout(left).unwrap_or_else(|_| {
        panic!("still blocked {FAILS_WITHIN:?} into a dial of a silent peer: the dial has no bound")
    });
    let failure = verdict.expect_err("a peer that says nothing cannot complete a handshake");
    assert!(failure.contains("no answer within"), "the silent handshake is named, got: {failure}");
    SilentDial { request, failure }
}

/// [`dial_silent_peer`] for a pump's open: only `WebsocketSessionUserOpenOutcome::Transport` is what the pump reconnects on.
pub fn open_silent_peer<S: 'static>(
    open: impl FnOnce(String) -> WebsocketSessionUserOpenOutcome<S> + Send + 'static,
) -> SilentDial {
    dial_silent_peer("127.0.0.1:0", move |url| match open(url) {
        WebsocketSessionUserOpenOutcome::Transport(why) => Err(why),
        WebsocketSessionUserOpenOutcome::Ready(_) => Ok(()),
        WebsocketSessionUserOpenOutcome::Auth(_) => {
            Err("WebsocketSessionUserOpenOutcome::Auth, not a transport failure".to_string())
        }
        WebsocketSessionUserOpenOutcome::Refused(_) => {
            Err("WebsocketSessionUserOpenOutcome::Refused, not a transport failure".to_string())
        }
        WebsocketSessionUserOpenOutcome::Stopped => {
            Err("WebsocketSessionUserOpenOutcome::Stopped, not a transport failure".to_string())
        }
    })
}

/// The dial's connection, or a panic saying the dial ended without ever reaching the peer.
fn accept_or_panic(
    listener: &TcpListener,
    dial: &mpsc::Receiver<Result<(), String>>,
    began: Instant,
) -> TcpStream {
    listener.set_nonblocking(true).expect("poll the listener");
    loop {
        if let Ok((held, _)) = listener.accept() {
            held.set_nonblocking(false).expect("a blocking peer socket");
            return held;
        }
        if let Ok(verdict) = dial.try_recv() {
            panic!("the dial ended before it reached the silent peer: {verdict:?}");
        }
        assert!(began.elapsed() < FAILS_WITHIN, "the dial never reached the silent peer");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Read up to the blank line that ends the upgrade request. The socket stays open and unanswered.
fn read_request(held: &mut TcpStream) -> String {
    held.set_read_timeout(Some(Duration::from_secs(5))).expect("a read timeout");
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|end| end == b"\r\n\r\n") {
        match held.read(&mut chunk) {
            Ok(n) if n > 0 => request.extend_from_slice(&chunk[..n]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}
