//! LIVE Binance perpetual feed smoke (network, NO creds — public streams), USDⓈ-M and COIN-M:
//!     cargo test -p vike-binance --test integration -- --ignored --nocapture binance_perp_feed_smoke::
//!
//! The lanes of binance's data client (`market_feed::DataClientSpecBinance` on the one engine) that a perpetual mount's `on_bar` and valuation ride must each
//! deliver a LIVE frame through the bridge's own seam: the kline lane (`subscribe_bars`, a REST seed
//! AND a live `kline` frame), the real mark price the same call pairs (`markPrice@1s`), and the two
//! lanes the recorder keeps (`subscribe_trades`, `subscribe_depth`).
//!
//! It exists because the first two went silent without a sound: Binance split its futures socket by
//! stream class (the March 2026 change notice; legacy retirement dated 2026-04-23, decision 0251), and the
//! legacy `/ws/<stream>` path the bridge dialled keeps accepting the socket and then delivers the
//! public class only. kline, markPrice and aggTrade need `/market/ws/<stream>`. The seed arrived, the
//! lane read "connected", and no live bar or mark ever followed (kline_1m 0 frames in 5 s against 11,
//! markPrice@1s 0 against 6, measured from the production node's own address). A still-forming kline
//! arrives within a second of the connect, so the deadline needs no bar close; the REST seed hands
//! over one forming bar itself, so the smoke counts a SECOND bar frame as the socket's. Keyless, like
//! `okx_candle_feed_smoke`.
//!
//! The COIN-M test runs the same lanes on `BTCUSD_PERP.P` (`dstream`, `dapi`) and adds what only that book
//! can get wrong: the mark states a funding rate, and a bar's volume is the BASE asset on both halves of
//! the kline lane (a COIN-M frame's `v` and a REST row's index 5 are contract counts), so the seed's closed
//! bars and the first closed LIVE bar must equal the history path's bars of the same minutes. That waits
//! for a minute to close (up to 75 s more).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_binance::feed::DataClientSpecBinance;
use vike_binance::history::KlineSourceBinance;
use vike_bridge_core::data_client::ws::DataClientWebsocket;
use vike_data::{DataClient, DataClientSink, RecordingSink, SinkCall};
use vike_venues::venue_urls::VenueUrls;

const SYMBOL: &str = "BTCUSDT.P";
const COIN_M: &str = "BTCUSD_PERP.P";
const INTERVAL: &str = "1m";

/// The history path's source over the release's `venue_url` rows.
fn klines() -> KlineSourceBinance {
    KlineSourceBinance::from_urls(&VenueUrls::seed("binance"))
        .expect("the seed holds every binance row")
}

#[test]
#[ignore = "network — public Binance futures streams, run manually (see module doc)"]
fn binance_perp_feeds_deliver_live_frames() {
    vike_log::test_init();
    let (_sink, mut feeds) = deliver_live_frames(SYMBOL);
    feeds.shutdown();
}

#[test]
#[ignore = "network — public Binance COIN-M streams, run manually (see module doc)"]
fn binance_coin_m_feeds_deliver_live_frames() {
    vike_log::test_init();
    let (sink, mut feeds) = deliver_live_frames(COIN_M);

    let funding = sink
        .fundings()
        .into_iter()
        .find_map(|(_, s, f)| (s == COIN_M).then_some(f))
        .unwrap_or_else(|| panic!("the COIN-M mark states no funding rate: {:?}", sink.calls()));
    assert!(
        funding.rate.is_finite() && funding.next_timestamp.is_some_and(|t| t > funding.timestamp),
        "{funding:?}"
    );

    let (_, _, _, seeded) = sink
        .seeded_bars()
        .into_iter()
        .find(|(_, s, _, _)| s == COIN_M)
        .expect("the COIN-M seed arrived");
    let tail = &seeded[seeded.len().saturating_sub(10)..];
    let (first, last) =
        (tail.first().expect("a seed").timestamp, tail.last().expect("a seed").timestamp);
    let history =
        vike_binance::history::fetch_klines_range(&klines(), COIN_M, INTERVAL, first, last)
            .expect("the history path reaches COIN-M");
    for bar in tail {
        let twin = history
            .iter()
            .find(|h| h.timestamp == bar.timestamp)
            .expect("the same minute in history");
        assert_eq!(
            bar.volume.to_bits(),
            twin.volume.to_bits(),
            "seeded {bar:?} vs history {twin:?}"
        );
    }

    let deadline = Instant::now() + Duration::from_secs(75);
    let closed = loop {
        let closed = sink.recorded().into_iter().find_map(|c| match c {
            SinkCall::CloseBar { symbol, bar, .. } if symbol == COIN_M => Some(bar),
            _ => None,
        });
        if let Some(bar) = closed {
            break bar;
        }
        assert!(Instant::now() < deadline, "no closed COIN-M bar in 75 s");
        std::thread::sleep(Duration::from_millis(500));
    };
    let history = vike_binance::history::fetch_klines_range(
        &klines(),
        COIN_M,
        INTERVAL,
        closed.timestamp,
        closed.timestamp,
    )
    .expect("the history path reaches COIN-M");
    let twin = history
        .iter()
        .find(|h| h.timestamp == closed.timestamp)
        .expect("the closed minute in history");
    assert_eq!(
        (closed.open, closed.high, closed.low, closed.close, closed.volume.to_bits()),
        (twin.open, twin.high, twin.low, twin.close, twin.volume.to_bits()),
        "live {closed:?} vs history {twin:?}"
    );
    eprintln!("binance COIN-M: funding {funding:?} · closed live bar {closed:?} = history");
    feeds.shutdown();
}

