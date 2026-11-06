//! Loopback HTTP doubles (`test-support` only, so no shipped build compiles them).
//!
//! [`redirecting_origin`] is the stand-in every no-redirect agent test dials: the credentialed agents
//! of alpaca, ig and oanda, vike-ai's provider agent, the tradehub's Telegram long-poll agent and
//! vike-alerting's webhook transport. It is an origin that answers every request with a `302` to a
//! SECOND loopback listener whose hits are counted, so a test proves the agent never follows a
//! redirect (and so never replays a credential, a token-bearing path or an alert to the `Location`
//! host). [`serve_replies`] is the scripted origin of the catalog-lookup tests: canned answers in
//! order, each request line recorded. [`spawn_on_loopback`] is the hubs' test server: a real `serve`
//! loop over an ephemeral loopback listener. They live here, not in `vike-bridge-core`, because this
//! crate is the one test-infrastructure leaf all of them already reach.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A loopback origin answering every request with a 302 to a SECOND loopback listener, plus that
/// listener's hit counter: the stand-in for the host a followed redirect would replay credentials
/// to.
pub fn redirecting_origin() -> (String, Arc<AtomicUsize>) {
    fn serve(reply: String) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        // Named: this module ships under `test-support`, so `bare_thread_spawn_gate` reads it as src.
        std::thread::Builder::new()
            .name("loopback-http".into())
            .spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    seen.fetch_add(1, SeqCst);
                    let _ = conn.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut buf = [0u8; 8192];
                    for _ in 0..2 {
                        let _ = conn.read(&mut buf); // a POST's head and body can be two writes
                    }
                    let _ = conn.write_all(reply.as_bytes());
                }
            })
            .expect("spawn the loopback listener");
        (port, hits)
    }
    let (target, target_hits) =
        serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into());
    let (origin, _) = serve(format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target}/stolen\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    ));
    (format!("http://127.0.0.1:{origin}"), target_hits)
}

/// A loopback HTTP server that answers `replies` (`(status, JSON body)`) to successive connections,
/// in order, and records each request's first line: the catalog lookups' double (binance's family
/// `catalog_symbol`, bybit's `catalog_symbol`). Returns `http://127.0.0.1:<port>` followed by `path`
/// (the endpoint, for a caller that is handed one) and the request log.
pub fn serve_replies(path: &str, replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}{path}", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::Builder::new()
        .name("loopback-replies".into())
        .spawn(move || {
            for (status, body) in replies {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                log.lock().expect("log").push(head.lines().next().unwrap_or("").to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        })
        .expect("spawn the loopback listener");
    (base, seen)
}

/// Bind an ephemeral loopback listener, run `serve` over it on a detached thread named `thread`, and
/// return the assigned address for a client to connect to: the datahub's and the backtesthub's test
/// servers. No test joins the thread; `serve` runs for the life of the test process.
pub fn spawn_on_loopback(
    thread: &str,
    serve: impl FnOnce(TcpListener) + Send + 'static,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    // Named: this module ships under `test-support`, so `bare_thread_spawn_gate` reads it as src.
    std::thread::Builder::new()
        .name(thread.into())
        .spawn(move || serve(listener))
        .expect("spawn the loopback server");
    addr
}
