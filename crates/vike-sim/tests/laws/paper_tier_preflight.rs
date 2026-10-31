//! **A PAPER book of a roster venue holds the bracket the live adapter would refuse** (decision 0201),
//! end to end: the live core (`spawn_core`) over the real paper exchange (`PaperExecutionClient`, the
//! simulator's own fill primitives), the bracket lowered by `lower_bracket`'s capability preflight.
//!
//! `VenueCaps::reduce_only_kinds` lists what the venue's ADAPTER refuses by name (IG places a reduce-only
//! market only; Dukascopy's jar holds a native stop-loss AND take-profit, so its row refuses nothing a
//! bracket needs and it is the control here), and a bracket's exits are a reduce-only stop and
//! a reduce-only limit. An engine the mount told `paper` sends its orders to this simulator and never to
//! that adapter, so it is not judged by the axis; the same bracket on a demo or live engine is refused
//! whole. The unit matrices (`vike-venues`, `vike-core`) pin the verdicts; this proves the simulator
//! really does what the preflight now lets through: the entry fills, the held exits are released, the
//! take-profit fills at its limit and the stop-loss sibling is cancelled.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{AccountTier, RiskLimits, Strategy};
use vike_paper::{PaperExecutionClient, PaperFill, replay_bars};

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
    let b = |i: i64, o, h, l, c| crate::bars::bar(60_000 * i, o, h, l, c, 10.0);
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
    done: impl Fn(&[PaperFill], &[std::sync::Arc<str>]) -> bool,
) -> (Vec<PaperFill>, Vec<String>) {
    let client = PaperExecutionClient::new(venue, SYMBOL, 0.0, 0.0, 0.0);
    let fills: Arc<Mutex<Vec<PaperFill>>> = Arc::clone(&client.fills);
    let mut engine = ExecutionEngine::new(
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
            strategy: Some(StrategyMount {
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

/// IG (both exits refused by its adapter), told `paper`: the bracket is accepted, the simulator fills the entry
/// and the take-profit, and the ring confesses once per refused `(venue, kind)`. Dukascopy is the control since
/// its jar holds a native take-profit (its row lists `limit` among the reduce-only kinds): the same bracket
/// passes there with NOTHING to confess.
#[test]
fn a_paper_book_of_ig_holds_the_bracket_the_live_adapter_refuses_and_dukascopy_confesses_nothing() {
    for (venue, kinds) in [("ig", &["stop", "limit"][..]), ("dukascopy", &[] as &[&str])] {
        let (fills, ring) = run(venue, AccountTier::Paper, |fills, _| fills.len() >= 2);
        assert_eq!(
            fills.len(),
            2,
            "{venue}: the entry and the take-profit, nothing else: {fills:?}"
        );
        assert_eq!(fills[0].side, 1, "{venue}: the long entry fills first");
        assert_eq!(fills[1].side, -1, "{venue}: then the exit");
        assert!(
            fills[1].px >= TAKE_PROFIT - 1e-9,
            "{venue}: the take-profit fills at its limit or better: {fills:?}"
        );
        assert!(
            !ring.iter().any(|n| n.contains("REFUSED WHOLE")),
            "{venue}: the bracket was not refused: {ring:?}"
        );
        let notes: Vec<&String> = ring.iter().filter(|n| n.starts_with("paper:")).collect();
        assert_eq!(notes.len(), kinds.len(), "{venue}: one confession per refused kind: {notes:?}");
        for kind in kinds {
            let words = format!("the live {venue} adapter would refuse this ({kind})");
            assert!(notes.iter().any(|n| n.contains(&words)), "{venue}: `{words}` in {notes:?}");
        }
    }
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
