use super::*;
use std::assert_matches;
// This file's BODY spells it `super::paramscan::RankMetric` (the fully-qualified style every
// `super::paramscan::` / `super::run::` call site here uses); a test comparing VALUES wants the
// bare name, and its module path is the shortest honest path to it.
use crate::run::profile::{ProfileWalkforward, WalkforwardWindowForm};
use crate::search::paramscan::RankMetric;
use crate::walkforward::walkforward_tests::assert_bit_equal_to_the_fold_before_protocol_nine;

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
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// A two-point `[sweep]` grid over `buy_hold`'s `size`. Every `search = "sweep"` fixture needs
/// one: the load-time rule below refuses that spelling on a profile with nothing to search.
const GRID: &str = "[paramscan]\nsize = [1.0, 2.0]\n";

fn profile(extra: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!("{BASE}{extra}"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// `BASE` at an HOURLY interval with a range wide enough for every bar [`memory_store_1h`]
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
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// `n` strictly-rising hourly bars in the DataFusion-free double, under `BASE_1H`'s own
/// `(venue, symbol, interval)`.
///
/// ⚠ `vike_data::HistoryStoreMemory`, not a temp-dir `HistoryStoreDataFusion` — so this one needs nothing
/// beyond `test-support`, where [`seeded_store`] below needs the `vike-store` dev-dependency. That is
/// available because `crates/vike-backtest/Cargo.toml` declares
/// `vike-data = { path = "../vike-data", features = ["test-support"] }` as a DEV-dependency,
/// and a dev-dependency's features reach a `src/` `#[cfg(test)]` module exactly as they reach
/// `tests/` (`crates/vike-backtest/tests/walkforward_optimize.rs` is the worked example). The
/// comment on `empty_store` below said the opposite for as long as that dev-dep has existed.
fn memory_store_1h(n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    use vike_marketdata::Bar;

    let store = vike_data::HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                timestamp: i as i64 * 3_600_000,
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

/// [`BASE_1H`] with `[data]` keys and a two-split walk. Its window runs far past
/// [`memory_store_1h`]'s tape, so an armed rule always has something to say.
fn armed_walk(data_keys: &str, extra: &str) -> ProfileBacktest {
    let toml = BASE_1H.replace("kind = \"bar\"", &format!("kind = \"bar\"\n{data_keys}"));
    assert_ne!(toml, BASE_1H, "the fixture's text moved");
    ProfileBacktest::from_toml_str(&format!("{toml}\n[walkforward]\nn_splits = 2\n{extra}"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// An armed coverage gate read nothing on either walk driver: the pre-flight's checks are its own
/// list, and `crate::run::data_plan::enforce` was not on it.
#[test]
fn an_armed_coverage_gate_refuses_a_walk_on_both_drivers() {
    let fixed = armed_walk("require_coverage = true", "");
    let why = run_walkforward(&fixed, memory_store_1h(100)).expect_err("the store stops short");
    assert!(why.to_string().contains("data.require_coverage"), "{why}");

    let searched =
        armed_walk("require_coverage = true", &format!("search = \"paramscan\"\n{GRID}"));
    let why = run_walkforward_optimized(&searched, memory_store_1h(100)).expect_err("short too");
    assert!(why.to_string().contains("data.require_coverage"), "{why}");
}

/// `on_gap = "warn"` runs the walk and puts the finding on the report; an unarmed walk adds no key.
#[test]
fn a_warning_gate_reaches_a_walks_report_and_a_clean_walk_adds_no_key() {
    let warned = armed_walk("require_coverage = true\non_gap = \"warn\"", "");
    let report = run_walkforward(&warned, memory_store_1h(100)).expect("warn runs");
    let doc = serde_json::to_value(&report).unwrap();
    let lines =
        doc["overall"]["data_warnings"].as_array().unwrap_or_else(|| panic!("no key: {doc}"));
    assert!(lines.iter().any(|l| l.to_string().contains("does not cover the window")), "{doc}");

    let clean =
        ProfileBacktest::from_toml_str(&format!("{BASE_1H}\n[walkforward]\nn_splits = 2\n"))
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
    let report = run_walkforward(&clean, memory_store_1h(100)).expect("runs");
    // Its one line is its USDT booked one to one: the store holds no USDT-USD series (decision 0243, amended
    // 2026-11-16).
    let doc = serde_json::to_value(&report).unwrap();
    assert_eq!(
        doc["overall"]["data_warnings"],
        serde_json::json!([crate::run::data_plan::USDT_USD_NOT_FOUND]),
        "{doc}"
    );
    assert_bit_equal_to_the_fold_before_protocol_nine(&report, 1000.0, periods_per_year(&clean));
}

/// A walk reads the same grid per window, so a store that holds none for its symbol is said once,
/// on the walk's report, like every other finding of the pre-flight.
#[test]
fn a_snap_walk_says_when_the_store_holds_no_grid_for_its_symbol() {
    let toml = BASE_1H.replace("cash = 1000.0", "cash = 1000.0\nsnap_to_properties = true");
    let snap = ProfileBacktest::from_toml_str(&format!("{toml}\n[walkforward]\nn_splits = 2\n"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let report = run_walkforward(&snap, memory_store_1h(100)).expect("the walk runs");
    assert!(
        report.overall.data_warnings.iter().any(|l| l.contains("holds no instrument properties")),
        "{:?}",
        report.overall.data_warnings
    );
}

/// A window form that yields NO window over this series is a refusal, not an all-zeros
/// report. A walk that evaluated nothing must never wear the clothes of one that did — the
/// same rule the zero-split refusal has always enforced, now reached by a second route.
#[test]
fn a_window_form_that_yields_no_window_is_refused_by_the_driver() {
    let store = memory_store_1h(120);
    let profile = ProfileBacktest::from_toml_str(&format!(
        "{BASE_1H}[walkforward]\ntrain = \"400bars\"\ntest = \"100bars\"\n"
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in);
    let err = run_walkforward(&profile, store).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("NO windows") && msg.contains("120"), "{msg}");
}

/// A QUOTED bare number gets the grammar's own message, naming the field. (An UNQUOTED
/// `train = 90` is a TOML integer and fails as a serde TYPE error first — a different
/// message for the same mistake, accepted deliberately: see this stage's plan, D7.)
#[test]
fn a_quoted_bare_number_window_is_refused_by_name() {
    let err = ProfileBacktest::from_toml_str(&format!(
        "{BASE}[walkforward]\ntrain = \"90\"\ntest = \"30d\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("walkforward.train") && msg.contains("no unit"), "{msg}");
    let unquoted = ProfileBacktest::from_toml_str(&format!(
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
    let err = ProfileBacktest::from_toml_str(&format!(
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
    let err = ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\nmode = \"rolling\"\n"))
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
            ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 4\n{junk}\n"))
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
    let half = ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\ntrain = \"12mo\"\n"))
        .unwrap_err();
    assert!(format!("{half}").contains("BOTH"), "{half}");
    let zero = ProfileBacktest::from_toml_str(&format!(
        "{BASE}[walkforward]\ntrain = \"0d\"\ntest = \"3mo\"\n"
    ))
    .unwrap_err();
    let msg = format!("{zero}");
    assert!(msg.contains("walkforward.train") && msg.contains("zero-length"), "{msg}");
}

/// The pre-existing zero-split refusal survives the form change unchanged — and it is still
/// reached through the SPLIT-COUNT form, which is the half the form resolver could have
/// broken: `n_splits = 0` is `Some(0)`, so `window_form` must still answer
/// [`WalkforwardWindowForm::Splits`] rather than "no window form declared".
#[test]
fn a_zero_split_count_is_still_refused_at_load() {
    let err = ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
        .unwrap_err();
    assert!(format!("{err}").contains("n_splits must be >= 1"), "{err}");
    let config: ProfileWalkforward = toml::from_str("n_splits = 0").expect("the section parses");
    assert_eq!(config.window_form().unwrap(), WalkforwardWindowForm::Splits);
}

/// The duration form round-trips every suffix through the profile door, and mixing suffixes
/// inside it is legal: `train`/`test`/`step` are ONE form, and the exclusivity that matters
/// is against `n_splits`.
#[test]
fn the_duration_knobs_round_trip_every_suffix() {
    let walkforward = profile(
        "[walkforward]\ntrain = \"12mo\"\ntest = \"1000bars\"\nstep = \"3d\"\n\
             purge = \"8h\"\nembargo = \"1d\"\n",
    )
    .walkforward
    .expect("the section is present");
    assert_eq!(walkforward.train_span().unwrap(), Some(vike_model::time::Span::Months(12)));
    assert_eq!(walkforward.test_span().unwrap(), Some(vike_model::time::Span::Bars(1000)));
    assert_eq!(walkforward.step_span().unwrap(), Some(vike_model::time::Span::Ms(3 * 86_400_000)));
    assert_eq!(walkforward.purge_span().unwrap(), Some(vike_model::time::Span::Ms(8 * 3_600_000)));
    assert_eq!(walkforward.embargo_span().unwrap(), Some(vike_model::time::Span::Ms(86_400_000)));
    assert_eq!(walkforward.n_splits, None);
    assert_eq!(walkforward.window_form().unwrap(), WalkforwardWindowForm::Duration);
}

/// `n_splits` is OPTIONAL now, and a section carrying only it still answers the count form
/// with the same number it always did.
#[test]
fn the_split_count_form_still_answers_with_its_count() {
    let walkforward = profile("[walkforward]\nn_splits = 6\n").walkforward.expect("present");
    assert_eq!(walkforward.n_splits, Some(6));
    assert_eq!(walkforward.window_form().unwrap(), WalkforwardWindowForm::Splits);
    assert_eq!(walkforward.train_span().unwrap(), None);
    assert_eq!(walkforward.purge_span().unwrap(), None);
}

#[test]
fn the_walkforward_section_parses_and_defaults_to_absent() {
    assert!(profile("").walkforward.is_none(), "no [walkforward] ⇒ None (unchanged profiles)");
    assert_eq!(profile("[walkforward]\nn_splits = 4\n").walkforward.unwrap().n_splits, Some(4));
}

#[test]
fn a_zero_split_count_is_rejected_at_load() {
    let err = ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 0\n"))
        .unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.to_string().contains("n_splits"), "{err}");
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
            ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\nn_splits = 2\n{junk}\n"))
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
    let config = p.walkforward.as_ref().expect("the section is present");
    assert_eq!(config.n_splits, Some(4));
    assert!(config.search.is_none(), "search absent");
    assert!(config.mode.is_none(), "mode absent");
    assert!(config.rank_by.is_none(), "rank_by absent");
    assert_eq!(config.window_search().unwrap(), WalkforwardWindowSearch::None);
    assert_eq!(config.walk_mode().unwrap(), WalkforwardMode::Anchored);
    assert_eq!(config.rank_metric().unwrap(), RankMetric::default());
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
    assert_eq!(search("none"), WalkforwardWindowSearch::None);
    // Case and surrounding whitespace are forgiven — the `queue_model_kind` idiom. The SPELLING
    // is not: see below.
    for spelling in ["paramscan", "PARAMSCAN", " paramscan "] {
        assert_eq!(search(spelling), WalkforwardWindowSearch::Sweep, "search = {spelling:?}");
    }
    // `sweep`, the word before the owner's ruling 20 (phase 4), is refused like any other word.
    let old_word = "sweep";
    assert!(
        ProfileBacktest::from_toml_str(&format!(
            "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"{old_word}\"\n"
        ))
        .is_err(),
        "search = {old_word:?} is refused since ruling 20"
    );

    // ⚠ `"grid"` is REFUSED, and refused by NAME rather than falling into the catch-all. It
    // parsed as a synonym for exactly one release — the design doc said `sweep`, the first
    // implementation said `grid`, and both were accepted rather than choosing. A profile
    // written in that window has to be told what to write, not merely that it is wrong.
    let renamed = ProfileBacktest::from_toml_str(&format!(
        "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"grid\"\n"
    ))
    .unwrap_err();
    let renamed = format!("{renamed}");
    assert!(
        renamed.contains("renamed") && renamed.contains("\"paramscan\""),
        "the removed spelling must name its replacement, not just fail: {renamed}"
    );

    // ...and an actual typo still lands in the catch-all, which now advertises `paramscan`.
    let err = ProfileBacktest::from_toml_str(&format!(
        "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"gird\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unknown walkforward.search") && msg.contains("paramscan"), "{msg}");
}

/// `mode` round-trips both [`WalkforwardMode`] variants and refuses anything else at load. It is a
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
    assert_eq!(mode("anchored"), WalkforwardMode::Anchored);
    assert_eq!(mode("Rolling"), WalkforwardMode::Rolling);
    assert_eq!(mode(" rolling "), WalkforwardMode::Rolling);
    let err = ProfileBacktest::from_toml_str(&format!(
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
    let err = ProfileBacktest::from_toml_str(&format!(
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
    let err = ProfileBacktest::from_toml_str(&format!(
        "{BASE}[walkforward]\nn_splits = 2\nsearch = \"paramscan\"\n"
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("[paramscan]"), "the refusal names what is missing: {msg}");
    // The rule is about the PAIRING, not about the key: the same profile with a grid loads...
    assert!(
        ProfileBacktest::from_toml_str(&format!(
            "{BASE}{GRID}[walkforward]\nn_splits = 2\nsearch = \"paramscan\"\n"
        ))
        .is_ok(),
        "grid + [paramscan] is the legal pairing"
    );
    // ...and the CONTROL never needs one, so an explicit `none` beside no grid stays legal.
    assert!(
        ProfileBacktest::from_toml_str(&format!(
            "{BASE}[walkforward]\nn_splits = 2\nsearch = \"none\"\n"
        ))
        .is_ok(),
        "the control searches nothing, so it requires nothing"
    );
}

/// Every runner test needs SOME `HistoryStore` handle, even the guards that return before touching
/// it. This one is an EMPTY temp-dir `HistoryStoreDataFusion` (the `vike-store` dev-dependency), like
/// `run`'s own store-backed tests; the profile-shape tests above stay on
/// the trait-only double.
///
/// ⚠ CORRECTED. This comment used to argue that the lane choice was FORCED — "this crate has no
/// DataFusion-free double; `vike_data::HistoryStoreMemory` lives behind a `test-support` feature
/// vike-backtest does not dev-depend on". It does dev-depend on it, with that exact feature
/// (`crates/vike-backtest/Cargo.toml`), and a dev-dependency's features reach a `src/`
/// `#[cfg(test)]` module just as they reach `tests/` — [`memory_store_1h`] above is the proof, and
/// `crates/vike-backtest/tests/walkforward_optimize.rs` had been spending the same fact from
/// outside the crate the whole time. The tests below stay on the temp-dir store because that is
/// what they were written against and moving them would change what they exercise, not because
/// the alternative is unavailable.
fn empty_store() -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> =
        Arc::new(vike_store::HistoryStoreDataFusion::open(dir.path()).unwrap());
    (dir, store)
}

#[test]
fn a_profile_without_the_section_is_a_clean_error() {
    let (_dir, store) = empty_store();
    let err = run_walkforward(&profile(""), store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.to_string().contains("[walkforward]"));
}

#[test]
fn a_tick_profile_is_a_clean_error() {
    let (_dir, store) = empty_store();
    let toml = BASE.replace("kind = \"bar\"", "kind = \"tick\"");
    let p = ProfileBacktest::from_toml_str(&format!("{toml}[walkforward]\nn_splits = 2\n"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    // A tick profile walks its own lane (`super::ticks`), which counts windows in seed bars and is refused
    // in words without them.
    let err = run_walkforward(&p, store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.to_string().contains("seed_bar_interval_ms"));
}

/// An empty bar slice is a DATA error, not a silent zero-window report.
#[test]
fn an_empty_bar_slice_is_a_data_error() {
    let (_dir, store) = empty_store();
    let err = run_walkforward(&profile("[walkforward]\nn_splits = 2\n"), store).unwrap_err();
    assert_matches!(err, HarnessError::Data(ref m) if m.contains("no bars"), "{err}");
}

/// A cross-venue two-series slice cannot be walked forward (the splitter takes ONE series).
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
file = "trend/buy_hold.rs"
[walkforward]
n_splits = 2
"#;
    let p =
        ProfileBacktest::from_toml_str(toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let err = run_walkforward(&p, store).unwrap_err();
    assert_matches!(err, HarnessError::Validation(ref m) if m.to_string().contains("ONE bar series"));
}

/// End-to-end over a REAL store: one window per split, a stitched curve, finite summary stats.
#[test]
fn runs_one_window_per_split_over_a_real_store() {
    use vike_marketdata::Bar;
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars: Vec<Bar> = (0..240)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                timestamp: i as i64 * 1000,
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
    assert_eq!(rep.steps.len(), 4, "one OOS window per split");
    assert!(!rep.equity_curve.is_empty(), "the stitched curve is populated");
    assert!(rep.overall.total_return.is_finite());
    assert!((0.0..=1.0).contains(&rep.profitable_step_share));
    assert_bit_equal_to_the_fold_before_protocol_nine(&rep, 1000.0, periods_per_year(&p));
}

/// The harness door APPLIES its own resolver — the half a cross-plane test cannot see.
///
/// The cross-plane gate (`crates/vike-backtesthub/tests/walkforward_annualization.rs`) proves
/// the STUDIO door's applied factor and proves both doors' DERIVATIONS agree. What neither it
/// nor anything else covered is the single `periods_per_year(profile)` argument at this
/// module's `walk_forward_over_splits` call — and this branch exists precisely because a doc
/// comment asserted a property nothing checked.
///
/// ⚠ It runs at `"1h"` deliberately. `runs_one_window_per_split_over_a_real_store` above uses
/// `"1d"`, where the derived factor and a hardcoded `252` are THE SAME NUMBER — so that test
/// would pass against either spelling and cannot gate this. At `"1h"` they differ by
/// `sqrt(24)`, and the `assert_ne!` is what stops the bit-exact assertion from passing when
/// both sides regress together.
#[test]
fn the_harness_door_applies_the_derived_annualization_not_the_daily_anchor() {
    use vike_analytics::metrics::sharpe;
    use vike_analytics::results::DAILY_PERIODS_PER_YEAR;
    use vike_marketdata::Bar;
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars: Vec<Bar> = (0..240)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1;
            Bar {
                timestamp: i as i64 * 1000,
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

    let p = ProfileBacktest::from_toml_str(&format!(
        "{}{}",
        BASE.replace("interval = \"1d\"", "interval = \"1h\""),
        "[walkforward]\nn_splits = 4\n"
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in);
    let rep = run_walkforward(&p, store).unwrap();

    let derived = periods_per_year(&p);
    assert_eq!(derived, DAILY_PERIODS_PER_YEAR * 24.0, "1h resolves to 6,048");

    // Precondition: a dispersion-free curve makes `sharpe` return 0.0 at EVERY factor, which
    // would make the inequality below vacuous.
    let at_daily = sharpe(&rep.equity_curve, DAILY_PERIODS_PER_YEAR);
    assert!(at_daily != 0.0 && at_daily.is_finite(), "fixture must have dispersion: {at_daily}");

    assert_eq!(
        rep.overall.sharpe.to_bits(),
        sharpe(&rep.equity_curve, derived).to_bits(),
        "the reported overall.sharpe IS the stitched curve annualized at the derived factor"
    );
    assert_ne!(
        rep.overall.sharpe, at_daily,
        "...and NOT at the daily anchor — the regression this test exists to catch"
    );
    assert_bit_equal_to_the_fold_before_protocol_nine(&rep, 1000.0, derived);
}
/// `BASE` with a range wide enough for every bar [`seeded_store`] writes. The frozen `BASE`
/// stops at 100_000 ms, so a longer series would be silently TRUNCATED by the loader — and a
/// truncated series changes the split arithmetic, which is exactly what the tests below
/// measure.
fn wide_profile(extra: &str) -> ProfileBacktest {
    let base = BASE.replace("to = \"100000\"", "to = \"100000000\"");
    ProfileBacktest::from_toml_str(&format!("{base}{extra}"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// A real (temp-dir) store holding ONE bar series under `BASE`'s own `(venue, symbol,
/// interval)`: `closes` as flat OHLC bars one second apart. Flat OHLC is all these tests need —
/// `buy_hold` reads nothing but the close, and the engine fills at the NEXT bar's open.
fn seeded_store(closes: &[f64]) -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    use vike_marketdata::Bar;

    let dir = tempfile::tempdir().unwrap();
    let hist = vike_store::HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, &close)| Bar {
            timestamp: i as i64 * 1000,
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
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(hist);
    (dir, store)
}

/// Every headline number a [`WalkforwardResults`] carries, as IEEE-754 BIT PATTERNS. Two reports
/// then compare EXACTLY — and two `NaN` Sharpes compare EQUAL, which `f64` itself will not do — so
/// "these two runs produced the same report" cannot pass by accident on a degenerate curve.
/// The workspace's frozen-fixture convention, applied to a report instead of a file.
fn report_bits(rep: &WalkforwardResults) -> Vec<u64> {
    let mut out = vec![
        rep.overall.total_return.to_bits(),
        rep.overall.sharpe.to_bits(),
        rep.profitable_step_share.to_bits(),
        rep.steps.len() as u64,
    ];
    out.extend(rep.equity_curve.iter().map(|v| v.to_bits()));
    for w in &rep.steps {
        out.push(w.test_range.0 as u64);
        out.push(w.test_range.1 as u64);
        out.push(w.results.total_return.to_bits());
    }
    out
}

/// The `size` a searched window chose, read back out of its `chosen_params`.
fn chosen_size(rep: &WalkforwardResults, window: usize) -> f64 {
    rep.steps[window]
        .chosen_params
        .as_ref()
        .expect("a searched window records what it chose")
        .iter()
        .find(|(k, _)| k == "size")
        .and_then(|(_, v)| v.as_float())
        .expect("the grid's one axis is a float array named `size`")
}

/// THE CONTROL'S OWN GATE: [`WalkforwardWindowSearch::None`] through the optimizing driver reproduces the
/// fixed-parameter walk BIT FOR BIT. If those two disagreed, the control would be a different
/// program rather than a comparison, and every "the search bought nothing" verdict measured
/// against it would be measuring the seam instead of the search.
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
        control.steps.iter().all(|w| w.chosen_params.is_none()),
        "a window that chose nothing records nothing"
    );
    assert_bit_equal_to_the_fold_before_protocol_nine(&fixed, 1000.0, periods_per_year(&p));
    assert_bit_equal_to_the_fold_before_protocol_nine(&control, 1000.0, periods_per_year(&p));
}

/// The fixed walk HONOURS `[walkforward].mode` and cannot MOVE with it: the two modes differ in
/// `train_start` alone, and this driver's closure discards the training half. Pinned rather
/// than merely asserted in a doc, because it is precisely the invariance the optimizing driver
/// breaks — and the invariance is what makes wiring the knob into both drivers safe.
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
        "[paramscan]\nsize = [0.1, 1.0]\n",
        "[walkforward]\nn_splits = 2\nsearch = \"paramscan\"\nrank_by = \"return\"\n"
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
    let ppy = periods_per_year(&anchored_p);
    for searched in [&anchored, &rolling] {
        assert_bit_equal_to_the_fold_before_protocol_nine(searched, 1000.0, ppy);
    }
}

// ---- the ledger currency: a walk-forward books ONE currency too (decision 0183 item 13) ---------

/// A bar-mode `[walkforward]` profile over `symbol` on `oanda`, at hourly bars, with `extra` lines
/// appended under `[engine]`. Cash 10,000 in whatever currency the ledger is.
fn fx_walk_profile(symbol: &str, extra: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "oanda"
symbols = ["{symbol}"]
kind = "bar"
interval = "1h"
from = "0"
to = "9999999999999"

[engine]
cash = 10000.0
{extra}

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "{symbol}"

[walkforward]
n_splits = 2
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in)
}

/// `n` slowly-rising hourly bars of `symbol` from `px`, under venue `oanda`.
fn memory_store_fx(symbol: &str, px: f64, n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    use vike_marketdata::Bar;

    let store = vike_data::HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = px + i as f64 * 0.01;
            Bar {
                timestamp: i as i64 * 3_600_000,
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
    store.append_bars("oanda", symbol, "1h", &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// ⚠ **THE HOLE THIS CLOSES.** The fixed walk (and the optimizing walk's control arm) builds each
/// window's `StrategyEngine` straight from `bar_engine_params`, so neither guard site — `run_backtest`
/// and `run_backtest_over_bars` — was on its path: a `[walkforward]` profile on USDJPY at 150 with
/// `cash = 10000` booked 10,000 "dollars" where the truth is 66.62. Refused in words, by BOTH
/// drivers, before a bar is read.
#[test]
fn a_walk_over_a_foreign_quote_is_refused_on_the_dollar_ledger() {
    let p = fx_walk_profile("USDJPY", "");
    for refused in [
        run_walkforward(&p, memory_store_fx("USDJPY", 150.0, 120)),
        run_walkforward_optimized(&p, memory_store_fx("USDJPY", 150.0, 120)),
    ] {
        let m = match refused {
            Err(HarnessError::Data(m)) => m,
            other => panic!("a yen-quoted walk must be refused on the dollar ledger: {other:?}"),
        };
        assert!(m.contains("USDJPY (JPY)") && m.contains("USD ledger"), "{m}");
        assert!(m.contains("engine.account_currency = \"JPY\""), "names the way out: {m}");
    }
}

/// A stated ledger that is not the symbol's quote is refused too: `account_currency = "EUR"` on USDJPY
/// used to RUN on this door (the ledger check did nothing here), booking yen as euros.
#[test]
fn a_walk_whose_ledger_is_not_the_symbols_quote_is_refused() {
    let p = fx_walk_profile("USDJPY", "account_currency = \"EUR\"");
    let m = match run_walkforward(&p, memory_store_fx("USDJPY", 150.0, 120)) {
        Err(HarnessError::Data(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(m.contains("USDJPY (JPY)") && m.contains("EUR ledger"), "{m}");
    assert!(m.contains("JPY/EUR conversion-rate series"), "names the missing rate: {m}");
}

/// The matching ledger runs, and the report SAYS what its curve is in; a dollar walk carries no
/// label and serialises without the key (so every dollar document is byte-identical), and an absent
/// key and an explicit `"USD"` are the SAME report.
#[test]
fn a_walk_on_the_symbols_own_ledger_runs_and_a_dollar_walk_stays_unlabelled() {
    let yen = run_walkforward(
        &fx_walk_profile("USDJPY", "account_currency = \"JPY\""),
        memory_store_fx("USDJPY", 150.0, 120),
    )
    .expect("a yen ledger takes a yen-quoted walk");
    assert_eq!(yen.account_currency.as_deref(), Some("JPY"));
    let hourly = vike_analytics::results::periods_per_year_for_interval("1h");
    assert_bit_equal_to_the_fold_before_protocol_nine(&yen, 10_000.0, hourly);
    assert_eq!(serde_json::to_value(&yen).unwrap()["account_currency"], "JPY");

    let absent =
        run_walkforward(&fx_walk_profile("EURUSD", ""), memory_store_fx("EURUSD", 1.08, 120))
            .expect("a dollar-quoted walk runs as before");
    assert_eq!(absent.account_currency, None);
    let absent_json = serde_json::to_string(&absent).unwrap();
    assert!(
        !absent_json.contains("account_currency"),
        "no key on a dollar document: {absent_json}"
    );

    let stated = run_walkforward(
        &fx_walk_profile("EURUSD", "account_currency = \"usd\""),
        memory_store_fx("EURUSD", 1.08, 120),
    )
    .expect("an explicit dollar ledger runs");
    assert_eq!(serde_json::to_string(&stated).unwrap(), absent_json, "absent == explicit USD");
}

/// Two candidates of one window could not READ, in different words: the walk names the LOWER in grid order,
/// whichever the pool finished first (the lower one is held back here), so two runs refuse alike. Both loops.
#[test]
fn a_walk_refuses_with_the_first_failed_read_in_grid_order() {
    use crate::walkforward::combinatorial::{CombinatorialLane, CombinatorialSplits};
    use std::ops::Range;
    let p = profile("[paramscan]\nsize = [1.0, 2.0, 3.0]\n");
    let points = crate::search::paramscan::expand_paramscan(&p).unwrap();
    let score_of = |point: &ParamscanPoint| -> WalkScore {
        let row = |error: Option<&str>| ParamscanRow {
            overrides: point.overrides.clone(),
            report: None,
            error: error.map(str::to_string),
            score: None,
        };
        match point.overrides[0].1.as_float().map(|size| size as i64) {
            Some(1) => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                (row(Some("x")), f64::NAN, Some("the first point's read".to_string()))
            }
            Some(2) => (row(Some("y")), f64::NAN, Some("the second point's read".to_string())),
            _ => (row(None), 1.0, None),
        }
    };
    let run =
        |_: &ProfileBacktest, _: Range<usize>| Ok::<_, HarnessError>(TradingRecord::default());
    let named = |why: HarnessError| {
        let why = why.to_string();
        assert!(why.contains("the first point's read") && !why.contains("second"), "{why}");
    };
    let windows =
        [WalkforwardSplit { train_start: 0, train_end: 10, test_start: 10, test_end: 20 }];
    let score = |_: &Range<usize>, point: &ParamscanPoint| score_of(point);
    let lane =
        WalkLane { bars: None, hold: &|train: Range<usize>| train, score: &score, run: &run };
    let search = WalkforwardWindowSearch::Sweep;
    named(walk_windows(&p, &windows, &points, search, None, lane).expect_err("refused"));
    let splits = CombinatorialSplits { groups: 2, test_groups: 1, embargo_bars: 0 };
    let bounds = vike_analytics::validation::group_bounds(20, 2);
    let score = |point: &ParamscanPoint, _: Range<usize>| score_of(point);
    let lane =
        CombinatorialLane { bars: None, len: 20, score: &score, run: &run, data_warnings: &[] };
    let cut = crate::walkforward::combinatorial::combinatorial_paths;
    named(cut(&p, splits, &bounds, &points, search, None, lane).expect_err("refused"));
}
