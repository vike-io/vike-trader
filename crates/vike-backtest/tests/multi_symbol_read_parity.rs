//! THE READ HALF of the multi-symbol lane: does `Broker::position(sym)` / `price(sym)` /
//! `bars(sym)` answer about `sym` in BOTH engines?
//!
//! The two-leg routing program (#909/#911/#913/#914/#916/#917/#918/#922/#924/#927/#997) fixed the
//! WRITE half: an order verb's `symbol` argument used to be discarded live, so a two-leg strategy
//! backtested correctly and traded wrongly. `vike-core/tests/wiring/multi_symbol_routing.rs` is that
//! program's regression proof, and it covers orders and bar DELIVERY.
//!
//! It does not cover the READS. A strategy decides from `position`/`price`/`bars` and only then
//! submits; routing the submission correctly while answering the reads about the WRONG instrument
//! moves the divergence one step upstream instead of removing it — the strategy now sends a
//! correctly-addressed order it should never have sent.
//!
//! # Why the probe is portable, and why the law is intrinsic
//!
//! [`ReadProbe`] is `impl<B: Broker> Strategy<B>`, so the backtest engine and the live core run the
//! SAME source. But the two engines cannot be compared by tuple equality: a backtest step is
//! atomic (every symbol's bar `i` exists before any `on_bar` fires) while the live core is
//! event-driven (leg B's bar `i` has not arrived when leg A's dispatch runs), so `bars(B).len()`
//! legitimately differs. The comparison is therefore a LAW checked identically on both runs —
//! [`assert_reads_are_symbol_addressed`] — over observations that carry the same account state.
//! The two legs are given disjoint price bands (leg A in the 100s, leg B in the 50s) and opposite
//! positions, so a read that answers about the wrong instrument is unambiguous from one number.
//!
//! Positions are established differently per engine ON PURPOSE: the backtest probe submits and the
//! engine fills at the next bar, while the live run injects two `Event::Fill`s. The law is about
//! what a read ANSWERS, not about how the position got there, and using each engine's own natural
//! mechanism keeps the test free of a paper-fill dependency it does not need.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use vike_backtest::{EngineParams, StrategyEngine};
use vike_core::{spawn_core, CoreConfig, MountLeg, StrategyMount};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::{Bar, Broker, Strategy};

const VENUE: &str = "binance";
/// The MOUNTED symbol — leg A. Its closes live in the 100s.
const LEG_A: &str = "BTCUSDT";
/// The DECLARED second leg. Its closes live in the 50s, so one close identifies the series.
const LEG_B: &str = "ETHUSDT";
const INTERVAL: &str = "1m";

/// Leg A ends up LONG this much, leg B SHORT this much — distinct magnitudes AND distinct signs,
/// so a position read that answers about the wrong leg cannot coincidentally look right.
const POS_A: f64 = 3.0;
const POS_B: f64 = -7.0;

const STEPS: usize = 4;

fn bar(ts: i64, px: f64, symbol: &str) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        // The BACKTEST caller stamps this; live, the runtime's `Ingest::BarClose` arm does (#924).
        symbol: Some(symbol.to_string()),
    }
}

fn series(symbol: &str, base: f64) -> Vec<Bar> {
    (0..STEPS).map(|i| bar(60_000 + i as i64 * 60_000, base + i as f64, symbol)).collect()
}

/// What the strategy could SEE at one dispatch. Every field is a read the portable `Broker`
/// surface promises to answer about the symbol NAMED, not about whatever series happens to be
/// dispatching.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    /// `bar.symbol` of the bar this dispatch carried — which series woke the strategy.
    dispatch: Option<String>,
    pos_a: f64,
    pos_b: f64,
    px_a: f64,
    px_b: f64,
    /// `bars(LEG_A).last().close`, `0.0` when that series is empty.
    last_a: f64,
    last_b: f64,
}

/// PORTABLE by construction (`impl<B: Broker> Strategy<B>`) — the backtest and the live core mount
/// the very same code, which is what makes a disagreement between them a RUNTIME defect rather
/// than two strategy copies drifting.
struct ReadProbe {
    looks: Arc<Mutex<Vec<Look>>>,
    /// Backtest only: submit the two legs once, so the engine's own fill path establishes the
    /// positions the law is read against. The live run injects fills instead (see the module doc).
    seed: bool,
    seeded: bool,
}

impl<B: Broker> Strategy<B> for ReadProbe {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.looks.lock().unwrap().push(Look {
            dispatch: bar.symbol.clone(),
            pos_a: broker.position(LEG_A),
            pos_b: broker.position(LEG_B),
            px_a: broker.price(LEG_A),
            px_b: broker.price(LEG_B),
            last_a: broker.bars(LEG_A).last().map_or(0.0, |b| b.close),
            last_b: broker.bars(LEG_B).last().map_or(0.0, |b| b.close),
        });
        if self.seed && !self.seeded {
            self.seeded = true;
            broker.submit_market(LEG_A, 1, POS_A);
            broker.submit_market(LEG_B, -1, -POS_B);
        }
    }
}

