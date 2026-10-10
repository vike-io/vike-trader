use super::*;
use std::assert_matches;
// This file's BODY spells it `super::sweep::RankMetric` (the fully-qualified style every
// `super::sweep::` / `super::run::` call site here uses); a test comparing VALUES wants the
// bare name, and `harness`'s own re-export is the shortest honest path to it.
use crate::harness::{RankMetric, WalkforwardCfg, WindowForm};

const BASE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
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
symbol = "BTCUSDT"
"#;

/// A two-point `[sweep]` grid over `buy_hold`'s `size`. Every `search = "sweep"` fixture needs
/// one: the load-time rule below refuses that spelling on a profile with nothing to search.
const GRID: &str = "[sweep]\nsize = [1.0, 2.0]\n";

fn profile(extra: &str) -> BacktestProfile {
    BacktestProfile::from_toml_str(&format!("{BASE}{extra}")).unwrap()
}

/// `BASE` at an HOURLY interval with a range wide enough for every bar [`mem_store_1h`]
/// writes. Declared beside `BASE` rather than edited into it: other tests pin `BASE`'s `"1d"`
/// interval through the annualization, and the frozen `to = "100000"` would silently TRUNCATE
/// an hourly series — a truncated series changes the split arithmetic, which is exactly what
/// the window-form tests measure.
const BASE_1H: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "9999999999999"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// `n` strictly-rising hourly bars in the DataFusion-free double, under `BASE_1H`'s own
/// `(venue, symbol, interval)`.
///
/// ⚠ `vike_data::MemHistStore`, not a temp-dir `DataFusionHist` — so this one needs nothing
/// beyond `test-support`, where [`seeded_store`] below needs `datafusion-store`. That is
/// available because `crates/vike-backtest/Cargo.toml` declares
/// `vike-data = { path = "../vike-data", features = ["test-support"] }` as a DEV-dependency,
/// and a dev-dependency's features reach a `src/` `#[cfg(test)]` module exactly as they reach
/// `tests/` (`crates/vike-backtest/tests/walkforward_optimize.rs` is the worked example). The
/// comment on `empty_store` below said the opposite for as long as that dev-dep has existed.
fn mem_store_1h(n: usize) -> Arc<dyn HistStore + Send + Sync> {
    use vike_model::Bar;

    let store = vike_data::MemHistStore::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                ts: i as i64 * 3_600_000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1h", &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// A window form that yields NO window over this series is a refusal, not an all-zeros
/// report. A walk that evaluated nothing must never wear the clothes of one that did — the
/// same rule the zero-split refusal has always enforced, now reached by a second route.
#[test]
fn a_window_form_that_yields_no_window_is_refused_by_the_driver() {
    let store = mem_store_1h(120);
    let profile = BacktestProfile::from_toml_str(&format!(
        "{BASE_1H}[walkforward]\ntrain = \"400bars\"\ntest = \"100bars\"\n"
    ))
    .unwrap();
    let err = run_walkforward(&profile, store).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("NO windows") && msg.contains("120"), "{msg}");
}

/// A QUOTED bare number gets the grammar's own message, naming the field. (An UNQUOTED
/// `train = 90` is a TOML integer and fails as a serde TYPE error first — a different
/// message for the same mistake, accepted deliberately: see this stage's plan, D7.)
#[test]
fn a_quoted_bare_number_window_is_refused_by_name() {
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\ntrain = \"90\"\ntest = \"30d\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("walkforward.train") && msg.contains("no unit"), "{msg}");
    let unquoted = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\ntrain = 90\ntest = \"30d\"\n"
    ))
    .unwrap_err();
    assert_matches!(unquoted, HarnessError::Parse(_), "{unquoted}");
}

/// Declaring both window forms is refused BY NAME at load, never resolved by a precedence
/// rule. The message must name both keys, because a silent winner is how an operator ends
/// up reading a report from a protocol they did not ask for.
#[test]
fn declaring_both_window_forms_is_refused_by_name() {
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\nn_splits = 6\ntrain = \"12mo\"\ntest = \"3mo\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("n_splits") && msg.contains("train/test"), "{msg}");
}

