//! The order language across the boundary (`ABI_VERSION` 5): the `tactics_cross` fixture, compiled
//! in as a `#[path]` module of this binary (its `build::<OrderBrokerSimulator>` called directly) and built as a
//! strategy file from the same bytes, run on the bar lane
//! over a tick grid under two sets of in-bar rules, and compared bit for bit: the result, the trades,
//! the metrics, the orders left working, and every fill and order end the engine delivered, with its
//! tag. The same artifact then
//! runs a TWO-symbol rule (`ABI_VERSION` 8, `pair_diffs`): a hedge on a second symbol through
//! `Tactics::on`, compared the same way over a two-symbol run.
//!
//! ⚠ **Two rule sets, because the seam answers differently under each.** With the simulator's
//! defaults a reduce-only order is refused while flat (the live meaning), so an exit issued with its
//! entry goes as one order list (`OrderBroker::place_group`); with the reference rules it rests at
//! once (`OrderBroker::rests_reduce_only_while_flat` answers `true`), a close at this bar's close and
//! the fills-a-day close take the simulator's own fills. Both runs reuse the ONE artifact, built
//! inside the headline test (trap 5), never in a `#[test]` of their own.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_model::{Fill, OrderEvent, RiskLimits, Strategy};
use vike_rust_strategy::host::StrategyFileHost;
use vike_simulator::{InBarRules, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

use super::diffs::{metric_diffs, pending_diffs, result_shape_diffs, trade_diffs};
use super::drive::{Run, build_through_the_production_builder, load_or_panic};
use super::series::{engine_params, inputs_of, pair_universe, universe};
use super::{
    OTHER_SYMBOL, TACTICS_NO_INPUT_PARAMS, TACTICS_NO_LEDGER_PARAMS, TACTICS_PAIR_PARAMS_TOML,
    TACTICS_PARAMS_TOML, TACTICS_SOURCE, tactics_cross,
};

/// The tick the engine's grid states, so `OrderBroker::instrument_grid` answers and the rule snaps every
/// level to it: a strategy file that read no grid would send other prices.
const TICK: f64 = 0.05;

/// The two in-bar rule sets the rule runs under, named.
fn rule_sets() -> [(&'static str, InBarRules); 2] {
    let reference = InBarRules {
        path_walk: true,
        fill_at_close: true,
        price_order: true,
        queue_order: true,
        committed_stops: true,
        trail_from_fill: true,
        reduce_only_at_fill: true,
    };
    [("defaults", InBarRules::default()), ("reference", reference)]
}

/// Records every fill and every order end the engine delivers, in order, then forwards each hook
/// the bar lane emits unchanged, so the run it describes is the run compared.
struct TagSpy {
    inner: Box<dyn Strategy<OrderBrokerSimulator>>,
    log: Rc<RefCell<Vec<String>>>,
}

impl Strategy<OrderBrokerSimulator> for TagSpy {
    fn warmup(&self) -> usize {
        self.inner.warmup()
    }
    fn on_start(&mut self, b: &mut OrderBrokerSimulator) {
        self.inner.on_start(b);
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        self.inner.on_bar(b, bar);
    }
    fn on_schedule(&mut self, b: &mut OrderBrokerSimulator, tag: &str) {
        self.inner.on_schedule(b, tag);
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, f: &Fill) {
        let line =
            format!("{} fill {:?} {} {:?} @ {:?}", f.timestamp, f.tag, f.side, f.size, f.price);
        self.log.borrow_mut().push(line);
        self.inner.on_fill(b, f);
    }
    fn on_order_event(&mut self, b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.log.borrow_mut().push(format!("end {:?} {:?}", event.tag, event.kind));
        self.inner.on_order_event(b, event);
    }
    fn on_stop(&mut self, b: &mut OrderBrokerSimulator) {
        self.inner.on_stop(b);
    }
}

/// One run of `strategy` under `rules` over `universe`, and the log of what it was delivered. The
/// run loads the inputs the strategy declares, read before it is wrapped (the spy forwards no
/// `inputs`).
fn drive(
    strategy: Box<dyn Strategy<OrderBrokerSimulator>>,
    rules: InBarRules,
    universe: Vec<(String, Vec<Bar>)>,
) -> (Run, Vec<String>) {
    let inputs = inputs_of(&strategy.inputs(), false);
    let log = Rc::default();
    let spy = TagSpy { inner: strategy, log: Rc::clone(&log) };
    let grid = RiskLimits { tick_size: Some(TICK), ..RiskLimits::new() };
    let params =
        StrategyEngineConfig { risk_limits: Some(grid), in_bar: rules, inputs, ..engine_params() };
    let mut engine = StrategyEngine::new(universe, spy, params);
    let result = engine.run();
    let pending = engine.core.symbol.iter().flat_map(|s| s.pending.clone()).collect();
    (Run { result, pending }, log.take())
}

/// The compiled-in half: the fixture module's own `build`, its bound and all.
fn compiled_strategy(params_toml: &str) -> Box<dyn Strategy<OrderBrokerSimulator>> {
    let params: Params = toml::from_str(params_toml).expect("valid TOML");
    tactics_cross::build::<OrderBrokerSimulator>(&params)
}

fn compiled(params_toml: &str, rules: InBarRules) -> (Run, Vec<String>) {
    drive(compiled_strategy(params_toml), rules, universe())
}

fn strategy_file(so: &Path, params_toml: &str) -> Box<dyn Strategy<OrderBrokerSimulator>> {
    Box::new(StrategyFileHost::<OrderBrokerSimulator>::new(
        load_or_panic(so).vtable,
        params_toml,
        "{}",
    ))
}

fn loaded(so: &Path, rules: InBarRules) -> (Run, Vec<String>) {
    drive(strategy_file(so, TACTICS_PARAMS_TOML), rules, universe())
}

/// The prefixes of the tags whose FILLS the compiled run must hold: the entries both ways, the exits
/// of both, the trail of the short side and the close of it. A run missing one did not take that
/// path, so the comparison would agree about it for nothing.
const MUST_FILL: [&str; 6] =
    ["\"long#", "\"short#", "\"long guard#", "\"short guard#", "\"short trail#", "\"drop short#"];

/// The tag prefix of the two-symbol row's orders on the second symbol (`<symbol>/<name>#<n>`).
const HEDGE: &str = "\"ETHUSDT/hedge#";

/// How many fills in `log` name a tag starting with `prefix` (which opens with the tag's quote).
fn fills(log: &[String], prefix: &str) -> usize {
    log.iter().filter(|l| l.contains(" fill ") && l.contains(prefix)).count()
}

/// Refuses a compiled run that took too few of the paths this fixture exists for. A floor, set
/// below what the run measures.
fn assert_not_vacuous(label: &str, run: &Run, log: &[String]) {
    let r = &run.result;
    assert!(r.n_trades >= 8, "[{label}] the rule must trade repeatedly, got {}", r.n_trades);
    assert!(
        r.trades.iter().any(|t| t.is_long) && r.trades.iter().any(|t| !t.is_long),
        "[{label}] the rule must trade both ways"
    );
    for prefix in MUST_FILL {
        assert!(fills(log, prefix) > 0, "[{label}] no fill of a {prefix}…\" order:\n{log:#?}");
    }
    assert!(
        log.iter().any(|l| l.starts_with("end Some(\"dip#") && l.contains("Canceled")),
        "[{label}] no free order was cancelled, so `OrderBroker::cancel` crossed for nothing"
    );
}

/// Every difference between a compiled run and a strategy file's run of the same rule.
fn run_diffs(c: &Run, c_log: &[String], p: &Run, p_log: &[String]) -> Vec<String> {
    let mut d = result_shape_diffs(&c.result, &p.result);
    d.extend(trade_diffs(&c.result.trades, &p.result.trades));
    d.extend(metric_diffs(&c.result, &p.result));
    d.extend(pending_diffs(&c.pending, &p.pending));
    if c_log != p_log {
        let at = c_log.iter().zip(p_log).position(|(a, b)| a != b).unwrap_or(c_log.len());
        d.push(format!(
            "the delivered fills and order ends differ ({} vs {} lines), first at line {at}: \
             compiled {:?} vs strategy file {:?}",
            c_log.len(),
            p_log.len(),
            c_log.get(at),
            p_log.get(at)
        ));
    }
    d
}

/// Builds the fixture (once), runs both halves under both rule sets, prints what the compiled run
/// took, and returns every difference, labelled. CALLED from the headline test.
pub(super) fn tactics_diffs() -> Vec<String> {
    let so = build_through_the_production_builder(TACTICS_SOURCE, "tactics_cross");
    // Every set's figures are printed before any is judged, so one red run reports them all.
    let runs =
        rule_sets().map(|(label, rules)| (label, rules, compiled(TACTICS_PARAMS_TOML, rules)));
    for (label, _, (c, c_log)) in &runs {
        let counts: Vec<String> = ["\"cap close#", "\"long touch#", "\"dip#"]
            .iter()
            .chain(MUST_FILL.iter())
            .map(|p| format!("{p}…\" {}", fills(c_log, p)))
            .collect();
        let cancels = c_log.iter().filter(|l| l.contains("Canceled")).count();
        println!(
            "MEASURED tactics [{label}]: {} closed trades, {} log lines, {cancels} cancelled, \
             fills by tag: {}",
            c.result.n_trades,
            c_log.len(),
            counts.join(", ")
        );
    }
    let mut diffs = Vec::new();
    for (label, rules, (c, c_log)) in runs {
        assert_not_vacuous(label, &c, &c_log);
        let (p, p_log) = loaded(&so, rules);
        let d = run_diffs(&c, &c_log, &p, &p_log);
        diffs.extend(d.into_iter().map(|x| format!("[tactics, {label}] {x}")));
    }
    diffs.extend(pair_diffs(&so));
    diffs
}

/// The two-symbol row (`ABI_VERSION` 8): the same artifact under [`TACTICS_PAIR_PARAMS_TOML`], a
/// rule that hedges on a second symbol (`Tactics::on`), run over both symbols under both rule sets
/// and compared as above, and the symbols it declares (`Strategy::symbols`, `vike_tactics::RuleRun`'s
/// own answer) compared too. Before 8 a strategy file's order on another symbol was refused in the
/// guest; now it reaches the simulator as the compiled-in one's does, so the hedge's fills must be
/// in both logs, bit for bit.
fn pair_diffs(so: &Path) -> Vec<String> {
    let mut diffs = Vec::new();
    let (c_symbols, p_symbols) = (
        compiled_strategy(TACTICS_PAIR_PARAMS_TOML).symbols(),
        strategy_file(so, TACTICS_PAIR_PARAMS_TOML).symbols(),
    );
    assert_eq!(c_symbols, [OTHER_SYMBOL], "the compiled rule declares the second symbol");
    if c_symbols != p_symbols {
        diffs.push(format!(
            "[tactics pair] the declared symbols differ: compiled {c_symbols:?} vs strategy file \
             {p_symbols:?}"
        ));
    }
    for (label, rules) in rule_sets() {
        let (c, c_log) = drive(compiled_strategy(TACTICS_PAIR_PARAMS_TOML), rules, pair_universe());
        let hedges = fills(&c_log, HEDGE);
        println!(
            "MEASURED tactics pair [{label}]: {} closed trades, {} log lines, {hedges} hedge fills",
            c.result.n_trades,
            c_log.len()
        );
        assert!(
            hedges > 0 && fills(&c_log, "\"long#") > 0,
            "[pair, {label}] the rule must fill on both symbols, or this row compares nothing \
             across the symbol slot:\n{c_log:#?}"
        );
        let p = strategy_file(so, TACTICS_PAIR_PARAMS_TOML);
        let (p, p_log) = drive(p, rules, pair_universe());
        let d = run_diffs(&c, &c_log, &p, &p_log);
        diffs.extend(d.into_iter().map(|x| format!("[tactics pair, {label}] {x}")));
    }
    diffs
}

/// The rule's `ABI_VERSION` 6 reads decide what it trades, under both rule sets: the input it
/// declares (`Tactics::input`) and the closed net (`Tactics::closed_net`), each turned off alone,
/// change its trades. Otherwise the strategy file half reproducing them would prove nothing about the
/// `inputs` export or the two reads. The compiled half alone: no cargo, so a `#[test]` of its own.
#[test]
fn the_rules_input_and_closed_net_change_what_it_trades() {
    for (off, key) in
        [(TACTICS_NO_INPUT_PARAMS, "input"), (TACTICS_NO_LEDGER_PARAMS, "ledger_gain")]
    {
        for (label, rules) in rule_sets() {
            let (on, _) = compiled(TACTICS_PARAMS_TOML, rules);
            let (without, _) = compiled(off, rules);
            assert!(
                !trade_diffs(&on.result.trades, &without.result.trades).is_empty(),
                "[{label}] turning `{key}` OFF changed nothing the rule traded, so that read decides \
                 nothing in this fixture and the comparison would agree about it for nothing"
            );
        }
    }
}
