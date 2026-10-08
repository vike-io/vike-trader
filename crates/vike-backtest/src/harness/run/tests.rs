//! Store-backed `run_backtest` tests over a concrete `DataFusionHist` fixture.

use super::*;

// The concrete store the tests build; the non-test code drives `run_backtest` through the
// `Arc<dyn HistStore>` seam, so this import belongs to the tests alone.
use vike_data::DataFusionHist;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_model::SymbolProperties;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// `strategy.params.symbol` is set explicitly (rather than relying on `BuyHold`'s `bar.symbol`
/// fallback): bar-mode always sets `EngineParams.default_venue`, which re-tags each bar's
/// `symbol` field as `"SYMBOL.VENUE"` (`format_instrument`) for filter-grid lookups —a
/// different string from the bare symbol key `SimBroker` indexes positions by (see
/// `tests/filters_fills.rs`'s `OpenClose` doc comment). An explicit `symbol` param sidesteps
/// that mismatch identically to how a real strategy would.
fn bar_profile(kind: &str, extra_engine: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "{kind}"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0
{extra_engine}

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "{SYMBOL}"
"#
    )
}

#[test]
fn bar_mode_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let profile = BacktestProfile::from_toml_str(&bar_profile("bar", "")).unwrap();
    let result = run_backtest(&profile, store).unwrap();

    // BuyHold opens a 1.0-unit position on the first bar and holds — no CLOSED trade, but
    // final_equity tracks the mark against the last bar's close (100.0 cash spent -> equity
    // re-marked at 102.0), so equity has moved off the starting cash.
    assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
}

#[test]
fn bar_mode_no_symbol_param_runs_without_panic() {
    // The footgun fix: a plain bar-mode profile (no `snap_to_properties`) must NOT set
    // `default_venue`, so bars keep their BARE symbol and `buy_hold` routes on `bar.symbol`
    // with no magic `strategy.params.symbol`. Before the fix this panicked ("unknown symbol").
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let toml = format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#
    );
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();
    let result = run_backtest(&profile, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "buy_hold ran and opened a position (no symbol param)");
}

#[test]
fn tick_mode_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
    store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();

    let profile = BacktestProfile::from_toml_str(&bar_profile("tick", "")).unwrap();
    let result = run_backtest(&profile, store).unwrap();

    assert_ne!(result.final_equity, 1000.0, "buy_hold should open a position and move equity");
}

#[test]
fn tick_snap_to_properties_gates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let quotes = vec![quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)];
    store.append_quotes(VENUE, SYMBOL, &quotes, None).unwrap();
    store
        .append_symbol_properties(
            VENUE,
            SYMBOL,
            &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
            None,
        )
        .unwrap();

    // snap ON: the opening buy (size 1.0) is below the recorded min_qty -> gated -> no fill.
    let profile_on =
        BacktestProfile::from_toml_str(&bar_profile("tick", "snap_to_properties = true")).unwrap();
    let result_on = run_backtest(&profile_on, store.clone()).unwrap();
    assert_eq!(result_on.final_equity, 1000.0, "snap-on must gate the sub-min_qty opening buy");

    // snap OFF (default): raw replay, the buy fills.
    let profile_off =
        BacktestProfile::from_toml_str(&bar_profile("tick", "snap_to_properties = false")).unwrap();
    let result_off = run_backtest(&profile_off, store).unwrap();
    assert_ne!(result_off.final_equity, 1000.0, "snap-off must fill the opening buy");
}

#[test]
fn bar_snap_to_properties_gates() {
    // Bar-mode snapping is a DISTINCT path from tick: `run_backtest` sets `params.properties`
    // directly (bars bypass replay_ticks) AND sets `default_venue` (needed as the properties
    // venue key), so `bar_profile` supplies the explicit `strategy.params.symbol`.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();
    store
        .append_symbol_properties(
            VENUE,
            SYMBOL,
            &[(0, SymbolProperties { min_qty: 1e9, ..Default::default() })],
            None,
        )
        .unwrap();

    // snap ON: the opening buy (size 1.0) is below the recorded min_qty → gated → no fill.
    let profile_on =
        BacktestProfile::from_toml_str(&bar_profile("bar", "snap_to_properties = true")).unwrap();
    let result_on = run_backtest(&profile_on, store.clone()).unwrap();
    assert_eq!(result_on.final_equity, 1000.0, "bar snap-on must gate the sub-min_qty open");

    // snap OFF: raw fill.
    let profile_off =
        BacktestProfile::from_toml_str(&bar_profile("bar", "snap_to_properties = false")).unwrap();
    let result_off = run_backtest(&profile_off, store).unwrap();
    assert_ne!(result_off.final_equity, 1000.0, "bar snap-off must fill the open");
}