/// A `[walkforward]` table with no window form at all is a NEW failure state created by
/// making `n_splits` optional, and it is refused rather than defaulted.
#[test]
fn a_walkforward_table_with_no_window_form_is_refused() {
    let err = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nmode = \"rolling\"\n"))
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no window form") && msg.contains("n_splits"), "{msg}");
}

/// `purge` under the split-count form is refused, and the message names the decision record
/// that rules it out — so the next reader finds the argument rather than re-running it.
#[test]
fn purge_on_the_split_count_form_is_refused_and_names_the_decision() {
    for junk in ["purge = \"8h\"", "embargo = \"1d\""] {
        let err =
            BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 4\n{junk}\n"))
                .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("0046") && msg.contains("train/test"), "{junk}: {msg}");
    }
}

/// Half a duration form is refused; so is a zero-length one. Both are wrong on their own
/// terms — no store, no interval and no verb is needed to say so — which is why they fail at
/// LOAD while "this range holds no window" does not.
#[test]
fn a_half_declared_or_zero_length_duration_form_is_refused_at_load() {
    let half = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\ntrain = \"12mo\"\n"))
        .unwrap_err();
    assert!(format!("{half}").contains("BOTH"), "{half}");
    let zero = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\ntrain = \"0d\"\ntest = \"3mo\"\n"
    ))
    .unwrap_err();
    let msg = format!("{zero}");
    assert!(msg.contains("walkforward.train") && msg.contains("zero-length"), "{msg}");
}

/// The pre-existing zero-split refusal survives the form change unchanged — and it is still
/// reached through the SPLIT-COUNT form, which is the half the form resolver could have
/// broken: `n_splits = 0` is `Some(0)`, so `window_form` must still answer
/// [`WindowForm::Splits`] rather than "no window form declared".
#[test]
fn a_zero_split_count_is_still_refused_at_load() {
    let err = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
        .unwrap_err();
    assert!(format!("{err}").contains("n_splits must be >= 1"), "{err}");
    let cfg: WalkforwardCfg = toml::from_str("n_splits = 0").expect("the section parses");
    assert_eq!(cfg.window_form().unwrap(), WindowForm::Splits);
}

/// The duration form round-trips every suffix through the profile door, and mixing suffixes
/// inside it is legal: `train`/`test`/`step` are ONE form, and the exclusivity that matters
/// is against `n_splits`.
#[test]
fn the_duration_knobs_round_trip_every_suffix() {
    let wf = profile(
        "[walkforward]\ntrain = \"12mo\"\ntest = \"1000bars\"\nstep = \"3d\"\n\
             purge = \"8h\"\nembargo = \"1d\"\n",
    )
    .walkforward
    .expect("the section is present");
    assert_eq!(wf.train_span().unwrap(), Some(vike_model::time::Span::Months(12)));
    assert_eq!(wf.test_span().unwrap(), Some(vike_model::time::Span::Bars(1000)));
    assert_eq!(wf.step_span().unwrap(), Some(vike_model::time::Span::Ms(3 * 86_400_000)));
    assert_eq!(wf.purge_span().unwrap(), Some(vike_model::time::Span::Ms(8 * 3_600_000)));
    assert_eq!(wf.embargo_span().unwrap(), Some(vike_model::time::Span::Ms(86_400_000)));
    assert_eq!(wf.n_splits, None);
    assert_eq!(wf.window_form().unwrap(), WindowForm::Duration);
}

/// `n_splits` is OPTIONAL now, and a section carrying only it still answers the count form
/// with the same number it always did.
#[test]
fn the_split_count_form_still_answers_with_its_count() {
    let wf = profile("[walkforward]\nn_splits = 6\n").walkforward.expect("present");
    assert_eq!(wf.n_splits, Some(6));
    assert_eq!(wf.window_form().unwrap(), WindowForm::Splits);
    assert_eq!(wf.train_span().unwrap(), None);
    assert_eq!(wf.purge_span().unwrap(), None);
}

