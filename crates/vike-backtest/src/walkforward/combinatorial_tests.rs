use super::*;
use crate::search::paramscan::RankMetric;
use crate::walkforward::walkforward_tests::assert_bit_equal_to_the_fold_before_protocol_nine;
use vike_marketdata::Bar;

const BASE: &str = r#"
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

/// A two-point grid over `buy_hold`'s `size`: something for a searched split to choose between.
const GRID: &str = "[paramscan]\nsize = [1.0, 2.0]\n";

fn profile(extra: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!("{BASE}{extra}"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// `n` hourly bars that rise and fall in turn, so a group's return depends on where it sits.
fn store(n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    store_at(n, "1h", 3_600_000)
}

/// [`store`]'s bars at `interval`, `step_ms` apart.
fn store_at(n: usize, interval: &str, step_ms: i64) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = vike_data::HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = 100.0 + 10.0 * (i as f64 * 0.2).sin();
            Bar::ohlcv(i as i64 * step_ms, close, close, close, close, 1.0)
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", interval, &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// [`profile`] over minute bars, for [`store_at`]`(n, "1m", 60_000)`.
fn minute_profile(extra: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!("{}{extra}", BASE.replace("\"1h\"", "\"1m\"")))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

fn cut(groups: usize, test_groups: usize) -> CombinatorialSplits {
    CombinatorialSplits { groups, test_groups, embargo_bars: 0 }
}

fn walk(
    profile: &ProfileBacktest,
    splits: CombinatorialSplits,
    search: WalkforwardWindowSearch,
) -> Result<CombinatorialWalkforwardReport, HarnessError> {
    let objective = RankMetric::TotalReturn.objective();
    run_walkforward_combinatorial(profile, store(120), splits, search, &objective, None)
}

#[test]
fn the_control_walks_every_path_over_every_group_with_one_answer() {
    let report = walk(&profile(""), cut(4, 2), WalkforwardWindowSearch::None).unwrap();
    assert_eq!(report.splits, 6, "C(4, 2) splits");
    assert_eq!(report.paths.len(), 3, "C(3, 1) paths");
    let groups = group_bounds(120, 4);
    for path in &report.paths {
        let ranges: Vec<(usize, usize)> = path.steps.iter().map(|w| w.test_range).collect();
        assert_eq!(ranges, groups, "a path's steps are the groups, in time order");
        assert!(path.steps.iter().all(|w| w.chosen_params.is_none()), "the control chooses none");
        assert_eq!(
            path.equity_curve, report.paths[0].equity_curve,
            "the control trades the same parameters on every path"
        );
        let ppy = periods_per_year(&profile(""));
        assert_bit_equal_to_the_fold_before_protocol_nine(path, 1000.0, ppy);
        // The module doc's "A path's steps carry their COMPACT results"; `overall` keeps its own.
        assert!(
            path.steps
                .iter()
                .all(|s| s.results.extended.is_none() && s.results.closed_trades.is_none()),
            "a path's steps carry no long-form block"
        );
        assert!(path.overall.extended.is_some(), "each path's overall keeps its extended block");
    }
}

#[test]
fn a_searched_walk_records_each_groups_choice_on_every_path() {
    let report = walk(&profile(GRID), cut(4, 2), WalkforwardWindowSearch::Sweep).unwrap();
    assert_eq!(report.paths.len(), 3);
    for path in &report.paths {
        assert_eq!(path.steps.len(), 4);
        assert!(path.steps.iter().all(|w| w.chosen_params.is_some()), "{:?}", path.steps);
        assert_eq!(
            path.overall.strategy_file, "trend/buy_hold.rs",
            "each path is labelled like a walk"
        );
        let ppy = periods_per_year(&profile(GRID));
        assert_bit_equal_to_the_fold_before_protocol_nine(path, 1000.0, ppy);
    }
}

#[test]
fn an_embargoed_cut_still_walks_every_path() {
    let splits = CombinatorialSplits { groups: 5, test_groups: 2, embargo_bars: 3 };
    let report = walk(&profile(GRID), splits, WalkforwardWindowSearch::Sweep).unwrap();
    assert_eq!((report.splits, report.paths.len(), report.embargo_bars), (10, 4, 3));
}

#[test]
fn an_impossible_cut_or_a_search_with_nothing_to_search_is_refused() {
    for (groups, test_groups) in [(1, 1), (4, 0), (4, 4), (4, 5)] {
        let why = walk(&profile(""), cut(groups, test_groups), WalkforwardWindowSearch::None)
            .expect_err("no such cut");
        assert!(why.to_string().contains("test_groups"), "{why}");
    }
    let why = walk(&profile(""), cut(4, 2), WalkforwardWindowSearch::Sweep).expect_err("no grid");
    assert!(why.to_string().contains("[paramscan]"), "{why}");

    let objective = RankMetric::TotalReturn.objective();
    let why = run_walkforward_combinatorial(
        &profile(""),
        store(3),
        cut(4, 1),
        WalkforwardWindowSearch::None,
        &objective,
        None,
    )
    .expect_err("three bars cannot make four groups");
    assert!(why.to_string().contains("need at least 4 bars"), "{why}");
}

/// Form (d) of `[walkforward]` walks the same paths as the door that takes the cut by argument, the
/// control and the searched walk alike, and its `embargo` duration rounds UP to whole bars.
#[test]
fn the_profiles_combinatorial_form_walks_what_the_argument_door_walks() {
    let control = profile("[walkforward]\ngroups = 4\ntest_groups = 2\n");
    let by_table = run_walkforward_combinatorial_profile(&control, store(120), None).unwrap();
    let by_argument = walk(&profile(""), cut(4, 2), WalkforwardWindowSearch::None).unwrap();
    assert_eq!(by_table.paths.len(), by_argument.paths.len());
    for (a, b) in by_table.paths.iter().zip(&by_argument.paths) {
        assert_eq!(a.equity_curve, b.equity_curve);
    }

    let searched = profile(&format!(
        "{GRID}[walkforward]\ngroups = 4\ntest_groups = 2\nsearch = \"paramscan\"\n\
         rank_by = \"return\"\nembargo = \"90m\"\n"
    ));
    let report = run_walkforward_combinatorial_profile(&searched, store(120), None).unwrap();
    assert_eq!(report.embargo_bars, 2, "90 minutes of 1h bars is two bars of gap, never one");
    assert!(report.paths.iter().all(|p| p.steps.iter().all(|w| w.chosen_params.is_some())));
}

/// Form (d) is refused at load when it is mixed, half-declared, cuts no split or carries a key that
/// would configure nothing; and the stitched walk refuses it by name rather than guessing a list.
#[test]
fn the_combinatorial_form_is_refused_where_it_means_nothing() {
    for (table, needle) in [
        ("groups = 4\ntest_groups = 2\nn_splits = 3\n", "third PROTOCOL"),
        ("groups = 4\ntest_groups = 2\ntrain = \"10bars\"\ntest = \"5bars\"\n", "third PROTOCOL"),
        ("groups = 4\n", "BOTH walkforward.groups"),
        ("groups = 4\ntest_groups = 4\n", "cuts no split"),
        ("groups = 1\ntest_groups = 1\n", "cuts no split"),
        ("groups = 4\ntest_groups = 2\nmode = \"rolling\"\n", "configure nothing"),
        ("groups = 4\ntest_groups = 2\npurge = \"1h\"\n", "configure nothing"),
    ] {
        let why = ProfileBacktest::from_toml_str(&format!("{BASE}[walkforward]\n{table}"))
            .expect_err("refused at load");
        assert!(why.to_string().contains(needle), "{table:?}: {why}");
    }
    let combinatorial = profile("[walkforward]\ngroups = 4\ntest_groups = 2\n");
    let why = crate::walkforward::runner::run_walkforward(&combinatorial, store(120))
        .expect_err("the stitched walk has no list for form (d)");
    assert!(why.to_string().contains("combinatorial driver"), "{why}");
}

/// A path's every field but its curve, as its JSON says it (a `NaN` Sharpe is `null` on both sides).
fn numbers_of(path: &WalkforwardResults) -> serde_json::Value {
    let mut value = serde_json::to_value(path).unwrap();
    value.as_object_mut().unwrap().remove("equity_curve");
    value
}

/// The answer's curves are THINNED and its numbers are the WHOLE curves': with and without
/// `vike_runs::decimate`, every path reports the same overall and step results and profitable step
/// share, bit for bit, and differs in its curve alone, thinned exactly as a run record's series.
#[test]
fn a_thinned_path_reports_every_number_of_its_whole_curve() {
    const BARS: usize = 25_000;
    let profile = minute_profile(GRID);
    let objective = RankMetric::TotalReturn.objective();
    let whole = combinatorial_walk(
        &profile,
        store_at(BARS, "1m", 60_000),
        &|_, _| Ok(cut(4, 2)),
        WalkforwardWindowSearch::Sweep,
        &objective,
        None,
    )
    .unwrap();
    let thin = run_walkforward_combinatorial(
        &profile,
        store_at(BARS, "1m", 60_000),
        cut(4, 2),
        WalkforwardWindowSearch::Sweep,
        &objective,
        None,
    )
    .unwrap();
    assert_eq!((whole.equity_curve_stride, whole.equity_curve_len), (1, BARS));
    assert_eq!((thin.equity_curve_stride, thin.equity_curve_len), (2, BARS));
    assert_eq!((thin.groups, thin.test_groups, thin.splits), (4, 2, 6));
    assert_eq!(thin.paths.len(), whole.paths.len());
    let periods = periods_per_year(&profile);
    for (whole, thin) in whole.paths.iter().zip(&thin.paths) {
        assert_eq!(whole.equity_curve.len(), BARS, "one sample per bar");
        assert_eq!(
            thin.equity_curve,
            vike_runs::decimate(&whole.equity_curve, vike_runs::MAX_EQUITY_SAMPLES).0
        );
        assert_eq!(numbers_of(thin), numbers_of(whole));
        assert_bit_equal_to_the_fold_before_protocol_nine(whole, 1000.0, periods);
        let sharpe = vike_analytics::metrics::sharpe;
        assert_eq!(
            thin.overall.sharpe.to_bits(),
            sharpe(&whole.equity_curve, periods).to_bits(),
            "the reported Sharpe is the whole curve's"
        );
        assert_ne!(
            thin.overall.sharpe,
            sharpe(&thin.equity_curve, periods),
            "and the thinned curve's is another number, so this test can tell the two apart"
        );
    }
}

/// The control walk of `bars` minute bars under `cut`, framed as the daemon frames it (the report
/// as JSON text inside `Response::CombinatorialWalkforwardReport`): the WHOLE answer is over
/// `MAX_FRAME_LEN` and `write_frame` refuses it, the thinned one is written. Answers the thinned
/// report and its frame length.
fn refused_whole_and_carried_thinned(
    bars: usize,
    splits: CombinatorialSplits,
) -> (CombinatorialWalkforwardReport, usize) {
    use vike_backtesthub_client::proto::Response;
    use vike_hub_protocol::frame::{MAX_FRAME_LEN, write_frame};

    let objective = RankMetric::TotalReturn.objective();
    let whole = combinatorial_walk(
        &minute_profile(""),
        store_at(bars, "1m", 60_000),
        &|_, _| Ok(splits),
        WalkforwardWindowSearch::None,
        &objective,
        None,
    )
    .unwrap();
    let frame = |report: &CombinatorialWalkforwardReport| {
        let answer =
            Response::CombinatorialWalkforwardReport(serde_json::to_string(report).unwrap());
        let mut sink = Vec::new();
        (write_frame(&mut sink, &answer), serde_json::to_vec(&answer).unwrap().len())
    };
    let (written, whole_len) = frame(&whole);
    let thin = thinned(whole);
    let (thin_written, thin_len) = frame(&thin);
    eprintln!(
        "{} paths of {bars} minute bars: whole {whole_len} bytes, thinned {thin_len} bytes",
        thin.paths.len()
    );
    assert!(whole_len > MAX_FRAME_LEN as usize, "{whole_len}");
    let why = written.expect_err("the whole answer is over the cap");
    assert_eq!(why.kind(), std::io::ErrorKind::InvalidData, "{why}");
    assert!(thin_written.is_ok(), "the thinned answer crosses: {thin_len} bytes");
    (thin, thin_len)
}

/// The size the thinning exists for: a year of minute bars cut 6 / 3 (ten paths) is an answer the
/// wire refuses whole and carries thinned, each path keeping a run record's 20,000 samples.
#[test]
fn a_year_of_minute_bars_passes_the_frame_cap_whole_and_fits_thinned() {
    const BARS: usize = 525_600;
    let (thin, _) = refused_whole_and_carried_thinned(BARS, cut(6, 3));
    assert_eq!(thin.paths.len(), 10, "C(5, 2) paths");
    assert_eq!(thin.equity_curve_stride, BARS.div_ceil(vike_runs::MAX_EQUITY_SAMPLES));
}

/// The budget is the ANSWER's: 462 paths (12 / 6) over 12,000 minute bars fit thinned, at the
/// floor of 500 samples a path. A per-path budget of 20,000 would have kept all 12,000 samples of
/// every path, which is the whole answer the cap refuses.
#[test]
fn four_hundred_sixty_two_paths_fit_the_frame_cap_thinned() {
    const BARS: usize = 12_000;
    let (thin, thin_len) = refused_whole_and_carried_thinned(BARS, cut(12, 6));
    assert_eq!(thin.paths.len(), 462, "C(11, 5) paths");
    assert_eq!(curve_samples_per_path(462), MIN_CURVE_SAMPLES_PER_PATH, "the floor binds");
    assert_eq!(thin.equity_curve_stride, BARS.div_ceil(MIN_CURVE_SAMPLES_PER_PATH));
    assert!(thin.paths.iter().all(|p| p.equity_curve.len() <= 501));
    let cap = vike_hub_protocol::frame::MAX_FRAME_LEN as usize;
    assert!(thin_len < cap / 8, "well under the cap: {thin_len} bytes");
}

/// The budget's numbers over EVERY cut the ceiling admits: no cut makes more than 560 paths
/// (17 / 4), so no answer keeps more than 280,560 curve samples (the floor's 500 a path, plus the
/// last sample `decimate` may append) or more than `MAX_OUT_OF_SAMPLE_RUNS` windows.
#[test]
fn the_largest_admitted_cut_keeps_a_bounded_answer() {
    let (mut widest, mut most_samples) = ((0, 0, 0), 0);
    // `k = 1` is one path, so `MAX_OUT_OF_SAMPLE_RUNS` bounds `groups` itself.
    for groups in 2..=MAX_OUT_OF_SAMPLE_RUNS {
        // C(N - 1, k - 1) is symmetric and rises to its middle, so the lower half says it all.
        for test_groups in 1..=groups.div_ceil(2) {
            let runs = out_of_sample_runs(groups, test_groups);
            if runs > MAX_OUT_OF_SAMPLE_RUNS {
                break;
            }
            let paths = runs / groups;
            if paths > widest.2 {
                widest = (groups, test_groups, paths);
            }
            most_samples = most_samples.max(paths * (curve_samples_per_path(paths) + 1));
        }
    }
    assert_eq!(widest, (17, 4, 560));
    assert_eq!(most_samples, 280_560);
    assert_eq!(curve_samples_per_path(1), vike_runs::MAX_EQUITY_SAMPLES);
    assert_eq!(curve_samples_per_path(10), vike_runs::MAX_EQUITY_SAMPLES);
    assert_eq!(curve_samples_per_path(100), 2_000);
}

/// The ceiling is counted exactly at its edge and without overflow far past it, and a cut past it
/// is refused before any engine runs.
#[test]
fn a_cut_past_the_ceiling_is_refused_by_its_run_count() {
    assert_eq!(out_of_sample_runs(12, 6), 5_544);
    assert_eq!(out_of_sample_runs(17, 4), 9_520);
    assert_eq!(out_of_sample_runs(4, 2), 12, "C(3, 1) x 4");
    assert_eq!(out_of_sample_runs(MAX_OUT_OF_SAMPLE_RUNS, 1), MAX_OUT_OF_SAMPLE_RUNS);
    assert_eq!(out_of_sample_runs(14, 5), MAX_OUT_OF_SAMPLE_RUNS + 1, "10,010 is past it");
    assert_eq!(out_of_sample_runs(1_000_000, 500_000), MAX_OUT_OF_SAMPLE_RUNS + 1);
    let why = walk(&profile(""), cut(14, 5), WalkforwardWindowSearch::None).expect_err("past it");
    assert!(why.to_string().contains("out-of-sample group runs"), "{why}");
}

/// Every training segment a split searches and every group it tests runs on a VIEW of the bars held
/// once (`vike_simulator::BarSeriesShared`). Redone the old way, over each range's own copy
/// (`bars[range].to_vec()`): each split chooses the point its segments' copies rank first, and every
/// path's step is its group run alone over its own copy by that point, to the bit. The embargo cuts a
/// split's training into two segments, so a segment cut at the wrong bar is red.
#[test]
fn every_segment_and_group_scores_on_its_view_what_its_copy_scores() {
    let (p, store) = (profile(GRID), store(120));
    let (groups, test_groups, embargo) = (5, 2, 3);
    let splits = CombinatorialSplits { groups, test_groups, embargo_bars: embargo };
    let objective = RankMetric::TotalReturn.objective();
    let search = WalkforwardWindowSearch::Sweep;
    let report =
        run_walkforward_combinatorial(&p, store.clone(), splits, search, &objective, None).unwrap();
    let (loaded, funding) =
        crate::run::load_profile_bars(&p, &store, None, &mut Vec::new()).unwrap();
    let bars = &loaded[0].1;
    let copy = |range: Range<usize>| vec![("BTCUSDT".to_string(), bars[range].to_vec())];
    let points = expand_paramscan(&p).unwrap();
    let bounds = group_bounds(bars.len(), groups);
    // `expected[g]`: group g's results by the split that tested it, in the splitter's order.
    let mut expected: Vec<Vec<serde_json::Value>> = vec![Vec::new(); groups];
    let mut split_training = false;
    for (train, test) in combinatorial_purged_splits(bars.len(), groups, test_groups, embargo) {
        let segments = contiguous_runs(&train);
        split_training |= segments.len() > 1;
        let scores: Vec<f64> = points
            .iter()
            .map(|point| {
                let (mut weighted, mut weight) = (0.0, 0.0);
                for segment in &segments {
                    let owned = bars[segment.clone()].to_vec();
                    let (_, score) = evaluation_scored_point_over_bars(
                        &p,
                        &store,
                        vec![("BTCUSDT".to_string(), vike_simulator::BarSeriesShared::from(owned))],
                        &funding,
                        &[],
                        &objective,
                        &point.profile,
                        point.overrides.clone(),
                        &Arc::default(),
                    );
                    weighted += score * segment.len() as f64;
                    weight += segment.len() as f64;
                }
                weighted / weight
            })
            .collect();
        let mut order: Vec<usize> = (0..points.len()).collect();
        order.sort_by(|&a, &b| vike_analytics::metrics::cmp_scores_desc(scores[a], scores[b]));
        let winner = &points[order[0]];
        for (group, &(start, end)) in bounds.iter().enumerate() {
            if test.binary_search(&start).is_err() {
                continue;
            }
            let mut record = run_backtest_over_bars(
                &winner.profile,
                &store,
                copy(start..end),
                &funding,
                &[],
                &Arc::default(),
            )
            .unwrap();
            record.source = vike_analytics::TradingRecordSource::WalkforwardStep {
                window: (start, end),
                params: Some(winner.overrides.clone()),
            };
            let mut alone = vike_analytics::results::TradingResults::from_record(
                None,
                &record,
                periods_per_year(&p),
            );
            (alone.extended, alone.closed_trades) = (None, None);
            expected[group].push(serde_json::to_value(&alone).unwrap());
        }
    }
    assert!(split_training, "no split's training was cut in two");
    assert_eq!(report.paths.len(), expected[0].len());
    for (j, path) in report.paths.iter().enumerate() {
        for (g, step) in path.steps.iter().enumerate() {
            let got = serde_json::to_value(&step.results).unwrap();
            assert_eq!(got, expected[g][j], "path {j}, group {g}");
        }
    }
}

#[test]
fn contiguous_runs_split_at_every_hole() {
    assert_eq!(contiguous_runs(&[0, 1, 2, 5, 6, 9]), vec![0..3, 5..7, 9..10]);
    assert!(contiguous_runs(&[]).is_empty());
}