/// `EngineCfg::emulator_release_stops` reaches the BAR-mode `EngineParams` construction site
/// (`bar_engine_params`): absent, a profile carries the harness default `false` (matching
/// `EngineParams::default()`); an explicit `true` survives into the built params. Before this
/// fix, `bar_engine_params`'s literal assigned neither and `..Default::default()` silently
/// discarded the profile's value on this lane (the tick lane's own inline literal had the same
/// hole — see `EngineCfg::emulator_release_stops`'s doc).
#[test]
fn emulator_release_stops_survives_into_engine_params() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let profile_absent = BacktestProfile::from_toml_str(&bar_profile("bar", "")).unwrap();
    let params = bar_engine_params(&profile_absent, &store).unwrap();
    assert!(!params.emulator_release_stops, "absent -> the harness default, false");

    let profile_true =
        BacktestProfile::from_toml_str(&bar_profile("bar", "emulator_release_stops = true"))
            .unwrap();
    let params = bar_engine_params(&profile_true, &store).unwrap();
    assert!(params.emulator_release_stops, "explicit true must survive into EngineParams");

    let profile_false =
        BacktestProfile::from_toml_str(&bar_profile("bar", "emulator_release_stops = false"))
            .unwrap();
    let params = bar_engine_params(&profile_false, &store).unwrap();
    assert!(!params.emulator_release_stops, "explicit false must survive into EngineParams");
}

/// A bar profile with extra `[data]` lines — the twin of [`bar_profile`], which splices into
/// `[engine]` instead. `data.warmup` and `data.detail_interval` are `[data]` keys, so neither
/// can be reached through that helper. The range is wide enough for a multi-DAY fixture (the
/// detail-tape tests need one coarse bar per day and 24 finer bars inside each).
fn data_profile(extra_data: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1d"
from = "0"
to = "300000000"
{extra_data}

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#
    )
}

/// Parse a profile WITHOUT `BacktestProfile::validate` — serde only.
///
/// ⚠ Load-bearing for the refusal tests below, not a shortcut. `from_toml_str` validates, and
/// the cross-table half of these keys' rules (`detail_interval` against `[walkforward]` and
/// `engine.cash_gate`) belongs in `BacktestProfile::refusals`; the moment the resolvers are
/// ALSO called from there, a `from_toml_str` on a deliberately-bad profile would fail at the
/// parse instead of reaching the resolver under test, and the assertion would silently stop
/// testing the thing it names. Going through serde alone keeps each test pinned to the
/// resolver that owns the sentence.
fn parse_unvalidated(toml_src: &str) -> BacktestProfile {
    toml::from_str::<BacktestProfile>(toml_src).expect("the fixture is well-formed TOML")
}

/// `data.warmup` reaches `EngineParams::warmup` through `bar_engine_params`, in both span
/// shapes — and absent leaves it `None`, which is the byte-identical no-op every profile
/// written before the key existed relies on.
///
/// The duration arm is the one worth a case each way: `"3d"` over `1d` bars is exactly 3, and
/// `"25h"` over `1d` bars is 1.04 bars of history — which must round UP to 2, because the key
/// states a MINIMUM and 1 bar is not 25 hours.
#[test]
fn warmup_reaches_engine_params_in_both_span_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let absent = BacktestProfile::from_toml_str(&data_profile("")).unwrap();
    assert_eq!(
        bar_engine_params(&absent, &store).unwrap().warmup,
        None,
        "an absent data.warmup must leave the gate at exactly Strategy::warmup()"
    );

    let bars = BacktestProfile::from_toml_str(&data_profile("warmup = \"200bars\"")).unwrap();
    assert_eq!(bar_engine_params(&bars, &store).unwrap().warmup, Some(200));

    let exact = BacktestProfile::from_toml_str(&data_profile("warmup = \"3d\"")).unwrap();
    assert_eq!(bar_engine_params(&exact, &store).unwrap().warmup, Some(3));

    let rounds_up = BacktestProfile::from_toml_str(&data_profile("warmup = \"25h\"")).unwrap();
    assert_eq!(
        bar_engine_params(&rounds_up, &store).unwrap().warmup,
        Some(2),
        "a duration that is not a whole number of bars rounds UP — 1 bar is not 25 hours"
    );
}

