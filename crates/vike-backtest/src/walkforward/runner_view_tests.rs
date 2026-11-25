//! A walk runs every window, every training half and every searched point on a VIEW of the bars its
//! pre-flight held once (`vike_simulator::BarSeriesShared`). Each run here is held to the one the old
//! path made over its own copy of the same bars (`bars[range].to_vec()`), to the bit.

use std::sync::Arc;

use vike_analytics::TradingRecordSource;
use vike_analytics::metrics::cmp_scores_desc;
use vike_analytics::results::TradingResults;
use vike_data::HistoryStore;
use vike_marketdata::Bar;
use vike_simulator::BarSeriesShared;

use super::*;
use crate::search::paramscan::expand_paramscan;
use crate::search::paramscan::grid::evaluation_scored_point_over_bars;

/// A two-point grid over `buy_hold`'s `size`: on a total-return ranking the larger size wins a
/// training half that rose and the smaller one a half that fell.
const GRID: &str = "[paramscan]\nsize = [1.0, 2.0]\n";

/// `n` hourly bars of a wave of 200 bars with a drift, so training halves ending at different bars
/// rose or fell, and their searches choose differently.
fn wave_store(n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = vike_data::HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close =
                100.0 + 10.0 * (i as f64 * std::f64::consts::TAU / 200.0).sin() + 0.001 * i as f64;
            Bar::ohlcv(i as i64 * 3_600_000, close, close, close, close, 1.0)
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1h", &bars, None).expect("the double stores bars");
    Arc::new(store)
}

/// `buy_hold` over [`wave_store`], walked in four splits of `mode`, with `extra` in its table.
fn walk_profile(mode: &str, extra: &str) -> ProfileBacktest {
    let toml = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1h\"\n\
         from = \"0\"\nto = \"9999999999999\"\n\n[engine]\ncash = 1000.0\n\n[strategy]\n\
         file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\nsymbol = \"BTCUSDT\"\n\n\
         [walkforward]\nn_splits = 4\nmode = \"{mode}\"\n{extra}"
    );
    ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in)
}

/// Each window's step is that window run alone over its own copy of the bars: both modes, fixed and
/// searched (the winner's point). A view cut at the wrong bar, or a run holding another's state, is red.
#[test]
fn each_window_of_a_walk_is_its_window_run_alone() {
    let store = wave_store(400);
    for mode in ["anchored", "rolling"] {
        for search in
            [String::new(), format!("search = \"paramscan\"\nrank_by = \"return\"\n{GRID}")]
        {
            let p = walk_profile(mode, &search);
            let report = if search.is_empty() {
                run_walkforward(&p, store.clone())
            } else {
                run_walkforward_optimized(&p, store.clone())
            };
            let report = report.expect("the walk runs");
            let (series, funding) = load_profile_bars(&p, &store, None, &mut Vec::new()).unwrap();
            let points = expand_paramscan(&p).unwrap();
            assert_eq!(report.steps.len(), 4);
            for step in &report.steps {
                let (start, end) = step.test_range;
                let point =
                    points.iter().find(|pt| step.chosen_params.as_ref() == Some(&pt.overrides));
                let alone = vec![("BTCUSDT".to_string(), series[0].1[start..end].to_vec())];
                let run_profile = point.map_or(&p, |pt| &pt.profile);
                let mut record = crate::run::run_backtest_over_bars(
                    run_profile,
                    &store,
                    alone,
                    &funding,
                    &[],
                    &Arc::default(),
                )
                .unwrap();
                record.source = TradingRecordSource::WalkforwardStep {
                    window: step.test_range,
                    params: step.chosen_params.clone(),
                };
                let alone = TradingResults::from_record(None, &record, periods_per_year(&p));
                assert_eq!(
                    serde_json::to_value(&step.results).unwrap(),
                    serde_json::to_value(&alone).unwrap(),
                    "{mode} {search:?}: the window {start}..{end}"
                );
            }
        }
    }
}

/// Every point of every window's TRAINING half scores on its view (the walk's own pre-flight, its own
/// window list) exactly the row and score that point scores alone over its own copy of that range, and
/// the point each window chose is the one those copies rank first. A training half cut at the wrong
/// bar moves a score and, on this wave, the choice.
#[test]
fn every_point_of_every_training_half_scores_on_its_view_what_its_copy_scores() {
    let store = wave_store(400);
    for mode in ["anchored", "rolling"] {
        let p =
            walk_profile(mode, &format!("search = \"paramscan\"\nrank_by = \"return\"\n{GRID}"));
        let report = run_walkforward_optimized(&p, store.clone()).expect("the walk runs");
        let (walk_mode, _, metric) = walkforward_knobs(&p).unwrap();
        let objective = metric.objective();
        let held = one_series_preflight(&p, &store, None).unwrap();
        let (symbol, view) = &held.series[0];
        let config = p.walkforward.as_ref().unwrap();
        let windows =
            crate::walkforward::windows::resolve_windows(config, view, &p.data.interval, walk_mode)
                .unwrap();
        let (loaded, _) = load_profile_bars(&p, &store, None, &mut Vec::new()).unwrap();
        let points = expand_paramscan(&p).unwrap();
        assert_eq!((windows.len(), points.len()), (report.steps.len(), 2));
        let mut chosen = Vec::new();
        for (split, step) in windows.iter().zip(&report.steps) {
            let train = split.train_start..split.train_end;
            let mut scores = Vec::new();
            for point in &points {
                let score = |bars: BarSeriesShared| {
                    evaluation_scored_point_over_bars(
                        &p,
                        &store,
                        vec![(symbol.clone(), bars)],
                        &held.funding,
                        &held.conversions,
                        &objective,
                        &point.profile,
                        point.overrides.clone(),
                        &Arc::default(),
                    )
                };
                let (on_view, view_score) = score(view.slice(train.clone()));
                let (alone, alone_score) = score(loaded[0].1[train.clone()].to_vec().into());
                assert_eq!(
                    serde_json::to_value(&on_view).unwrap(),
                    serde_json::to_value(&alone).unwrap(),
                    "{mode}: the training half {train:?} of {:?}",
                    point.overrides
                );
                assert_eq!(view_score.to_bits(), alone_score.to_bits());
                scores.push(alone_score);
            }
            let mut order: Vec<usize> = (0..points.len()).collect();
            order.sort_by(|&a, &b| cmp_scores_desc(scores[a], scores[b]));
            let first = &points[order[0]].overrides;
            assert_eq!(step.chosen_params.as_ref(), Some(first), "{mode}: the window {train:?}");
            chosen.push(first.clone());
        }
        assert!(chosen.windows(2).any(|w| w[0] != w[1]), "{mode}: every window chose one point");
    }
}
