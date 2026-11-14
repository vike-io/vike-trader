//! Live smoke: does the Hyperliquid **TESTNET** market feed actually deliver BTC quotes + bars?
//!
//! This ISOLATES the feed (the engine over `vike_hyperliquid::feed::DataClientSpecHyperliquid`) from the `vike-tradehub` live
//! start's sink→core→maker wiring — the diagnostic for "the A-S maker placed 0 orders on a live
//! testnet start". If this smoke sees quotes, the feed is fine and the bug is downstream (the start
//! wiring); if it sees nothing, the feed/subscription itself is the culprit.
//!
//! Public/keyless market data, read-only, no account. `#[ignore]`d so CI never dials the venue:
//!   cargo test -p vike-hyperliquid --test hyperliquid_market_feed_smoke -- --ignored --nocapture

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::data_client::ws::DataClientWebsocket;
use vike_data::{DataClient, RecordingSink};
use vike_hyperliquid::feed::DataClientSpecHyperliquid;
use vike_model::VenueNetwork;

#[test]
#[ignore = "hits real Hyperliquid testnet WS; run explicitly with --ignored"]
fn testnet_btc_market_feed_delivers_quotes_and_bars() {
    let sink = Arc::new(RecordingSink::default());
    // EXACTLY what vike-tradehub's live_runtime does: the feed on the testnet network, subscribe the
    // start's symbol's bars + quotes. The A-S maker prices on the quote lane.
    let seed = vike_venues::venue_urls::VenueUrls::seed("hyperliquid");
    let spec = DataClientSpecHyperliquid::new(VenueNetwork::Testnet, &seed)
        .expect("the seed's testnet rows");
    let mut feeds = DataClientWebsocket::new(spec, sink.clone(), || {});
    feeds.subscribe_bars("BTC", "1m").expect("subscribe_bars BTC@1m");
    feeds.subscribe_quotes("BTC").expect("subscribe_quotes BTC");

    // Perp quotes tick frequently; give the WS ample time to connect + deliver a snapshot/updates.
    std::thread::sleep(Duration::from_secs(25));
    let calls = sink.calls();
    feeds.shutdown();

    let count = |sub: &str| calls.iter().filter(|c| c.to_lowercase().contains(sub)).count();
    let (quotes, bars, marks) = (count("quote"), count("bar"), count("mark"));
    println!(
        "HL TESTNET BTC feed in 25s: {} sink calls — {quotes} quote, {bars} bar, {marks} mark",
        calls.len()
    );
    for c in calls.iter().take(8) {
        println!("   {c}");
    }

    assert!(
        !calls.is_empty(),
        "HL testnet BTC feed delivered NOTHING in 25s — the FEED is the bug"
    );
    assert!(
        quotes > 0,
        "no quote emissions — subscribe_quotes is silent; the maker's on_quote_tick starves"
    );
}