/// The `data.warmup` spellings that are refused rather than coerced, each by name.
///
/// A calendar span has no fixed step count, a duration has no meaning on a tick tape, and a
/// duration over an unparseable base interval has no divisor — in every one of those cases the
/// alternative to refusing is silently picking a number, which is the irreproducible answer
/// the key exists to remove.
#[test]
fn warmup_refuses_the_spellings_it_cannot_resolve() {
    let months = parse_unvalidated(&data_profile("warmup = \"3mo\""));
    let err = months.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("CALENDAR span"), "got {err:?}");

    // Tick lane: the count spelling is fine (the gate is an EVENT index), the duration is not.
    let tick = parse_unvalidated(
        &data_profile("warmup = \"7d\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    let err = tick.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("bar-mode only"), "got {err:?}");
    let tick_count = parse_unvalidated(
        &data_profile("warmup = \"200bars\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    assert_eq!(tick_count.data.warmup_steps().unwrap(), Some(200));

    // A duration needs the base interval as its divisor, so an unparseable one is refused
    // HERE rather than guessed at.
    let bad_base = parse_unvalidated(
        &data_profile("warmup = \"7d\"").replace("interval = \"1d\"", "interval = \"1fortnight\""),
    );
    let err = bad_base.data.warmup_steps().unwrap_err().to_string();
    assert!(err.contains("data.interval"), "got {err:?}");

    // The grammar itself refuses a zero count, so there is no spelling meaning "no warm-up".
    let zero = parse_unvalidated(&data_profile("warmup = \"0bars\""));
    assert!(zero.data.warmup_steps().is_err());
}

/// The gate the key opens is REAL, and the number the report carries is the number the run
/// gated on — the pair that makes a declared warm-up auditable rather than merely accepted.
///
/// Three bars and `warmup = "3bars"` means `index >= 3` is never true, so `buy_hold` never
/// submits and equity never leaves the starting cash; the control (no key) opens a position on
/// the first bar. `BacktestResult::warmup` reading 3 is what lets
/// `vike_analytics::zero_trade` diagnose the flat run as `warmup-shortfall` instead of
/// reporting a strategy that "never signalled".
#[test]
fn a_declared_warmup_gates_the_run_and_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &bars, None).unwrap();

    let gated = BacktestProfile::from_toml_str(&data_profile("warmup = \"3bars\"")).unwrap();
    let result = run_backtest(&gated, store.clone()).unwrap();
    assert_eq!(
        result.final_equity, 1000.0,
        "a 3-bar warm-up over 3 bars must never open the dispatch gate"
    );
    assert_eq!(result.warmup, 3, "the report must carry the number the run actually gated on");

    let control = BacktestProfile::from_toml_str(&data_profile("")).unwrap();
    let result = run_backtest(&control, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "the control must trade from the first bar");
    assert_eq!(result.warmup, 0);
}

/// A strategy declaring its own warm-up, so [`EngineParams::warmup`]'s `max` semantics can be
/// proven in BOTH directions. Counts the `on_bar` calls that got past the gate.
struct DeclaresWarmup {
    declared: usize,
    fired: std::rc::Rc<std::cell::Cell<usize>>,
}

impl vike_model::Strategy<vike_sim::SimBroker> for DeclaresWarmup {
    fn warmup(&self) -> usize {
        self.declared
    }
    fn on_bar(&mut self, _ctx: &mut vike_sim::SimBroker, _bar: &Bar) {
        self.fired.set(self.fired.get() + 1);
    }
}

/// `EngineParams::warmup` is a FLOOR on `Strategy::warmup()`, never a replacement — the
/// property `data.warmup`'s doc promises an operator, and the one that makes the key safe to
/// expose at all.
///
/// Driven at the ENGINE rather than through a profile because no registry strategy declares a
/// non-zero warm-up, so a profile-level test could only ever exercise the `0` side of the
/// `max` and would pass just as well against a plain assignment. Five bars, and the gate is
/// asserted by COUNTING dispatches (`5 - effective`) as well as by reading the reported
/// number, so a resolution that agreed with the report but not with the fold loop would still
/// fail.
#[test]
fn the_configured_warmup_is_a_floor_not_a_replacement() {
    let series = vec![(
        SYMBOL.to_string(),
        vec![
            flat_bar_zero_volume(0, 100.0),
            flat_bar_zero_volume(1, 100.0),
            flat_bar_zero_volume(2, 100.0),
            flat_bar_zero_volume(3, 100.0),
            flat_bar_zero_volume(4, 100.0),
        ],
    )];
    let run = |declared: usize, configured: Option<usize>| -> (usize, usize) {
        let fired = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let strategy = DeclaresWarmup { declared, fired: std::rc::Rc::clone(&fired) };
        let params = EngineParams { warmup: configured, ..Default::default() };
        let result = StrategyEngine::new(series.clone(), strategy, params).run();
        (fired.get(), result.warmup)
    };

    // The configured floor is HIGHER: it wins.
    assert_eq!(run(1, Some(4)), (1, 4), "a higher configured floor must raise the gate");
    // The strategy's own requirement is HIGHER: it wins, and the configured number is inert.
    assert_eq!(
        run(4, Some(1)),
        (1, 4),
        "a lower configured floor must NOT lower a strategy's own warm-up — a run on \
         unconverged indicators is the failure this gate exists to prevent"
    );
    // Neither side asks for anything: the frozen behaviour.
    assert_eq!(run(0, None), (5, 0));
    // Only the strategy asks: unchanged from before the field existed.
    assert_eq!(run(2, None), (3, 2));
}

/// `data.detail_interval` loads one finer series per resolved symbol into the shape
/// `EngineParams::granular_by_symbol` takes — the door onto the granular sub-bar lane, which
/// no profile could reach before.
#[test]
fn detail_interval_loads_one_finer_series_per_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let coarse = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(86_400_000, 101.0),
        flat_bar_zero_volume(172_800_000, 102.0),
    ];
    store.append_bars(VENUE, SYMBOL, "1d", &coarse, None).unwrap();
    let fine: Vec<Bar> =
        (0..72).map(|h| flat_bar_zero_volume(h * 3_600_000, 100.0 + h as f64 * 0.01)).collect();
    store.append_bars(VENUE, SYMBOL, "1h", &fine, None).unwrap();

    let absent = BacktestProfile::from_toml_str(&data_profile("")).unwrap();
    assert!(
        load_profile_detail_bars(&absent, &store).unwrap().is_empty(),
        "an absent key must load nothing at all — the byte-identical no-op"
    );

    let on = BacktestProfile::from_toml_str(&data_profile("detail_interval = \"1h\"")).unwrap();
    let loaded = load_profile_detail_bars(&on, &store).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, SYMBOL, "the key must be a symbol of the run, by construction");
    assert_eq!(loaded[0].1.len(), 72);

    // ...and it reaches the engine: the whole run must still complete with the tape mounted.
    let result = run_backtest(&on, store).unwrap();
    assert_ne!(result.final_equity, 1000.0, "the run must still trade with a detail tape on");
}

