//! A loopback stand-in for Deribit's book-summary endpoint, for the options suites on both sides of the
//! crate boundary (the lib's `src/rest/options_tests.rs` and the `server` group's
//! `tests/lanes/rest_lane_surfaces.rs`): one BTC book it serves to every request, and a count of the
//! requests it answered (a request is one line the venue would have billed).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// BTC's book: one far-future expiry, five strikes, a call and a put each.
pub fn btc_book() -> String {
    let mut rows = Vec::new();
    for strike in [80_000, 90_000, 100_000, 110_000, 120_000] {
        for cp in ["C", "P"] {
            rows.push(format!(
                r#"{{"instrument_name":"BTC-25JUN37-{strike}-{cp}","bid_price":0.05,"ask_price":0.06,"mark_price":0.055,"mark_iv":62.5,"open_interest":12.0,"volume":3.0,"underlying_price":104000.0}}"#
            ));
        }
    }
    format!("[{}]", rows.join(","))
}

/// The running stand-in: the URL to hand `DeribitOptionsProvider::with_book_summary_url`, and the count of
/// requests it answered.
pub struct FakeDeribit {
    /// The book-summary URL on the ephemeral loopback port.
    pub url: String,
    /// How many requests it answered.
    pub hits: Arc<AtomicUsize>,
}

/// Start the stand-in on an ephemeral loopback port: [`btc_book`] for `currency=BTC`, an empty book for
/// any other currency.
pub fn spawn_fake_deribit() -> FakeDeribit {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr: SocketAddr = listener.local_addr().expect("the assigned port");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let book = btc_book();
    std::thread::Builder::new()
        .name("test-fake-deribit".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Ok(clone) = stream.try_clone() else { continue };
                let mut reader = BufReader::new(clone);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {}
                    }
                }
                counter.fetch_add(1, Ordering::SeqCst);
                let currency = request_line
                    .split("currency=")
                    .nth(1)
                    .and_then(|rest| rest.split(['&', ' ']).next())
                    .unwrap_or("");
                let rows = if currency == "BTC" { book.as_str() } else { "[]" };
                let body = format!(r#"{{"jsonrpc":"2.0","result":{rows}}}"#);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
            }
        })
        .expect("spawn the fake venue");
    FakeDeribit { url: format!("http://{addr}/api/v2/public/get_book_summary_by_currency"), hits }
}
