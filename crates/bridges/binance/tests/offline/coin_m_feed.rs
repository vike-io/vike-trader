//! **A COIN-M contract's live lanes, over frames RECORDED from Binance's public COIN-M socket**
//! (`wss://dstream.binance.com`, `btcusd_perp` and `btcusd_261225`, 2026-11-08; public market data, no
//! account).
//!
//! Each test runs the REAL lane plan, through the data-client engine's own lane runner
//! (`vike_bridge_core::data_client::ws::run_plan`), against a loopback websocket that wears the COIN-M table
//! (`vike_binance::feed::BinanceTables::urls_for` of a COIN-M name over the release's seed,
//! only the host replaced), pushes the
//! recorded frames, and reads what reached the sink and which path the lane dialled. What the lanes do
//! with a COIN-M frame that they do not do with a USDⓈ-M one:
//!
//! - the kline lane (the shared `feed_main`) reads the bar's volume from `q` (the base asset), because
//!   `v` on this book is a CONTRACT count (`kline_volume` names the column);
//! - the mark lane is the shared one, and the frame states the funding rate the datahub keeps;
//! - the trade lane rides `@aggTrade` (the aggregate lane), which streams on `dstream`.

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::Message;
use tungstenite::handshake::server::{Request, Response};
use vike_binance::family::feed::{decode_kline_frame, feed_main, mark_main, trades_main};
use vike_binance::family::history::klines::VolumeColumn;
use vike_binance::family::{FamilySpec, UrlTable};
use vike_binance::feed::{BinanceTables, DataClientSpecBinance, kline_volume};
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, run_plan};
use vike_data::{RecordingSink, SinkCall};
use vike_venues::venue_urls::VenueUrls;

/// `btcusd_perp@kline_1m`, the forming frame and then the CLOSED one of the same minute (spaces as sent).
const KLINES: &[&str] = &[
    r#"{"e":"kline","E":1794160198677,"s":"BTCUSD_PERP","k":{"t":1794160140000, "T":1794160199999, "s":"BTCUSD_PERP", "i":"1m", "f":1163308136, "L":1163308152, "o":"75640.0", "c":"75640.0", "h":"75640.0", "l":"75639.9", "v":"865", "n":17, "x":false, "q":"1.14357557", "V":"439", "Q":"0.58038075", "B":"0"}}"#,
    r#"{"e":"kline","E":1794160203001,"s":"BTCUSD_PERP","k":{"t":1794160140000, "T":1794160199999, "s":"BTCUSD_PERP", "i":"1m", "f":1163308136, "L":1163308152, "o":"75640.0", "c":"75640.0", "h":"75640.0", "l":"75639.9", "v":"865", "n":17, "x":true, "q":"1.14357557", "V":"439", "Q":"0.58038075", "B":"0"}}"#,
];
/// `btcusd_perp@markPrice@1s`: the mark, and the funding rate with its next settlement.
const PERP_MARK: &[&str] = &[
    r#"{"e":"markPriceUpdate","E":1794160197000,"s":"BTCUSD_PERP","p":"75639.90000000","ap":"75639.90000000","P":"75629.40441605","i":"75679.50824586","r":"0.00003710","T":1794182400000,"st":2}"#,
];
/// `btcusd_261225@markPrice@1s`: a quarterly states `r` 0 and `T` 0, so a mark and no funding.
const QUARTERLY_MARK: &[&str] = &[
    r#"{"e":"markPriceUpdate","E":1794160197000,"s":"BTCUSD_261225","p":"76233.34704826","ap":"76233.34704826","P":"75629.43226399","i":"75679.55855982","r":"0.00000000","T":0,"st":2}"#,
];
/// `btcusd_perp@aggTrade`, two consecutive aggregate trades (`q` in contracts).
const AGG_TRADES: &[&str] = &[
    r#"{"e":"aggTrade","E":1794160198827,"a":502730032,"s":"BTCUSD_PERP","p":"75640.0","q":"18","f":1163308152,"l":1163308152,"T":1794160198676,"m":false,"st":2}"#,
    r#"{"e":"aggTrade","E":1794160206102,"a":502730033,"s":"BTCUSD_PERP","p":"75639.9","q":"2","f":1163308153,"l":1163308153,"T":1794160205952,"m":true,"st":2}"#,
];

