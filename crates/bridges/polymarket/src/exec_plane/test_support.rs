//! A loopback HTTP double for this crate's OFFLINE tests of the REST plane that has no seam of its
//! own (the L1 key calls, the `/balance-allowance` read, the relayer's `/nonce` and `/submit`).
//!
//! It is a REAL socket, so the production code under test runs byte for byte as it does against the
//! venue — the proxy-aware `crate::egress::agent`, the signed headers, the redirect refusal — and
//! only the host differs. [`FakeServer::start`] takes a handler that answers each request from what
//! it carried; every request is also recorded, so a test asserts both what the code SENT and what it
//! did with the answer. Nothing here can reach a venue: it binds `127.0.0.1:0`.
//!
//! A test that dials it must first call [`declare_direct_egress`], because the process-wide egress
//! defaults to a SOCKS tunnel that would swallow a loopback dial (the same declaration
//! `exec_tests.rs`'s `a_signed_call_never_follows_a_redirect` makes).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// What the double answers with.
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub(crate) fn json(status: u16, body: serde_json::Value) -> Self {
        Reply { status, body: body.to_string() }
    }
    pub(crate) fn text(status: u16, body: &str) -> Self {
        Reply { status, body: body.to_string() }
    }
}

/// One request the double received.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub method: String,
    /// The request target: path plus query, as sent.
    pub target: String,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub(crate) fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
    pub(crate) fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, q)| q)
    }
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == want).map(|(_, v)| v.as_str())
    }
}

/// The running double. Dropping it stops the thread.
pub(crate) struct FakeServer {
    pub base: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub(crate) fn start(handler: impl Fn(&Recorded) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking listener");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen2, stop2) = (seen.clone(), stop.clone());
        let join = std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).expect("blocking stream");
                stream.set_read_timeout(Some(Duration::from_secs(5))).expect("read timeout");
                let Some(req) = read_request(&mut stream) else { continue };
                let reply = handler(&req);
                seen2.lock().unwrap().push(req);
                let wire = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                let _ = stream.write_all(wire.as_bytes());
            }
        });
        FakeServer { base, seen, stop, join: Some(join) }
    }

    /// Every request received so far, in arrival order.
    pub(crate) fn seen(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Read one HTTP request: the head up to the blank line, then `Content-Length` bytes of body. A
/// client may write the head and the body in separate segments, so a single `read` is not enough.
fn read_request(stream: &mut std::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let want: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + want {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
    Some(Recorded { method, target, headers, body })
}

/// Route this process's Polymarket HTTP straight to the loopback double (no SOCKS tunnel). The same
/// declaration twice is `Ok`, so every test that needs it may call it.
pub(crate) fn declare_direct_egress() {
    let _ = crate::egress::declare_egress(crate::egress::EgressSettings::from_lookup(|f| {
        (f == "socks_proxy").then(|| "none".to_string())
    }));
    assert_eq!(
        crate::egress::proxy_url(),
        None,
        "a loopback double must be dialled direct: another declaration won in this process"
    );
}
