//! The study's unit tests: profile pins, the params readers, the context store and the seam.

use std::assert_matches;
use std::path::PathBuf;
use std::sync::Mutex;

use vike_data::{DataError, TsRange};
use vike_model::QuoteTick;
use vike_user_research::{SimOutcome, StudySim};

use super::matrix::{Lane, Market, Strat, parse_lanes, parse_strats};
use super::*;

// ---- the profile text: the pins that prove the port re-tuned nothing --------------------
//
// Carried from the binary verbatim, minus each one's `BacktestProfile::from_toml_str(..)
// .expect("must parse")` line — that type lives in `vike-backtest`, which this crate may not
// name. Where the binary proved "the simulator's own deserializer accepts this", these prove
// only "this is well-formed TOML"; the schema half now runs host-side.

/// The no-knob run (lane l2, latency 0, spread grid) must generate EXACTLY the binary's TOML.
#[test]
fn default_profile_is_byte_identical_to_the_batch_binary() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
    let got = profile_toml("TOK", 100, 200, false, 0, &matrix[0]); // as-g0.05
    let want = "name = \"batch\"\n\
                [data]\nkind = \"tick\"\nfrom = \"100\"\nto = \"200\"\n\
                [[data.series]]\nvenue = \"polymarket\"\nsymbol = \"TOK\"\nkind = \"tick\"\n\
                [engine]\ncash = 1000.0\nslippage = 0.0\nqueue_model = \"prob_power\"\n\
                [strategy]\nname = \"spread_maker\"\n\
                [strategy.params]\nqty = 1.0\ntick_size = 0.001\nmin_half_spread_ticks = 0\ngamma = 0.05\n";
    assert_eq!(got, want);
}

/// `latency_ms = 250` lands as `[engine] order_latency_ms = 250`, and the profile is still
/// well-formed TOML.
#[test]
fn latency_knob_emits_the_engine_order_latency_line() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
    let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
    assert!(text.contains("order_latency_ms = 250\n"), "{text}");
    toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
}

/// The L1 lane omits queue_model entirely — the tick lane then uses the default optimistic
/// spread-crossing Tick fill model.
#[test]
fn l1_lane_has_no_queue_model() {
    let matrix = run_matrix(&[Lane::L1], &[Strat::Spread], 0.0);
    let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
    assert!(!text.contains("queue_model"), "{text}");
    toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
}

/// The trailing profile mounts `trailing_scalper` with the user-fixed 2s in-strategy exit
/// delay and carries NO spread_maker-only params.
#[test]
fn trailing_profile_names_the_scalper_with_its_exit_delay() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
    assert_eq!(matrix.len(), 2);
    let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
    assert!(text.contains("name = \"trailing_scalper\"\n"), "{text}");
    assert!(text.contains("exit_delay_ms = 2000\n"), "{text}");
    assert!(!text.contains("tick_size"), "{text}");
    assert!(!text.contains("min_half_spread_ticks"), "{text}");
    toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
}

/// Every trailing config's profile carries the market's real `[from, to]` window as
/// `market_open_ms`/`market_close_ms` — for BOTH the baseline (whose own cutoff knobs are 0,
/// so this is inert) and `trail-cut` (whose knobs are actually armed).
#[test]
fn trailing_configs_carry_the_markets_window_as_open_close_params() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
    assert_eq!(matrix.len(), 2);
    for spec in &matrix {
        let text = profile_toml("TOK", 111, 222, false, 250, spec);
        assert!(text.contains("market_open_ms = 111\n"), "{}: {text}", spec.label);
        assert!(text.contains("market_close_ms = 222\n"), "{}: {text}", spec.label);
        toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
    }
}

/// `trail-cut` names both entry-timing cutoff knobs at the user-fixed target values (5s open
/// delay, 30s close cutoff) — the config this study measures against `trail-d2000`.
#[test]
fn trail_cut_config_arms_both_entry_timing_cutoffs() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
    let spec = matrix.iter().find(|s| s.label == "trail-cut").expect("trail-cut present");
    let text = profile_toml("TOK", 100, 200, false, 250, spec);
    assert!(text.contains("entry_open_delay_ms = 5000\n"), "{text}");
    assert!(text.contains("entry_cutoff_before_close_ms = 30000\n"), "{text}");
}

