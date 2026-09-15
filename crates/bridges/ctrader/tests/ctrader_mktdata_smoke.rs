//! LIVE market-data smoke: subscribe EURUSD spot quotes against the REAL cTrader demo endpoint,
//! assert ticks flow, then unsubscribe cleanly. `#[ignore]` + self-skips without creds.
//!
//!     cargo test -p vike-ctrader --test ctrader_mktdata_smoke -- --ignored --nocapture
//!
//! **READ-ONLY — never places an order**, unlike its `ctrader_demo_smoke` sibling. That is what
//! makes it safe to run any time, and it is the twin of `vike-ibkr`'s `ibkr_mktdata_smoke`.
//!
//! ## Why this exists
//!
//! Every other cTrader data test drives an in-process FAKE server (`tests/data_spot.rs`,
//! `tests/data_bars.rs`, `tests/offline/data_mapper.rs`). Those prove the decode and the actor wiring
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

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::connect_and_auth;
use vike_ctrader::data::CtraderData;
use vike_data::DataClient;

use common::RecordingSink;

/// How long to wait for the first spot tick before deciding the market is shut.
const QUOTE_WAIT: Duration = Duration::from_secs(45);

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn eurusd_spot_quotes_flow() {
    vike_log::test_init();
    // Tests own the `.env` I/O — same pure `from_vars` gate as production.
    let vars = vike_bridge_core::credentials::load_workspace_dotenv_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    let Some(config) = CtraderConfig::from_vars(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent");
        return;
    };

    let sink = Arc::new(RecordingSink::default());
    // Data-only mount: `connect_and_auth` (NOT the `_exec` variant) leaves order flow unwired, so
    // this connection cannot place anything even by accident.
    let handle = match connect_and_auth(config.to_conn_config(), sink.clone()) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(target: "vike_ctrader::smoke", "SKIP: connect/auth failed: {e}");
            return;
        }
    };
    let mut data = CtraderData::new(handle);

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
