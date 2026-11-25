use super::*;
use crate::search::paramscan;
use vike_model::params::ParamValue;
use vike_model::run_request::progress::ProgressMode;

fn profile(params: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
file = "trend/buy_hold.rs"
{params}
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in)
}

/// A [`SearchEvaluator`] with NO backtest behind it: it scores a candidate off its `size`
/// override alone and records the width of every batch it was handed. Enough to drive dispatch
/// end to end, which is the whole point — the seam must be exercisable without a store.
#[derive(Default)]
struct StubEvaluator {
    batches: std::sync::Mutex<Vec<usize>>,
}

impl SearchEvaluator for StubEvaluator {
    fn evaluate(&self, batch: Vec<SearchCandidate>) -> Vec<SearchEvaluation> {
        self.batches.lock().expect("no test panics while holding this").push(batch.len());
        batch
            .into_iter()
            .map(|c| {
                let score = c
                    .iter()
                    .find(|(k, _)| k == "size")
                    .and_then(|(_, v)| v.as_float())
                    .unwrap_or(f64::NAN);
                SearchEvaluation {
                    row: ParamscanRow {
                        overrides: c,
                        report: None,
                        // No backtest ran, so there is no report to carry — the `score` is the
                        // stub's own, which is all the ranking below reads.
                        error: Some("stub evaluator: no backtest".to_string()),
                        score: Some(score),
                    },
                    score,
                }
            })
            .collect()
    }

    fn rank_by(&self) -> RankBy {
        RankBy::Objective("stub".to_string())
    }
}

/// A [`Search`] with no algorithm: it submits one fixed batch and returns what came back.
/// It exists to prove `Box<dyn Search>` DISPATCH works — that the trait is object-safe, that
/// [`optimize`] runs the preflight, [`Search::accepts`] and [`Search::search`] in that
/// order, and that [`report_from_outcome`] is what ranks — with no backtest, no store and no
/// store engine anywhere near it. A fourth real method costs one match arm at a
/// dispatching call site and nothing here.
struct StubOptimizer {
    sizes: Vec<f64>,
}

impl Search for StubOptimizer {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn search(
        &self,
        _base: &ProfileBacktest,
        evaluation: &dyn SearchEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let batch: Vec<SearchCandidate> =
            self.sizes.iter().map(|&s| vec![("size".to_string(), ParamValue::Float(s))]).collect();
        let rows = evaluation.evaluate(batch).into_iter().map(|e| e.row).collect();
        Ok(SearchOutcome { rows, summary: Some("stub: 1 batch".to_string()) })
    }
}

#[test]
fn a_boxed_optimizer_dispatches_and_its_outcome_is_ranked() {
    let base = profile("[strategy.params]\nsize = 1.0");
    let evaluation = StubEvaluator::default();
    // The whole point of object safety: the method is chosen at RUNTIME, behind a box.
    let opt: Box<dyn Search> = Box::new(StubOptimizer { sizes: vec![2.0, 5.0, 3.0] });
    assert_eq!(opt.name(), "stub");

    let out = optimize(opt.as_ref(), &base, &evaluation).expect("the stub search runs");

    assert_eq!(
        *evaluation.batches.lock().unwrap(),
        vec![3],
        "one batch of three, exactly as submitted — nothing above the searcher re-batches it"
    );
    assert_eq!(out.summary.as_deref(), Some("stub: 1 batch"), "the summary rides through");
    // ⚠ …and it rides through in BOTH places: `SearchReport.summary` for the engine binary, which
    // has printed it to stderr since euler shipped, and `report.summary` for a caller that only
    // ever sees the JSON document — every REMOTE one. One value, two readers. Before stage 7 a
    // remote euler or tpe search reported its budget nowhere at all.
    assert_eq!(
        out.report.summary.as_deref(),
        Some("stub: 1 batch"),
        "the report carries it too, for the caller that never sees stderr"
    );
    assert_eq!(out.report.rank_by, RankBy::Objective("stub".to_string()));
    // Submitted 2, 5, 3 (evaluation order); the ASSEMBLER is what puts them best-first.
    let ranked: Vec<f64> = out
        .report
        .rows
        .iter()
        .map(|r| r.overrides[0].1.as_float().expect("a float axis"))
        .collect();
    assert_eq!(ranked, vec![5.0, 3.0, 2.0], "the assembler ranks, not the searcher");
}

