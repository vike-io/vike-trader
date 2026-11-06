//! LIVE Binance USDⓈ-M perpetual feed smoke (network, NO creds — public streams):
//!     cargo test -p vike-binance --test binance_perp_feed_smoke -- --ignored --nocapture
//!
//! The lanes of `market_feed::Feeds` that a perpetual mount's `on_bar` and valuation ride must each
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

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink, RecordingSink, SinkCall};

const SYMBOL: &str = "BTCUSDT.P";
const INTERVAL: &str = "1m";

#[test]
#[ignore = "network — public Binance futures streams, run manually (see module doc)"]
fn binance_perp_feeds_deliver_live_frames() {
    vike_log::test_init();

    let sink = Arc::new(RecordingSink::default());
    let mut feeds =
        vike_binance::market_feed::Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {});
    let bars = feeds.subscribe_bars(SYMBOL, INTERVAL).expect("subscribe_bars ok");
    let trades = feeds.subscribe_trades(SYMBOL).expect("subscribe_trades ok");
    let depth = feeds.subscribe_depth(SYMBOL).expect("subscribe_depth ok");

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
                SinkCall::MarkTick { px, ts, .. } => mark = Some((px, ts)),
                SinkCall::Trade { trade: t, .. } if t.local_ts > 0 => trade = Some(t.price),
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
            "no live {missing:?} for {SYMBOL} after 25 s (seed delivered: {}; bar frames \
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
    let (mark_px, mark_ts) = mark.expect("checked above");
    assert!(mark_px > 0.0 && mark_ts > 0, "a sane mark: {mark_px} @ {mark_ts}");
    eprintln!(
        "binance live: {bar_frames} bar frames, last ts={} close={} · mark {mark_px} · trade {} · {books} book snapshots",
        bar.ts,
        bar.close,
        trade.expect("checked above"),
    );

    feeds.unsubscribe(bars);
    feeds.unsubscribe(trades);
    feeds.unsubscribe(depth);
    feeds.shutdown();
}
