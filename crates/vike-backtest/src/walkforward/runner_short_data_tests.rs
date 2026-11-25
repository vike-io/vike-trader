//! A walk over a SHORT series: the windows a range cannot form are refused in words naming the
//! numbers (bars, splits, the window length, the warm-up), and the edge sizes that can form them
//! run. Every case here once was, or sat next to, a walk that panicked or reported a window of zeros.

use std::sync::Arc;

use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_model::params::Params;
use vike_simulator::OrderBrokerSimulator;

use super::*;
use crate::run::profile::ProfileBacktest;
use crate::walkforward::combinatorial::run_walkforward_combinatorial_profile;

/// `n` one-minute bars of a wave with a drift, every bar's open the previous close, so a strategy
/// that trades on them trades on bars whose open and close differ.
fn store_1m(n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = vike_data::HistoryStoreMemory::new();
    let mut previous = 100.0_f64;
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = 100.0 + 5.0 * (i as f64 / 7.0).sin() + i as f64 * 0.01;
            let open = std::mem::replace(&mut previous, close);
            Bar {
                timestamp: i as i64 * 60_000,
                open,
                high: open.max(close) + 0.5,
                low: open.min(close) - 0.5,
                close,
                volume: 10.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// A one-minute `buy_hold` profile with `data_keys` under `[data]` and `extra` after it; `GRID` in
/// `extra` becomes a two-point `[paramscan]` table.
fn profile(data_keys: &str, extra: &str) -> ProfileBacktest {
    profile_built_by(vike_strategy_lab::stand_in, data_keys, extra)
}

/// [`profile`] whose strategy `build` builds.
fn profile_built_by(
    build: crate::run::strategy_builder::StandIn,
    data_keys: &str,
    extra: &str,
) -> ProfileBacktest {
    let extra = extra.replace("GRID", "[paramscan]\nsize = [1.0, 2.0]\n");
    ProfileBacktest::from_toml_str(&format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"9999999999999\"\n{data_keys}\n\n[engine]\ncash = 1000.0\n\n\
         [strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\n\
         symbol = \"BTCUSDT\"\n\n{extra}\n"
    ))
    .expect("the fixture profile loads")
    .with_stand_in(build)
}

/// Both walk drivers over `profile`: the fixed walk, and the searching one with the same table.
fn both_walks(
    profile: &ProfileBacktest,
    n: usize,
) -> [Result<WalkforwardResults, HarnessError>; 2] {
    [run_walkforward(profile, store_1m(n)), run_walkforward_optimized(profile, store_1m(n))]
}

/// A strategy that waits `WARMUP` bars of its own before its first decision, and makes none.
struct WarmsUp;
const WARMUP: usize = 150;
impl Strategy<OrderBrokerSimulator> for WarmsUp {
    fn warmup(&self) -> usize {
        WARMUP
    }
}
fn warms_up(_: &str, _: &Params) -> Result<Box<dyn Strategy<OrderBrokerSimulator> + Send>, String> {
    Ok(Box::new(WarmsUp))
}

/// A strategy whose warm-up is its `slow` param, as a moving-average cross's is its slow length.
struct WarmsUpBySlow(usize);
impl Strategy<OrderBrokerSimulator> for WarmsUpBySlow {
    fn warmup(&self) -> usize {
        self.0
    }
}
fn warms_up_by_slow(
    _: &str,
    params: &Params,
) -> Result<Box<dyn Strategy<OrderBrokerSimulator> + Send>, String> {
    let slow = params.get("slow").and_then(vike_model::params::ParamValue::as_f64).unwrap_or(0.0);
    Ok(Box::new(WarmsUpBySlow(slow as usize)))
}

/// **The reported walk**: 400 one-minute bars, `n_splits = 2`, on every mode of both drivers, ran.
#[test]
fn four_hundred_one_minute_bars_walk_in_two_splits_on_every_mode() {
    for mode in ["anchored", "rolling"] {
        for search in ["", "search = \"paramscan\"\nGRID"] {
            let walk =
                profile("", &format!("[walkforward]\nn_splits = 2\nmode = \"{mode}\"\n{search}"));
            for report in both_walks(&walk, 400) {
                let report = report.unwrap_or_else(|e| panic!("{mode} [{search}]: {e}"));
                let ranges: Vec<_> = report.steps.iter().map(|s| s.test_range).collect();
                assert_eq!(ranges, [(133, 266), (266, 400)], "{mode} [{search}]");
            }
        }
    }
}

/// **The panic, reproduced**: `n_splits = 2` over fewer than three bars cut three EMPTY chunks, so
/// `walkforward_splits` answered windows that all start at bar 0 and the stitch's ascending
/// `debug_assert` in `walk_forward_over_windows` panicked (a daemon test thread died on it; a release
/// daemon walked windows of no bars and reported zeros). Refused now, on both drivers and both modes.
#[test]
fn two_splits_over_fewer_than_three_bars_is_refused_not_a_panic() {
    for n in [1usize, 2] {
        for mode in ["anchored", "rolling"] {
            let walk = profile(
                "",
                &format!(
                    "[walkforward]\nn_splits = 2\nmode = \"{mode}\"\nsearch = \"paramscan\"\nGRID"
                ),
            );
            for refused in both_walks(&walk, n) {
                let why = refused.expect_err("three empty chunks").to_string();
                assert!(
                    why.contains(&format!(
                        "cuts the {n} bar(s) of this range into 3 equal chunks of 0 bar(s)"
                    )),
                    "{why}"
                );
            }
        }
    }
}

/// The edges of the split count over 400 bars: one split runs, 399 (chunks of one bar) runs, and 400
/// (chunks of none) is refused with the numbers and the largest count that fits, before the splitter
/// runs, so a count of a trillion costs nothing.
#[test]
fn a_split_count_whose_chunks_are_empty_is_refused_with_the_numbers() {
    let one = profile("", "[walkforward]\nn_splits = 1\n");
    let report = run_walkforward(&one, store_1m(400)).expect("one split runs");
    assert_eq!(report.steps.iter().map(|s| s.test_range).collect::<Vec<_>>(), [(200, 400)]);

    let most = profile("", "[walkforward]\nn_splits = 399\n");
    assert_eq!(
        run_walkforward(&most, store_1m(400)).expect("chunks of one bar run").steps.len(),
        399
    );

    for (k, chunks) in [(400usize, "401"), (1_000_000_000_000, "1000000000001")] {
        let walk =
            profile("", &format!("[walkforward]\nn_splits = {k}\nsearch = \"paramscan\"\nGRID"));
        for refused in both_walks(&walk, 400) {
            let why = refused.expect_err("empty chunks are refused").to_string();
            for words in [
                format!("n_splits = {k}"),
                "the 400 bar(s)".to_string(),
                format!("{chunks} equal chunks of 0 bar(s)"),
                "a window needs at least 1 bar".to_string(),
                "Set n_splits to at most 399".to_string(),
            ] {
                assert!(why.contains(&words), "{k}: missing {words:?} in {why}");
            }
        }
    }
}

/// The smallest series: two bars carry one split of one bar each; one bar carries none, and says so.
#[test]
fn the_smallest_series_walks_once_or_is_refused_in_words() {
    let walk = profile("", "[walkforward]\nn_splits = 1\nsearch = \"paramscan\"\nGRID");
    for report in both_walks(&walk, 2) {
        assert_eq!(report.expect("two bars, one split").steps.len(), 1);
    }
    for refused in both_walks(&walk, 1) {
        let why = refused.expect_err("one bar cannot be split").to_string();
        assert!(
            why.contains("No n_splits fits this range: one split needs at least 2 bars"),
            "{why}"
        );
    }
}

/// **A window no longer than the warm-up decides nothing**, so a walk of them is refused: at
/// `data.warmup = "133bars"` the two-split chunks of 133 bars are, the one-split chunk of 200 is
/// not. A warm-up equal to the whole series fits no split at all.
#[test]
fn a_window_inside_the_warm_up_is_refused_naming_it() {
    let warm =
        |k: usize| profile("warmup = \"133bars\"", &format!("[walkforward]\nn_splits = {k}\n"));
    for refused in both_walks(&warm(2), 400) {
        let why = refused.expect_err("chunks of 133 bars inside a 133-bar warm-up").to_string();
        for words in [
            "n_splits = 2",
            "chunks of 133 bar(s)",
            "at least 134 bars",
            "133 warm-up",
            "at most 1",
        ] {
            assert!(why.contains(words), "missing {words:?} in {why}");
        }
    }
    for report in both_walks(&warm(1), 400) {
        assert_eq!(report.expect("a chunk of 200 bars outlasts the warm-up").steps.len(), 1);
    }

    let whole = profile("warmup = \"400bars\"", "[walkforward]\nn_splits = 1\n");
    let why = run_walkforward(&whole, store_1m(400)).expect_err("nothing fits").to_string();
    assert!(
        why.contains("No n_splits fits this range: one split needs at least 802 bars"),
        "{why}"
    );
}

/// The STRATEGY's own warm-up is the engine's too, read off the built strategy.
#[test]
fn the_strategys_own_warm_up_counts() {
    let walk = profile_built_by(warms_up, "", "[walkforward]\nn_splits = 2\n");
    let why = run_walkforward(&walk, store_1m(400)).expect_err("150 > 133").to_string();
    assert!(why.contains(&format!("{WARMUP} warm-up bar(s)")), "{why}");
    assert!(why.contains("Set n_splits to at most 1"), "{why}");
}

/// **A SEARCHED walk is held to the warm-up of the grid point that warms up least**, never to the base
/// parameters', which it never runs. `slow = 200` beside a grid of 30, 50 and 80 over 600 bars in four
/// splits (chunks of 120): the fixed walk runs the base and is refused at 200; the searched walk, and
/// the searched combinatorial cut of five 120-bar groups, run. A grid whose least warm-up (130) is still
/// inside the chunk is refused at that number.
#[test]
fn a_searched_walk_is_held_to_the_least_warm_up_of_its_grid() {
    let walk = |grid: &str, extra: &str| {
        profile_built_by(
            warms_up_by_slow,
            "",
            &format!("slow = 200.0\n[walkforward]\n{extra}\n[paramscan]\nslow = [{grid}]\n"),
        )
    };
    let fits = "30.0, 50.0, 80.0";
    let why = run_walkforward(&walk(fits, "n_splits = 4"), store_1m(600))
        .expect_err("the fixed walk runs the base, slow = 200")
        .to_string();
    assert!(
        why.contains("first 200 warm-up bar(s) (data.warmup, else the strategy's own)."),
        "{why}"
    );

    let searched = walk(fits, "n_splits = 4\nsearch = \"paramscan\"");
    let report = run_walkforward_optimized(&searched, store_1m(600))
        .unwrap_or_else(|e| panic!("every grid point outlasts its 120-bar chunk: {e}"));
    assert_eq!(report.steps.len(), 4);
    let cut = walk(fits, "groups = 5\ntest_groups = 1\nsearch = \"paramscan\"");
    let report = run_walkforward_combinatorial_profile(&cut, store_1m(600), None)
        .unwrap_or_else(|e| panic!("every grid point outlasts its 120-bar group: {e}"));
    assert_eq!(report.splits, 5);

    let too_long = walk("130.0, 150.0", "n_splits = 4\nsearch = \"paramscan\"");
    let why = run_walkforward_optimized(&too_long, store_1m(600))
        .expect_err("the least grid warm-up, 130, is inside the chunk")
        .to_string();
    for words in [
        "chunks of 120 bar(s)",
        "first 130 warm-up bar(s)",
        "at the [paramscan] point that warms up least",
        "Set n_splits to at most 3",
    ] {
        assert!(why.contains(words), "missing {words:?} in {why}");
    }
    let cut = walk("130.0, 150.0", "groups = 5\ntest_groups = 1\nsearch = \"paramscan\"");
    let why = run_walkforward_combinatorial_profile(&cut, store_1m(600), None)
        .expect_err("the least grid warm-up, 130, is inside the group")
        .to_string();
    assert!(why.contains("first 130 warm-up bar(s)"), "{why}");
    assert!(why.contains("at the [paramscan] point that warms up least"), "{why}");
}

/// The DURATION form: a test window inside the warm-up is refused naming `walkforward.test`; a
/// searching walk's TRAINING half inside it is refused naming `walkforward.train`, and the same
/// windows walk unsearched, where the training half is never run.
#[test]
fn a_duration_window_inside_the_warm_up_is_refused_naming_its_key() {
    let short_test =
        profile("warmup = \"10bars\"", "[walkforward]\ntrain = \"100bars\"\ntest = \"10bars\"\n");
    let why = run_walkforward(&short_test, store_1m(400)).expect_err("test 10 <= 10").to_string();
    for words in [
        "warm-up of 10 bar(s)",
        "out-of-sample window is 10 bar(s)",
        "Lengthen walkforward.test past",
    ] {
        assert!(why.contains(words), "missing {words:?} in {why}");
    }

    let short_train = profile(
        "warmup = \"10bars\"",
        "[walkforward]\ntrain = \"5bars\"\ntest = \"50bars\"\nmode = \"rolling\"\nsearch = \"paramscan\"\nGRID",
    );
    let why = run_walkforward_optimized(&short_train, store_1m(400))
        .expect_err("train 5 <= 10")
        .to_string();
    for words in ["shortest training half 5", "walkforward.test and walkforward.train"] {
        assert!(why.contains(words), "missing {words:?} in {why}");
    }
    let report =
        run_walkforward(&short_train, store_1m(400)).expect("the fixed walk runs no train half");
    assert!(!report.steps.is_empty());
}

/// The duration form's spans at their edges: a window of one bar walks, and spans as long as an `i64`
/// count allows yield no window over 400 bars, refused as such rather than overflowing.
#[test]
fn duration_spans_at_their_edges_walk_or_are_refused() {
    let tiny = profile(
        "",
        "[walkforward]\ntrain = \"1bars\"\ntest = \"1bars\"\nsearch = \"paramscan\"\nGRID",
    );
    for report in both_walks(&tiny, 3) {
        assert_eq!(report.expect("1-bar windows over 3 bars").steps.len(), 2);
    }
    const HUGE: &str = "9223372036854775807bars";
    for (train, test, step) in
        [(HUGE, "1bars", "1bars"), ("1bars", HUGE, HUGE), ("1bars", "1bars", HUGE)]
    {
        let walk = profile(
            "",
            &format!(
                "[walkforward]\ntrain = \"{train}\"\ntest = \"{test}\"\nstep = \"{step}\"\npurge = \"{HUGE}\"\nembargo = \"{HUGE}\""
            ),
        );
        let why = run_walkforward(&walk, store_1m(400)).expect_err("no window fits").to_string();
        assert!(
            why.contains("yields NO windows over the 400 bar(s)"),
            "{train}/{test}/{step}: {why}"
        );
    }
    // A calendar span past ten thousand years overflowed the month arithmetic (a panic in a debug
    // build); it is refused naming the key.
    let ages = profile("", "[walkforward]\ntrain = \"9999999999mo\"\ntest = \"1mo\"\n");
    let why =
        run_walkforward(&ages, store_1m(400)).expect_err("longer than any series").to_string();
    assert!(why.contains("walkforward.train is 9999999999 months"), "{why}");

    // A gap ROUNDS UP to whole bars, and within one bar of `i64::MAX` milliseconds the round-up
    // overflowed (a panic in a debug build, wrong words in release); it is refused naming the key.
    let gap = profile(
        "",
        "[walkforward]\ntrain = \"10bars\"\ntest = \"10bars\"\npurge = \"153722867280912m\"\n",
    );
    let why = run_walkforward(&gap, store_1m(400)).expect_err("too long to count").to_string();
    assert!(
        why.contains("walkforward.purge is 9223372036854720000 ms, too long to count"),
        "{why}"
    );
}

/// The other modes over one, two and three bars answer (a report or a refusal) and never panic: a
/// grid search, the plain permutation test and permutation training, the class of short-data sweep
/// that found the walk's empty chunks.
#[test]
fn the_other_modes_answer_on_the_tiniest_series() {
    use crate::permutation::run_permutation_profile;
    use crate::search::paramscan::{RankMetric, run_paramscan};
    for n in [1usize, 2, 3] {
        let scan = profile("", "GRID");
        let report = run_paramscan(&scan, store_1m(n), RankMetric::default())
            .unwrap_or_else(|e| panic!("a scan over {n} bar(s): {e}"));
        assert_eq!(report.rows.len(), 2, "{n}");
        for train in ["false", "true\nGRID"] {
            let test = profile("", &format!("[permutation]\npermutations = 3\ntrain = {train}"));
            let report = run_permutation_profile(&test, store_1m(n), None).unwrap_or_else(|e| {
                panic!("a permutation test over {n} bar(s), train {train}: {e}")
            });
            assert_eq!(report.permuted_scores.len(), 3, "{n}");
        }
    }
}

/// The COMBINATORIAL form: a group no longer than the warm-up is refused with the numbers, and a
/// searching cut whose embargo leaves a split no training bar is refused naming the embargo, where it
/// read "every one of the 2 candidate(s) failed" with no cause.
#[test]
fn a_combinatorial_cut_that_cannot_decide_or_train_is_refused_in_words() {
    let cut = |data: &str, extra: &str| {
        profile(data, &format!("[walkforward]\ngroups = 4\ntest_groups = 2\n{extra}"))
    };
    let why = run_walkforward_combinatorial_profile(
        &cut("warmup = \"100bars\"", ""),
        store_1m(400),
        None,
    )
    .expect_err("groups of 100 bars inside a 100-bar warm-up")
    .to_string();
    for words in ["4 groups", "groups of 100 bar(s)", "100 warm-up", "Cut at most 3 groups"] {
        assert!(why.contains(words), "missing {words:?} in {why}");
    }
    let report =
        run_walkforward_combinatorial_profile(&cut("warmup = \"99bars\"", ""), store_1m(400), None)
            .expect("groups of 100 bars outlast a 99-bar warm-up");
    assert_eq!(report.splits, 6);

    let embargoed = cut("", "embargo = \"400bars\"\nsearch = \"paramscan\"\nGRID");
    let why = run_walkforward_combinatorial_profile(&embargoed, store_1m(400), None)
        .expect_err("an embargo of the whole series leaves no training bar")
        .to_string();
    for words in ["walkforward.embargo resolves to 400 bar(s)", "no training bar"] {
        assert!(why.contains(words), "missing {words:?} in {why}");
    }
}
