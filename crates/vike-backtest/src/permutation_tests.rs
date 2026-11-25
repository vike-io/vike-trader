use super::*;
use crate::search::paramscan::RankMetric;

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

/// A two-point grid over `buy_hold`'s `size`: something for permutation training to search.
const GRID: &str = "[paramscan]\nsize = [1.0, 2.0]\n";

fn profile(extra: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!("{BASE}{extra}"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// 80 hourly bars of a drifting zigzag under `symbol`, each with a real shape, the first at
/// `start_ms`.
fn write(store: &vike_data::HistoryStoreMemory, symbol: &str, start_ms: i64) {
    let bars: Vec<Bar> = (0..80)
        .map(|i| {
            let open = 100.0 + 5.0 * (i as f64 * 0.3).sin() + 0.1 * i as f64;
            let close = open * (1.0 + 0.004 * (i as f64 * 1.7).cos());
            let (high, low) = (open.max(close) * 1.001, open.min(close) * 0.999);
            Bar::ohlcv(start_ms + i as i64 * 3_600_000, open, high, low, close, 1.0)
        })
        .collect();
    store.append_bars("binance", symbol, "1h", &bars, None).expect("the double stores bars");
}

fn store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = vike_data::HistoryStoreMemory::new();
    write(&store, "BTCUSDT", 0);
    Arc::new(store)
}

fn objective() -> SearchObjective {
    RankMetric::TotalReturn.objective()
}

#[test]
fn the_plain_test_scores_every_history_and_reports_masters_p_value() {
    let report = run_permutation_test(&profile(""), store(), 9, 7, &objective(), None).unwrap();
    assert!(!report.trained);
    assert!(report.real_params.is_none(), "the plain test chooses nothing");
    assert!(report.real_score.is_finite());
    assert_eq!(report.permuted_scores.len(), 9);
    let at_or_above = report
        .permuted_scores
        .iter()
        .filter(|score| score.is_nan() || **score >= report.real_score)
        .count();
    assert_eq!(report.at_or_above, at_or_above);
    assert_eq!(report.p_value, (1 + at_or_above) as f64 / 10.0);
    assert_eq!(report.strategy_file, "trend/buy_hold.rs");
}

#[test]
fn one_seed_gives_one_report_whichever_thread_finishes_first() {
    let bits = |report: &PermutationTestReport| -> Vec<u64> {
        report.permuted_scores.iter().map(|score| score.to_bits()).collect()
    };
    let a = run_permutation_test(&profile(""), store(), 6, 3, &objective(), None).unwrap();
    let b = run_permutation_test(&profile(""), store(), 6, 3, &objective(), None).unwrap();
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(a.p_value, b.p_value);
}

#[test]
fn permutation_training_searches_every_history_and_names_the_real_winner() {
    let report =
        run_permutation_training(&profile(GRID), store(), 5, 11, &objective(), None).unwrap();
    assert!(report.trained);
    assert_eq!(report.permuted_scores.len(), 5);
    let winner = report.real_params.as_ref().expect("the real history's search chose a point");
    assert_eq!(winner[0].0, "size");
    assert_eq!(report.p_value, (1 + report.at_or_above) as f64 / 6.0);
}

/// Each history is scored on a VIEW (`vike_simulator::BarSeriesShared`): the real one on the bars the
/// pre-flight held once, each permuted one on its own series that every grid point shares. Redone the
/// old way, over each history's own copy (`.to_vec()` of the loaded bars, permuted as the test permutes
/// them), every score is the same to the bit, plain and trained, and training names the same winner.
#[test]
fn every_history_scores_on_its_view_what_its_own_copy_scores() {
    let store = store();
    let (loaded, funding) =
        crate::run::load_profile_bars(&profile(""), &store, None, &mut Vec::new()).unwrap();
    let views: Vec<&[Bar]> = loaded.iter().map(|(_, bars)| bars.as_slice()).collect();
    let history = |seed: Option<u64>| -> Vec<Bar> {
        seed.map_or_else(|| loaded[0].1.to_vec(), |s| permute_bars(&views, s).unwrap().remove(0))
    };
    // The best of `points` over a copy of `bars` by the search's one rule (`NaN` last, first wins).
    let best = |p: &ProfileBacktest, points: &[ParamscanPoint], bars: Vec<Bar>| -> (f64, usize) {
        let scores: Vec<f64> = points
            .iter()
            .map(|point| {
                evaluation_scored_point_over_bars(
                    p,
                    &store,
                    vec![("BTCUSDT".to_string(), BarSeriesShared::from(bars.clone()))],
                    &funding,
                    &[],
                    &objective(),
                    &point.profile,
                    point.overrides.clone(),
                    &Arc::default(),
                )
                .1
            })
            .collect();
        let mut order: Vec<usize> = (0..points.len()).collect();
        order.sort_by(|&a, &b| vike_analytics::metrics::cmp_scores_desc(scores[a], scores[b]));
        (scores[order[0]], order[0])
    };

    let plain = profile("");
    let report = run_permutation_test(&plain, store.clone(), 4, 7, &objective(), None).unwrap();
    let own = [ParamscanPoint { profile: plain.clone(), overrides: Vec::new() }];
    assert_eq!(report.real_score.to_bits(), best(&plain, &own, history(None)).0.to_bits());
    for (i, score) in report.permuted_scores.iter().enumerate() {
        let alone = best(&plain, &own, history(Some(history_seed(7, i)))).0;
        assert_eq!(score.to_bits(), alone.to_bits(), "plain history {i}");
    }

    let trained = profile(GRID);
    let points = expand_paramscan(&trained).unwrap();
    let report = run_permutation_training(&trained, store.clone(), 3, 11, &objective(), None);
    let report = report.unwrap();
    let (real, winner) = best(&trained, &points, history(None));
    assert_eq!(report.real_score.to_bits(), real.to_bits());
    assert_eq!(report.real_params.as_ref(), Some(&points[winner].overrides));
    for (i, score) in report.permuted_scores.iter().enumerate() {
        let alone = best(&trained, &points, history(Some(history_seed(11, i)))).0;
        assert_eq!(score.to_bits(), alone.to_bits(), "trained history {i}");
    }
}

/// Two runs could not READ, in different words: the test names the FIRST in its fixed order (the real history,
/// then each permuted one in seed order, and in one its grid's points in order), whichever finished first (the
/// first is held back here), so two runs of one test refuse alike. Plain and trained.
#[test]
fn a_permutation_test_refuses_with_the_first_failed_read_in_its_order() {
    // A history's one bar closes at its seed (the real one at 0); a point is its `size`, the plain run 0.
    let history_of = |s: u64| -> History {
        let bar = Bar::ohlcv(0, s as f64, s as f64, s as f64, s as f64, 1.0);
        vec![("BTCUSDT".to_string(), BarSeriesShared::from(vec![bar]))]
    };
    let run = |_: &ProfileBacktest, overrides: Vec<(String, ParamValue)>, history: History| {
        let which = history[0].1[0].close as i64;
        let point = overrides.first().and_then(|(_, v)| v.as_float()).map_or(0, |s| s as i64);
        let fail = |words: &str| (f64::NAN, Some(words.to_string()), Some(words.to_string()));
        match (which, point) {
            (1, 0 | 1) => {
                std::thread::sleep(std::time::Duration::from_millis(200));
                fail("history 1, its first run")
            }
            (1, 2) | (2, _) => fail("a later run"),
            _ => (1.0, None, None),
        }
    };
    let base = profile("[paramscan]\nsize = [1.0, 2.0, 3.0]\n");
    let points = expand_paramscan(&base).unwrap();
    let scorer = HistoryScorer { profile: &base, run: &run };
    for (train, points) in [(false, &[][..]), (true, &points[..])] {
        let seeds = vec![1, 2, 3];
        let refused = scorer
            .histories(points, history_of(0), &history_of, seeds, train, None)
            .expect_err("refused");
        let first =
            matches!(&refused, HarnessError::Data(why) if why == "history 1, its first run");
        assert!(first, "train {train}: {refused}");
    }
}

#[test]
fn nothing_to_compare_or_nothing_to_search_is_refused() {
    let why = run_permutation_test(&profile(""), store(), 0, 1, &objective(), None).unwrap_err();
    assert!(why.to_string().contains("at least one permuted history"), "{why}");
    let why =
        run_permutation_training(&profile(""), store(), 3, 1, &objective(), None).unwrap_err();
    assert!(why.to_string().contains("[paramscan]"), "{why}");
}

/// Two series of one length on two clocks: the load passes them (it compares lengths only), and
/// one draw cannot permute them jointly.
#[test]
fn a_misaligned_pair_of_symbols_is_refused_before_any_run() {
    let store = vike_data::HistoryStoreMemory::new();
    write(&store, "BTCUSDT", 0);
    write(&store, "ETHUSDT", 60_000);
    let two = BASE.replace("symbols = [\"BTCUSDT\"]", "symbols = [\"BTCUSDT\", \"ETHUSDT\"]");
    assert_ne!(two, BASE, "the fixture's text moved");
    let two =
        ProfileBacktest::from_toml_str(&two).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let why = run_permutation_test(&two, Arc::new(store), 3, 1, &objective(), None).unwrap_err();
    assert!(why.to_string().contains("aligned"), "{why}");
}

/// A `[permutation]` table runs the test it names: the same histories as the door that takes its
/// knobs by argument, and permutation training when `train` is set.
#[test]
fn a_permutation_table_runs_the_test_it_names() {
    let bits = |report: &PermutationTestReport| -> Vec<u64> {
        report.permuted_scores.iter().map(|score| score.to_bits()).collect()
    };
    let table = profile("[permutation]\npermutations = 4\nseed = 9\nrank_by = \"return\"\n");
    let by_table = run_permutation_profile(&table, store(), None).unwrap();
    let by_argument =
        run_permutation_test(&profile(""), store(), 4, 9, &objective(), None).unwrap();
    assert!(!by_table.trained);
    assert_eq!(bits(&by_table), bits(&by_argument));
    assert_eq!(by_table.p_value, by_argument.p_value);

    let training = profile(&format!("{GRID}[permutation]\npermutations = 2\ntrain = true\n"));
    let report = run_permutation_profile(&training, store(), None).unwrap();
    assert!(report.trained && report.real_params.is_some());
}

/// What is wrong with `[permutation]` on its own terms is refused at load.
#[test]
fn a_permutation_table_is_refused_where_it_means_nothing() {
    let grid_without_train = format!("{GRID}[permutation]\npermutations = 3\n");
    for (extra, needle) in [
        ("[permutation]\npermutations = 0\n", "permutation.permutations must be >= 1"),
        ("[permutation]\npermutations = 3\nrank_by = \"profit\"\n", "unknown permutation.rank_by"),
        ("[permutation]\npermutations = 3\ntrain = true\n", "has none"),
        (grid_without_train.as_str(), "searched only by permutation"),
        ("[permutation]\npermutations = 3\n[walkforward]\nn_splits = 2\n", "two programs"),
    ] {
        let why =
            ProfileBacktest::from_toml_str(&format!("{BASE}{extra}")).expect_err("refused at load");
        assert!(why.to_string().contains(needle), "{extra:?}: {why}");
    }
}

#[test]
fn a_stopped_test_answers_the_cancelled_refusal() {
    let stop = AtomicBool::new(true);
    let why =
        run_permutation_test(&profile(""), store(), 3, 1, &objective(), Some(&stop)).unwrap_err();
    assert_eq!(why.to_string(), cancelled_run().to_string());
}