/// Subscribes `symbol`'s bars, trades and depth and waits until each lane (and the paired mark) has
/// delivered a LIVE frame; returns the sink and the still-running feeds.
fn deliver_live_frames(
    symbol: &str,
) -> (Arc<RecordingSink>, DataClientWebsocket<DataClientSpecBinance>) {
    let sink = Arc::new(RecordingSink::default());
    let spec = DataClientSpecBinance::new(&VenueUrls::seed("binance"))
        .expect("the seed holds every binance row");
    let mut feeds =
        DataClientWebsocket::new(spec, Arc::clone(&sink) as Arc<dyn DataClientSink>, || {});
    feeds.subscribe_bars(symbol, INTERVAL).expect("subscribe_bars ok");
    feeds.subscribe_trades(symbol).expect("subscribe_trades ok");
    feeds.subscribe_depth(symbol).expect("subscribe_depth ok");

    // "Live" is what the REST seed cannot fake: the seed hands over ONE still-forming bar (so a second
    // bar frame is the socket's), the trade warmup carries no receive stamp (`local_ts` 0), and the
    // seeded book publishes once (so a third snapshot is a diff off the socket). A mark has no seed.
    let deadline = Instant::now() + Duration::from_secs(25);
    let (bar_frames, last_bar, mark, trade, books) = loop {
        let (mut bar_frames, mut last_bar, mut mark, mut trade, mut books) =
            (0, None, None, None, 0);
        for call in sink.recorded() {
            match call {
                SinkCall::FormingBar { bar, .. } | SinkCall::CloseBar { bar, .. } => {
                    bar_frames += 1;
                    last_bar = Some(bar);
                }
                SinkCall::MarkTick { price: px, timestamp: ts, .. } => mark = Some((px, ts)),
                SinkCall::Trade { trade: t, .. } if t.local_timestamp > 0 => trade = Some(t.price),
                SinkCall::L2Snapshot { .. } => books += 1,
                _ => {}
            }
        }
        if bar_frames >= 2 && mark.is_some() && trade.is_some() && books >= 3 {
            break (bar_frames, last_bar, mark, trade, books);
        }
        let missing: Vec<&str> = [
            (bar_frames < 2, "bar"),
            (mark.is_none(), "mark"),
            (trade.is_none(), "trade"),
            (books < 3, "book"),
        ]
        .into_iter()
        .filter_map(|(absent, what)| absent.then_some(what))
        .collect();
        assert!(
            Instant::now() < deadline,
            "no live {missing:?} for {symbol} after 25 s (seed delivered: {}; bar frames \
             {bar_frames}, of which 1 is the seed's; book snapshots {books}) — a lane dials a \
             futures path Binance does not serve its stream class on (decision 0251: kline, \
             markPrice and aggTrade live on /market, depth and trade on /public), or the stream \
             is down",
            !sink.seeded_bars().is_empty(),
        );
        std::thread::sleep(Duration::from_millis(200));
    };

    let bar = last_bar.expect("two bar frames were counted");
    assert!(bar.close > 0.0 && bar.high >= bar.low, "a sane candle: {bar:?}");
    let (mark_price, mark_ts) = mark.expect("checked above");
    assert!(mark_price > 0.0 && mark_ts > 0, "a sane mark: {mark_price} @ {mark_ts}");
    eprintln!(
        "binance live {symbol}: {bar_frames} bar frames, last ts={} close={} · mark {mark_price} · trade {} · {books} book snapshots",
        bar.timestamp,
        bar.close,
        trade.expect("checked above"),
    );
    (sink, feeds)
}