#[test]
fn the_walkforward_section_parses_and_defaults_to_absent() {
    assert!(profile("").walkforward.is_none(), "no [walkforward] ⇒ None (unchanged profiles)");
    assert_eq!(profile("[walkforward]\nn_splits = 4\n").walkforward.unwrap().n_splits, Some(4));
}

#[test]
fn a_zero_split_count_is_rejected_at_load() {
    let err = BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
        .unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.contains("n_splits"), "{err}");
}

#[test]
fn an_unknown_walkforward_key_is_a_parse_error() {
    // `deny_unknown_fields` on the section: a typo'd knob fails at load rather than silently
    // doing nothing (the same contract every other profile section has). Each of the three
    // knobs this section grew is one letter away from a spelling serde would have to reject,
    // and the misspelling of an OPTIONAL field is the dangerous one — it would deserialize to
    // `None` and read exactly like "the operator did not ask for it".
    for junk in ["splits = 3", "serach = \"grid\"", "walk_mode = \"rolling\""] {
        let err =
            BacktestProfile::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 2\n{junk}\n"))
                .unwrap_err();
        assert_matches!(err, HarnessError::Parse(_), "{junk}: {err}");
    }
}

/// The three knobs this section grew are all ABSENT-BY-DEFAULT, and absent resolves to the walk
/// it described before they existed: no search, an anchored train window, and the sweep's own
/// default ranking metric. This is the byte-identical-for-existing-profiles claim, made against
/// the resolvers rather than against prose.
#[test]
fn the_new_knobs_default_to_the_pre_optimization_walk() {
    let p = profile("[walkforward]\nn_splits = 4\n");
    let cfg = p.walkforward.as_ref().expect("the section is present");
    assert_eq!(cfg.n_splits, Some(4));
    assert!(cfg.search.is_none(), "search absent");
    assert!(cfg.mode.is_none(), "mode absent");
    assert!(cfg.rank_by.is_none(), "rank_by absent");
    assert_eq!(cfg.window_search().unwrap(), WindowSearch::None);
    assert_eq!(cfg.walk_mode().unwrap(), WalkMode::Anchored);
    assert_eq!(cfg.rank_metric().unwrap(), RankMetric::default());
}

/// `search` takes both spellings of the grid and an explicit control, normalizes case and
/// whitespace like every other string knob in a profile, and a TYPO fails at LOAD naming the
/// valid set. The last part is the one that matters: a silent degradation to the control would
/// run the no-search walk under a report the operator reads as an optimization's.
#[test]
fn the_search_knob_round_trips_and_a_typo_fails_at_load() {
    let search = |s: &str| {
        profile(&format!("{GRID}[walkforward]\nn_splits = 2\nsearch = \"{s}\"\n"))
            .walkforward
            .expect("the section is present")
            .window_search()
            .unwrap()
    };
    assert_eq!(search("none"), WindowSearch::None);
    // Case and surrounding whitespace are forgiven — the `queue_model_kind` idiom. The SPELLING
    // is not: see below.
    for spelling in ["sweep", "SWEEP", " sweep "] {
        assert_eq!(search(spelling), WindowSearch::Sweep, "search = {spelling:?}");
    }

    // ⚠ `"grid"` is REFUSED, and refused by NAME rather than falling into the catch-all. It
    // parsed as a synonym for exactly one release — the design doc said `sweep`, the first
    // implementation said `grid`, and both were accepted rather than choosing. A profile
    // written in that window has to be told what to write, not merely that it is wrong.
    let renamed = BacktestProfile::from_toml_str(&format!(
        "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"grid\"\n"
    ))
    .unwrap_err();
    let renamed = format!("{renamed}");
    assert!(
        renamed.contains("renamed") && renamed.contains("\"sweep\""),
        "the removed spelling must name its replacement, not just fail: {renamed}"
    );

    // ...and an actual typo still lands in the catch-all, which now advertises `sweep`.
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"gird\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown walkforward.search") && msg.contains("sweep"), "{msg}");
}

