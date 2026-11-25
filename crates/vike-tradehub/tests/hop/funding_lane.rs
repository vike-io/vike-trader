//! **The funding-rate lane end to end** (decision 0262): a venue's public funding rate goes from the
//! gatewayhub's venue client through the datahub's strategy lane into the tradehub's core, and a PAPER engine
//! there pays it at the settlement's own time on the position it holds.
//!
//! Every part is real but the venue: a scripted venue client in a gatewayhub on loopback (`DataLaneScript`, the
//! testkit) -> its wire sink, the call a venue's mark lane makes -> the tube (hop 1; the datahub's client asks
//! for funding frames because the gatewayhub advertises `stream_funding`) -> the datahub's `DataClientStream`,
//! `FeedBroker`, `DataClientSinkRouting` and strategy-lane session (hop 2; the tradehub's feed asks the same way) -> the
//! tradehub's `DataClientStrategyLane` -> the core's lane sink (`CoreLaneSink`) -> a real core (`spawn_core`) charging paper
//! funding as the production roots set it (`charge_paper_funding`, `vike_model::ContractValue::funding`'s law). The rig is `crates/vike-datahub/tests/price_hop.rs`'s
//! two-hop path with the core where its probe was.
//!
//! The numbers are the binance demo account's settlement of 2026-10-24 16:00 UTC (0.001 BTCUSDT long, demo
//! `fundingRate` 0.0001, `markPrice` 79,748.2, charged 0.00797482 USDT), as in `vike-simulator`'s paper-funding law.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vike_data::DataClientBuilder;

use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_data::{DataClient, DataClientSink, HistoryStore, HistoryStoreMemory};
use vike_datahub::feeds::{DataClientBrokerSharing, FeedBroker};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StrategyLane, StrategyLaneLimits};
use vike_datahub_client::strat::{
    DataClientStrategyLane, StrategyLaneTimings, StreamOpenerStrategyLane,
};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{PositionEntry, PositionKey};
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_hub_protocol::auth::HubKeys;
use vike_market_stream::market::DataClientStream;
use vike_marketdata::FundingRate;
use vike_model::AccountTier;
use vike_model::events::PositionSide;
use vike_tradehub_core::CoreLaneSink;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT.P";
const QTY: f64 = 0.001;
const MARK: f64 = 79_748.2;
const RATE: f64 = 0.0001;
const LANE_KEY: &[u8] = b"funding-lane-trade-key";
const WAIT: Duration = Duration::from_secs(15);

fn keys() -> HubKeys {
    HubKeys::new(b"funding-lane-observe".to_vec(), b"funding-lane-control".to_vec())
        .with_trade(LANE_KEY.to_vec())
}

/// A real gatewayhub over a scripted venue, and a real datahub whose strategy lane takes the venue through
/// the broker: the datahub's address, the scripted venue, and the gateway (held to the end of the test).
fn gateway_and_datahub() -> (String, DataLaneScript, LoopbackGateway) {
    let data = DataLaneScript::new(&[VENUE]);
    let gateway = LoopbackGateway::start(data.clone(), RestLaneScript::new());
    let link = gateway.link();
    let builder: DataClientBuilder = Box::new(move |label, sink| {
        DataClientStream::new(StreamOpenerGateway::new(link.clone()), label, sink)
            .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
            .map_err(|e| format!("the gatewayhub client could not start its reader: {e}"))
    });
    let broker = FeedBroker::new(builder, |_| DataClientBrokerSharing::Shared);
    let hub =
        StrategyLane::new(broker, vec![VENUE.to_string()], None, StrategyLaneLimits::PRODUCTION);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the datahub");
    let addr = listener.local_addr().expect("the datahub's port").to_string();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    std::thread::Builder::new()
        .name("funding-lane-datahub".into())
        .spawn(move || {
            let _ = serve_with_lanes(
                listener,
                store,
                None,
                Some(keys()),
                None,
                None,
                lanes,
                None,
                ServerTimings::PRODUCTION,
            );
        })
        .expect("spawn the datahub");
    (addr, data, gateway)
}

/// A real core whose one engine is a PAPER account holding [`QTY`] of the perpetual (no mark yet: the
/// venue's mark arrives over the lane), handed the production funding law.
fn paper_core() -> CoreHandle {
    let mut engine = engine_on(VENUE, SYMBOL, RecordingClient::default());
    engine.set_tier(AccountTier::Paper);
    let key: PositionKey = (VENUE.into(), SYMBOL.into(), PositionSide::Both);
    engine
        .account
        .positions
        .insert(key, PositionEntry { size: QTY, average_price: MARK, ..Default::default() });
    spawn_core(
        engine,
        CoreConfig { seed_cash: 10_000.0, charge_paper_funding: true, ..CoreConfig::default() },
    )
}