/// A base profile whose `strategy.params` is not a table never LOADS (`strategy.params` is a
/// `vike_model::params::Params`), so no searcher can be handed one — which is what makes
/// [`SearchEvaluator::evaluate`] infallible by type: there is no point build left to fail.
#[test]
fn a_non_table_params_profile_never_reaches_a_search() {
    let text = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\n\
                to = \"100000\"\n[engine]\ncash = 1000.0\n[strategy]\nfile = \"trend/buy_hold.rs\"\nparams = 3\n";
    match ProfileBacktest::from_toml_str(text) {
        Err(HarnessError::Parse(_)) => {}
        Err(other) => panic!("refused through the wrong door: {other:?}"),
        Ok(_) => panic!("a non-table strategy.params must not load"),
    }
}

// ── The statistical-significance floor ────────────────────────────────────────────────────

/// A report carrying the ONE field the floor judges, and nothing else pretending to be real.
fn report_with(n_trades: usize) -> vike_analytics::results::TradingResults {
    vike_analytics::results::TradingResults {
        name: None,
        final_equity: 1000.0,
        total_return: 0.0,
        n_trades,
        win_rate: 1.0,
        sharpe: 0.0,
        max_drawdown: 0.0,
        profit_factor: 1.0,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    }
}

fn scored(n_trades: usize, score: f64) -> SearchEvaluation {
    SearchEvaluation {
        row: ParamscanRow {
            overrides: vec![],
            report: Some(report_with(n_trades)),
            error: None,
            score: Some(score),
        },
        score,
    }
}

/// A DISARMED floor returns the evaluation untouched — the property that makes an unarmed run
/// byte-identical to one built before the knob existed, which is what
/// `crates/vike-backtesthub/tests/backtesthub_profile_roundtrip.rs`'s byte comparison rests on.
#[test]
fn a_disarmed_floor_changes_nothing() {
    let out = apply_trade_floor(scored(0, 1.5), SearchTradeFloor::DISARMED);
    assert_eq!(out.score.to_bits(), 1.5f64.to_bits());
    assert_eq!(out.row.score.map(f64::to_bits), Some(1.5f64.to_bits()));
    // …including a zero-trade run, which is legitimate information about a parameter region and
    // must stay rankable when nobody asked for a floor.
    assert!(SearchTradeFloor::DISARMED.admits(0));
    assert!(!SearchTradeFloor::DISARMED.is_armed());
    assert_eq!(SearchTradeFloor::new(0), SearchTradeFloor::DISARMED, "an explicit 0 IS disarmed");
}

/// The defect in one assertion: a two-trade point scoring 99 loses to a fifty-trade point
/// scoring 1, once the floor is armed. **Both** the steering score and the STAMPED row score
/// are floored — flooring only the steering value would move the refinement centre and leave
/// the report's #1 row the thin point, because `sort_scored_rows` reads the row.
#[test]
fn a_thin_point_is_unrankable_in_both_places() {
    let floor = SearchTradeFloor::new(50);
    let thin = apply_trade_floor(scored(2, 99.0), floor);
    assert!(thin.score.is_nan(), "the steering score is unrankable: {}", thin.score);
    assert!(thin.row.score.expect("still stamped").is_nan(), "…and so is the stamped one");

    let solid = apply_trade_floor(scored(50, 1.0), floor);
    assert_eq!(solid.score.to_bits(), 1.0f64.to_bits(), "exactly at the floor is ADMITTED");

    // The ordering rule the whole flag exists to change.
    assert_eq!(
        vike_analytics::metrics::cmp_scores_desc(thin.score, solid.score),
        std::cmp::Ordering::Greater,
        "the thin point must sort AFTER the solid one"
    );
}

/// ⚠ A FAILED point keeps `score: None` rather than gaining a stamped `NaN` — that would add a
/// `"score": null` key to a document that had none, which is a `--json` shape change for a row
/// kind that was already unrankable.
#[test]
fn a_failed_point_gains_no_stamped_score() {
    let failed = SearchEvaluation {
        row: ParamscanRow {
            overrides: vec![],
            report: None,
            error: Some("boom".to_string()),
            score: None,
        },
        score: f64::NAN,
    };
    let out = apply_trade_floor(failed, SearchTradeFloor::new(50));
    assert!(out.row.score.is_none(), "no report, no trade count, no stamp");
    assert!(out.row.error.is_some(), "and the error survives");
}