/// The `spread_maker` grid NEVER receives `market_open_ms`/`market_close_ms`.
#[test]
fn spread_maker_configs_never_carry_the_window_params() {
    let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
    let text = profile_toml("TOK", 100, 200, false, 0, &matrix[0]);
    assert!(!text.contains("market_open_ms"), "{text}");
    assert!(!text.contains("market_close_ms"), "{text}");
}

/// both lanes × both strategies = (12 spread + 2 trailing) × 2 lanes = 28 runs per market.
#[test]
fn full_matrix_is_28_runs() {
    let matrix = run_matrix(&[Lane::L1, Lane::L2], &[Strat::Spread, Strat::Trailing], 0.0);
    assert_eq!(matrix.len(), 28);
}

/// 5m ⇒ exactly [end-300s, end]; 15m ⇒ [end-900s, end] — no pre-open or settlement padding.
#[test]
fn window_is_the_markets_exact_life() {
    assert_eq!(window_for("btc-5m", 1_000_000), (700_000, 1_000_000));
    assert_eq!(window_for("xrp-15m", 1_000_000), (100_000, 1_000_000));
}

// ---- params ------------------------------------------------------------------------------

fn params(text: &str) -> toml::Value {
    toml::from_str::<toml::Value>(text).expect("test params must parse")
}

/// Absent knobs preserve the binary's defaults; `both` expands; junk is refused.
#[test]
fn lane_and_strategy_selectors_keep_the_binarys_defaults() {
    assert_eq!(parse_lanes(None), Some(vec![Lane::L2]));
    assert_eq!(parse_lanes(Some("l1")), Some(vec![Lane::L1]));
    assert_eq!(parse_lanes(Some("both")), Some(vec![Lane::L1, Lane::L2]));
    assert_eq!(parse_lanes(Some("l3")), None);
    assert_eq!(parse_strats(None), Some(vec![Strat::Spread]));
    assert_eq!(parse_strats(Some("trailing")), Some(vec![Strat::Trailing]));
    assert_eq!(parse_strats(Some("both")), Some(vec![Strat::Spread, Strat::Trailing]));
    assert_eq!(parse_strats(Some("maker")), None);
}

/// An EMPTY params table is the binary's no-flags invocation, minus the universe it always
/// required.
#[test]
fn knob_defaults_match_the_binarys_no_flag_invocation() {
    let k = read_knobs(&params("")).expect("defaults");
    assert!(!k.fee);
    assert_eq!(k.floor, 0.0);
    assert_eq!(k.latency_ms, 0);
    assert_eq!(k.lanes, vec![Lane::L2]);
    assert_eq!(k.strats, vec![Strat::Spread]);
}

/// `floor = 1` is an Integer to the TOML parser; an operator writing a whole number of ticks
/// must not have to know that.
#[test]
fn floor_accepts_an_integer_as_well_as_a_float() {
    assert_eq!(read_knobs(&params("floor = 1")).expect("int floor").floor, 1.0);
    assert_eq!(read_knobs(&params("floor = 0.5")).expect("float floor").floor, 0.5);
}

/// STRICTER than the binary, deliberately: `--floor banana` used to run the whole sweep with
/// no floor and say nothing.
#[test]
fn a_wrong_typed_knob_is_refused_rather_than_defaulted() {
    for bad in ["floor = \"banana\"", "latency_ms = \"250ms\"", "fee = 1", "lane = 7"] {
        let err = read_knobs(&params(bad)).expect_err(bad);
        assert_matches!(err, StudyError::Study(_), "{bad}: {err}");
    }
    assert!(read_knobs(&params("latency_ms = -5")).is_err());
    assert!(read_knobs(&params("lane = \"l3\"")).is_err());
    assert!(read_knobs(&params("strategy = \"maker\"")).is_err());
}

/// The universe is DATA. A missing or malformed row is a refusal naming its index — where the
/// binary silently dropped the market and shrank every denominator below it.
#[test]
fn the_universe_is_read_from_params_and_a_bad_row_is_refused() {
    let good =
        params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"TOK\"\nend_date_ms = 1000000\n");
    assert_eq!(
        read_universe(&good).expect("one market"),
        vec![Market {
            family: "btc-5m".to_string(),
            token: "TOK".to_string(),
            end_date_ms: 1_000_000,
        }]
    );
    assert!(read_universe(&params("")).is_err(), "absent universe");
    assert!(read_universe(&params("universe = []")).is_err(), "empty universe");
    assert!(
        read_universe(&params("[[universe]]\nfamily = \"f\"\ntoken = \"T\"\n")).is_err(),
        "row with no end_date_ms"
    );
    assert!(
        read_universe(&params("universe = [\"f\\tT\\t1\"]")).is_err(),
        "a TSV line is not a table — the study takes the columns, not the file"
    );
}

