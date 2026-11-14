//! **A PAPER engine is not judged by the live adapter's reduce-only axis** (decision 0201), end to end:
//! the live core (`spawn_core`) over the real paper exchange (`ExecutionClientPaper`, the simulator's own
//! fill primitives), the bracket lowered by `lower_bracket`'s capability preflight.
//!
//! `VenueCapabilities::reduce_only_kinds` lists what the venue's ADAPTER refuses by name (IG places a reduce-only
//! market only; Dukascopy's jar holds a native stop-loss AND take-profit, so its row refuses nothing a
//! bracket needs), and a bracket's exits are a reduce-only stop and a reduce-only limit. An engine the
//! mount told `paper` sends its orders to this simulator and never to that adapter, so it is not judged
//! by the axis; the same bracket on a demo or live engine is refused whole. The unit matrices
//! (`vike-venues`, `vike-core`) pin the verdicts; this proves the simulator really does what the preflight
//! lets through on Dukascopy (the entry fills, the held exits are released, the take-profit fills at its
//! limit and the stop-loss sibling is cancelled), and that on IG the core lets the exits through, says so,
//! and the paper book then refuses the order: an IG epic's unit is its market row's contract size, which a
//! paper book does not hold (`vike_paper::paper_contract_value`, decision 0170's catalog amendment).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, LiveBroker, Mount, spawn_core};
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{AccountTier, Fill, RiskLimits, Strategy};
use vike_paper::{ExecutionClientPaper, replay_bars};

const SYMBOL: &str = "EURUSD";
const INTERVAL: &str = "1m";
const STOP: f64 = 98.0;
const TAKE_PROFIT: f64 = 102.0;

/// One long bracket on the first bar, market entry, and nothing after.
struct BracketOnce {
    sent: bool,
}

impl Strategy<LiveBroker> for BracketOnce {
    fn on_bar(&mut self, ctx: &mut LiveBroker, _bar: &Bar) {
        if !self.sent {
            self.sent = true;
            ctx.submit_bracket(1, 1.0, None, STOP, TAKE_PROFIT);
        }
    }
}

/// Bar 1 is the strategy's trigger; the entry fills at bar 2's open (the paper book's next-open rule),
/// which releases the exits; bar 3 trades through the take-profit (high 103) without touching the stop.
fn bars() -> Vec<Bar> {
    let b = |i: i64, o, h, l, c| vike_simulator::test_support::bar(60_000 * i, o, h, l, c, 10.0);
    vec![
        b(1, 100.0, 100.5, 99.5, 100.0),
        b(2, 100.0, 101.0, 99.5, 100.5),
        b(3, 100.5, 103.0, 100.0, 102.0),
        b(4, 102.0, 102.5, 101.5, 102.0),
    ]
}

/// Run the bracket on a `venue` engine told `tier`, wait until `done` says the run is settled, and return
/// the paper fills and the core's recent-events ring.
fn run(
    venue: &str,
    tier: AccountTier,
    done: impl Fn(&[Fill], &[std::sync::Arc<str>]) -> bool,
) -> (Vec<Fill>, Vec<String>) {
    let client = ExecutionClientPaper::new(venue, SYMBOL, 0.0, 0.0, 0.0);
    let fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let mut engine = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        SYMBOL,
    );
    engine.set_tier(tier); // what the mount does for every engine it builds
    let handle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: venue.to_string(),
                symbol: SYMBOL.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(BracketOnce { sent: false }),
            }),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    replay_bars(&handle.bar_sender(), venue, SYMBOL, INTERVAL, &bars());
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if done(&fills.lock().unwrap(), &cell.load_full().recent_events) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.shutdown_and_join();
    let ring = cell.load_full().recent_events.iter().map(|n| n.to_string()).collect();
    let fills = fills.lock().unwrap().clone();
    (fills, ring)
}

/// Dukascopy, told `paper` (its jar holds a native take-profit, so its row lists `limit` among the reduce-only
/// kinds): the bracket is accepted, the simulator fills the entry and the take-profit, and nothing is confessed.
/// IG, told `paper` (both exits refused by its adapter): the core lets the bracket through and the ring confesses
/// once per refused `(venue, kind)`, and the paper book refuses the entry, since only an IG market row states what
/// one unit is worth, so nothing fills.
#[test]
fn a_paper_bracket_fills_on_dukascopy_and_on_ig_the_core_confesses_and_the_book_refuses_the_epic() {
    let notes_of = |ring: &[String]| -> Vec<String> {
        ring.iter().filter(|n| n.starts_with("paper:")).cloned().collect()
    };

    let (fills, ring) = run("dukascopy", AccountTier::Paper, |fills, _| fills.len() >= 2);
    assert_eq!(fills.len(), 2, "the entry and the take-profit, nothing else: {fills:?}");
    assert_eq!(fills[0].side, 1, "the long entry fills first");
    assert_eq!(fills[1].side, -1, "then the exit");
    assert!(fills[1].price >= TAKE_PROFIT - 1e-9, "the take-profit fills at its limit: {fills:?}");
    assert!(!ring.iter().any(|n| n.contains("REFUSED WHOLE")), "not refused: {ring:?}");
    assert!(notes_of(&ring).is_empty(), "nothing to confess: {ring:?}");

    let (fills, ring) = run("ig", AccountTier::Paper, |_, ring| {
        ring.iter().filter(|n| n.starts_with("paper:")).count() >= 2
    });
    assert!(!ring.iter().any(|n| n.contains("REFUSED WHOLE")), "ig: not refused whole: {ring:?}");
    let notes = notes_of(&ring);
    assert_eq!(notes.len(), 2, "ig: one confession per refused kind: {notes:?}");
    for kind in ["stop", "limit"] {
        let words = format!("the live ig adapter would refuse this ({kind})");
        assert!(notes.iter().any(|n| n.contains(&words)), "ig: `{words}` in {notes:?}");
    }
    assert!(fills.is_empty(), "ig: the paper book refuses an epic it holds no row of: {fills:?}");
}

/// The contrast, on the same simulator: told `demo` or `live`, the engine is judged by the venue's whole
/// row, so the bracket is refused whole and the simulator never sees an order.
#[test]
fn the_same_bracket_on_a_demo_or_live_engine_is_refused_whole_and_nothing_fills() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        // Only IG: Dukascopy takes the bracket whole since its jar holds a native take-profit.
        let venue = "ig";
        let (fills, ring) = run(venue, tier, |_, ring| {
            ring.iter().any(|n| n.contains(&format!("bracket for {venue} REFUSED WHOLE")))
        });
        assert!(
            ring.iter().any(|n| n.contains(&format!("bracket for {venue} REFUSED WHOLE"))),
            "{venue} {tier:?}: refused whole, in words: {ring:?}"
        );
        assert!(fills.is_empty(), "{venue} {tier:?}: nothing was sent: {fills:?}");
        assert!(
            !ring.iter().any(|n| n.starts_with("paper:")),
            "{venue} {tier:?}: nothing was let through: {ring:?}"
        );
    }
}