fn funding_paid(core: &CoreHandle) -> f64 {
    core.snapshot()
        .portfolio
        .venues
        .iter()
        .find(|v| v.venue == VENUE)
        .map_or(0.0, |v| v.funding_paid)
}

fn marked(core: &CoreHandle) -> bool {
    core.snapshot().marks.iter().any(|(v, s, px)| *v == VENUE && *s == SYMBOL && *px == MARK)
}

#[test]
fn a_venue_funding_rate_crosses_both_hops_and_a_paper_engine_pays_it_once_at_its_time() {
    let (datahub, data, _gateway) = gateway_and_datahub();
    let core = paper_core();
    let sink =
        Arc::new(CoreLaneSink::new(core.bar_sender(), core.market_sender(), core.tick_sender()));
    let opener = StreamOpenerStrategyLane::new(datahub, keys(), StrategyLaneTimings::default());
    let mut feed = DataClientStrategyLane::new(opener, VENUE, sink as Arc<dyn DataClientSink>)
        .expect("the feed");
    // The perpetual's bars, the subscription its mark lane (and so its funding) rides; the first subscribe
    // waits for the dial, and a transient refusal while the lane comes up is retried.
    let deadline = Instant::now() + WAIT;
    while let Err(why) = feed.subscribe_bars(SYMBOL, "1m") {
        assert!(Instant::now() < deadline, "the strategy lane never took the bars: {why:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let venue = data.session(0);
    // The whole path is up when the venue's mark reaches the core.
    let deadline = Instant::now() + WAIT;
    while !marked(&core) {
        assert!(Instant::now() < deadline, "the venue's mark never reached the core");
        venue.sink.mark_tick(VENUE, SYMBOL, MARK, vike_model::now_ms());
        std::thread::sleep(Duration::from_millis(20));
    }

    // The venue states its rate for a settlement 1.5 s ahead, once, and its mark keeps ticking.
    let settlement = vike_model::now_ms() + 1_500;
    venue.sink.funding(
        VENUE,
        SYMBOL,
        FundingRate {
            timestamp: vike_model::now_ms(),
            symbol: SYMBOL.to_string(),
            rate: RATE,
            interval_ms: Some(28_800_000),
            next_timestamp: Some(settlement),
            predicted: None,
            price: None,
        },
    );
    let tick = || {
        venue.sink.mark_tick(VENUE, SYMBOL, MARK, vike_model::now_ms());
        std::thread::sleep(Duration::from_millis(50));
    };
    while vike_model::now_ms() < settlement - 100 {
        tick();
        assert_eq!(funding_paid(&core), 0.0, "nothing is paid before the settlement");
    }
    let deadline = Instant::now() + WAIT;
    while funding_paid(&core) == 0.0 {
        assert!(Instant::now() < deadline, "the settlement was never booked");
        tick();
    }
    for _ in 0..6 {
        tick(); // a second booking would land by now
    }
    let paid = funding_paid(&core);
    let want = vike_model::ContractValue::linear(1.0).funding(QTY, MARK, RATE);
    assert!((paid + want).abs() < 1e-12, "paid {paid}, the law owes {want}");
    assert!((want - 0.00797482).abs() < 5e-9, "the binance demo charged 0.00797482: {want}");
    drop(feed);
    core.shutdown_and_join();
}

/// **A paper engine down across THREE settlements catches them up at the restart, through the datahub's real
/// stored-funding read** (decision 0262's amendment of 2026-11-08): a real datahub whose store holds the
/// venue's settlements (as its funding recorder or a backfill writes them) and whose strategy lane advertises
/// `funding_history`; the read the tradehub hands its core (`vike_datahub_client::strat::funding_history`, one
/// Trade-scope dial); a real core over the restored paper engine. The venue's first funding message after the
/// restart books the three settlements once, each at its stored rate and stored price (the engine's mark where
/// the store holds none), and later messages book nothing more.
#[test]
fn a_restarted_paper_engine_catches_up_three_settlements_through_the_datahubs_stored_series() {
    // Three settlements that passed while the node was down, eight hours apart, and the venue's next one ahead
    // of the clock (the core arms a timer there, which must not fire during the test).
    const H: i64 = 3_600_000;
    let t4 = (vike_model::now_ms() / H + 2) * H;
    let (t3, t2, t1) = (t4 - 8 * H, t4 - 16 * H, t4 - 24 * H);
    let point = |ts: i64, rate: f64, close: f64| vike_marketdata::Bar {
        timestamp: ts,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close,
        volume: 0.0,
        funding: Some(rate),
        bid: None,
        ask: None,
        symbol: None,
    };
    let store = HistoryStoreMemory::new();
    store
        .append_bars(
            VENUE,
            SYMBOL,
            vike_data::source::FUNDING_INTERVAL,
            &[point(t1, RATE, MARK), point(t2, 0.0002, 0.0), point(t3, -0.0001, 80_000.0)],
            None,
        )
        .expect("the stored settlements");
    let builder: DataClientBuilder = Box::new(
        |_: &str, _: Arc<dyn DataClientSink>| -> Result<Box<dyn DataClient + Send>, String> {
            Err("this test needs no venue client".to_string())
        },
    );
    let broker = FeedBroker::new(builder, |_| DataClientBrokerSharing::Shared);
    let hub =
        StrategyLane::new(broker, vec![VENUE.to_string()], None, StrategyLaneLimits::PRODUCTION);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the datahub");
    let datahub = listener.local_addr().expect("the datahub's port").to_string();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    std::thread::Builder::new()
        .name("funding-catchup-datahub".into())
        .spawn(move || {
            let _ = serve_with_lanes(
                listener,
                store,
                None,
                Some(keys()),
                None,
                None,
                lanes,
                None,
                ServerTimings::PRODUCTION,
            );
        })
        .expect("spawn the datahub");

    // Before the node went down: a paper engine told of the settlement at T1, snapshotted.
    let mut before = engine_on(VENUE, SYMBOL, RecordingClient::default());
    before.set_tier(AccountTier::Paper);
    let _ = before.note_funding_rate(SYMBOL, RATE, None, t1);
    let snap = before.snapshot_state();
    // The restart: a fresh paper engine, the journaled state put back, holding QTY (paper does nothing while down).
    let mut engine = engine_on(VENUE, SYMBOL, RecordingClient::default());
    engine.set_tier(AccountTier::Paper);
    engine.restore_runtime_state(&snap);
    let key: PositionKey = (VENUE.into(), SYMBOL.into(), PositionSide::Both);
    engine
        .account
        .positions
        .insert(key, PositionEntry { size: QTY, average_price: MARK, ..Default::default() });
    let read = datahub.clone();
    let core = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            charge_paper_funding: true,
            funding_history: Some(Box::new(
                move |venue: &str, symbol: &str, after: i64, until: i64| {
                    vike_datahub_client::strat::funding_history(
                        &read,
                        &keys(),
                        venue,
                        symbol,
                        after,
                        until,
                    )
                },
            )),
            ..CoreConfig::default()
        },
    );
    let sink = CoreLaneSink::new(core.bar_sender(), core.market_sender(), core.tick_sender());
    let deadline = Instant::now() + WAIT;
    while !marked(&core) {
        assert!(Instant::now() < deadline, "the mark never reached the core");
        sink.mark_tick(VENUE, SYMBOL, MARK, vike_model::now_ms());
        std::thread::sleep(Duration::from_millis(20));
    }
    let tell = || {
        sink.funding(
            VENUE,
            SYMBOL,
            FundingRate {
                timestamp: vike_model::now_ms(),
                symbol: SYMBOL.to_string(),
                rate: 0.0003,
                interval_ms: Some(28_800_000),
                next_timestamp: Some(t4),
                predicted: None,
                price: None,
            },
        );
    };
    let law =
        |price: f64, rate: f64| vike_model::ContractValue::linear(1.0).funding(QTY, price, rate);
    let owed = law(MARK, RATE) + law(MARK, 0.0002) + law(80_000.0, -0.0001);
    tell();
    let deadline = Instant::now() + WAIT;
    while (funding_paid(&core) + owed).abs() > 1e-12 {
        assert!(
            Instant::now() < deadline,
            "the three settlements were never booked: paid {}, owed {owed}",
            funding_paid(&core)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..5 {
        tell();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!((funding_paid(&core) + owed).abs() < 1e-12, "booked once: {}", funding_paid(&core));
    core.shutdown_and_join();
}