/// Observations from the point both engines carry the same account state. The first two dispatches
/// are the seeding step (backtest) / the pre-leg-B-bar step (live), and are deliberately excluded:
/// they differ for reasons that are about the ENGINES' cadence, not about symbol addressing.
const SETTLED_FROM: usize = 2;

fn settled(looks: &[Look]) -> &[Look] {
    assert!(
        looks.len() > SETTLED_FROM,
        "the probe must observe past the seeding step, got {} looks",
        looks.len()
    );
    &looks[SETTLED_FROM..]
}

/// THE LAW, applied identically to both engines' observations.
///
/// A read names a symbol; the answer must be about THAT symbol, whichever series woke the
/// strategy. Nothing here mentions an engine — that is the point.
fn assert_reads_are_symbol_addressed(looks: &[Look], engine: &str) {
    for (i, l) in looks.iter().enumerate() {
        assert_eq!(
            l.pos_a, POS_A,
            "{engine}[{i}] dispatch={:?}: position({LEG_A}) must be leg A's own position \
             ({POS_A}); reading {} means the answer came from whichever series dispatched",
            l.dispatch, l.pos_a
        );
        assert_eq!(
            l.pos_b, POS_B,
            "{engine}[{i}] dispatch={:?}: position({LEG_B}) must be leg B's own position ({POS_B})",
            l.dispatch
        );
        assert!(
            (100.0..200.0).contains(&l.px_a),
            "{engine}[{i}] dispatch={:?}: price({LEG_A}) = {} is outside leg A's price band — a \
             price answered about the wrong instrument",
            l.dispatch,
            l.px_a
        );
        assert!(
            (50.0..100.0).contains(&l.px_b),
            "{engine}[{i}] dispatch={:?}: price({LEG_B}) = {} is outside leg B's price band",
            l.dispatch,
            l.px_b
        );
    }
}

// ---------------------------------------------------------------------------------------------
// the two runs
// ---------------------------------------------------------------------------------------------

fn run_backtest() -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = ReadProbe { looks: Arc::clone(&looks), seed: true, seeded: false };
    let params = EngineParams { cash: 1_000_000.0, ..EngineParams::default() };
    let mut engine = StrategyEngine::new(
        vec![(LEG_A.to_string(), series(LEG_A, 100.0)), (LEG_B.to_string(), series(LEG_B, 50.0))],
        probe,
        params,
    );
    engine.run();
    let out = looks.lock().unwrap().clone();
    out
}

fn fill(symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // minted by this helper — same `t-<symbol>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", symbol),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn run_live() -> Vec<Look> {
    let looks = Arc::new(Mutex::new(Vec::new()));
    let probe = ReadProbe { looks: Arc::clone(&looks), seed: false, seeded: true };

    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        LEG_A,
    );
    engine.extra_symbols = vec![LEG_B.into()];

    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            // THE DECLARATION — without it the mount never receives leg B's bars at all and the
            // per-symbol read tables are not built.
            symbols: vec![MountLeg::same_venue(LEG_B)],
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: LEG_A.into(),
            interval: INTERVAL.into(),
            strategy: Box::new(probe),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    // Establish the positions the law reads against, BEFORE any bar, so every observation sees
    // them. Injected as venue fills — the live engine's own inbound path.
    handle.event_sender().blocking_send(Event::Fill(fill(LEG_A, 1, POS_A, 100.0))).unwrap();
    handle.event_sender().blocking_send(Event::Fill(fill(LEG_B, -1, -POS_B, 50.0))).unwrap();

    let (a, b) = (series(LEG_A, 100.0), series(LEG_B, 50.0));
    for i in 0..STEPS {
        // The same per-step order the backtest fans out in: leg A then leg B.
        for (sym, bars) in [(LEG_A, &a), (LEG_B, &b)] {
            handle
                .bar_sender()
                .close(BarUpdate {
                    venue: VENUE.into(),
                    symbol: sym.into(),
                    interval: INTERVAL.into(),
                    // Live bars arrive symbol-less; the runtime stamps the series symbol (#924).
                    bar: Bar { symbol: None, ..bars[i].clone() },
                })
                .unwrap();
        }
    }
    handle.shutdown_and_join();
    let out = looks.lock().unwrap().clone();
    out
}

// ---------------------------------------------------------------------------------------------
// the gates
// ---------------------------------------------------------------------------------------------

/// Both engines wake the strategy on the same series in the same order — the precondition that
/// makes every comparison below about the READS rather than about dispatch scheduling.
#[test]
fn both_engines_dispatch_the_same_series_in_the_same_order() {
    let bt: Vec<Option<String>> = run_backtest().iter().map(|l| l.dispatch.clone()).collect();
    let live: Vec<Option<String>> = run_live().iter().map(|l| l.dispatch.clone()).collect();
    assert_eq!(bt, live, "the two engines must fan the same bars out to the same hooks");
    assert!(
        bt.iter().any(|d| d.as_deref() == Some(LEG_B)),
        "leg B must dispatch at all, else every law below holds vacuously: {bt:?}"
    );
}