/// `mode` round-trips both [`WalkMode`] variants and refuses anything else at load. It is a
/// settable knob at all only because the runner started reading the train half — until then
/// the two modes returned byte-identical reports.
#[test]
fn the_mode_knob_round_trips_and_a_typo_fails_at_load() {
    let mode = |s: &str| {
        profile(&format!("[walkforward]\nn_splits = 2\nmode = \"{s}\"\n"))
            .walkforward
            .expect("the section is present")
            .walk_mode()
            .unwrap()
    };
    assert_eq!(mode("anchored"), WalkMode::Anchored);
    assert_eq!(mode("Rolling"), WalkMode::Rolling);
    assert_eq!(mode(" rolling "), WalkMode::Rolling);
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\nn_splits = 2\nmode = \"expanding\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown walkforward.mode") && msg.contains("rolling"), "{msg}");
}

/// `rank_by` names one of the four [`RankMetric`]s — and it names them through
/// `RankMetric::from_str_ci` itself, the SAME parser the sweep bin's `--rank-by` uses, so this
/// test is also the pin that the two doors cannot start accepting different spellings.
#[test]
fn the_rank_by_knob_round_trips_and_a_typo_fails_at_load() {
    let metric = |s: &str| {
        profile(&format!("[walkforward]\nn_splits = 2\nrank_by = \"{s}\"\n"))
            .walkforward
            .expect("the section is present")
            .rank_metric()
            .unwrap()
    };
    assert_eq!(metric("sharpe"), RankMetric::Sharpe);
    assert_eq!(metric("RETURN"), RankMetric::TotalReturn);
    assert_eq!(metric("max_dd"), RankMetric::MaxDrawdown);
    assert_eq!(metric(" equity "), RankMetric::FinalEquity);
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\nn_splits = 2\nrank_by = \"sortino\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown walkforward.rank_by") && msg.contains("sharpe"), "{msg}");
}

/// `search = "sweep"` with no `[sweep]` table fails at LOAD rather than at the first window: the
/// pairing is a contradiction in the profile TEXT — no store, no data slice and no verb can
/// make it true — which puts it in the `n_splits = 0` class and not in the bar-mode class,
/// whose rules depend on what the profile resolves against a store. The runtime twin in
/// `run_walkforward_optimized_with` survives for the profiles that never came through here.
#[test]
fn grid_search_without_a_sweep_table_is_refused_at_load() {
    let err = BacktestProfile::from_toml_str(&format!(
        "{BASE}[walkforward]\nn_splits = 2\nsearch = \"sweep\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("[sweep]"), "the refusal names what is missing: {msg}");
    // The rule is about the PAIRING, not about the key: the same profile with a grid loads...
    assert!(
        BacktestProfile::from_toml_str(&format!(
            "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"sweep\"\n"
        ))
        .is_ok(),
        "grid + [sweep] is the legal pairing"
    );
    // ...and the CONTROL never needs one, so an explicit `none` beside no grid stays legal.
    assert!(
        BacktestProfile::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nsearch = \"none\"\n"
        ))
        .is_ok(),
        "the control searches nothing, so it requires nothing"
    );
}

/// Every runner test needs SOME `HistStore` handle, even the guards that return before touching
/// it. This one is an EMPTY temp-dir `DataFusionHist`, so it rides the `datafusion-store` lane
/// exactly like `harness::run`'s own store-backed tests; the profile-shape tests above stay on
/// the default trait-only build.
///
/// ⚠ CORRECTED. This comment used to argue that the lane choice was FORCED — "this crate has no
/// DataFusion-free double; `vike_data::MemHistStore` lives behind a `test-support` feature
/// vike-backtest does not dev-depend on". It does dev-depend on it, with that exact feature
/// (`crates/vike-backtest/Cargo.toml`), and a dev-dependency's features reach a `src/`
/// `#[cfg(test)]` module just as they reach `tests/` — [`mem_store_1h`] above is the proof, and
/// `crates/vike-backtest/tests/walkforward_optimize.rs` had been spending the same fact from
/// outside the crate the whole time. The tests below stay on the temp-dir store because that is
/// what they were written against and moving them would change what they exercise, not because
/// the alternative is unavailable.
#[cfg(feature = "datafusion-store")]
fn empty_store() -> (tempfile::TempDir, Arc<dyn HistStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistStore + Send + Sync> =
        Arc::new(vike_data::DataFusionHist::open(dir.path()).unwrap());
    (dir, store)
}