/// The four `data.detail_interval` refusals, each replacing a silence that would read as a
/// working realism knob.
#[test]
fn detail_interval_refuses_every_silent_no_op() {
    // Not an interval the store could hold.
    let bad = parse_unvalidated(&data_profile("detail_interval = \"3mo\""));
    let err = bad.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("not a valid interval"), "got {err:?}");

    // Equal to the base: at most one sub-bar per step, so no ambiguity is resolved.
    let equal = parse_unvalidated(&data_profile("detail_interval = \"1d\""));
    let err = equal.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("strictly FINER"), "got {err:?}");

    // Coarser than the base: the same, and the opposite of what the operator asked for.
    let coarser = parse_unvalidated(
        &data_profile("detail_interval = \"4h\"").replace("interval = \"1d\"", "interval = \"1h\""),
    );
    let err = coarser.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("strictly FINER"), "got {err:?}");

    // Tick mode: `run_ticks` never reads the per-symbol sub-bar buckets.
    let tick = parse_unvalidated(
        &data_profile("detail_interval = \"1h\"").replace("kind = \"bar\"", "kind = \"tick\""),
    );
    let err = tick.data.detail_interval_ms().unwrap_err().to_string();
    assert!(err.contains("bar-mode only"), "got {err:?}");
}

/// A detail interval the store holds NOTHING for is refused, not silently downgraded.
///
/// `StrategyEngine::new` skips an empty sub-bar vector, so without this the run would fall
/// back to the coarse adverse-first guess for that symbol alone while the operator believed
/// the tape was mounted — a per-symbol silence, which is the hardest kind to notice.
#[test]
fn a_detail_interval_the_store_cannot_serve_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(DataFusionHist::open(dir.path()).unwrap());
    store.append_bars(VENUE, SYMBOL, "1d", &[flat_bar_zero_volume(0, 100.0)], None).unwrap();

    let on = BacktestProfile::from_toml_str(&data_profile("detail_interval = \"1h\"")).unwrap();
    let err = load_profile_detail_bars(&on, &store).unwrap_err().to_string();
    assert!(err.contains("holds no 1h bars"), "got {err:?}");
}
