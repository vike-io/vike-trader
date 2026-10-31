//! **A Binance perpetual's kline, mark-price and depth lanes dial the path their stream class lives on**
//! (decision 0251).
//!
//! Binance's USDⓈ-M futures socket is routed by stream class since its March 2026 upgrade (changelog
//! entries 2026-03-05 and 2026-04-02): the legacy `/ws/<stream>` path the family used to dial keeps
//! answering and delivers the PUBLIC class
//! only (bookTicker, depth, `trade`), while kline, markPrice and aggTrade need `/market/ws/<stream>`.
//! Measured 2026-10-31 from the production node's own address, 5 s windows, frames on the legacy
//! path against `/market`: `kline_1m` 0 against 11, `markPrice@1s` 0 against 6, `aggTrade` 0 against
//! 52. The socket opens, no error comes back, and the lane retries for ever on a feed that never
//! speaks, so a perpetual mount heard no bar and no mark.
//!
//! These tests run the three REAL lane bodies (`feed_main`, `mark_main`, `depth_main`) against a
//! loopback listener wearing Binance's own `UrlTable` (only the host replaced) and read the request
//! target of the first dial, so the path is what the shipped code builds, not a copy of it. The
//! pure URL builders are pinned beside the code (`family/perp_lane_tests.rs`,
//! `market_feed_tests.rs`, `market_data.rs`); what only this file can say is that the LANE passes
//! the right class to its builder.
//!
//! Kill proof: empty `BINANCE_URLS`'s `perp_market_path` and `perp_public_path` (the legacy layout)
//! and the three perp lane tests read `/ws/...` and fail.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_binance::family::market_feed::{FeedCtx, depth_main, feed_main, mark_main};
use vike_binance::family::{FamilySpec, UrlTable};
use vike_binance::market_feed::BINANCE_URLS;

/// Runs `lane` with Binance's table, `redirect` pointing one of its hosts at a loopback listener,
/// and returns the request target (`/market/ws/btcusdt@kline_1m`) of its FIRST dial. The listener
/// answers nothing, so the lane fails its handshake and retries until the stop flag this raises
/// ends it.
fn first_dial(
    redirect: impl FnOnce(&'static str) -> UrlTable,
    lane: impl FnOnce(FeedCtx) + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("non-blocking accept");
    // Leaked on purpose: `UrlTable` holds `&'static str`, and a test process is short-lived.
    let host: &'static str =
        Box::leak(format!("ws://{}", listener.local_addr().expect("local addr")).into_boxed_str());
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = FeedCtx {
        sink: Arc::new(vike_data::NoopSink),
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
        spec: FamilySpec { venue: "binance", display: "Binance", urls: redirect(host) },
    };
    let lane = std::thread::spawn(move || lane(ctx));

    let deadline = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("the lane never dialled the loopback listener: {e}"),
        }
    };
    stream.set_nonblocking(false).expect("blocking read");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("read bound");
    let mut request_line = String::new();
    BufReader::new(&stream).read_line(&mut request_line).expect("the HTTP upgrade request line");
    stop.store(true, SeqCst);
    drop(stream);
    lane.join().expect("the lane thread ends once its stop flag is up");

    let mut parts = request_line.split_whitespace();
    assert_eq!(parts.next(), Some("GET"), "a websocket upgrade: {request_line:?}");
    parts.next().expect("a request target").to_string()
}

/// Binance's table with its futures host replaced; the class paths stay Binance's own.
fn perp_host(host: &'static str) -> UrlTable {
    UrlTable { perp_ws: host, ..BINANCE_URLS }
}

/// kline is MARKET class: the legacy path answers and pushes nothing for it.
#[test]
fn the_perp_kline_lane_dials_the_market_path() {
    let target = first_dial(perp_host, |ctx| {
        feed_main("BTCUSDT.P".into(), "BTCUSDT".into(), "1m".into(), true, ctx, |_, _, _| {
            Ok(Vec::new())
        });
    });
    assert_eq!(target, "/market/ws/btcusdt@kline_1m");
}

/// markPrice is MARKET class: the real mark a perp mount is valued on rides this path.
#[test]
fn the_perp_mark_lane_dials_the_market_path() {
    let target = first_dial(perp_host, |ctx| mark_main("BTCUSDT.P".into(), "BTCUSDT".into(), ctx));
    assert_eq!(target, "/market/ws/btcusdt@markPrice@1s");
}

/// depth is PUBLIC class: it still answered on the legacy path, and moves with the layout so the
/// recorder's depth tape does not depend on a path Binance has announced it retires.
#[test]
fn the_perp_depth_lane_dials_the_public_path() {
    let target = first_dial(perp_host, |ctx| {
        depth_main("BTCUSDT.P".into(), "1m".into(), ctx, |_, _| Err("offline".into()));
    });
    assert_eq!(target, "/public/ws/btcusdt@depth@100ms");
}

/// Spot has no classes: the same kline lane on a spot symbol keeps the one spot host, path `/ws/`.
#[test]
fn the_spot_kline_lane_keeps_its_one_path() {
    let target = first_dial(
        |host| UrlTable { spot_ws: host, ..BINANCE_URLS },
        |ctx| {
            feed_main("BTCUSDT".into(), "BTCUSDT".into(), "1m".into(), false, ctx, |_, _, _| {
                Ok(Vec::new())
            });
        },
    );
    assert_eq!(target, "/ws/btcusdt@kline_1m");
}
