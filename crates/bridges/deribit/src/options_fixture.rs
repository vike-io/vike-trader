//! A loopback stand-in for Deribit's book-summary endpoint and its option listing read, for the options
//! suites on both sides of the crate boundary (this crate's `src/options_tests.rs` and the gatewayhub's
//! `crates/vike-gatewayhub/tests/lanes/rest_lane_surfaces.rs`): one BTC book it serves to every book
//! request, the BTC/ETH/SOL listing ([`option_listing`]) to every `get_instruments` request, and a count of
//! each (a request is one line the venue would have billed). MOVED from the gatewayhub's testkit with the
//! options lane; `test-support` only, never in a normal build.

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

/// The `currency=any&kind=option` listing: one option row per underlying in the venue's documented shape
/// (the captured fixture's fields), BTC and ETH coin-settled, SOL in the USDC book.
pub fn option_listing() -> String {
    let row = |name: &str, base: &str, settle: &str, index: &str| {
        format!(
            r#"{{"kind":"option","is_active":true,"instrument_name":"{name}","base_currency":"{base}","settlement_currency":"{settle}","price_index":"{index}"}}"#
        )
    };
    [
        row("BTC-25JUN37-100000-C", "BTC", "BTC", "btc_usd"),
        row("ETH-25JUN37-4000-C", "ETH", "ETH", "eth_usd"),
        row("SOL_USDC-25JUN37-150-C", "SOL", "USDC", "sol_usdc"),
    ]
    .join(",")
}

/// The running stand-in: the URLs to hand `DeribitOptionsProvider::with_book_summary_url` and
/// `with_instruments_url`, and the count of requests each answered.
pub struct FakeDeribit {
    /// The book-summary URL on the ephemeral loopback port.
    pub url: String,
    /// The option listing URL on the same port.
    pub instruments_url: String,
    /// How many book-summary requests it answered.
    pub hits: Arc<AtomicUsize>,
    /// How many listing requests it answered.
    pub listing_hits: Arc<AtomicUsize>,
}

/// Start the stand-in on an ephemeral loopback port: [`btc_book`] for `currency=BTC`, an empty book for
/// any other currency, and [`option_listing`] for a `get_instruments` request.
pub fn spawn_fake_deribit() -> FakeDeribit {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr: SocketAddr = listener.local_addr().expect("the assigned port");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    let listing_hits = Arc::new(AtomicUsize::new(0));
    let listing_counter = Arc::clone(&listing_hits);
    let book = btc_book();
    let listing = format!("[{}]", option_listing());
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
                let currency = request_line
                    .split("currency=")
                    .nth(1)
                    .and_then(|rest| rest.split(['&', ' ']).next())
                    .unwrap_or("");
                let rows = if request_line.contains("get_instruments") {
                    listing_counter.fetch_add(1, Ordering::SeqCst);
                    listing.as_str()
                } else {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if currency == "BTC" { book.as_str() } else { "[]" }
                };
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
    FakeDeribit {
        url: format!("http://{addr}/api/v2/public/get_book_summary_by_currency"),
        instruments_url: format!(
            "http://{addr}/api/v2/public/get_instruments?currency=any&kind=option"
        ),
        hits,
        listing_hits,
    }
}
