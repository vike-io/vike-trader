//! A SCRIPTED LOOPBACK IG GATEWAY for the offline suites: the real `IgSession` / `IgReconClient` /
//! `IgFactsSource` dial `127.0.0.1` and this double answers by the request line. It speaks HTTP/1.1 with
//! `connection: close`, reads each request's body, and records the request lines it served. It is NOT
//! IG: every body a test hands it is hand-written in the documented shape, so what a suite proves is this
//! crate's handling of that shape, never the venue's.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::json;
use vike_ig::IgConfig;

/// One scripted answer: the first route whose `key` the request line contains replies.
pub struct Route {
    pub key: &'static str,
    pub status: u16,
    pub body: String,
    pub headers: &'static [(&'static str, &'static str)],
    /// How long the double waits before it answers (a slow IG).
    pub delay: Duration,
}

impl Route {
    pub fn ok(key: &'static str, body: serde_json::Value) -> Self {
        Self { key, status: 200, body: body.to_string(), headers: &[], delay: Duration::ZERO }
    }

    pub fn status(key: &'static str, status: u16, body: serde_json::Value) -> Self {
        Self { key, status, body: body.to_string(), headers: &[], delay: Duration::ZERO }
    }

    /// This route answering only after `delay`.
    pub fn slow(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// `POST /session`: the token pair in the headers, the account in the body.
pub fn login_route() -> Route {
    Route {
        key: "POST /session",
        status: 200,
        body: json!({ "currentAccountId": "ACC1", "lightstreamerEndpoint": "" }).to_string(),
        headers: &[("CST", "cst-1"), ("X-SECURITY-TOKEN", "xst-1")],
        delay: Duration::ZERO,
    }
}

/// `GET /accounts`: one account `ACC1` in `currency`.
pub fn accounts_route(currency: &str) -> Route {
    Route::ok(
        "GET /accounts",
        json!({ "accounts": [
            { "accountId": "OTHER", "currency": "JPY", "balance": { "balance": 1.0 } },
            { "accountId": "ACC1", "currency": currency, "balance": { "balance": 1000.0 } },
        ] }),
    )
}

/// The link to page `page` (>= 1) of a paged activity history, in the shape IG's REST guide shows for
/// `metadata.paging.next` (a path under the gateway, `version=3` in the query, the `to` of the window).
pub fn activity_link(page: usize) -> String {
    format!(
        "/history/activity?version=3&from=2022-01-15T14:00:00&to=2022-01-15T13:{page:02}:00\
         &detailed=true&pageSize=50"
    )
}

/// A paged `GET /history/activity` (v3): `pages[i]` is the `activities` of page `i`; every page but the last
/// names the next one in `metadata.paging.next` (the last carries `null`). Page 0 answers the client's own
/// first request, page `i` the [`activity_link`] before it.
pub fn activity_pages(pages: Vec<Vec<serde_json::Value>>) -> Vec<Route> {
    let last = pages.len() - 1;
    pages
        .into_iter()
        .enumerate()
        .map(|(i, rows)| {
            // A route key is `&'static`; a test's few leaked keys cost nothing.
            let key = if i == 0 {
                "GET /history/activity?from=".to_string()
            } else {
                format!("to=2022-01-15T13:{i:02}:00")
            };
            let next = (i < last).then(|| activity_link(i + 1));
            Route::ok(
                Box::leak(key.into_boxed_str()),
                json!({ "activities": rows, "metadata": { "paging": { "size": 50, "next": next } } }),
            )
        })
        .collect()
}

/// The double: drops (and stops serving) with the test.
pub struct FakeIg {
    addr: SocketAddr,
    served: Arc<Mutex<Vec<String>>>,
    /// `(request line, body)` of every request served, in order.
    bodies: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
}

impl FakeIg {
    pub fn config(&self) -> IgConfig {
        IgConfig {
            api_key: "key".to_string(),
            identifier: "ident".to_string(),
            password: "pw".to_string(),
            rest_base: format!("http://{}", self.addr),
        }
    }

    /// Every request line served so far, in order.
    pub fn served(&self) -> Vec<String> {
        self.served.lock().unwrap().clone()
    }

    /// How many served request lines contain `text`.
    pub fn count(&self, text: &str) -> usize {
        self.served().iter().filter(|l| l.contains(text)).count()
    }

    /// The body of every served request whose line contains `text`, parsed as JSON, in order.
    pub fn bodies_of(&self, text: &str) -> Vec<serde_json::Value> {
        let all = self.bodies.lock().unwrap();
        all.iter()
            .filter(|(line, _)| line.contains(text))
            .map(|(_, body)| serde_json::from_str(body).unwrap_or(serde_json::Value::Null))
            .collect()
    }
}

impl Drop for FakeIg {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start the double over `routes`. An unmatched request is a 404 `{}`.
pub fn serve(routes: Vec<Route>) -> FakeIg {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let addr = listener.local_addr().expect("resolve assigned port");
    let served = Arc::new(Mutex::new(Vec::new()));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (log, sent, halt) = (Arc::clone(&served), Arc::clone(&bodies), Arc::clone(&stop));
    thread::spawn(move || {
        while !halt.load(Ordering::Relaxed) {
            let Ok((mut sock, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            sock.set_nonblocking(false).expect("blocking reads");
            sock.set_read_timeout(Some(Duration::from_secs(2))).expect("read timeout");
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 2048];
            let head_end = loop {
                if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break buf.len();
                }
                buf.extend_from_slice(&chunk[..n]);
            };
            let head = String::from_utf8_lossy(&buf[..head_end.min(buf.len())]).to_string();
            let wanted = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|v| v.parse::<usize>().ok())
                })
                .unwrap_or(0);
            while buf.len() < head_end + wanted {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let line = head.lines().next().unwrap_or_default().to_string();
            let route = routes.iter().find(|r| line.contains(r.key));
            let (status, body, headers) = route.map_or((404, "{}".to_string(), &[][..]), |r| {
                (r.status, r.body.clone(), r.headers)
            });
            let sent_body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).to_string();
            sent.lock().unwrap().push((line.clone(), sent_body));
            log.lock().unwrap().push(line);
            if let Some(route) = route {
                thread::sleep(route.delay);
            }
            let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(reply.as_bytes());
            let _ = sock.flush();
        }
    });
    FakeIg { addr, served, bodies, stop }
}