/// The report is deliberately UNTOUCHED, so `n_trades` stays in the `--json` document — the only
/// way an operator can see WHY a row fell to the bottom, since `ParamscanRow`'s
/// report-XOR-error rule forbids writing an explanation into `error`.
#[test]
fn the_floor_leaves_the_evidence_readable() {
    let out = apply_trade_floor(scored(3, 42.0), SearchTradeFloor::new(50));
    assert_eq!(out.row.report.expect("the report survives").n_trades, 3);
    assert!(out.row.error.is_none(), "a thin row is not a FAILED row");
}

// ── Progress ──────────────────────────────────────────────────────────────────────────────

/// A recording sink — the shape a caller other than [`SearchProgressStderr`] takes, and the proof that
/// [`SearchProgressSink`] is drivable with no terminal anywhere near it.
/// One recorded `SearchProgressEvent`, flattened to the three fields a test asserts on:
/// `(completed, total, best)`. Named because `clippy::type_complexity` refuses the inline
/// `Mutex<Vec<(u64, Option<u64>, Option<f64>)>>` under `-D warnings`, and because the name is
/// what the assertions below read as.
type SeenPoint = (u64, Option<u64>, Option<f64>);

#[derive(Default)]
struct RecordingSink {
    seen: std::sync::Mutex<Vec<SeenPoint>>,
}

impl SearchProgressSink for RecordingSink {
    fn point(&self, event: &SearchProgressEvent) {
        self.seen.lock().expect("no test panics holding this").push((
            event.completed,
            event.total,
            event.best,
        ));
    }
}

/// So the test can keep a handle on the sink the tracker owns. `Arc<T>` forwards to `T`, which
/// is also the shape a real caller wanting to read its own sink back would use.
impl<T: SearchProgressSink> SearchProgressSink for Arc<T> {
    fn point(&self, event: &SearchProgressEvent) {
        (**self).point(event);
    }
}

/// The running best is folded through THE ordering rule and never `f64::max`: a `NaN` point may
/// not become the incumbent, and the first rankable point must.
#[test]
fn the_running_best_never_crowns_an_unrankable_point() {
    let sink = Arc::new(RecordingSink::default());
    let tracker = SearchProgressTracker::new(Box::new(Arc::clone(&sink)), Some(4));
    tracker.observe(f64::NAN);
    tracker.observe(1.0);
    tracker.observe(f64::NAN);
    tracker.observe(0.5);
    let seen = sink.seen.lock().expect("readable").clone();
    assert_eq!(seen.len(), 4, "one event per point: {seen:?}");
    assert_eq!(seen[0].2, None, "nothing rankable yet");
    assert_eq!(seen[1].2, Some(1.0));
    assert_eq!(seen[2].2, Some(1.0), "a NaN does not displace a finite incumbent");
    assert_eq!(seen[3].2, Some(1.0), "and neither does a WORSE finite one");
    assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert!(seen.iter().all(|s| s.1 == Some(4)), "the total is carried on every event");
}

/// An ETA needs a total AND elapsed time. Neither alone produces one — a mean of zero would
/// render `eta 0.0s` on a search about to take an hour.
#[test]
fn an_eta_is_withheld_rather_than_guessed() {
    let event = |completed: u64, total: Option<u64>, ms: u64| SearchProgressEvent {
        completed,
        total,
        elapsed: Duration::from_millis(ms),
        best: None,
    };
    assert_eq!(event(1, None, 10_000).eta(), None, "euler's case: no denominator, no ETA");
    assert_eq!(event(1, Some(100), 0).eta(), None, "a zero mean is not an estimate");
    assert_eq!(
        event(10, Some(100), 10_000).eta(),
        Some(Duration::from_secs(90)),
        "1s/point, 90 to go"
    );
    assert_eq!(event(100, Some(100), 10_000).eta(), Some(Duration::ZERO), "finished");
}