/// A loopback `dstream`: accepts ONE websocket, reports its request target, pushes `frames`, then holds
/// the socket until the lane drops it.
// tungstenite's `Callback` fixes the closure's `Err` type (`ErrorResponse`, an HTTP response), which
// nothing here can box.
#[expect(clippy::result_large_err)]
fn fake_dstream(frames: &'static [&'static str]) -> (&'static str, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    // Leaked on purpose: `UrlTable` holds `&'static str`, and a test process is short-lived.
    let host: &'static str =
        Box::leak(format!("ws://{}", listener.local_addr().expect("local addr")).into_boxed_str());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let mut target = String::new();
        let read_target = |req: &Request, response: Response| {
            target = req.uri().to_string();
            Ok(response)
        };
        let Ok(mut ws) = tungstenite::accept_hdr(stream, read_target) else { return };
        let _ = tx.send(target);
        for frame in frames {
            if ws.send(Message::Text((*frame).into())).is_err() {
                return;
            }
        }
        while ws.read().is_ok() {}
    });
    (host, rx)
}

/// The COIN-M table of `symbol` with its futures stream host on the loopback (and its REST origin on a
/// port that refuses, so a warmup fails at once and the lane goes on with what the socket brought).
fn coin_m_on(symbol: &str, host: &'static str) -> UrlTable {
    let refused = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
    let rest: &'static str = Box::leak(format!("http://{refused}").into_boxed_str());
    let tables = BinanceTables::from_urls(&VenueUrls::seed("binance")).expect("the seed's rows");
    UrlTable { perp_websocket: host, perp_rest: rest, ..tables.urls_for(symbol) }
}

/// Runs the plan `lane` builds from a binance family row over `urls`, as the engine's `lane_kind` lane
/// of `symbol` (interval `1m` for bars), until `done` holds for what reached the sink, then stops it.
/// Returns the sink's calls.
fn run_until(
    urls: UrlTable,
    lane_kind: DataClientLane,
    symbol: &'static str,
    lane: impl FnOnce(FamilySpec) -> DataClientLanePlan + Send + 'static,
    done: impl Fn(&[SinkCall]) -> bool,
) -> Vec<SinkCall> {
    let sink = Arc::new(RecordingSink::default());
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = DataClientContext {
        venue: "binance",
        sink: Arc::clone(&sink) as Arc<dyn vike_data::DataClientSink>,
        status: Arc::new(Mutex::new(String::new())),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
        own: (),
    };
    let family = FamilySpec { venue: "binance", display: "Binance", urls };
    let lane = std::thread::spawn(move || {
        let interval = lane_kind.word("1m").to_string();
        run_plan::<DataClientSpecBinance>(lane(family), lane_kind, symbol, &interval, &ctx);
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(&sink.recorded()) {
        assert!(Instant::now() < deadline, "the lane never delivered: {:?}", sink.calls());
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::SeqCst);
    lane.join().expect("the lane ends once its stop flag is up");
    sink.recorded()
}

fn target(rx: &Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(5)).expect("the lane dialled the loopback")
}

/// **The COIN-M kline lane dials `dstream`'s market path and its bars carry the BASE-asset volume**, the
/// unit of the history path (`VolumeColumn::Index7`): `q` 1.14357557 BTC, where `v` says 865 contracts
/// (865 x 100 USD at 75,640). The column is the one production picks for the symbol (`kline_volume`);
/// the `v` column every other book keeps would book 865.
#[test]
fn the_coin_m_kline_lane_books_the_base_asset_volume() {
    let volume = kline_volume("BTCUSD_PERP.P");
    assert_eq!(volume, VolumeColumn::Index7);
    for usd_m_or_spot in ["BTCUSDT.P", "BTCUSDT_261225.P", "BTCUSDT", "BTCUSD_PERP"] {
        assert_eq!(kline_volume(usd_m_or_spot), VolumeColumn::Index5, "{usd_m_or_spot}");
    }
    let v = decode_kline_frame(KLINES[1], VolumeColumn::Index5).unwrap().expect("a kline");
    assert_eq!(v.bar.volume, 865.0);
    let q = decode_kline_frame(KLINES[1], volume).unwrap().expect("a kline");
    assert_eq!((q.bar.volume, q.closed), (1.14357557, true));

    let (host, dialled) = fake_dstream(KLINES);
    let calls = run_until(
        coin_m_on("BTCUSD_PERP.P", host),
        DataClientLane::Bars,
        "BTCUSD_PERP.P",
        move |family| {
            feed_main("BTCUSD_PERP", "1m", true, volume, &family.urls, |_, _, _| Ok(Vec::new()))
        },
        |calls| calls.iter().any(|c| matches!(c, SinkCall::CloseBar { .. })),
    );
    assert_eq!(target(&dialled), "/market/ws/btcusd_perp@kline_1m");
    let bars: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            SinkCall::FormingBar { symbol, interval, bar, .. } => {
                Some((false, symbol, interval, bar))
            }
            SinkCall::CloseBar { symbol, interval, bar, .. } => Some((true, symbol, interval, bar)),
            _ => None,
        })
        .collect();
    assert_eq!(bars.len(), 2, "{calls:?}");
    for (closed, symbol, interval, bar) in &bars {
        assert_eq!((symbol.as_str(), interval.as_str()), ("BTCUSD_PERP.P", "1m"));
        assert_eq!(
            (bar.timestamp, bar.open, bar.high, bar.low, bar.close),
            (1794160140000, 75640.0, 75640.0, 75639.9, 75640.0)
        );
        assert_eq!(
            bar.volume.to_bits(),
            1.14357557_f64.to_bits(),
            "closed={closed}: base asset, not contracts"
        );
    }
    assert!(calls.iter().any(|c| matches!(c,
        SinkCall::BarCloseTick { symbol, price: px, .. } if symbol == "BTCUSD_PERP.P" && *px == 75640.0)));
}

/// **The COIN-M mark lane states the mark AND the funding rate**: the shared `mark_main` on the COIN-M
/// table dials `dstream`'s market path, and the perpetual's frame carries `r`/`T`, so the datahub gets the
/// rate for the settlement at `T`. A quarterly's frame (`r` 0, `T` 0) prices the contract and states none.
#[test]
fn the_coin_m_mark_lane_states_the_mark_and_the_funding_rate() {
    let (host, dialled) = fake_dstream(PERP_MARK);
    let calls = run_until(
        coin_m_on("BTCUSD_PERP.P", host),
        DataClientLane::Mark,
        "BTCUSD_PERP.P",
        |family| mark_main("BTCUSD_PERP.P", "BTCUSD_PERP", &family.urls),
        |calls| calls.iter().any(|c| matches!(c, SinkCall::Funding { .. })),
    );
    assert_eq!(target(&dialled), "/market/ws/btcusd_perp@markPrice@1s");
    assert!(
        calls.iter().any(|c| matches!(c,
        SinkCall::MarkTick { symbol, price: px, timestamp: ts, .. }
            if symbol == "BTCUSD_PERP.P" && *px == 75639.9 && *ts == 1794160197000)),
        "{calls:?}"
    );
    let funding = calls
        .iter()
        .find_map(|c| match c {
            SinkCall::Funding { symbol, funding, .. } => Some((symbol.clone(), funding.clone())),
            _ => None,
        })
        .expect("a funding rate");
    assert_eq!(funding.0, "BTCUSD_PERP.P");
    assert_eq!(
        (funding.1.symbol.as_str(), funding.1.rate, funding.1.timestamp, funding.1.next_timestamp),
        ("BTCUSD_PERP.P", 0.0000371, 1794160197000, Some(1794182400000))
    );

    let (host, _) = fake_dstream(QUARTERLY_MARK);
    let calls = run_until(
        coin_m_on("BTCUSD_261225.P", host),
        DataClientLane::Mark,
        "BTCUSD_261225.P",
        |family| mark_main("BTCUSD_261225.P", "BTCUSD_261225", &family.urls),
        |calls| calls.iter().any(|c| matches!(c, SinkCall::MarkTick { .. })),
    );
    assert!(calls.iter().any(|c| matches!(c,
        SinkCall::MarkTick { symbol, price: px, .. } if symbol == "BTCUSD_261225.P" && *px == 76233.34704826)));
    assert!(!calls.iter().any(|c| matches!(c, SinkCall::Funding { .. })), "{calls:?}");
}

/// **The COIN-M trade lane rides `@aggTrade` on `dstream`'s market path**: the recorded aggregate trades
/// reach the sink under the series label (sizes in contracts), and their first id is published as the
/// lane's earliest live id, the boundary a backfill pages below (the aggregate lane records one; the raw
/// lane USDⓈ-M rides does not).
#[test]
fn the_coin_m_trade_lane_rides_agg_trade() {
    let (host, dialled) = fake_dstream(AGG_TRADES);
    let earliest: Arc<Mutex<HashMap<String, u64>>> = Arc::default();
    let ids = Arc::clone(&earliest);
    let calls = run_until(
        coin_m_on("BTCUSD_PERP.P", host),
        DataClientLane::Trades,
        "BTCUSD_PERP.P",
        move |family| trades_main("BTCUSD_PERP.P", family, ids),
        |calls| calls.iter().filter(|c| matches!(c, SinkCall::Trade { .. })).count() >= 2,
    );
    assert_eq!(target(&dialled), "/market/ws/btcusd_perp@aggTrade");
    let trades: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            SinkCall::Trade { symbol, trade, .. } => Some((
                symbol.as_str(),
                trade.timestamp,
                trade.price,
                trade.size,
                trade.is_buyer_maker,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        trades,
        [
            ("BTCUSD_PERP.P", 1794160198676, 75640.0, 18.0, false),
            ("BTCUSD_PERP.P", 1794160205952, 75639.9, 2.0, true)
        ]
    );
    assert_eq!(earliest.lock().unwrap().get("BTCUSD_PERP.P"), Some(&502730032));
}