#[cfg(feature = "datafusion-store")]
#[test]
fn a_profile_without_the_section_is_a_clean_error() {
    let (_dir, store) = empty_store();
    let err = run_walkforward(&profile(""), store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.contains("[walkforward]"));
}

#[cfg(feature = "datafusion-store")]
#[test]
fn a_tick_profile_is_a_clean_error() {
    let (_dir, store) = empty_store();
    let toml = BASE.replace("kind = \"bar\"", "kind = \"tick\"");
    let p =
        BacktestProfile::from_toml_str(&format!("{toml}[walkforward]\nn_splits = 2\n")).unwrap();
    let err = run_walkforward(&p, store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.contains("bar-mode only"));
}

/// An empty bar slice is a DATA error, not a silent zero-window report.
#[cfg(feature = "datafusion-store")]
#[test]
fn an_empty_bar_slice_is_a_data_error() {
    let (_dir, store) = empty_store();
    let err = run_walkforward(&profile("[walkforward]\nn_splits = 2\n"), store).unwrap_err();
    assert_matches!(err, HarnessError::Data(ref m) if m.contains("no bars"), "{err}");
}

/// A cross-venue two-series slice cannot be walked forward (the splitter takes ONE series).
#[cfg(feature = "datafusion-store")]
#[test]
fn a_multi_series_profile_is_a_clean_error() {
    let (_dir, store) = empty_store();
    let toml = r#"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[[data.series]]
venue = "binance"
symbol = "BTCUSDT"
[[data.series]]
venue = "binance"
symbol = "ETHUSDT"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[walkforward]
n_splits = 2
"#;
    let p = BacktestProfile::from_toml_str(toml).unwrap();
    let err = run_walkforward(&p, store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.contains("ONE bar series"));
}

/// End-to-end over a REAL store: one window per split, a stitched curve, finite summary stats.
#[cfg(feature = "datafusion-store")]
#[test]
fn runs_one_window_per_split_over_a_real_store() {
    use vike_data::DataFusionHist;
    use vike_model::Bar;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars: Vec<Bar> = (0..240)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                ts: i as i64 * 1000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let p = profile("[walkforward]\nn_splits = 4\n");
    let rep = run_walkforward(&p, store).unwrap();
    assert_eq!(rep.windows.len(), 4, "one OOS window per split");
    assert!(!rep.oos_equity_curve.is_empty(), "the stitched curve is populated");
    assert!(rep.oos_return.is_finite());
    assert!((0.0..=1.0).contains(&rep.wf_consistency));
}