/// ⚠ The JSON line must stay PARSEABLE, which is the whole reason a non-finite best renders
/// `null` rather than a bare `NaN` token — on the one output shape whose purpose is being
/// parsed, an unparseable line is the worst available failure. Matches `sweep`'s `serialize_opt_score`
/// rule: `null` means unrankable on both surfaces.
#[test]
fn the_json_event_renders_null_rather_than_a_nan_token() {
    let event =
        SearchProgressEvent { completed: 3, total: None, elapsed: Duration::ZERO, best: None };
    let line = event.render_json();
    assert!(line.contains("\"best\":null"), "{line}");
    assert!(line.contains("\"total\":null"), "{line}");
    assert!(line.contains("\"eta_s\":null"), "{line}");
    assert!(!line.to_ascii_lowercase().contains("nan"), "no NaN token anywhere: {line}");
    assert!(line.starts_with('{') && line.ends_with('}'), "one self-contained object: {line}");
    assert!(!line.contains('\n'), "the SINK owns the line ending: {line}");

    let nan_best = SearchProgressEvent {
        completed: 1,
        total: Some(2),
        elapsed: Duration::ZERO,
        best: Some(f64::NAN),
    };
    assert!(nan_best.render_json().contains("\"best\":null"), "a stamped NaN nulls too");
}

/// `ProgressMode::Off` constructs NO sink, so nothing is counted and nothing can be printed.
#[test]
fn off_builds_no_sink_at_all() {
    assert!(SearchProgressStderr::for_mode(ProgressMode::Off).is_none());
    assert!(SearchProgressStderr::for_mode(ProgressMode::Json).is_some(), "json is unconditional");
}

/// The throttle drops intermediate human lines and ALWAYS passes the final one — a progress line
/// frozen at `61/64` above a printed report reads as a search that stopped early.
#[test]
fn the_human_throttle_still_reports_the_finish() {
    let sink = SearchProgressStderr::human();
    let at = |completed: u64, ms: u64| SearchProgressEvent {
        completed,
        total: Some(4),
        elapsed: Duration::from_millis(ms),
        best: None,
    };
    assert!(sink.due(&at(1, 0)), "the first line always goes");
    assert!(!sink.due(&at(2, 10)), "10ms later is inside the window");
    assert!(sink.due(&at(3, 5_000)), "5s later is not");
    assert!(sink.due(&at(4, 5_001)), "…and the FINAL point passes regardless");
}

/// JSON mode is unthrottled: a machine consuming a stream wants every event.
#[test]
fn json_mode_emits_every_event() {
    let sink = SearchProgressStderr::json();
    for completed in 1..=5u64 {
        let event = SearchProgressEvent {
            completed,
            total: Some(5),
            elapsed: Duration::from_millis(1),
            best: None,
        };
        assert!(sink.due(&event), "event {completed} must not be throttled");
    }
}

// ── budget_hint ───────────────────────────────────────────────────────────────────────────

/// The grid's total is exact and equals what it evaluates; euler and genetic answer `None`
/// rather than publishing a bound their runs beat. ⚠ Read the `None`s as ASSERTIONS, not as
/// omissions — each impl's doc carries the argument, and a future method that CAN promise a
/// number joins this test by changing its row.
#[test]
fn only_the_exact_methods_promise_a_total() {
    let p = profile("[strategy.params]\nsize = 1.0\n[paramscan]\nsize = [1.0, 2.0, 3.0]");
    assert_eq!(crate::search::paramscan::SearchGrid.budget_hint(&p), Some(3));
    assert_eq!(
        crate::search::tpe::SearchTpe::new(vike_ml_training::search::tpe::TpeConfig::new(8, 1))
            .budget_hint(&p),
        Some(8),
        "tpe's loop is `for _ in 0..n_trials` with no early stop and no dedup"
    );
    assert_eq!(
        crate::search::euler::SearchEuler::new(crate::search::SearchEulerConfig::default())
            .budget_hint(&p),
        None,
        "euler's cost is an upper bound it routinely beats"
    );
    assert_eq!(
        crate::search::genetic::SearchGenetic::new(
            crate::search::genetic::SearchGeneticConfig::new(1)
        )
        .budget_hint(&p),
        None,
        "genetic dedups and can converge early"
    );
}

// ── the overfitting statistics ──────────────────────────────────────────────────────────────

/// A scored row carrying a `size` override and nothing else — enough to key
/// [`OverfitCapturedReturns`] and to be ranked.
fn scored_row(size: f64, score: f64) -> ParamscanRow {
    ParamscanRow {
        overrides: vec![("size".to_string(), ParamValue::Float(size))],
        report: None,
        error: None,
        score: Some(score),
    }
}

/// Build a capture from `(size, column)` pairs keyed by the SAME [`SearchCandidate`] the evaluator keys
/// by, so the join being tested is the real one.
fn captured(buckets: usize, cols: &[(f64, Vec<f64>)]) -> OverfitCapturedReturns {
    let by_candidate = cols
        .iter()
        .map(|(size, col)| {
            let cand: SearchCandidate = vec![("size".to_string(), ParamValue::Float(*size))];
            (cand, col.clone())
        })
        .collect();
    OverfitCapturedReturns { buckets, by_candidate }
}

