//! Live smoke for the `ticker.{instrument}.100ms` bid/ask streaming feed (Part B) — connects to REAL
//! Deribit mainnet (public, keyless, not geo-blocked, read-only: no auth, no order, no account).
//! `#[ignore]`d so CI never dials the venue; run it explicitly to prove the feed connects →
//! subscribes → decodes a live top-of-book:
//!
//! ```sh
//! cargo test -p vike-deribit --test ticker_bidask_live_smoke -- --ignored --nocapture
//! ```
//!
//! Discovers a real, currently-listed near-the-money BTC option off the public book-summary (so the
//! subscribed instrument id never rots at expiry), subscribes its `ticker`, and asserts a row with a
//! POSITIVE bid AND ask arrives within a few seconds — the runtime twin of the fixture-pinned
//! `parse_ticker` unit test in `options_feed.rs`. Run during an open, liquid session (an ATM/ITM BTC
//! call reliably has a resting bid).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_deribit::chain::DeribitOptionsProvider;
use vike_deribit::options_feed::{MAINNET_WS, TickerRow, spawn_deribit_ticker_feed};

#[test]
#[ignore = "hits real Deribit mainnet WS; run explicitly with --ignored"]
fn ticker_streams_live_bidask_for_a_real_btc_option() {
    // 1) Discover a real BTC option near the money off the public book-summary — hardcoding an id
    //    would rot the moment that expiry rolls off, so we resolve one live.
    let mut provider = DeribitOptionsProvider::new();
    let expiries = provider.list_expiries("BTC").expect("list BTC expiries");
    // Prefer a non-0DTE expiry (0DTE wings can be thin); fall back to the very front if that's all.
    let expiry = expiries
        .iter()
        .find(|e| e.dte >= 1)
        .unwrap_or_else(|| expiries.first().expect("at least one BTC expiry"))
        .date
        .clone();
    let chain = provider.fetch_chain("BTC", &expiry, Some(8), 0.0).expect("fetch BTC chain");
    let spot = chain.underlying_price.expect("chain carries an underlying spot");
    // The call closest to spot that actually has a resting bid (an ATM/ITM call reliably does).
    let inst = chain
        .rows
        .iter()
        .filter_map(|r| r.call.as_ref())
        .filter(|q| q.bid.is_some_and(|b| b > 0.0))
        .min_by(|a, b| (a.strike - spot).abs().total_cmp(&(b.strike - spot).abs()))
        .and_then(|q| q.instrument_name.clone())
        .expect("a BTC call with a live bid (run during an open, liquid session)");
    println!("subscribing ticker for {inst} (expiry {expiry}, spot {spot})");

    // 2) Subscribe its ticker and wait for a decoded row with a POSITIVE bid AND ask.
    let (tx, rx) = mpsc::channel::<TickerRow>();
    let feed = spawn_deribit_ticker_feed(MAINNET_WS.to_string(), vec![inst.clone()], move |row| {
        // A dead receiver (the test already asserted) just drops the row — never panics the feed.
        let _ = tx.send(row);
    });

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut got: Option<TickerRow> = None;
    while Instant::now() < deadline {
        let Ok(row) = rx.recv_timeout(Duration::from_secs(20)) else { break };
        assert_eq!(row.instrument_name, inst, "only the subscribed instrument should stream");
        if row.best_bid.is_some_and(|b| b > 0.0) && row.best_ask.is_some_and(|a| a > 0.0) {
            got = Some(row);
            break;
        }
    }
    let row = got.expect("a ticker row with a positive bid AND ask within the window");
    let (bid, ask) = (row.best_bid.unwrap(), row.best_ask.unwrap());
    println!(
        "live ticker {inst}: bid {bid} ask {ask} mark {:?} iv%(percent) {:?} oi {:?} vol {:?}",
        row.mark_price, row.mark_iv, row.open_interest, row.volume
    );
    // bid/ask are COIN units (a fraction of BTC); a real resting book is not crossed.
    assert!(bid <= ask, "bid {bid} must not exceed ask {ask}");
    // The load-bearing UNIT contract: `mark_iv` here is a PERCENT (unlike markprice's decimal `iv`).
    // A real BTC option IV in percent sits well above 1 and well below 1000.
    if let Some(iv) = row.mark_iv {
        assert!(iv > 1.0 && iv < 1000.0, "mark_iv {iv} looks off for a percent-encoded IV");
    }

    feed.shutdown();
}
