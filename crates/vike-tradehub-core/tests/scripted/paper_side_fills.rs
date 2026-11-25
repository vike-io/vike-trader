//! The owner's ruling of 2026-11-16 END TO END: paper fills like the real venue, never at the mid.
//! Scripted quotes go through the SAME [`vike_tradehub_core::TickSink`] a cTrader or Polymarket
//! mount's feed drives, whose synthesizer folds each window's mid bar AND its two sides; the core
//! hands both to the paper book (`ExecutionClient::on_bar_sides`), which fills a buy on the ask's path.
//! The owner's Polymarket example: bid 0.48 / ask 0.52, a market buy of 100 costs 52.00, never the
//! mid's 50.00.

use crate::common;

use std::time::Duration;

use vike_core::LiveBroker;
use vike_data::DataClientSink;
use vike_execution::{Command, OrderIntent};
use vike_log::poll::wait_until;
use vike_marketdata::TickQuote;
use vike_model::{OrderRequest, OrderType, Strategy};
use vike_tradehub_core::{
    MountPlacement, StrategyCorePaperConfig, TickSink, build_paper_strategy_core_with,
};

/// This binary's own operator-HALT sentinel root (`common::no_halt`): a path it names and never creates.
const HALT_ROOT_PREFIX: &str = "vike-tradehub-core-paper-side-fills-owns-this-halt-";

const VENUE: &str = "polymarket";
const TOKEN: &str = "SCRIPT_OUTCOME_TOKEN";

/// A mount that trades nothing itself: the order is the operator's.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn quote(sink: &TickSink, ts: i64, bid: f64, ask: f64) {
    sink.quote(
        VENUE,
        TOKEN,
        TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1_000.0,
            ask_size: 1_000.0,
            symbol: TOKEN.into(),
        },
    );
}

#[test]
fn a_paper_market_buy_through_the_tick_sink_pays_the_ask_never_the_mid() {
    vike_log::test_init();
    let (_halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
    let mount = build_paper_strategy_core_with(
        Box::new(Idle),
        &MountPlacement::new(VENUE, TOKEN, 1_000.0, 0.01),
        StrategyCorePaperConfig { halt, ..Default::default() },
    );
    let sink = TickSink::new(&mount.handle, VENUE, TOKEN, "1m", 60_000);

    // window 0 opens on the 0.48 / 0.52 book (mid 0.50); the order fills at that quote, at once or as the
    // quote comes if the order reached the book first (decision 0285's amendment: the quote is the clock)
    quote(&sink, 1_000, 0.48, 0.52);
    mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: "buy-100".into(),
        venue: VENUE.into(),
        symbol: TOKEN.into(),
        side: 1,
        qty: 100.0,
        order_type: OrderType::Market,
        ..Default::default()
    }))));
    assert!(
        wait_until(Duration::from_secs(10), || mount.handle.snapshot().order("buy-100").is_some()),
        "the market order never reached the paper book"
    );
    // a quote in window 1 closes window 0: its bar and its sides reach the book
    quote(&sink, 61_000, 0.48, 0.52);
    assert!(
        wait_until(Duration::from_secs(10), || !mount.fills.lock().unwrap().is_empty()),
        "the market order never filled"
    );
    let fill = mount.fills.lock().unwrap()[0].clone();
    assert_eq!((fill.side, fill.size, fill.price), (1, 100.0, 0.52), "the ask, never the mid 0.50");
    assert_eq!(fill.price * fill.size, 52.0, "a market buy of 100 costs 52.00");

    let cell = mount.handle.snapshot_cell();
    mount.handle.shutdown_and_join();
    assert!(cell.load_full().fault.is_none());
}