/// The harness door APPLIES its own resolver — the half a cross-plane test cannot see.
///
/// The cross-plane gate (`crates/vike-studio-core/tests/walkforward_annualization.rs`) proves
/// the STUDIO door's applied factor and proves both doors' DERIVATIONS agree. What neither it
/// nor anything else covered is the single `periods_per_year(profile)` argument at this
/// module's `walk_forward_over_windows` call — and this branch exists precisely because a doc
/// comment asserted a property nothing checked.
///
/// ⚠ It runs at `"1h"` deliberately. `runs_one_window_per_split_over_a_real_store` above uses
/// `"1d"`, where the derived factor and a hardcoded `252` are THE SAME NUMBER — so that test
/// would pass against either spelling and cannot gate this. At `"1h"` they differ by
/// `sqrt(24)`, and the `assert_ne!` is what stops the bit-exact assertion from passing when
/// both sides regress together.
#[cfg(feature = "datafusion-store")]
#[test]
fn the_harness_door_applies_the_derived_annualization_not_the_daily_anchor() {
    use vike_analytics::metrics::sharpe;
    use vike_analytics::report::DAILY_PERIODS_PER_YEAR;
    use vike_data::DataFusionHist;
    use vike_model::Bar;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars: Vec<Bar> = (0..240)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                ts: i as i64 * 1000,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1h", &bars, None).unwrap();

    let p = BacktestProfile::from_toml_str(&format!(
        "{}{}",
        BASE.replace("interval = \"1d\"", "interval = \"1h\""),
        "[walkforward]\nn_splits = 4\n"
    ))
    .unwrap();
    let rep = run_walkforward(&p, store).unwrap();

    let derived = periods_per_year(&p);
    assert_eq!(derived, DAILY_PERIODS_PER_YEAR * 24.0, "1h resolves to 6,048");

    // Precondition: a dispersion-free curve makes `sharpe` return 0.0 at EVERY factor, which
    // would make the inequality below vacuous.
    let at_daily = sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR);
    assert!(at_daily != 0.0 && at_daily.is_finite(), "fixture must have dispersion: {at_daily}");

    assert_eq!(
        rep.oos_sharpe.to_bits(),
        sharpe(&rep.oos_equity_curve, derived).to_bits(),
        "the reported oos_sharpe IS the stitched curve annualized at the derived factor"
    );
    assert_ne!(
        rep.oos_sharpe, at_daily,
        "...and NOT at the daily anchor — the regression this test exists to catch"
    );
}
/// `BASE` with a range wide enough for every bar [`seeded_store`] writes. The frozen `BASE`
/// stops at 100_000 ms, so a longer series would be silently TRUNCATED by the loader — and a
/// truncated series changes the split arithmetic, which is exactly what the tests below
/// measure.
#[cfg(feature = "datafusion-store")]
fn wide_profile(extra: &str) -> BacktestProfile {
    let base = BASE.replace("to = \"100000\"", "to = \"100000000\"");
    BacktestProfile::from_toml_str(&format!("{base}{extra}")).unwrap()
}

/// A real (temp-dir) store holding ONE bar series under `BASE`'s own `(venue, symbol,
/// interval)`: `closes` as flat OHLC bars one second apart. Flat OHLC is all these tests need —
/// `buy_hold` reads nothing but the close, and the engine fills at the NEXT bar's open.
#[cfg(feature = "datafusion-store")]
fn seeded_store(closes: &[f64]) -> (tempfile::TempDir, Arc<dyn HistStore + Send + Sync>) {
    use vike_model::Bar;

    let dir = tempfile::tempdir().unwrap();
    let hist = vike_data::DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, &close)| Bar {
            ts: i as i64 * 1000,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect();
    hist.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(hist);
    (dir, store)
}

/// Every number a [`WalkForwardReport`] carries, as IEEE-754 BIT PATTERNS. Two reports then
/// compare EXACTLY — and two `NaN` Sharpes compare EQUAL, which `f64` itself will not do — so
/// "these two runs produced the same report" cannot pass by accident on a degenerate curve.
/// The workspace's frozen-fixture convention, applied to a report instead of a file.
#[cfg(feature = "datafusion-store")]
fn report_bits(rep: &WalkForwardReport) -> Vec<u64> {
    let mut out = vec![
        rep.oos_return.to_bits(),
        rep.oos_sharpe.to_bits(),
        rep.wf_consistency.to_bits(),
        rep.windows.len() as u64,
    ];
    out.extend(rep.oos_equity_curve.iter().map(|v| v.to_bits()));
    for w in &rep.windows {
        out.push(w.test_range.0 as u64);
        out.push(w.test_range.1 as u64);
        out.push(w.oos_return.to_bits());
    }
    out
}

/// The `size` a searched window chose, read back out of its `chosen_params`.
#[cfg(feature = "datafusion-store")]
fn chosen_size(rep: &WalkForwardReport, window: usize) -> f64 {
    rep.windows[window]
        .chosen_params
        .as_ref()
        .expect("a searched window records what it chose")
        .iter()
        .find(|(k, _)| k == "size")
        .and_then(|(_, v)| v.as_float())
        .expect("the grid's one axis is a float array named `size`")
}

