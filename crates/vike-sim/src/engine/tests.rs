use super::*;
use vike_model::Strategy;

/// A deliberately SYMMETRIC portfolio strategy: it routes on the bar's own symbol tag and
/// asks for the same target weight on every instrument, holding no cross-symbol state of its
/// own. That symmetry is what makes the test a test — any asymmetry in the ANSWER is then the
/// engine's, which is the thing under examination.
struct TargetEach {
    pct: f64,
}

impl Strategy<SimBroker> for TargetEach {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        // `default_venue` is unset below, so `format_instrument` leaves the bare symbol.
        let sym = bar.symbol.clone().expect("StrategyEngine::new tags every bar");
        ctx.strategy_order_target_percent(&sym, self.pct);
    }
}

/// Six aligned bars whose level and drift are a function of `base`, so the three instruments
/// have genuinely different returns — without that, dropping a different one of them would
/// not move the account and the defect test below could not fire.
fn series(base: f64, drift: f64) -> Vec<Bar> {
    (0..6)
        .map(|i| {
            let px = base * (1.0 + drift * i as f64);
            Bar {
                ts: 1_700_000_000_000 + i as i64 * 60_000,
                open: px,
                high: px * 1.01,
                low: px * 0.99,
                close: px * 1.002,
                volume: 1_000_000.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

fn bars_of(symbol: &str) -> Vec<Bar> {
    match symbol {
        "AAA" => series(100.0, 0.03),
        "BBB" => series(10.0, -0.01),
        "CCC" => series(1.0, 0.07),
        other => panic!("unknown fixture symbol {other:?}"),
    }
}

/// Run the same universe in the given LIST ORDER under the given mode.
///
/// `max_open_positions = 2` over three instruments that all want to open on the same step is
/// what makes the cross-section BIND: exactly one of the three is refused every run, and
/// which one is refused is precisely the allocator's tie-break — the decision
/// [`DecideMode`] is about. `cash_gate` is set on BOTH arms so the two differ in the
/// tie-break alone and not in which fill lane ran.
fn run_order(order: &[&str], decide: DecideMode) -> vike_analytics::BacktestResult {
    let loaded: Vec<(String, Vec<Bar>)> =
        order.iter().map(|s| ((*s).to_string(), bars_of(s))).collect();
    let p = EngineParams {
        cash: 10_000.0,
        fee_rate: 0.001,
        slippage: 0.0005,
        cash_gate: true,
        max_open_positions: 2,
        decide,
        ..Default::default()
    };
    StrategyEngine::new(loaded, TargetEach { pct: 0.4 }, p).run()
}

/// Per-symbol PnL as a NAME-KEYED, name-sorted list — the comparison that is meaningful
/// across two different list orders. `BacktestResult::per_symbol_pnl` is emitted in `symbols`
/// order, so comparing it positionally across a permutation would compare different
/// instruments to each other and "fail" for a reason that is not the property under test.
fn pnl_by_name(r: &vike_analytics::BacktestResult) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> =
        r.per_symbol_pnl.iter().map(|(s, p)| (s.clone(), p.to_bits())).collect();
    v.sort();
    v
}

const FORWARD: &[&str] = &["AAA", "BBB", "CCC"];
const REVERSED: &[&str] = &["CCC", "BBB", "AAA"];

/// THE POINT OF THE MODE: the same universe, typed in two orders, answers identically —
/// bit-for-bit, not approximately. Equity is compared on its BITS deliberately: the f64
/// fold order of `SimBroker::equity_now` is one of the dependencies the mode closes, so a
/// tolerance here would hide exactly the failure this asserts against.
#[test]
fn simultaneous_is_invariant_under_a_symbol_list_permutation() {
    let a = run_order(FORWARD, DecideMode::Simultaneous);
    let b = run_order(REVERSED, DecideMode::Simultaneous);
    assert_eq!(
        a.final_equity.to_bits(),
        b.final_equity.to_bits(),
        "simultaneous: final equity moved with the symbol list order ({} vs {})",
        a.final_equity,
        b.final_equity
    );
    assert_eq!(a.n_trades, b.n_trades, "simultaneous: trade COUNT moved with the list order");
    assert_eq!(
        pnl_by_name(&a),
        pnl_by_name(&b),
        "simultaneous: per-symbol PnL moved with the list order"
    );
    assert_eq!(
        a.equity_curve.iter().map(|e| e.to_bits()).collect::<Vec<_>>(),
        b.equity_curve.iter().map(|e| e.to_bits()).collect::<Vec<_>>(),
        "simultaneous: the equity CURVE moved with the list order"
    );
}

/// THE DEFECT, pinned — and the proof that the test above is not vacuous.
///
/// Under the default mode the same universe in two orders answers DIFFERENTLY, because the
/// allocator's tie-break is the collection order and the open-position cap refuses whichever
/// instrument the operator happened to type last. If this ever passes, the fixture has stopped
/// reaching the mechanism and the invariance test beside it is proving nothing.
#[test]
fn sequential_is_order_dependent_the_defect_itself() {
    let a = run_order(FORWARD, DecideMode::Sequential);
    let b = run_order(REVERSED, DecideMode::Sequential);
    assert_ne!(
        pnl_by_name(&a),
        pnl_by_name(&b),
        "the default mode answered IDENTICALLY under both symbol orders, so this fixture no \
             longer reaches the list-order dependency and \
             simultaneous_is_invariant_under_a_symbol_list_permutation is now vacuous — give both \
             tests a fixture where the cross-section binds"
    );
}

/// The default mode is the FROZEN answer, not merely an equivalent one: naming
/// `DecideMode::Sequential` explicitly must produce the identical run to a profile that names
/// nothing, on every field a fixture compares. Cheap, and it is the assertion that would
/// catch the `cross_section` indirection having quietly changed a fold's order.
#[test]
fn naming_sequential_is_byte_identical_to_the_default() {
    let named = run_order(FORWARD, DecideMode::Sequential);
    let defaulted = run_order(FORWARD, DecideMode::default());
    assert_eq!(named.final_equity.to_bits(), defaulted.final_equity.to_bits());
    assert_eq!(named.n_trades, defaulted.n_trades);
    assert_eq!(pnl_by_name(&named), pnl_by_name(&defaulted));
}