/// A deterministic pseudo-random column of `t` small returns, seeded so two trials differ.
fn column(t: usize, seed: u64) -> Vec<f64> {
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..t)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            // ±1%, centred a hair above zero so the series is not degenerate.
            ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.02 + 0.0002
        })
        .collect()
}

/// ⚠ **The whole point of the feature, end to end: a matrix of bucketed returns produces a
/// PBO, an effective-N and a deflated Sharpe.** Before this existed the numbers were
/// uncomputable because the curve was destroyed in `paramscan::grid::row_from_outcome`.
#[test]
fn a_bucketed_matrix_yields_the_three_statistics() {
    let t = DEFAULT_CSCV_SPLITS * 4;
    let cols: Vec<(f64, Vec<f64>)> = (0..6).map(|i| (i as f64, column(t, i as u64 + 1))).collect();
    let rows: Vec<ParamscanRow> = (0..6).map(|i| scored_row(i as f64, 6.0 - i as f64)).collect();

    let stats = overfit_stats(&rows, &captured(t, &cols), DEFAULT_CSCV_SPLITS)
        .expect("six columns of T = 64 is a computable matrix");
    assert_eq!(stats.trials, 6, "one column per row");
    assert_eq!(stats.buckets, t, "T is the column LENGTH");
    assert_eq!(stats.splits, DEFAULT_CSCV_SPLITS);
    assert_eq!(stats.excluded, 0);
    assert_eq!(stats.observations, t, "n_obs is T — see OverfitReturnBuckets' invariance argument");
    let pbo = stats.pbo.expect("a non-degenerate matrix has a PBO");
    assert!((0.0..=1.0).contains(&pbo), "PBO is a probability: {pbo}");
    let eff = stats.effective_n.expect("effective N is finite for any non-empty set");
    assert!((1.0..=6.0).contains(&eff), "effective N is clamped to [1, N]: {eff}");
    let dsr = stats.deflated_sharpe.expect("DSR is a probability");
    assert!((0.0..=1.0).contains(&dsr), "deflated Sharpe is a probability: {dsr}");
    assert!(
        ["low", "medium", "high"].contains(&stats.verdict.as_str()),
        "the verdict is one of three words: {}",
        stats.verdict
    );
}

/// ⚠ **`None`, never a zeroed block.** A document carrying `pbo: 0.0` for a search that
/// measured nothing would read as "assessed, and clean" — the one wrong answer this whole
/// feature could produce. Each refusal below is a reachable state, not a hypothetical.
#[test]
fn nothing_measurable_is_absent_rather_than_zero() {
    let rows = vec![scored_row(1.0, 1.0)];
    assert!(
        overfit_stats(&rows, &OverfitCapturedReturns::default(), DEFAULT_CSCV_SPLITS).is_none(),
        "a search that never opted in"
    );
    let short = captured(DEFAULT_CSCV_SPLITS, &[(1.0, vec![0.01, -0.01, 0.02])]);
    assert!(
        overfit_stats(&rows, &short, DEFAULT_CSCV_SPLITS).is_none(),
        "T = 3 cannot be split into 16 CSCV blocks"
    );
    let unjoinable = captured(8, &[(99.0, column(32, 7))]);
    assert!(
        overfit_stats(&rows, &unjoinable, 8).is_none(),
        "a capture whose candidate matches no ranked row joins nothing"
    );
    assert!(
        overfit_stats(&rows, &captured(8, &[(1.0, column(32, 7))]), 1).is_none(),
        "a split count below 2 is refused here rather than panicking inside pbo_cscv"
    );
}

/// ONE trial is no selection, so its PBO is "not assessed" (`None`), never the 1.0 `pbo_cscv`
/// answers for a lone column ranked against itself — that would read as "certainly overfit" for a
/// search that chose nothing. The rest of the block is still measured.
#[test]
fn a_single_trial_has_no_pbo() {
    let t = DEFAULT_CSCV_SPLITS * 2;
    let stats = overfit_stats(
        &[scored_row(1.0, 1.0)],
        &captured(t, &[(1.0, column(t, 3))]),
        DEFAULT_CSCV_SPLITS,
    )
    .expect("one column of T = 32 is a matrix");
    assert_eq!(stats.trials, 1);
    assert_eq!(stats.pbo, None, "one trial: PBO is not assessed");
    assert!(stats.deflated_sharpe.is_some(), "the winner is still deflated");
}