/// THE CONTROL'S OWN GATE: [`WindowSearch::None`] through the optimizing driver reproduces the
/// fixed-parameter walk BIT FOR BIT. If those two disagreed, the control would be a different
/// program rather than a comparison, and every "the search bought nothing" verdict measured
/// against it would be measuring the seam instead of the search.
#[cfg(feature = "datafusion-store")]
#[test]
fn the_control_reproduces_the_fixed_parameter_walk_exactly() {
    let closes: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.1).collect();
    let (_dir, store) = seeded_store(&closes);
    let p = wide_profile("[walkforward]\nn_splits = 3\n");
    let fixed = run_walkforward(&p, Arc::clone(&store)).unwrap();
    let control = run_walkforward_optimized(&p, store).unwrap();
    assert_eq!(
        report_bits(&fixed),
        report_bits(&control),
        "the control run IS the fixed-parameter walk, reached through the other driver"
    );
    assert!(
        control.windows.iter().all(|w| w.chosen_params.is_none()),
        "a window that chose nothing records nothing"
    );
}

/// The fixed walk HONOURS `[walkforward].mode` and cannot MOVE with it: the two modes differ in
/// `train_start` alone, and this driver's closure discards the training half. Pinned rather
/// than merely asserted in a doc, because it is precisely the invariance the optimizing driver
/// breaks — and the invariance is what makes wiring the knob into both drivers safe.
#[cfg(feature = "datafusion-store")]
#[test]
fn both_walk_modes_leave_the_fixed_walk_report_identical() {
    let closes: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.1).collect();
    let (_dir, store) = seeded_store(&closes);
    let anchored_p = wide_profile("[walkforward]\nn_splits = 3\nmode = \"anchored\"\n");
    let rolling_p = wide_profile("[walkforward]\nn_splits = 3\nmode = \"rolling\"\n");
    let anchored = run_walkforward(&anchored_p, Arc::clone(&store)).unwrap();
    let rolling = run_walkforward(&rolling_p, store).unwrap();
    assert_eq!(
        report_bits(&anchored),
        report_bits(&rolling),
        "the fixed walk reads no train half, so the mode cannot move its report"
    );
}

/// ...and THE MODE IS NOT DECORATION once a window searches: `Anchored` and `Rolling` pick
/// DIFFERENT winners over the same bars. This is the other half of the pair above — together
/// they say the knob does nothing where it should do nothing and something where it should.
#[cfg(feature = "datafusion-store")]
#[test]
fn the_two_walk_modes_choose_different_winners_when_a_window_searches() {
    // Up hard, then down: bars 0..100 climb 100 -> 199, bars 100..300 fall back to 99.5. With
    // `n_splits = 2` the splitter's chunk is 100, so the SECOND window (test `[200, 300)`)
    // trains on `[0, 200)` anchored — net UP, where the bigger `size` wins by 10x — and on
    // `[100, 200)` rolling — net DOWN, where that same size LOSES by 10x. The FIRST window's
    // train half is `[0, 100)` under both modes (`Rolling`'s `train_start` saturates at 0), so
    // its winner is this comparison's own control: it must agree.
    let closes: Vec<f64> = (0..300)
        .map(|i| if i < 100 { 100.0 + i as f64 } else { 199.0 - (i - 100) as f64 * 0.5 })
        .collect();
    let (_dir, store) = seeded_store(&closes);
    let grid = concat!(
        "[sweep]\nsize = [0.1, 1.0]\n",
        "[walkforward]\nn_splits = 2\nsearch = \"sweep\"\nrank_by = \"return\"\n"
    );
    let anchored_p = wide_profile(&format!("{grid}mode = \"anchored\"\n"));
    let rolling_p = wide_profile(&format!("{grid}mode = \"rolling\"\n"));
    let anchored = run_walkforward_optimized(&anchored_p, Arc::clone(&store)).unwrap();
    let rolling = run_walkforward_optimized(&rolling_p, store).unwrap();

    assert_eq!(
        chosen_size(&anchored, 0),
        chosen_size(&rolling, 0),
        "window 0's train half is identical under both modes, so its winner must be too"
    );
    assert_eq!(chosen_size(&anchored, 1), 1.0, "the anchored train half is net UP");
    assert_eq!(chosen_size(&rolling, 1), 0.1, "the rolling train half is net DOWN");
}
