//! LIVE end-to-end market-maker smoke over the daemon's own tick pumps (network, NO credentials, NO venue
//! order: a LOCAL accepting client), one test per public-stream venue of the live-smoke lane:
//!     cargo test -p vike-gatewayhub --test server -- --ignored --nocapture --test-threads=1 market_maker_smoke::
//!
//! The venue's live public feed, started by the production pump table
//! (`crates/vike-gatewayhub/src/data/feed_tick.rs`'s `feed_tick_table`, over the release seed's addresses), drives
//! `SpreadMaker` in a local core: the maker rests a tagged bid + ask and RE-QUOTES them in place via modify as
//! the real mid moves. Orders go to a local accepting client, so no order leaves the box and no credential is
//! read: there is nothing to self-skip on, and every test is `#[ignore]`d because it dials the venue's public
//! streams. It proves the composition (live market data -> `on_quote_tick` -> tagged submit -> modify) end to
//! end, the same proof on binance, bybit and okx.
//!
//! It moved here from the binance bridge's tests (binance only), so one venue-neutral smoke covers the three
//! venues and no bridge needs a `vike-strategies-check` dev edge. As a member of a group binary it installs no global
//! `tracing` subscriber (`crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation"), so its result lines
//! go to stdout.
//!
//! Needs the `data` feature (the pump table); the default build compiles this file to nothing.
#![cfg(feature = "data")]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, Mount, spawn_core};
use vike_execution::testing::accept_submit;
use vike_execution::{Account, BalanceMode, Engine, ExecutionClient, RiskGate};
use vike_gatewayhub::data::feed_tick::feed_tick_table;
use vike_model::OrderRequest;
use vike_model::RiskLimits;
use vike_model::events::{Event, OrderModified};
use vike_strategies_check::makers::spread_maker::SpreadMaker;

/// Accepts every order (so it rests, modifiable) and counts modifications — no real orders leave the box.
struct MakerClient {
    events: VecDeque<Event>,
    modifications: Arc<AtomicUsize>,
}

impl ExecutionClient for MakerClient {
    fn submit(&mut self, request: &OrderRequest) {
        accept_submit(&mut self.events, request);
    }
    fn cancel(&mut self, _order_client_id: &str) {}
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifications.fetch_add(1, Ordering::Relaxed);
        self.events.push_back(Event::OrderModified(OrderModified {
            order_client_id: order.order_client_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            new_trigger_price: None,
            timestamp: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// `SpreadMaker` on `venue`'s live public feed of `symbol` re-quotes IN PLACE: once it has modified four
/// times, the core holds exactly one resting bid and one resting ask.
fn spread_maker_runs_on_live_data(venue: &str, symbol: &str, tick_size: f64) {
    let modifications = Arc::<AtomicUsize>::default();
    let engine = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        MakerClient { events: VecDeque::new(), modifications: Arc::clone(&modifications) },
        venue,
        symbol,
    );
    let config = CoreConfig {
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: venue.into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            strategy: Box::new(SpreadMaker::new(0.001, 5.0)), // 0.001 a side, ±5 off mid
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let pumps = feed_tick_table(vike_secrets::venue_setting::seeded_venue_settings());
    let pump = pumps(venue, symbol, tick_size, handle.tick_sender())
        .unwrap_or_else(|why| panic!("the seed holds every {venue} row: {why}"));

    // wait until the maker has re-quoted on live data at least a few times
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let n = modifications.load(Ordering::Relaxed);
        if n >= 4 {
            let snap = cell.load();
            println!(
                "{venue} SpreadMaker live: {n} re-quotes, {} resting orders",
                snap.orders.len()
            );
            // modify-in-place, NOT churn: exactly the two tagged orders regardless of re-quote count
            assert_eq!(snap.orders.len(), 2, "{venue}: one bid + one ask, re-priced in place");
            assert!(snap.orders.iter().any(|o| o.side == 1), "{venue}: resting bid");
            assert!(snap.orders.iter().any(|o| o.side == -1), "{venue}: resting ask");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{venue}: maker idle after 25s ({n} modifications) — market closed or stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    drop(pump); // the last owner of the pump's stop handle: stops and joins its thread
    handle.shutdown_and_join();
    println!(
        "{venue} market-maker green: live feed → tagged bid/ask → live modify re-quoting (2 orders, no churn)"
    );
}

#[test]
#[ignore = "network — public Binance streams, run by hand (see module doc)"]
fn binance_spread_maker_runs_on_live_data() {
    spread_maker_runs_on_live_data("binance", "BTCUSDT", 0.01);
}

#[test]
#[ignore = "network — public Bybit streams, run by hand (see module doc)"]
fn bybit_spread_maker_runs_on_live_data() {
    spread_maker_runs_on_live_data("bybit", "BTCUSDT", 0.1);
}

#[test]
#[ignore = "network — public OKX streams, run by hand (see module doc)"]
fn okx_spread_maker_runs_on_live_data() {
    spread_maker_runs_on_live_data("okx", "BTC-USDT-SWAP", 0.1);
}