/// A row with no retained vector is EXCLUDED and COUNTED — the visible difference between a
/// thin matrix and a small one. The reachable causes are a failed point, a resume's reused
/// trial, and a curve that touched zero.
#[test]
fn a_row_without_a_column_is_counted_rather_than_zero_filled() {
    let t = DEFAULT_CSCV_SPLITS * 2;
    let cols: Vec<(f64, Vec<f64>)> = (0..3).map(|i| (i as f64, column(t, i as u64 + 1))).collect();
    // Five ranked rows, three of which retained a column.
    let rows: Vec<ParamscanRow> = (0..5).map(|i| scored_row(i as f64, 5.0 - i as f64)).collect();

    let stats = overfit_stats(&rows, &captured(t, &cols), DEFAULT_CSCV_SPLITS).unwrap();
    assert_eq!(stats.trials, 3, "only the rows that retained a column are columns");
    assert_eq!(stats.excluded, 2, "…and the other two are REPORTED, not padded with zeros");
}

/// ⚠ **The column order is the RANKED row order, and `observed_sharpe` is row 0's.**
/// `pbo_cscv`'s first-argmax tie-break reads the column INDEX, so a statistic assembled in
/// HashMap-iteration order would depend on rayon scheduling. Two orderings of the same three
/// trials are compared: the numbers must move, which is the falsifiable form of "order is
/// load-bearing".
#[test]
fn the_winner_is_row_zero_and_the_order_is_the_ranking() {
    let t = DEFAULT_CSCV_SPLITS * 2;
    // A deliberately strong column and a deliberately weak one.
    let strong: Vec<f64> = (0..t).map(|i| 0.01 + (i % 2) as f64 * 0.001).collect();
    let weak: Vec<f64> = (0..t).map(|i| -0.01 + (i % 3) as f64 * 0.002).collect();
    let cap = captured(t, &[(1.0, strong), (2.0, weak)]);

    let strong_first =
        overfit_stats(&[scored_row(1.0, 2.0), scored_row(2.0, 1.0)], &cap, DEFAULT_CSCV_SPLITS)
            .unwrap();
    let weak_first =
        overfit_stats(&[scored_row(2.0, 2.0), scored_row(1.0, 1.0)], &cap, DEFAULT_CSCV_SPLITS)
            .unwrap();

    assert!(
        strong_first.observed_sharpe.unwrap() > weak_first.observed_sharpe.unwrap(),
        "observed_sharpe follows ROW 0, not the map: {:?} vs {:?}",
        strong_first.observed_sharpe,
        weak_first.observed_sharpe
    );
}

/// The pseudo-curve is the bridge to `overfit::sharpe_moments`, which is the ONE home for the
/// per-period/non-excess conventions. Its ratios must BE the returns it was built from, or
/// every moment above is computed over a different series than the one retained.
#[test]
fn the_pseudo_curve_round_trips_its_returns() {
    let rets = vec![0.01, -0.02, 0.03, 0.0, -0.005];
    let curve = curve_from_returns(&rets);
    assert_eq!(curve.len(), rets.len() + 1, "one more point than returns");
    assert_eq!(curve[0], 1.0, "a growth index, so no cash scale is invented");
    let back = vike_analytics::metrics::returns(&curve);
    assert_eq!(back.len(), rets.len());
    for (a, b) in back.iter().zip(&rets) {
        assert!((a - b).abs() < 1e-12, "{a} vs {b}");
    }
}

/// The split count is EVEN, and that is load-bearing rather than cosmetic: `pbo_cscv` panics
/// on an odd one.
#[test]
fn the_default_split_count_is_even_and_at_least_four() {
    assert!(DEFAULT_CSCV_SPLITS.is_multiple_of(2), "pbo_cscv asserts an even split count");
    // `const` block rather than a runtime `assert!`: the operand is a constant, so this is a
    // COMPILE-TIME check now — it fails the build rather than a test run, and clippy's
    // `assertions_on_constants` refuses the runtime spelling under `-D warnings`.
    const { assert!(DEFAULT_CSCV_SPLITS >= 4) };
    assert!(
        OverfitReturnBuckets::DEFAULT_BUCKETS.is_multiple_of(DEFAULT_CSCV_SPLITS),
        "the bucket count divides by the split count, so no CSCV group is short — \
             OverfitReturnBuckets' own doc argues why that matters"
    );
}

