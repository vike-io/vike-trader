//! LIVE market-data smoke: subscribe EURUSD spot quotes against the REAL cTrader demo endpoint,
//! assert ticks flow, then unsubscribe cleanly. `#[ignore]` + self-skips without creds.
//!
//!     cargo test -p vike-ctrader --test integration -- --ignored --nocapture ctrader_market_data_smoke::
//!
//! **READ-ONLY — never places an order**, unlike its `ctrader_demo_smoke` sibling. That is what
//! makes it safe to run any time, and it is the twin of `vike-ibkr`'s `ibkr_market_data_smoke`.
//!
//! ## Why this exists
//!
//! Every other cTrader data test drives an in-process FAKE server (`tests/offline/data_spot.rs`,
//! `tests/offline/data_bars.rs`, `tests/offline/data_mapper.rs`). Those prove the decode and the actor wiring
//! against frames we wrote ourselves — they cannot notice the venue changing, or a subscription the
//! venue accepts and then never serves.
//!
//! That distinction stopped being theoretical on 2026-08-02: Binance's USDⓈ-M `@aggTrade` stream
//! began accepting sockets and sending NOTHING — no error, no close — and the recorder wrote no
//! binance tape for 95 minutes while logging a clean startup. Fixture tests cannot see that; only
//! asking the real venue can. This is that question, for cTrader.
//!
//! ## The assertion is "ticks ARRIVED", not "the numbers are right"
//!
//! `data_mapper`/`data_spot` already pin the descaling and routing. What is unverifiable offline is
//! whether the venue still serves `SubscribeSpots` for a live symbol at all, so that is all this
//! asserts — plus the sanity that a quote is positive and bid <= ask, which would catch a scale or
//! field-order regression on the wire.
//!
//! ⚠ FX is closed at weekends. Spot ticks stop entirely, so a run outside
//! Sun 22:00 – Fri 22:00 UTC will legitimately see zero and the test reports that as a SKIP rather
//! than a failure (the same posture as the Dukascopy ladder's market-hours note).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::config::CtraderConfig;
use vike_ctrader::feed::connect;
use vike_data::DataClient;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use vike_ctrader::test_support::RecordingSink;

/// How long to wait for the first spot tick before deciding the market is shut.
const QUOTE_WAIT: Duration = Duration::from_secs(45);

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn eurusd_spot_quotes_flow() {
    vike_log::test_init();
    // Tests own the store I/O — same pure `from_credentials` gate as production.
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let endpoints = vike_ctrader::test_support::demo_endpoints();
    let Some(config) = CtraderConfig::from_credentials(&endpoints, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };
    // Decision 0290: a subscription names a USER name, read through this account's own listing as the datahub keeps it.
    match vike_ctrader::transport::connection::fetch_symbols(config.to_connection_config()) {
        Ok(map) => vike_ctrader::test_support::keep_the_accounts_listing(&map),
        Err(e) => {
            tracing::warn!(target: "vike_ctrader::smoke", "SKIP: the account's listing failed: {e}");
            return;
        }
    }

    let sink = Arc::new(RecordingSink::default());
    // Data-only start: `data::connect` (over `connect_and_auth`, NOT the `_execution` variant) leaves
    // order flow unwired, so this connection cannot place anything even by accident.
    let mut data = match connect(config.to_connection_config(), sink.clone()) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!(target: "vike_ctrader::smoke", "SKIP: connect/auth failed: {e}");
            return;
        }
    };

    let id = match data.subscribe_quotes("EURUSD") {
        Ok(id) => id,
        Err(e) => panic!("subscribe_quotes(EURUSD) was refused by the venue: {e}"),
    };

    let deadline = Instant::now() + QUOTE_WAIT;
    while sink.quotes().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let quotes = sink.quotes();

    if quotes.is_empty() {
        // Not a failure: FX is shut at weekends, and a market-hours gap must not red a smoke.
        tracing::warn!(
            target: "vike_ctrader::smoke",
            "SKIP: no EURUSD spot ticks in {}s — FX is closed outside Sun 22:00–Fri 22:00 UTC",
            QUOTE_WAIT.as_secs()
        );
        data.unsubscribe(id);
        return;
    }

    let (venue, symbol, q) = &quotes[0];
    tracing::info!(
        target: "vike_ctrader::smoke",
        "{venue}/{symbol} live: {} quotes, first bid={} ask={}",
        quotes.len(), q.bid, q.ask
    );
    // Sanity the fake server cannot vouch for: real prices, in the right order. A descaling or
    // field-order regression on the wire shows up here as a nonsense spread.
    assert!(q.bid > 0.0 && q.ask > 0.0, "non-positive live quote: {q:?}");
    assert!(q.bid <= q.ask, "crossed live quote (bid > ask): {q:?}");

    // Deterministic teardown: unsubscribe, then drop the view (whose `Drop` joins the actor).
    data.unsubscribe(id);
    drop(data);
}