// ---- the context-backed store --------------------------------------------------------------

/// A counting `HistStore` double: every `scan_quotes` call increments an `AtomicU64` and
/// returns an empty `Vec` (the row VALUES don't matter for a hit-count proof). Every other
/// verb is stubbed to the cheapest legal answer. This is the store the *caller* would have
/// opened — it sits under the `StudyContext`, which the adapter under test sits on top of.
struct CountingStore {
    quote_calls: std::sync::atomic::AtomicU64,
}

impl CountingStore {
    fn new() -> Self {
        Self { quote_calls: std::sync::atomic::AtomicU64::new(0) }
    }
    fn quotes_seen(&self) -> u64 {
        self.quote_calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl HistStore for CountingStore {
    fn scan_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.quote_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Vec::new())
    }

    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

fn ctx_over(store: Arc<dyn HistStore + Send + Sync>) -> StudyContext {
    StudyContext::new(store, TsRange::all(), PathBuf::from("."))
}

/// Two identical `scan_quotes` calls through the adapter reach the underlying store exactly
/// ONCE; a DIFFERENT range reaches it again — proving the memoization is keyed on the full
/// `(venue, symbol, range)` tuple, not just `(venue, symbol)`.
#[test]
fn identical_scan_hits_the_store_once_different_range_hits_again() {
    let inner = Arc::new(CountingStore::new());
    let cached = ContextStore::new(ctx_over(inner.clone()));

    let r1 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
    let r2 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
    assert_eq!(r1, r2);
    assert_eq!(
        inner.quotes_seen(),
        1,
        "the second identical scan must be served from the cache, not the store"
    );

    let _r3 = cached.scan_quotes("polymarket", "TOK", TsRange::of(300, 400)).unwrap();
    assert_eq!(inner.quotes_seen(), 2, "a different range is a miss and must reach the store");
}

/// One market's 28 cells each call `scan_quotes` on the SAME `(venue, symbol, range)` — the
/// exact shape the per-market closure produces. The store must see exactly one call.
#[test]
fn a_whole_markets_matrix_still_hits_the_store_once() {
    let inner = Arc::new(CountingStore::new());
    let cached = ContextStore::new(ctx_over(inner.clone()));
    for _ in 0..28 {
        cached.scan_quotes("polymarket", "TOK", TsRange::of(1, 2)).unwrap();
    }
    assert_eq!(inner.quotes_seen(), 1);
}

/// The narrowings are asserted, not merely documented: the verbs with no context behind them
/// REFUSE. An `Ok(empty)` here would let "this contract cannot ask" wear the same answer as
/// "the store holds nothing", and `properties_as_of` inherits the refusal through the trait's
/// own default.
#[test]
fn the_unreachable_verbs_refuse_rather_than_answer_empty() {
    let cached = ContextStore::new(ctx_over(Arc::new(CountingStore::new())));
    assert!(cached.scan_symbol_properties("polymarket", "TOK", TsRange::all()).is_err());
    assert!(cached.properties_as_of("polymarket", "TOK", 1).is_err());
    assert!(cached.scan_equity("polymarket", "TOK", TsRange::all()).is_err());
    assert!(cached.scan_exec_fills("polymarket", "TOK").is_err());
    assert!(cached.scan_exec_orders("polymarket", "TOK").is_err());
    assert!(cached.append_quotes("polymarket", "TOK", &[], None).is_err());
    assert!(cached.list_series().is_err(), "the trait's own refusal, inherited on purpose");
}

// ---- the seam ------------------------------------------------------------------------------

/// A [`StudySim`] double: records every profile it was handed and answers a fixed outcome.
///
/// No `#[derive(Default)]`: `SimOutcome` is `Copy` but NOT `Default`, so the derive would not
/// compile — the answer is always supplied explicitly by [`scripted`].
struct ScriptedSim {
    seen: Mutex<Vec<toml::Value>>,
    answer: SimOutcome,
}

impl StudySim for ScriptedSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        _store: Arc<dyn HistStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        self.seen.lock().unwrap().push(profile.clone());
        Ok(self.answer)
    }
}

fn scripted(answer: SimOutcome) -> Arc<ScriptedSim> {
    Arc::new(ScriptedSim { seen: Mutex::new(Vec::new()), answer })
}