// ── The stop flag ──────────────────────────────────────────────────────────────────────────

/// `SearchEvaluatorBars::with_cancel`, the twin of the flag the backtesthub arms on
/// `SearchEvaluatorStore`: a CLEAR flag changes nothing (every point runs and carries its report), a SET
/// flag answers every point with the cancelled row — no report, `NaN` steering score, the
/// candidate's own overrides kept — without running a backtest. Neutering the check in
/// `evaluate_one` turns the second half red.
#[test]
fn a_set_cancel_flag_turns_every_bars_point_into_a_cancelled_row() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let base = profile("[strategy.params]\nsize = 1.0\nsymbol = \"BTCUSDT\"");
    let store: Arc<dyn vike_data::HistoryStore + Send + Sync> =
        Arc::new(vike_data::HistoryStoreMemory::new());
    let bars: Vec<vike_marketdata::Bar> = (0..5)
        .map(|i| vike_marketdata::test_support::flat_bar_zero_volume(i * 1000, 100.0))
        .collect();
    let objective = paramscan::RankMetric::Sharpe.objective();
    let batch = || -> Vec<SearchCandidate> {
        [1.0, 2.0].iter().map(|&s| vec![("size".to_string(), ParamValue::Float(s))]).collect()
    };
    let flag = AtomicBool::new(false);
    let evaluation = SearchEvaluatorBars::new(
        &base,
        store,
        vec![("BTCUSDT".to_string(), bars)],
        &objective,
        "sharpe",
        paramscan::ParamscanExecution::Sequential,
    )
    .with_cancel(&flag);

    let live = evaluation.evaluate(batch());
    assert_eq!(live.len(), 2);
    for e in &live {
        assert!(e.row.error.is_none(), "a clear flag runs the point: {:?}", e.row.error);
        assert!(e.row.report.is_some(), "a clear flag keeps the report");
    }

    flag.store(true, Ordering::Relaxed);
    let stopped = evaluation.evaluate(batch());
    assert_eq!(stopped.len(), 2, "one row per candidate, cancelled or not");
    for (e, size) in stopped.iter().zip([1.0, 2.0]) {
        assert!(e.row.report.is_none(), "a cancelled point ran no backtest");
        assert!(
            e.row.error.as_deref().is_some_and(|m| m.starts_with("cancelled:")),
            "the row names the cancellation: {:?}",
            e.row.error
        );
        assert!(e.score.is_nan() && e.row.score.is_none(), "a cancelled point is unrankable");
        assert_eq!(e.row.overrides[0].1.as_float(), Some(size), "the candidate is kept");
    }
}

/// `evaluator::evaluate_batch`, both evaluators' one `evaluate`: the rows come back in the batch's
/// order on either exec, a one-point batch stays on the calling thread even under `Parallel` (the
/// pool rule), and only a `Parallel` neighbourhood leaves it. A mutant that drops the width rule or
/// swaps the two arms goes red.
#[test]
fn a_batch_keeps_its_order_and_only_a_parallel_neighbourhood_leaves_the_calling_thread() {
    use crate::search::paramscan::ParamscanExecution;
    let caller = std::thread::current().id();
    let tag = |c: SearchCandidate| SearchEvaluation {
        row: ParamscanRow {
            overrides: c,
            report: None,
            error: (std::thread::current().id() != caller).then(|| "pooled".to_string()),
            score: None,
        },
        score: 0.0,
    };
    let batch = |n: i64| -> Vec<SearchCandidate> {
        (0..n).map(|i| vec![("i".to_string(), ParamValue::Int(i))]).collect()
    };
    for (exec, n, pooled) in [
        (ParamscanExecution::Sequential, 4, false),
        (ParamscanExecution::Parallel, 1, false),
        (ParamscanExecution::Parallel, 4, true),
    ] {
        let rows = evaluator::evaluate_batch(exec, batch(n), tag);
        let order: Vec<SearchCandidate> = rows.iter().map(|e| e.row.overrides.clone()).collect();
        assert_eq!(order, batch(n), "{exec:?} x{n}: the batch's order");
        assert!(rows.iter().all(|e| e.row.error.is_some() == pooled), "{exec:?} x{n}: the thread");
    }
}