/// **An IG paper market buy fills at the ASK, and the engine books the epic at its contract size** (the owner's
/// ruling of 2026-11-16), end to end through the core. Before ig's demo login states the size, the epic's order is
/// refused in the owner's words; the node then states it as the journaled `AdmitSymbol` of the paper engine's
/// mounted symbol (`TradeLeaseRegistry::state_paper_units` in `vike-tradehub`), the engine values the epic at
/// 10 000 (a Mini's `contractSize`) and tells its book, and the next market buy pays the ask, never the mid.
#[test]
fn an_ig_paper_market_buy_fills_at_the_ask_at_the_epics_stated_contract_size() {
    const EPIC: &str = "CS.D.EURUSD.MINI.IP";
    vike_log::test_init();
    let (_halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
    let mount = build_paper_strategy_core_with(
        Box::new(Idle),
        &MountPlacement::new("ig", EPIC, 1_000_000.0, 0.00001),
        StrategyCorePaperConfig { halt, ..Default::default() },
    );
    let sink = TickSink::new(&mount.handle, "ig", EPIC, "1m", 60_000);
    let quote = |ts: i64| {
        sink.quote(
            "ig",
            EPIC,
            TickQuote {
                timestamp: ts,
                local_timestamp: 0,
                bid: 1.08495,
                ask: 1.08505,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: EPIC.into(),
            },
        );
    };
    let buy = |order_client_id: &str| {
        Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "ig".into(),
            symbol: EPIC.into(),
            side: 1,
            qty: 1.0,
            order_type: OrderType::Market,
            ..Default::default()
        })))
    };
    let reason = |order_client_id: &str| {
        mount.handle.snapshot().order(order_client_id).and_then(|o| o.reason.clone())
    };

    quote(1_000);
    mount.handle.send_command(buy("unstated"));
    assert!(
        wait_until(Duration::from_secs(10), || reason("unstated").is_some()),
        "the unstated epic's order must be refused, never left silent"
    );
    let refused = reason("unstated").unwrap();
    assert!(refused.starts_with("no price: no demo login for ig"), "{refused}");

    mount.handle.send_command(Command::AdmitSymbol(Box::new(vike_execution::SymbolAdmission {
        route_key: "ig".into(),
        symbol: EPIC.into(),
        grid: vike_model::SymbolGrid::default(),
        contract_value: Some(vike_model::ContractValue::linear(10_000.0)),
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    })));
    assert!(
        wait_until(Duration::from_secs(10), || {
            mount.handle.snapshot().contract_value_of("ig", EPIC)
                == vike_model::ContractValue::linear(10_000.0)
        }),
        "the engine books the epic at its stated contract size"
    );
    mount.handle.send_command(buy("stated"));
    assert!(
        wait_until(Duration::from_secs(10), || mount.handle.snapshot().order("stated").is_some()),
        "the market order never reached the paper book"
    );
    assert_eq!(reason("stated"), None, "taken once the size is stated");
    quote(61_000);
    assert!(
        wait_until(Duration::from_secs(10), || !mount.fills.lock().unwrap().is_empty()),
        "the market order never filled"
    );
    let fill = mount.fills.lock().unwrap()[0].clone();
    assert_eq!((fill.side, fill.size, fill.price), (1, 1.0, 1.08505), "the ask, never the mid");

    let cell = mount.handle.snapshot_cell();
    mount.handle.shutdown_and_join();
    assert!(cell.load_full().fault.is_none());
}

/// **A paper book starts on the ONE paper balance whatever its mount states** (the owner's ruling of 2026-11-16, what
/// the shipped daemon hands as `StrategyCorePaperConfig::account_seed`): a mount stating `seed_cash` 1,000 still opens
/// a 100,000 paper account, so a 52,000 buy passes the 1x margin gate at the ask; a builder handed no `account_seed` (a
/// rehearsal's) keeps the mount's own 1,000 and denies it.
#[test]
fn a_paper_book_starts_on_the_one_paper_balance_whatever_its_mount_states() {
    vike_log::test_init();
    let buy = |account_seed: Option<f64>| {
        let (_halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
        let mut risk_limits = vike_model::RiskLimits::new();
        risk_limits.initial_margin_requirement = Some(1.0);
        let mount = build_paper_strategy_core_with(
            Box::new(Idle),
            &MountPlacement::new(VENUE, TOKEN, 1_000.0, 0.01),
            StrategyCorePaperConfig { halt, risk_limits, account_seed, ..Default::default() },
        );
        let sink = TickSink::new(&mount.handle, VENUE, TOKEN, "1m", 60_000);
        quote(&sink, 1_000, 0.48, 0.52);
        mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            order_client_id: "buy-100k".into(),
            venue: VENUE.into(),
            symbol: TOKEN.into(),
            side: 1,
            qty: 100_000.0,
            order_type: OrderType::Market,
            ..Default::default()
        }))));
        let mut ts = 1_000;
        let settled = wait_until(Duration::from_secs(10), || {
            ts += 1_000;
            quote(&sink, ts, 0.48, 0.52);
            let snap = mount.handle.snapshot();
            !mount.fills.lock().unwrap().is_empty()
                || snap.recent_events.iter().any(|e| e.contains("insufficient-margin"))
        });
        assert!(settled, "the buy neither filled nor was denied");
        let filled = mount.fills.lock().unwrap().first().map(|f| f.price);
        mount.handle.shutdown_and_join();
        filled
    };
    assert_eq!(buy(Some(vike_tradehub_core::PAPER_SEED_CASH)), Some(0.52), "the ask, on 100,000");
    assert_eq!(buy(None), None, "a rehearsal's own 1,000 denies it");
}