/// No simulator ⇒ `NoSim`, naming the work, never a number.
#[test]
fn a_host_with_no_simulator_is_refused_by_name() {
    let ctx = ctx_over(Arc::new(CountingStore::new()));
    let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"TOK\"\nend_date_ms = 1000\n");
    match run(&ctx, &p) {
        Err(StudyError::NoSim(what)) => {
            assert!(what.contains("12 Polymarket maker backtests"), "{what}");
            assert!(what.contains("1 markets"), "{what}");
        }
        other => panic!("expected NoSim, got {other:?}"),
    }
}

/// The whole sweep, end to end, through the seam: the full matrix runs once per market and the
/// aggregate lands as metrics under the stable `{lane}/{family}/{config}/{metric}` name.
#[test]
fn the_sweep_aggregates_one_metric_group_per_cell() {
    let sim = scripted(SimOutcome { total_return: 0.25, n_trades: 4, win_rate: 0.5 });
    let ctx = ctx_over(Arc::new(CountingStore::new())).with_sim(sim.clone());
    let p = params(
        "lane = \"both\"\nstrategy = \"both\"\nfee = true\nlatency_ms = 250\n\
         [[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 1000000\n\
         [[universe]]\nfamily = \"btc-5m\"\ntoken = \"B\"\nend_date_ms = 2000000\n",
    );
    let out = run(&ctx, &p).expect("the sweep runs");

    assert_eq!(out.metric_value("markets"), Some(2.0));
    assert_eq!(out.metric_value("runs"), Some(56.0)); // 2 markets x 28 cells
    assert_eq!(out.metric_value("failed"), Some(0.0));
    assert_eq!(out.metric_value("cells"), Some(28.0));
    assert_eq!(out.metric_value("param/fee"), Some(1.0));
    assert_eq!(out.metric_value("param/latency_ms"), Some(250.0));
    // one cell, both markets folded into it
    assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/n"), Some(2.0));
    assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/sum_ret"), Some(0.5));
    assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/trades"), Some(8.0));
    assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/avg_win"), Some(0.5));
    assert_eq!(out.metric_value("l1/btc-5m/trail-cut/n"), Some(2.0));
    assert_eq!(sim.seen.lock().unwrap().len(), 56);

    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["per_cell.tsv", "failures.tsv"]);
}

/// A host that refuses a run is counted and REPORTED, not swallowed: the binary's stderr line
/// becomes a row of `failures.tsv`.
#[test]
fn a_refused_run_lands_in_the_failures_artifact() {
    struct AlwaysRefuses;
    impl StudySim for AlwaysRefuses {
        fn run_one(
            &self,
            _profile: &toml::Value,
            _store: Arc<dyn HistStore + Send + Sync>,
        ) -> Result<SimOutcome, String> {
            Err("no strategy named spread_maker\nsecond line".to_string())
        }
    }
    let ctx = ctx_over(Arc::new(CountingStore::new())).with_sim(Arc::new(AlwaysRefuses));
    let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 1000\n");
    let out = run(&ctx, &p).expect("a refused RUN is not a refused STUDY");
    assert_eq!(out.metric_value("failed"), Some(12.0));
    assert_eq!(out.metric_value("runs"), Some(0.0));
    assert_eq!(out.metric_value("markets"), Some(0.0));
    let (_, body) = out.artifacts().iter().find(|(n, _)| n == "failures.tsv").expect("present");
    assert!(body.contains("A\tas-g0.05\trun\tno strategy named spread_maker second line\n"));
}

/// A market whose life falls outside the window the run was handed is COUNTED, never clamped:
/// truncating a market's window would silently change what its return means.
#[test]
fn markets_outside_the_handed_window_are_counted_not_clamped() {
    let sim = scripted(SimOutcome { total_return: 0.0, n_trades: 0, win_rate: 0.0 });
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(CountingStore::new());
    let ctx =
        StudyContext::new(store, TsRange::of(0, 1_000), PathBuf::from(".")).with_sim(sim.clone());
    let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 9000000\n");
    let out = run(&ctx, &p).expect("still runs");
    assert_eq!(out.metric_value("markets_outside_window"), Some(1.0));
    assert_eq!(out.metric_value("markets_requested"), Some(1.0));
    assert_eq!(sim.seen.lock().unwrap().len(), 12, "not clamped away");
}
