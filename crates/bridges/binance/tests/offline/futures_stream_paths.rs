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
//! These tests run the REAL lane plans (`feed_main`, `mark_main`, `depth_main`, `trades_main`) through the
//! data-client engine's own lane runner (`vike_bridge_core::data_client::ws::run_plan`) against a
//! loopback listener wearing Binance's own `UrlTable` (only the host replaced) and read the request
//! target of the first dial, so the path is what the shipped code builds, not a copy of it. The
//! pure URL builders are pinned beside the code (`family/feed/tick/perp_lane_tests.rs`,
//! `feed/feed_tests.rs`, `feed/tick/mod.rs`); what only this file can say is that the LANE passes
//! the right class to its builder.
//!
//! Kill proof: empty `crates/bridges/binance/src/feed/mod.rs`'s `PATHS`' `perp_market_path` and
//! `perp_public_path` (the legacy layout) and the three perp lane tests read `/ws/...` and fail.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_binance::family::feed::{depth_main, feed_main, mark_main, trades_main};
use vike_binance::family::history::klines::VolumeColumn;
use vike_binance::family::{FamilySpec, UrlTable};
use vike_binance::feed::{BinanceTables, DataClientSpecBinance};
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, run_plan};
use vike_venues::venue_urls::VenueUrls;

/// Binance's spot and USDⓈ-M table over the release's `venue_url` seed: what production dials.
fn binance_urls() -> UrlTable {
    BinanceTables::from_urls(&VenueUrls::seed("binance"))
        .expect("the seed holds every binance row")
        .linear
}

/// Runs the plan `lane` builds from Binance's family row (its table with `redirect` pointing one of its
/// hosts at a loopback listener) as the engine's `lane_kind` lane of `symbol`, and returns the request
/// target (`/market/ws/btcusdt@kline_1m`) of its FIRST dial. The listener answers nothing, so the lane
/// fails its handshake and retries until the stop flag this raises ends it.
fn first_dial(
    redirect: impl FnOnce(&'static str) -> UrlTable,
    lane_kind: DataClientLane,
    symbol: &'static str,
    lane: impl FnOnce(FamilySpec) -> DataClientLanePlan + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("non-blocking accept");
    // Leaked on purpose: `UrlTable` holds `&'static str`, and a test process is short-lived.
    let host: &'static str =
        Box::leak(format!("ws://{}", listener.local_addr().expect("local addr")).into_boxed_str());
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = DataClientContext {
        venue: "binance",
        sink: Arc::new(vike_data::NoopSink),
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
        own: (),
    };
    let family = FamilySpec { venue: "binance", display: "Binance", urls: redirect(host) };
    let lane = std::thread::spawn(move || {
        let interval = lane_kind.word("1m").to_string();
        run_plan::<DataClientSpecBinance>(lane(family), lane_kind, symbol, &interval, &ctx);
    });

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
    UrlTable { perp_websocket: host, ..binance_urls() }
}

/// kline is MARKET class: the legacy path answers and pushes nothing for it.
#[test]
fn the_perp_kline_lane_dials_the_market_path() {
    let target = first_dial(perp_host, DataClientLane::Bars, "BTCUSDT.P", |family| {
        feed_main("BTCUSDT", "1m", true, VolumeColumn::Index5, &family.urls, |_, _, _| {
            Ok(Vec::new())
        })
    });
    assert_eq!(target, "/market/ws/btcusdt@kline_1m");
}

/// markPrice is MARKET class: the real mark a perp mount is valued on rides this path.
#[test]
fn the_perp_mark_lane_dials_the_market_path() {
    let target = first_dial(perp_host, DataClientLane::Mark, "BTCUSDT.P", |family| {
        mark_main("BTCUSDT.P", "BTCUSDT", &family.urls)
    });
    assert_eq!(target, "/market/ws/btcusdt@markPrice@1s");
}

/// depth is PUBLIC class: it still answered on the legacy path, and moves with the layout so the
/// recorder's depth tape does not depend on a path Binance has announced it retires.
#[test]
fn the_perp_depth_lane_dials_the_public_path() {
    let target = first_dial(perp_host, DataClientLane::Depth, "BTCUSDT.P", |family| {
        depth_main("BTCUSDT.P", &family, |_, _| Err("offline".into()))
    });
    assert_eq!(target, "/public/ws/btcusdt@depth@100ms");
}

/// The trades lane both venues' `subscribe_trades` plan (`trades_main`, which replaced a copy in each
/// venue) splits `.P` by its family row's venue and dials that row's OWN table: a perp rides the futures
/// host's PUBLIC `@trade` (Binance's raw lane), a spot symbol the spot host's `@aggTrade`.
/// Kill proof: read a venue's const table instead of the row (the real host is dialled, never the
/// loopback) or drop the `.P` split (the perp asks the spot host).
#[test]
fn the_trades_lane_dials_the_hosts_of_its_own_spec() {
    let perp = first_dial(perp_host, DataClientLane::Trades, "BTCUSDT.P", |family| {
        trades_main("BTCUSDT.P", family, Arc::default())
    });
    assert_eq!(perp, "/public/ws/btcusdt@trade");
    let spot = first_dial(
        |host| UrlTable { spot_websocket: host, ..binance_urls() },
        DataClientLane::Trades,
        "BTCUSDT",
        |family| trades_main("BTCUSDT", family, Arc::default()),
    );
    assert_eq!(spot, "/ws/btcusdt@aggTrade");
}

/// Spot has no classes: the same kline lane on a spot symbol keeps the one spot host, path `/ws/`.
#[test]
fn the_spot_kline_lane_keeps_its_one_path() {
    let target = first_dial(
        |host| UrlTable { spot_websocket: host, ..binance_urls() },
        DataClientLane::Bars,
        "BTCUSDT",
        |family| {
            feed_main("BTCUSDT", "1m", false, VolumeColumn::Index5, &family.urls, |_, _, _| {
                Ok(Vec::new())
            })
        },
    );
    assert_eq!(target, "/ws/btcusdt@kline_1m");
}
