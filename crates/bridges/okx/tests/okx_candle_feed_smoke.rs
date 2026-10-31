//! LIVE OKX candle-feed smoke (network, NO creds — public candle stream):
//!     cargo test -p vike-okx --test okx_candle_feed_smoke -- --ignored --nocapture
//!
//! The kline lane of `market_feed::Feeds` (`subscribe_bars`, the path every okx mount's `on_bar` rides) must
//! deliver a bar: the REST seed AND a live `candle<bar>` frame. It did not for as long as the lane dialled the
//! PUBLIC socket: OKX serves the candle channels on the business socket only and answers a candle subscribe on
//! the public one with a "wrong URL or channel" error, so the seed arrived and no live bar ever followed
//! (found by a live runtime mount whose `on_bar` never ran). A still-forming bar arrives within a second of the
//! subscribe, so the deadline needs no bar close. Keyless, like `okx_trades_feed_smoke`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink, RecordingSink};

const SYMBOL: &str = "BTC-USDT-SWAP";
const INTERVAL: &str = "1m";

#[test]
#[ignore = "network — public OKX candle stream, run manually (see module doc)"]
fn okx_candle_feed_delivers_live_bars() {
    vike_log::test_init();

    let sink = Arc::new(RecordingSink::default());
    let mut feeds =
        vike_okx::market_feed::Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {});
    let id = feeds.subscribe_bars(SYMBOL, INTERVAL).expect("subscribe_bars ok");

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let live: Vec<_> = sink.forming_bars().into_iter().chain(sink.closed_bars()).collect();
        if let Some((venue, symbol, interval, bar)) = live.first()
            && !sink.seeded_bars().is_empty()
        {
            assert!(bar.close > 0.0 && bar.high >= bar.low, "a sane candle: {bar:?}");
            eprintln!(
                "okx live bar: {venue}/{symbol}@{interval} ts={} close={}",
                bar.ts, bar.close
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no live {INTERVAL} bar for {SYMBOL} after 25s (seed delivered: {}) — the kline lane dials a socket \
             OKX does not serve candles on, or the stream is down",
            !sink.seeded_bars().is_empty()
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feeds.unsubscribe(id);
    feeds.shutdown();
}