/// THE BACKTEST SIDE OF THE LAW — the reference. `SimBroker` indexes every read by symbol, so this
/// has always held; it is asserted so the law itself is proven non-vacuous before the live run is
/// judged against it.
#[test]
fn backtest_reads_are_symbol_addressed() {
    assert_reads_are_symbol_addressed(settled(&run_backtest()), "backtest");
}

/// **THE GATE.** The same probe, the same law, the live core.
///
/// FAILS ON THE PRE-FIX CODE: `declared_views` built the per-symbol tables from the mount's
/// DECLARED legs only, leaving the mount's OWN symbol absent from them. `LiveBroker::declared_read`
/// then missed on `position(LEG_A)` and fell through to the scalar — which is the DISPATCHING
/// series' position — so during a leg-B bar `position(BTCUSDT)` answered with leg B's position.
///
/// That is not hypothetical. `vike_strategy::pairs::PairsZScore` runs `evaluate` from whichever
/// leg's bar completes the pair, and its `flatten` does `broker.position(&sym)` for BOTH legs and
/// submits `p.abs()` at `closing_side(p)`. Reached from a leg-B dispatch it read leg B's signed
/// size for leg A and therefore closed leg A with the wrong QUANTITY and, whenever the two legs'
/// signs differ (which for a beta-weighted spread is the normal case), the wrong SIDE — silently,
/// and only when live.
#[test]
fn live_reads_are_symbol_addressed() {
    assert_reads_are_symbol_addressed(settled(&run_live()), "live");
}

/// **THE INVERSION.** This test used to be `live_bars_still_ignore_the_symbol_argument`, a
/// CHARACTERIZATION pinning `bars`' wrong answer so it was wrong on the record rather than by
/// accident: `LiveBroker::bars` discarded its `symbol` argument (it was spelled `_symbol`) and
/// returned the DISPATCHING series, so `bars(LEG_B)` on a leg-A bar handed back leg A's history and
/// a strategy computing a spread from it read one leg twice. The pin asserted
/// `last_b == last_a`. It now asserts the opposite, which is what makes this the regression proof —
/// exactly as `both_legs_route_to_their_own_symbols` was for the order half.
///
/// `LiveBroker::bar_views` is the per-symbol bar table that closed it, built by the same
/// `CoreThread::declared_views` pass — and off the same per-leg venue rule — that already answered
/// `position`/`price`. `Broker::bars`' own licence ("single-symbol live brokers may ignore
/// `symbol`") is unaffected and still exercised: the table is EMPTY for an undeclared mount, which
/// is every mount `vike-run` builds, and the argument is ignored there byte-identically.
///
/// ⚠ NOT an equality against the backtest, deliberately — see the module doc. The engines' cadences
/// differ (a backtest step is atomic; live, leg B's bar `i` has not arrived when leg A's dispatch
/// runs), so at this dispatch live sees leg B's PREVIOUS close and the backtest its current one.
/// The law both must satisfy is that the answer comes from leg B's own series, which the disjoint
/// price bands make unambiguous from one number.
#[test]
fn live_bars_are_symbol_addressed() {
    let looks = run_live();
    let leg_a_dispatch = looks
        .iter()
        .skip(SETTLED_FROM)
        .find(|l| l.dispatch.as_deref() == Some(LEG_A))
        .expect("a settled leg-A dispatch");

    assert!(
        (100.0..200.0).contains(&leg_a_dispatch.last_a),
        "leg A's own history is correct on its own dispatch: {leg_a_dispatch:?}"
    );
    assert!(
        (50.0..100.0).contains(&leg_a_dispatch.last_b),
        "`bars({LEG_B})` must be leg B's OWN series on a leg-A dispatch, not the dispatching one \
         — {} is outside leg B's price band ({leg_a_dispatch:?}). `SimBroker::bars` indexes by \
         symbol and always answered this correctly (see `backtest_bars_are_symbol_addressed`).",
        leg_a_dispatch.last_b
    );
}

/// The backtest half of the law above — the behaviour the live path had to reach, and the proof it
/// is not vacuous (`SimBroker` has always indexed `bars` by symbol).
#[test]
fn backtest_bars_are_symbol_addressed() {
    let looks = run_backtest();
    let l = looks
        .iter()
        .skip(SETTLED_FROM)
        .find(|l| l.dispatch.as_deref() == Some(LEG_A))
        .expect("a settled leg-A dispatch");
    assert!((100.0..200.0).contains(&l.last_a), "bars({LEG_A}) is leg A's series: {l:?}");
    assert!(
        (50.0..100.0).contains(&l.last_b),
        "bars({LEG_B}) is leg B's OWN series even on a leg-A dispatch: {l:?}"
    );
}
