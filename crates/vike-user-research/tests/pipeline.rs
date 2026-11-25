//! The fixture studies (`tests/fixture_user_data/`, committed): compiled into THIS test binary as
//! modules (never into a shipped one), and run two ways that must agree bit for bit:
//!
//! - **directly**: the study's own `run`, handed a context built here;
//! - **across the study file's boundary**: `vike_user_research::file`'s two halves wired together in
//!   this process — the study's side ([`guest::run`], what the study file template calls) and the
//!   host's ([`host::start`], [`host::Parent::answer`], [`host::finished`]), every request and answer
//!   crossing as the bytes the sandboxed process relays, and the store a study hands its backtest
//!   read back through [`guest::serve`].
//!
//! What only a lane can show — the builder building these folders, the loader loading them, the jailed
//! process running them — is `crates/vike-strategy-sandbox/tests/study_child.rs`.
//!
//! ⚠ Both doubles are the workspace's OWN shared ones — `vike_data::HistoryStoreMemory` and
//! `vike_ml_training::seam::test_support::ScriptedLearner` — so the contract is proven against the same objects
//! a study meets in a real run, on a box with no LightGBM binary and no DataFusion. `poly_mm`, a REAL
//! study, is compiled here too: its own `#[cfg(test)]` module runs in this binary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_data::{ExecFillRow, HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_ml_prediction::FitImportance;
use vike_ml_training::seam::test_support::ScriptedLearner;
use vike_user_research::file::{guest, host};
use vike_user_research::{SimOutcome, StudyContext, StudyError, StudyFn, StudyOutcome, StudySim};

#[path = "fixture_user_data/research/studies/rust/alpha_pool/alpha_pool.rs"]
pub mod user_alpha_pool;
#[path = "fixture_user_data/research/studies/rust/book_export/book_export.rs"]
pub mod user_book_export;
#[path = "fixture_user_data/research/studies/rust/covered_call/covered_call.rs"]
pub mod user_covered_call;
#[path = "fixture_user_data/research/studies/rust/deep_hedging/deep_hedging.rs"]
pub mod user_deep_hedging;
#[path = "fixture_user_data/research/studies/rust/event_calibration/event_calibration.rs"]
pub mod user_event_calibration;
#[path = "fixture_user_data/research/studies/rust/fixture_bars/fixture_bars.rs"]
pub mod user_fixture_bars;
#[path = "fixture_user_data/research/studies/rust/fixture_fit/fixture_fit.rs"]
pub mod user_fixture_fit;
#[path = "fixture_user_data/research/studies/rust/forward_report/forward_report.rs"]
pub mod user_forward_report;
#[path = "fixture_user_data/research/studies/rust/funding_carry/funding_carry.rs"]
pub mod user_funding_carry;
#[path = "fixture_user_data/research/studies/rust/funding_spike_effect/funding_spike_effect.rs"]
pub mod user_funding_spike_effect;
#[path = "fixture_user_data/research/studies/rust/impact_fit/impact_fit.rs"]
pub mod user_impact_fit;
#[path = "fixture_user_data/research/studies/rust/instrument_graph/instrument_graph.rs"]
pub mod user_instrument_graph;
#[path = "fixture_user_data/research/studies/rust/microstructure/microstructure.rs"]
pub mod user_microstructure;
#[path = "fixture_user_data/research/studies/rust/option_pricing_network/option_pricing_network.rs"]
pub mod user_option_pricing_network;
#[path = "fixture_user_data/research/studies/rust/pairs_reversion/pairs_reversion.rs"]
pub mod user_pairs_reversion;
#[path = "fixture_user_data/research/studies/rust/poly_mm/poly_mm.rs"]
pub mod user_poly_mm;
#[path = "fixture_user_data/research/studies/rust/rl_episode/rl_episode.rs"]
pub mod user_rl_episode;
#[path = "fixture_user_data/research/studies/rust/sharpe_lstm/sharpe_lstm.rs"]
pub mod user_sharpe_lstm;
#[path = "fixture_user_data/research/studies/rust/text_base_rate/text_base_rate.rs"]
pub mod user_text_base_rate;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// A seeded store plus the window that covers it.
fn seeded_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    seeded_store_of(5)
}

/// [`seeded_store`] with `n` one-minute flat bars, at 100, 101 and so on.
fn seeded_store_of(n: i64) -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let store = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            vike_marketdata::test_support::flat_bar_unit_volume_on(
                SYMBOL,
                60_000 * i,
                100.0 + i as f64,
            )
        })
        .collect();
    store
        .append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("fixture"))
        .expect("HistoryStoreMemory accepts the fixture bars");
    (Arc::new(store), TimestampRange::of(0, 60_000 * (n - 1)))
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("vike-user-research-pipeline-{}", std::process::id()))
}

fn params(text: &str) -> toml::Value {
    toml::from_str(text).unwrap()
}

/// Run `entry` across the study file's boundary, both halves in this process: the start message,
/// every request the study makes answered by the parent (its one backtest reading the store it handed
/// over back through the study's side), and the finished answer read back. `ctx` is leaked: the
/// study's side holds its host for the run, as the study file holds its host link.
fn across_the_boundary(
    entry: StudyFn,
    ctx: StudyContext,
    params: &toml::Value,
) -> Result<StudyOutcome, StudyError> {
    let ctx: &'static StudyContext = Box::leak(Box::new(ctx));
    let start = host::start(ctx, params)?;
    let parent = host::Parent::new(ctx, Arc::new(guest::serve));
    let host: guest::Host = Arc::new(move |request: &[u8]| parent.answer(request));
    host::finished(&guest::run(entry, &start, host))
}

/// Both ways, compared: the same outcome (every metric's BITS, so a `NaN` is compared too, and every
/// artifact), or the same error (variant and words).
fn both_ways(entry: StudyFn, ctx: impl Fn() -> StudyContext, params: &toml::Value) -> String {
    let direct = entry(&ctx(), params);
    let across = across_the_boundary(entry, ctx(), params);
    match (&direct, &across) {
        (Ok(a), Ok(b)) => {
            let bits = |o: &StudyOutcome| -> Vec<(String, u64)> {
                o.metrics().iter().map(|(n, v)| (n.clone(), v.to_bits())).collect()
            };
            assert_eq!(bits(a), bits(b), "the metrics, bit for bit");
            assert_eq!(a.artifacts(), b.artifacts(), "the artifacts, byte for byte");
            format!("ok: {} metric(s), {} artifact(s)", a.metrics().len(), a.artifacts().len())
        }
        (Err(a), Err(b)) => {
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "the same refusal, variant and words");
            format!("refused: {a}")
        }
        _ => panic!("the two ways disagree: directly {direct:?}, across {across:?}"),
    }
}

/// Every fixture folder with an entry file is compiled by this binary (a new one joins the
/// `#[path]` modules above); the recipes-only folder holds none.
#[test]
fn every_fixture_study_folder_is_compiled_here() {
    let tier =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture_user_data/research/studies/rust");
    let mut with_entry: Vec<String> = std::fs::read_dir(&tier)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            e.path().join(format!("{name}.rs")).is_file().then_some(name)
        })
        .collect();
    with_entry.sort();
    assert_eq!(
        with_entry,
        [
            "alpha_pool",
            "book_export",
            "covered_call",
            "deep_hedging",
            "event_calibration",
            "fixture_bars",
            "fixture_fit",
            "forward_report",
            "funding_carry",
            "funding_spike_effect",
            "impact_fit",
            "instrument_graph",
            "microstructure",
            "option_pricing_network",
            "pairs_reversion",
            "poly_mm",
            "rl_episode",
            "sharpe_lstm",
            "text_base_rate"
        ]
    );
}

/// A recorded book chain and nothing else: the shared `HistoryStoreMemory` holds no book (its
/// "still-inert remainder"), so `book_export` reads this one.
struct BookStore(Vec<L2BookUpdate>);

impl HistoryStore for BookStore {
    fn scan_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let inside =
            |t: i64| range.start.is_none_or(|s| t >= s) && range.end.is_none_or(|e| t <= e);
        Ok(self.0.iter().filter(|u| inside(u.timestamp)).cloned().collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_trades,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Mids 100 -> 100.5 -> 101 -> 100.5 on a whole-number grid, one millisecond apart.
fn book_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let event = |timestamp, sequence, kind, bids: &[(f64, f64)], asks: &[(f64, f64)]| {
        let levels =
            |side: &[(f64, f64)]| side.iter().map(|&(p, q)| L2BookLevel::new(p, q)).collect();
        L2BookUpdate {
            timestamp,
            local_timestamp: 0,
            sequence,
            kind,
            tick_size: 1.0,
            bids: levels(bids),
            asks: levels(asks),
            symbol: String::new(),
        }
    };
    use L2BookUpdateKind::{Delta, Snapshot};
    Arc::new(BookStore(vec![
        event(1000, 1, Snapshot, &[(99.0, 1.0), (98.0, 2.0)], &[(101.0, 3.0), (102.0, 4.0)]),
        event(1001, 2, Delta, &[], &[(101.0, 0.0)]),
        event(1002, 3, Delta, &[(100.0, 5.0)], &[]),
        event(1003, 4, Delta, &[], &[(101.0, 1.0)]),
    ]))
}

const BOOK_RECIPE: &str = "venue = \"binance\"\nsymbol = \"BTCUSDT\"\nlevels = 2\n\
                           horizon_events = 1\nup_threshold = 0.001\ndown_threshold = 0.001\n";

/// `book_export` reads the recorded book through the study API and writes one TSV row per
/// labelled sample, the class balance as metrics.
#[test]
fn book_export_writes_the_labelled_rows_of_the_recorded_book() {
    let ctx = StudyContext::new(book_store(), TimestampRange::all(), scratch());
    let out = user_book_export::run(&ctx, &params(BOOK_RECIPE)).expect("it exports the book");
    let metric = |name| out.metric_value(name).unwrap();
    assert_eq!([metric("rows"), metric("columns")], [3.0, 8.0]);
    assert_eq!([metric("up"), metric("flat"), metric("down")], [2.0, 0.0, 1.0]);
    assert_eq!([metric("skipped_without_book"), metric("skipped_without_label")], [0.0, 1.0]);
    let (name, body) = &out.artifacts()[0];
    assert_eq!(name, "book_samples.tsv");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines[0],
        "timestamp\tlabel\thorizon_return\tmid\task_price_1\task_quantity_1\tbid_price_1\t\
         bid_quantity_1\task_price_2\task_quantity_2\tbid_price_2\tbid_quantity_2"
    );
    assert_eq!(lines[1], "1000\t1\t0.005\t100\t101\t3\t99\t1\t102\t4\t98\t2");
    // Level 2's ask is gone after the second event: a NaN price and a zero quantity.
    assert_eq!(lines[2], format!("1001\t1\t{}\t100.5\t102\t4\t99\t1\tNaN\t0\t98\t2", 0.5 / 100.5));
    assert_eq!(lines.len(), 4, "header + three rows: {body}");
}

/// `book_export` across the boundary: the export, its interval sampling, the store's refusal of the
/// depth lane, and the study's own refusals.
#[test]
fn book_export_answers_the_same_across_the_study_file_boundary() {
    let ctx = || StudyContext::new(book_store(), TimestampRange::all(), scratch());
    let recipes = [
        BOOK_RECIPE.to_string(),
        format!("{BOOK_RECIPE}interval_ms = 1\n"),
        format!("{BOOK_RECIPE}label = \"mean_of_next\"\n"),
        format!("{BOOK_RECIPE}lane = \"depth\"\n"),
        format!("{BOOK_RECIPE}label = \"smoothed\"\n"),
        "venue = \"binance\"\n".to_string(),
        "venue = \"binance\"\nsymbol = \"BTCUSDT\"\nlevels = 0\n".to_string(),
    ];
    let said: Vec<String> =
        recipes.iter().map(|r| both_ways(user_book_export::run, ctx, &params(r))).collect();
    assert!(said[..3].iter().all(|s| s.starts_with("ok")), "{said:?}");
    assert!(said[3..].iter().all(|s| s.starts_with("refused")), "{said:?}");
}

/// The smoothed label is OPT-IN: naming the default changes nothing, and the mean of the next two
/// mids (100.75 from 100, then from 100.5) labels differently from the plain mid two events later.
#[test]
fn book_export_labels_by_the_mean_of_the_next_mids_only_when_asked() {
    let ctx = StudyContext::new(book_store(), TimestampRange::all(), scratch());
    let run = |extra: &str| {
        let recipe =
            format!("{BOOK_RECIPE}{extra}").replace("horizon_events = 1", "horizon_events = 2");
        user_book_export::run(&ctx, &params(&recipe)).expect("it exports the book")
    };
    assert_eq!(run(""), run("label = \"mid_after\"\n"), "the default is the plain label");
    let plain = run("");
    let smoothed = run("label = \"mean_of_next\"\n");
    // Plain: 100 -> 101 is up, 100.5 -> 100.5 is flat. Smoothed: both are up.
    assert_eq!([plain.metric_value("up"), plain.metric_value("flat")], [Some(1.0), Some(1.0)]);
    assert_eq!(
        [smoothed.metric_value("up"), smoothed.metric_value("flat")],
        [Some(2.0), Some(0.0)]
    );
}

/// A study FILE plays RL episodes over the store's bars (decision 0291), split by time: nine bars make
/// a window of three, so training draws from bars 0 to 2, bars 3 to 5 are the embargo, and the score is
/// taken on bars 6 to 8 alone. One unit held from the first decision fills at the next bar's open and is
/// marked at the last close: 101 to 102 in training, 107 to 108 in evaluation. The study answers the
/// same both ways.
#[test]
fn a_study_plays_an_rl_episode_over_the_stores_bars() {
    let context = || {
        let (store, window) = seeded_store_of(9);
        StudyContext::new(store, window, scratch())
    };
    let out = user_rl_episode::run(&context(), &params("target = 1")).expect("two episodes");
    let metric = |name: &str| out.metric_value(name);
    assert_eq!(
        [metric("training_bars"), metric("embargo_bars"), metric("evaluation_start")],
        [Some(3.0), Some(3.0), Some(6.0)],
        "the split is reported"
    );
    assert_eq!(metric("training_reward"), Some(1.0));
    assert_eq!(metric("steps"), Some(3.0), "one step per bar of the evaluation window");
    assert_eq!(metric("total_reward"), Some(1.0));
    assert_eq!(metric("final_equity"), Some(10_001.0));
    let tsv = &out.artifacts()[0].1;
    assert_eq!(
        tsv.lines().count(),
        9,
        "header, then each episode's first observation and steps: {tsv}"
    );
    both_ways(user_rl_episode::run, context, &params("target = 1"));
}

/// A study that reports the kept instruments it was handed: their count and the time they were read.
fn kept_instruments_entry(ctx: &StudyContext, _: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let kept = ctx.kept_instruments().map_err(StudyError::Data)?;
    let mut out = StudyOutcome::new();
    out.metric("rows", kept.rows.len() as f64)?;
    out.metric("read_ms", kept.read_ms as f64)?;
    Ok(out)
}

/// The kept-instruments verb: the parent reads the store's kept rows and hands them over with the
/// time IT read them, so the study file's answer is the parent's read; a store that serves no kept
/// list is the same refusal both ways.
#[test]
fn the_kept_instruments_cross_with_the_time_the_parent_read_them() {
    use user_instrument_graph::fixture_store;
    let ctx = || StudyContext::new(fixture_store::store(), fixture_store::window(), scratch());
    let before = vike_model::time::clock::now_ms();
    let out = across_the_boundary(kept_instruments_entry, ctx(), &params("")).unwrap();
    let after = vike_model::time::clock::now_ms();
    assert_eq!(out.metric_value("rows"), Some(fixture_store::rows().len() as f64));
    let read_ms = out.metric_value("read_ms").unwrap() as i64;
    assert!((before..=after).contains(&read_ms), "{before} <= {read_ms} <= {after}");
    let direct = ctx().kept_instruments().unwrap();
    assert_eq!(direct.rows, fixture_store::rows(), "the rows as the store serves them");

    let plain = || {
        let (store, window) = seeded_store();
        StudyContext::new(store, window, scratch())
    };
    let said = both_ways(kept_instruments_entry, plain, &params(""));
    assert!(said.contains("serves no kept instrument list"), "{said}");
}

/// A study that reads a user's `csv-bars` lane through `bars_in_lane`, and what `bars` finds at the same
/// address: the sourceless series, which a lane never joins.
fn lane_entry(ctx: &StudyContext, _: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let lane = ctx
        .bars_in_lane("user-bitfinex", "user", "BTCUSDLONGS", "1h", ctx.window())
        .map_err(StudyError::Data)?;
    let plain = ctx.bars("user", "BTCUSDLONGS", "1h", ctx.window()).map_err(StudyError::Data)?;
    let mut out = StudyOutcome::new();
    out.metric("lane_bars", lane.len() as f64)?;
    out.metric("lane_last_close", lane.last().map_or(f64::NAN, |b| b.close))?;
    out.metric("plain_bars", plain.len() as f64)?;
    Ok(out)
}

/// A user's import lives in a `source=` lane that `bars` never meets: `bars_in_lane` reads it, the same
/// bars directly and across the study file's boundary.
#[test]
fn a_study_reads_a_user_lane_through_bars_in_lane() {
    let ctx = || {
        let store = HistoryStoreMemory::new();
        let bars: Vec<Bar> = (0..3)
            .map(|i| {
                vike_marketdata::test_support::flat_bar_unit_volume_on(
                    "BTCUSDLONGS",
                    3_600_000 * i,
                    90_000.0 + i as f64,
                )
            })
            .collect();
        store
            .append_bars_sourced("user", "user-bitfinex", "BTCUSDLONGS", "1h", &bars, Some("lane"))
            .expect("the double takes a user lane");
        StudyContext::new(Arc::new(store), TimestampRange::of(0, 2 * 3_600_000), scratch())
    };
    let direct = lane_entry(&ctx(), &params("")).unwrap();
    assert_eq!(direct.metric_value("lane_bars"), Some(3.0));
    assert_eq!(direct.metric_value("lane_last_close"), Some(90_002.0));
    assert_eq!(
        direct.metric_value("plain_bars"),
        Some(0.0),
        "a lane never joins the sourceless series"
    );
    both_ways(lane_entry, ctx, &params(""));
}

/// `instrument_graph`, the chapter-49 study, by hand: binance's two books (spot, perpetual) lead
/// bybit's and okx's perpetuals, bybit leads okx, and neither binance book leads the other. The
/// lead-lag network is the four nodes less the binance pair, every edge of weight one, so the
/// centrality is `a` on binance's books and `b` on the others with `λ a = 2 b`, `λ b = 2 a + b`
/// (`λ = (1 + √17) / 2`) and `2a² + 2b² = 1`: `a = 0.435162`, `b = 0.557345`. The same both ways.
#[test]
fn the_instrument_graph_study_by_hand() {
    use user_instrument_graph::fixture_store;
    let ctx = || StudyContext::new(fixture_store::store(), fixture_store::window(), scratch());
    let recipe = params(&fixture_store::recipe());
    let out = user_instrument_graph::run(&ctx(), &recipe).expect("the study runs on its fixture");
    for (metric, want) in [
        ("kept_rows", 10.0),
        ("assets", 2.0),
        ("assets_on_two_or_more_venues", 1.0),
        ("neg_risk_sets", 1.0),
        ("underlying_links", 3.0),
        ("asset_members", 7.0),
        ("series_read", 4.0),
        ("series_without_bars", 1.0),
        ("series_refused", 0.0),
        ("observations", 31.0),
        ("edges", 5.0),
        ("centrality_defined", 1.0),
    ] {
        assert_eq!(out.metric_value(metric), Some(want), "{metric}");
    }
    let table = &out.artifacts()[0];
    assert_eq!(table.0, "instruments.tsv");
    assert_eq!(
        table.1,
        "instrument\tleadership\tcentrality\n\
         binance:BTCUSDT\t0.666667\t0.435162\n\
         binance:BTCUSDT.P\t0.666667\t0.435162\n\
         bybit:BTCUSDT.P\t-0.333333\t0.557345\n\
         okx:BTCUSDT.P\t-1.000000\t0.557345\n"
    );
    let edges: Vec<String> = out.artifacts()[1]
        .1
        .lines()
        .skip(1)
        .map(|line| line.split('\t').take(4).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        edges,
        [
            "binance:BTCUSDT bybit:BTCUSDT.P 1 1.000000",
            "binance:BTCUSDT okx:BTCUSDT.P 2 1.000000",
            "binance:BTCUSDT.P bybit:BTCUSDT.P 1 1.000000",
            "binance:BTCUSDT.P okx:BTCUSDT.P 2 1.000000",
            "bybit:BTCUSDT.P okx:BTCUSDT.P 1 1.000000",
        ]
    );
    assert!(both_ways(user_instrument_graph::run, ctx, &recipe).starts_with("ok"));
    // An asset no kept row states: the study's refusal, the same both ways.
    assert!(
        both_ways(user_instrument_graph::run, ctx, &params("asset = \"DOGE\"\ninterval = \"1m\""))
            .contains("DOGE")
    );
}

/// The study reads the store through its context, the params reach its lenient reader, and metrics
/// and artifacts come back.
#[test]
fn a_fixture_study_reads_the_store_and_returns_a_result() {
    let (store, window) = seeded_store();
    let ctx = StudyContext::new(store, window, scratch());
    let out = user_fixture_bars::run(&ctx, &params("period = 2"))
        .expect("it runs against the seeded store");

    assert_eq!(out.metric_value("bars"), Some(5.0));
    assert_eq!(out.metric_value("mean_close"), Some(102.0));
    assert!(out.metric_value("sharpe").is_some(), "the analytics call produced a number");
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["closes.tsv"]);
    let body = &out.artifacts()[0].1;
    assert!(body.starts_with("ts\tclose\tmean_gap\n"), "{body}");
    assert_eq!(body.lines().count(), 6, "header + one line per bar: {body}");
}

/// The STUDY↔STRATEGY seam: a feature a USER wrote, pushed through the platform's own parity
/// harness (`crates/vike-indicators/src/test_support.rs`'s `assert_feature_parity`, a library module
/// so a study's author can reach it).
#[test]
fn a_user_authored_feature_passes_the_platform_parity_harness() {
    vike_indicators::test_support::assert_feature_parity(
        "fixture_bars::MeanGap",
        &user_fixture_bars::MeanGap::new(4),
    );
}

/// The params are the RECIPE: a different table steers the same study, and an unknown series is a
/// study-level refusal rather than a silent empty result.
#[test]
fn params_steer_the_study_and_an_uncovered_series_refuses() {
    let (store, window) = seeded_store();
    let ctx = StudyContext::new(store, window, scratch());
    let err = user_fixture_bars::run(&ctx, &params(r#"symbol = "ETHUSDT""#))
        .expect_err("the store holds no ETHUSDT bars");
    match err {
        StudyError::Study(m) => assert!(m.contains("ETHUSDT"), "{m}"),
        other => panic!("expected a study-level refusal, got {other:?}"),
    }
}

fn importance_learner() -> ScriptedLearner {
    ScriptedLearner::constant(0.75).with_importance(FitImportance {
        n_features: 2,
        gain: vec![1.5, 0.5],
        splits: vec![3, 1],
    })
}

/// The LEARNER seam, present: a real `vike_ml_training::Learner` reaches the study through the erased
/// `StudyLearner`, the fit happens, and the importance the double reports comes back as an artifact.
#[test]
fn a_fitting_study_reaches_a_real_learner_through_the_erasure() {
    let (store, window) = seeded_store();
    let ctx =
        StudyContext::new(store, window, scratch()).with_learner(Arc::new(importance_learner()));
    let out = user_fixture_fit::run(&ctx, &params("seed = 7"))
        .expect("it fits against the scripted learner");

    assert_eq!(out.metric_value("rows"), Some(3.0));
    assert_eq!(out.metric_value("p_first_row"), Some(0.75));
    assert_eq!(out.metric_value("importance_features"), Some(2.0));
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    // No `model.txt`: `ScriptedLearner` reports no model TEXT (the trait's documented default),
    // and the study records that absence rather than writing an empty file.
    assert_eq!(names, ["importance.tsv"]);
    assert!(out.artifacts()[0].1.contains("f0\t1.5\t3"), "{}", out.artifacts()[0].1);
}

/// The LEARNER seam, absent: the documented ceiling of a host with no LightGBM binary is a TYPED
/// refusal a caller can act on, not a number computed without a model.
#[test]
fn a_fitting_study_refuses_when_the_host_supplied_no_learner() {
    let (store, window) = seeded_store();
    let ctx = StudyContext::new(store, window, scratch());
    assert!(ctx.learner().is_none());
    match user_fixture_fit::run(&ctx, &params("")).expect_err("no learner was supplied") {
        StudyError::NoLearner(what) => {
            assert!(!what.is_empty(), "the refusal names what it wanted to fit");
            assert!(StudyError::NoLearner(what).to_string().contains("LightGBM"));
        }
        other => panic!("expected NoLearner, got {other:?}"),
    }
}

/// `fixture_bars` across the boundary: the store's reads, the analytics, the artifact, and the
/// store's own refusal shape, the same both ways.
#[test]
fn fixture_bars_answers_the_same_across_the_study_file_boundary() {
    let ctx = || {
        let (store, window) = seeded_store();
        StudyContext::new(store, window, scratch())
    };
    for recipe in ["period = 2", "period = 4", r#"symbol = "ETHUSDT""#, ""] {
        let said = both_ways(user_fixture_bars::run, ctx, &params(recipe));
        eprintln!("fixture_bars [{recipe}]: {said}");
    }
}

/// A study of the option-chain verbs: the range scan's rows, then the surface as of each snapshot
/// hour, each hour's mean mark a metric and the surface an artifact.
fn chain_study(ctx: &StudyContext, _: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let rows = ctx.chain("binance", "BTCUSDT", ctx.window())?;
    let mut out = StudyOutcome::new();
    out.metric("rows", rows.len() as f64)?;
    let mut surface = String::from("ts\tinstrument\tmark\n");
    for hour in [3_599, 7_199] {
        let at = ctx.chain_as_of("binance", "BTCUSDT", hour, 3_600_000)?;
        let marks: Vec<f64> = at.iter().filter_map(|r| r.mark).collect();
        out.metric(&format!("mean_mark_{hour}"), marks.iter().sum::<f64>() / marks.len() as f64)?;
        for r in &at {
            surface.push_str(&format!("{hour}\t{}\t{:?}\n", r.instrument, r.mark));
        }
    }
    out.artifact("surface.tsv", surface)?;
    Ok(out)
}

/// The option-chain verbs across the boundary: the scan crosses as the parent's rows, the
/// point-in-time fold runs beside the study, and both ways agree bit for bit.
#[test]
fn the_chain_verbs_answer_the_same_across_the_study_file_boundary() {
    let ctx = || {
        let store = HistoryStoreMemory::new();
        let row = |ts: i64, strike: f64, mark: f64| vike_data::ChainRow {
            ts,
            underlying: "BTCUSDT".into(),
            instrument: format!("BTC-231027-{strike}-P"),
            expiry_ms: 1_698_393_600_000,
            strike,
            kind: vike_marketdata::OptionKind::Put,
            bid: None,
            ask: Some(mark + 0.5),
            mark: Some(mark),
            iv: Some(0.55),
            open_interest: Some(2.0),
            volume: Some(0.0),
            delta: Some(-0.3),
            gamma: Some(0.0001),
            theta: Some(-12.0),
            vega: Some(4.0),
        };
        let first = [row(3_599, 30_000.0, 0.1), row(3_599, 31_000.0, 0.7)];
        store.append_chain_snapshot("binance", "BTCUSDT", &first, Some("c1")).unwrap();
        store
            .append_chain_snapshot("binance", "BTCUSDT", &[row(7_199, 30_000.0, 0.2)], Some("c2"))
            .unwrap();
        StudyContext::new(Arc::new(store), TimestampRange::all(), scratch())
    };
    let direct = chain_study(&ctx(), &params("")).expect("the chain study runs");
    assert_eq!(direct.metric_value("rows"), Some(3.0));
    assert_eq!(direct.metric_value("mean_mark_3599"), Some((0.1 + 0.7) / 2.0));
    assert_eq!(direct.metric_value("mean_mark_7199"), Some((0.2 + 0.7) / 2.0));
    assert!(both_ways(chain_study, ctx, &params("")).starts_with("ok"));
}

/// `fixture_fit` across the boundary, with the learner (its model HELD by the parent and asked per
/// row, since the double keeps no text) and without it (the typed refusal).
#[test]
fn fixture_fit_answers_the_same_across_the_study_file_boundary() {
    let with = || {
        let (store, window) = seeded_store();
        StudyContext::new(store, window, scratch()).with_learner(Arc::new(importance_learner()))
    };
    let without = || {
        let (store, window) = seeded_store();
        StudyContext::new(store, window, scratch())
    };
    assert!(both_ways(user_fixture_fit::run, with, &params("seed = 7")).starts_with("ok"));
    assert!(both_ways(user_fixture_fit::run, without, &params("")).starts_with("refused"));
}

/// A store holding one trade tape for [`VENUE`]/[`SYMBOL`] (the shared `HistoryStoreMemory` keeps no
/// ticks), and the kept instrument rows it serves, if any. Every other read is empty.
struct TapeStore(Vec<TickTrade>, Option<Vec<vike_model::Instrument>>);

impl HistoryStore for TapeStore {
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        Ok(self.1.clone())
    }
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let inside =
            |ts: i64| range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e);
        let ours = venue == VENUE && symbol == SYMBOL;
        Ok(self.0.iter().filter(|t| ours && inside(t.timestamp)).cloned().collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Five trades worked by hand in [`microstructure_measures_the_tape_by_hand`].
fn tape_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    use vike_marketdata::test_support::trade;
    let trades = vec![
        trade(1_000, 100.0, 2.0, false), // buy 2
        trade(2_000, 101.0, 1.0, true),  // sell 1
        trade(3_000, 102.0, 1.0, false), // buy 1
        trade(4_000, 101.0, 2.0, true),  // sell 2
        trade(5_000, 103.0, 2.0, false), // buy 2
    ];
    (Arc::new(TapeStore(trades, None)), TimestampRange::of(0, 10_000))
}

/// The tape above, `bucket_volume = 2`, `window = 2`, worked by hand:
/// - volume buckets: {buy 2}, {sell 1, buy 1}, {sell 2}, {buy 2}; imbalances 1, 0, 1, 1; VPIN (mean
///   of the last two) 0.5, 0.5, 1;
/// - the tick rule (start: buy) says buy, buy (101 > 100), buy, sell, buy: it disagrees with the
///   flag on the second trade only, 4 of 5; re-stamped, every bucket is one-sided, VPIN 1;
/// - imbalance bars of 2 close at trades 1, 4 and 5 (theta +2, -2, +2); runs bars of 2 at trades 1,
///   4 (sell run 3) and 5;
/// - dollar bars of 200 (2 at the first price): 811 traded in all, so 4;
/// - BVC-VPIN: closes 100, 102, 101, 103, changes 2, -1, 2, deviations over two 1.5 and 1.5, so
///   `|2 Φ(-1/1.5) - 1|` and `|2 Φ(2/1.5) - 1|` averaged; finite.
#[test]
fn microstructure_measures_the_tape_by_hand() {
    let (store, window) = tape_store();
    let ctx = StudyContext::new(store, window, scratch());
    let out = user_microstructure::run(&ctx, &params("bucket_volume = 2\nwindow = 2"))
        .expect("it runs against the seeded tape");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    assert_eq!(metric("trades"), 5.0);
    assert_eq!(metric("buckets"), 4.0);
    assert_eq!(metric("vpin"), 1.0);
    assert_eq!(metric("tick_rule_agreement"), 0.8);
    assert_eq!(metric("tick_rule_vpin"), 1.0);
    assert_eq!(metric("imbalance_bars"), 3.0);
    assert_eq!(metric("runs_bars"), 3.0);
    assert_eq!(metric("dollar_bars"), 4.0);
    let phi = vike_math::distributions::normal_cdf;
    let bvc = ((2.0 * phi(-1.0 / 1.5) - 1.0).abs() + (2.0 * phi(2.0 / 1.5) - 1.0).abs()) / 2.0;
    assert!((metric("bvc_vpin") - bvc).abs() < 1e-12, "{} against {bvc}", metric("bvc_vpin"));
    let tsv = &out.artifacts()[0];
    assert_eq!(tsv.0, "vpin.tsv");
    assert_eq!(tsv.1.lines().count(), 5, "a header and one line per bucket: {}", tsv.1);
}

/// The same tape read as an INVERSE contract (`contract_value = { kind = "inverse", per_unit = 1 }`):
/// a size is dollars, so 8 dollars traded in all make two bars of 3 (at trades 2 and 4), where the
/// linear reading (811 dollars) makes about 270. The recipe's contract prices the tape.
#[test]
fn microstructure_prices_dollar_bars_by_the_recipes_contract() {
    let (store, window) = tape_store();
    let ctx = StudyContext::new(store, window, scratch());
    let recipe = "bucket_volume = 2\nwindow = 2\ndollar_bucket = 3\n\
                  contract_value = { kind = \"inverse\", per_unit = 1.0 }";
    let out = user_microstructure::run(&ctx, &params(recipe)).expect("an inverse tape runs");
    assert_eq!(out.metric_value("dollar_bars"), Some(2.0));
    let linear = user_microstructure::run(&ctx, &params("bucket_volume = 2\ndollar_bucket = 3"))
        .expect("a linear tape runs");
    assert!(
        linear.metric_value("dollar_bars").is_some_and(|n| n > 100.0),
        "{:?}",
        linear.metrics()
    );
    let wrong =
        user_microstructure::run(&ctx, &params("contract_value = { kind = \"quarterly\" }"));
    assert!(matches!(wrong, Err(StudyError::Study(m)) if m.contains("contract_value")));
}

/// REVIEW-LIVE-MONEY L9: a recipe that names no `contract_value` over a symbol whose KEPT row is inverse is
/// refused in words, both ways, never read at linear 1.0 (a COIN-M tape's dollar bars about 600 times too
/// many); the recipe naming the contract runs, and a linear row reads the default.
#[test]
fn microstructure_refuses_an_unnamed_contract_whose_kept_row_is_inverse() {
    use vike_marketdata::test_support::trade;
    use vike_model::{Contract, ContractPerp, Instrument, Margining};
    let tape = vec![trade(1_000, 100.0, 2.0, false), trade(2_000, 101.0, 1.0, true)];
    let perp = |margining| {
        let perp = ContractPerp {
            margining,
            settle: "BTC".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        };
        Instrument::new("binance", "BTCUSDT", "BTC", "USD", Contract::Perp(perp))
    };
    let keeping = |margining| {
        let store: Arc<dyn HistoryStore + Send + Sync> =
            Arc::new(TapeStore(tape.clone(), Some(vec![perp(margining)])));
        move || StudyContext::new(Arc::clone(&store), TimestampRange::of(0, 10_000), scratch())
    };
    let unnamed = params("bucket_volume = 1");
    let said = both_ways(user_microstructure::run, keeping(Margining::Inverse), &unnamed);
    assert!(said.contains("is not a linear contract") && said.contains("contract_value"), "{said}");
    let named =
        params("bucket_volume = 1\ncontract_value = { kind = \"inverse\", per_unit = 1.0 }");
    let said = both_ways(user_microstructure::run, keeping(Margining::Inverse), &named);
    assert!(said.starts_with("ok"), "{said}");
    let said = both_ways(user_microstructure::run, keeping(Margining::Linear), &unnamed);
    assert!(said.starts_with("ok"), "{said}");
}

/// `microstructure` across the boundary, and its refusal of a window with no trades.
#[test]
fn microstructure_answers_the_same_across_the_study_file_boundary() {
    let ctx = || {
        let (store, window) = tape_store();
        StudyContext::new(store, window, scratch())
    };
    let said = both_ways(user_microstructure::run, ctx, &params("bucket_volume = 2\nwindow = 2"));
    assert!(said.starts_with("ok"), "{said}");
    let said = both_ways(user_microstructure::run, ctx, &params(r#"symbol = "ETHUSDT""#));
    assert!(said.starts_with("refused"), "{said}");
}

/// 300 one-minute bars whose closes take seeded steps of up to 1% either way, and the window over
/// them: enough rows for `sharpe_lstm` to fit and judge.
fn walk_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    let store = HistoryStoreMemory::new();
    let mut close = 100.0;
    let bars: Vec<Bar> = (0..300_i64)
        .map(|i| {
            close *= 1.0 + 0.01 * (((i * 7_919) % 2_001) as f64 / 1_000.0 - 1.0);
            vike_marketdata::test_support::flat_bar_unit_volume_on(SYMBOL, 60_000 * i, close)
        })
        .collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("fixture")).expect("the walk");
    (Arc::new(store), TimestampRange::of(0, 60_000 * 299))
}

/// `sharpe_lstm` (W24's consumer) across the boundary: it fits the learner on the first rows and
/// judges the later ones. The scripted classifier's constant 0.75 holds the position 2 * 0.75 - 1 =
/// 0.5 throughout; it keeps no text, so no model file is written. Without a learner: the typed
/// refusal.
#[test]
fn sharpe_lstm_fits_judges_and_answers_the_same_across_the_study_file_boundary() {
    let with = || {
        let (store, window) = walk_store();
        StudyContext::new(store, window, scratch()).with_learner(Arc::new(importance_learner()))
    };
    let recipe = params(&format!(
        "venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"{INTERVAL}\"\n[learner]\nsteps = 8\n"
    ));
    let out =
        user_sharpe_lstm::run(&with(), &recipe).expect("it fits against the scripted learner");
    assert_eq!(out.metric_value("later_mean_abs_position"), Some(0.5));
    assert_eq!(out.metric_value("later_turnover_per_bar"), Some(0.0));
    let rows = out.metric_value("rows").expect("rows");
    let later = out.metric_value("later_rows").expect("later rows");
    assert_eq!(out.metric_value("fit_rows"), Some(rows - later));
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["positions.tsv"], "a learner that keeps no text writes no model file");
    assert_eq!(out.artifacts()[0].1.lines().count() as f64, 1.0 + later, "header + a row per bar");
    assert!(both_ways(user_sharpe_lstm::run, with, &recipe).starts_with("ok"));

    let without = || {
        let (store, window) = walk_store();
        StudyContext::new(store, window, scratch())
    };
    assert!(both_ways(user_sharpe_lstm::run, without, &recipe).starts_with("refused"));
}

const TOKEN: &str = "1234567890";
const END_MS: i64 = 1_790_000_300_000;

/// A stand-in backtest that READS the store the study hands it (so every read crosses back into the
/// study's own memoizing store) and makes its numbers out of what it read, the store's refusals
/// included; a market called `refused` is the backtest's own refusal.
struct ReadingSim;

impl StudySim for ReadingSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistoryStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        let data = profile.get("data").ok_or("no [data]")?;
        let series = data
            .get("series")
            .and_then(toml::Value::as_array)
            .and_then(|a| a.first())
            .ok_or("no series")?;
        let text = |v: Option<&toml::Value>| v.and_then(toml::Value::as_str).map(str::to_string);
        let venue = text(series.get("venue")).ok_or("no venue")?;
        let symbol = text(series.get("symbol")).ok_or("no symbol")?;
        if symbol == "refused" {
            return Err("the stand-in backtest refuses this market\nacross two lines".to_string());
        }
        let bound = |k: &str| text(data.get(k)).and_then(|s| s.parse::<i64>().ok());
        let range =
            TimestampRange::of(bound("from").ok_or("no from")?, bound("to").ok_or("no to")?);
        let quotes = store.scan_quotes(&venue, &symbol, range).map_err(|e| e.to_string())?;
        let trades = store.scan_trades(&venue, &symbol, range).map_err(|e| e.to_string())?;
        let books = store.scan_book_updates(&venue, &symbol, range).map_err(|e| e.to_string())?;
        let refused =
            store.scan_symbol_properties(&venue, &symbol, range).err().map(|e| e.to_string());
        let kept = store.kept_instruments().map_err(|e| e.to_string())?;
        let mids: f64 = quotes.iter().map(|q| q.mid()).sum();
        Ok(SimOutcome {
            total_return: mids / 1000.0 + books.len() as f64 + f64::from(u8::from(kept.is_some())),
            n_trades: trades.len() as u64,
            win_rate: refused.map_or(0.0, |r| r.len() as f64 / 10_000.0),
        })
    }
}

fn poly_store() -> Arc<dyn HistoryStore + Send + Sync> {
    use vike_marketdata::test_support::{quote, trade};
    let store = HistoryStoreMemory::new();
    let from = END_MS - 300_000;
    let quotes: Vec<_> = (0..20)
        .map(|i| quote(from + 10_000 * i, 0.40 + 0.001 * i as f64, 0.42 + 0.001 * i as f64))
        .collect();
    let trades: Vec<_> = (0..7).map(|i| trade(from + 30_000 * i, 0.41, 5.0, i % 2 == 0)).collect();
    store.append_quotes("polymarket", TOKEN, &quotes, Some("fixture")).unwrap();
    store.append_trades("polymarket", TOKEN, &trades, Some("fixture")).unwrap();
    Arc::new(store)
}

/// `poly_mm`, a real study, across the boundary: every cell's backtest run BESIDE the file over the
/// study's own memoizing store, read back through the study (its reads, and its refusal of a verb the
/// context does not offer), a refused market reported in `failures.tsv`; and the typed refusal of a
/// run that offers no backtest.
#[test]
fn poly_mm_answers_the_same_across_the_study_file_boundary() {
    let recipe = params(&format!(
        "lane = \"both\"\nstrategy = \"both\"\nlatency_ms = 50\n\
         [[universe]]\nfamily = \"btc-5m\"\ntoken = \"{TOKEN}\"\nend_date_ms = {END_MS}\n\
         [[universe]]\nfamily = \"btc-5m\"\ntoken = \"refused\"\nend_date_ms = {END_MS}\n"
    ));
    let with_sim = || {
        StudyContext::new(poly_store(), TimestampRange::all(), scratch())
            .with_sim(Arc::new(ReadingSim))
    };
    let without = || StudyContext::new(poly_store(), TimestampRange::all(), scratch());
    let said = both_ways(user_poly_mm::run, with_sim, &recipe);
    assert!(said.starts_with("ok"), "{said}");
    assert!(both_ways(user_poly_mm::run, without, &recipe).starts_with("refused"));
}

// ---- the point-in-time text read, across the boundary ------------------------------------------

/// A study of the text history: what a decision at `decision_ms` could read, counted, and the ids it
/// read written out, so both ways must agree on the ROWS and not only on how many.
fn texts_study(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let decision_ms = params.get("decision_ms").and_then(toml::Value::as_integer).unwrap_or(0);
    let rows = ctx.texts_as_of("news", "rss", decision_ms, 3_600_000)?;
    let mut out = StudyOutcome::new();
    out.metric("texts", rows.len() as f64)?;
    let ids: Vec<String> =
        rows.iter().map(|r| format!("{}\t{}", r.received_ms, r.text_id())).collect();
    out.artifact("texts.tsv", ids.join("\n"))?;
    Ok(out)
}

/// The text verb crosses the study file's boundary as the point-in-time read it is: the same rows both
/// ways, and a row received after the decision never comes back, though it was published before it.
#[test]
fn the_point_in_time_text_read_answers_the_same_across_the_study_file_boundary() {
    let item = |received_ms: i64, url: &str| vike_data::TextRow {
        published_ms: 1_000,
        url: url.to_string(),
        ..vike_data::TextRow::new("news", "CoinDesk", received_ms)
    };
    let store = || -> Arc<dyn HistoryStore + Send + Sync> {
        let store = HistoryStoreMemory::new();
        let rows = [item(10_000, "https://a.test/1"), item(20_000, "https://a.test/2")];
        store.append_texts("news", "rss", &rows, Some("fixture")).unwrap();
        Arc::new(store)
    };
    let ctx = || StudyContext::new(store(), TimestampRange::all(), scratch());

    let said = both_ways(texts_study, ctx, &params("decision_ms = 15000"));
    assert!(said.starts_with("ok"), "{said}");
    let direct = texts_study(&ctx(), &params("decision_ms = 15000")).unwrap();
    assert_eq!(direct.metric_value("texts"), Some(1.0), "the second text was not received yet");
    let across = across_the_boundary(texts_study, ctx(), &params("decision_ms = 20000")).unwrap();
    assert_eq!(across.metric_value("texts"), Some(2.0), "received at the decision: in");
}

/// Thirty days of hourly bars and an eight-hourly funding series, deterministic (a Weyl sequence,
/// no generator): the hourly spread steps through three volatility regimes, volume follows the
/// regime, and about a third of the funding rates are spikes.
fn funding_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    const HOUR: i64 = 3_600_000;
    let unit = |i: i64, step: f64| (i as f64 * step).fract();
    let store = HistoryStoreMemory::new();
    let (mut close, mut prices, mut funding) = (100.0, Vec::new(), Vec::new());
    for h in 0..720i64 {
        let volatility = 0.002 * (1 + (h / 48) % 3) as f64;
        let open = close;
        close = open * (1.0 + volatility * (2.0 * unit(h, 0.618_033_988_749_895) - 1.0));
        let volume = 1_000.0 * volatility * (0.5 + unit(h, 0.414_213_562_373_095));
        prices.push(Bar::ohlcv(h * HOUR, open, open.max(close), open.min(close), close, volume));
        if h % 8 == 0 {
            let rate = if unit(h / 8, 0.754_877_666_246_693) < 0.35 { 0.001 } else { 0.0001 };
            let at = Bar::ohlcv(h * HOUR, 0.0, 0.0, 0.0, 0.0, 0.0);
            funding.push(Bar { funding: Some(rate), ..at });
        }
    }
    store.append_bars(VENUE, SYMBOL, "1h", &prices, Some("fixture-prices")).unwrap();
    store.append_bars(VENUE, SYMBOL, "funding", &funding, Some("fixture-funding")).unwrap();
    (Arc::new(store), TimestampRange::of(0, 720 * HOUR))
}

/// `funding_spike_effect`: double machine learning over both nuisance kinds, the same across the
/// boundary; the events that have 24 bars before and 8 after (funding hours 32..=712: 86), an
/// interval around the effect; and a recipe without a venue refused by name.
#[test]
fn funding_spike_effect_answers_the_same_across_the_study_file_boundary() {
    let ctx = || {
        let (store, window) = funding_store();
        StudyContext::new(store, window, scratch())
    };
    let base = format!("venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"1h\"\n");
    for nuisance in ["gaussian_process", "linear"] {
        let recipe = params(&format!("{base}nuisance = \"{nuisance}\""));
        let said = both_ways(user_funding_spike_effect::run, ctx, &recipe);
        assert!(said.starts_with("ok"), "{nuisance}: {said}");
        let out = user_funding_spike_effect::run(&ctx(), &recipe).unwrap();
        let metric = |m: &str| out.metric_value(m).unwrap();
        assert_eq!(metric("events"), 86.0, "{nuisance}");
        assert!(metric("spikes") > 0.0 && metric("spikes") < 86.0, "{nuisance}");
        assert!(metric("standard_error") > 0.0, "{nuisance}");
        assert!(
            metric("interval_low") < metric("effect") && metric("effect") < metric("interval_high"),
            "{nuisance}: {:?}",
            out.metrics()
        );
        eprintln!("funding_spike_effect [{nuisance}]: {:?}", out.metrics());
        let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["events.tsv"]);
        assert_eq!(out.artifacts()[0].1.lines().count(), 87, "header + one line per event");
    }
    let said = both_ways(user_funding_spike_effect::run, ctx, &params("interval = \"1h\""));
    assert!(said.contains("the recipe needs `venue`"), "{said}");
}

/// `funding_spike_effect` PURGES by label span (lookahead review 5): with a 20-bar horizon over an 8-hourly
/// funding, each label runs into the next two events', so every test fold loses the two training events on
/// each side whose labels overlap it (84 events, five folds: 2 + 4 + 4 + 4 + 2 = 16 left out with no
/// embargo); point folds left none out.
#[test]
fn a_funding_label_overlapping_a_test_fold_is_purged_from_training() {
    let (store, window) = funding_store();
    let ctx = StudyContext::new(store, window, scratch());
    let recipe = params(&format!(
        "venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"1h\"\nhorizon = 20\nembargo = 0\n\
         nuisance = \"linear\""
    ));
    let out = user_funding_spike_effect::run(&ctx, &recipe).unwrap();
    assert_eq!(out.metric_value("events"), Some(84.0));
    assert_eq!(out.metric_value("left_out_of_training"), Some(16.0), "{:?}", out.metrics());
}

/// `funding_spike_effect`'s DEFAULT embargo covers the confounders' lookback (lookahead review 9): 24 hourly
/// bars of confounders after an 8-bar label, events every 8 bars, so the three events after a test fold (8,
/// 16 and 24 bars on) read returns its last label holds and are embargoed (the old default was one). The
/// fourth, 32 bars on, only shares that label's last price. A stated `embargo` is used as stated.
#[test]
fn the_default_embargo_covers_every_event_whose_confounders_read_a_test_label() {
    let ctx = || {
        let (store, window) = funding_store();
        StudyContext::new(store, window, scratch())
    };
    let base = format!(
        "venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"1h\"\nnuisance = \"linear\"\n"
    );
    let out = user_funding_spike_effect::run(&ctx(), &params(&base)).unwrap();
    assert_eq!(out.metric_value("embargo"), Some(3.0), "{:?}", out.metrics());
    assert_eq!(
        out.metric_value("left_out_of_training"),
        Some(12.0),
        "three after each of four folds"
    );
    let stated =
        user_funding_spike_effect::run(&ctx(), &params(&format!("{base}embargo = 1"))).unwrap();
    assert_eq!(stated.metric_value("embargo"), Some(1.0));
    assert_eq!(stated.metric_value("left_out_of_training"), Some(4.0));
}

/// Thirty days of FOUR-hour bars and a funding every eight hours at 02:00, 10:00 and 18:00, so each funding
/// falls INSIDE a bar: the bar that opened before it closes after it. The bars are handed back too.
fn straddled_funding_store() -> (Arc<dyn HistoryStore + Send + Sync>, Vec<Bar>, TimestampRange) {
    const HOUR: i64 = 3_600_000;
    let unit = |i: i64, step: f64| (i as f64 * step).fract();
    let store = HistoryStoreMemory::new();
    let (mut close, mut prices, mut funding) = (100.0, Vec::new(), Vec::new());
    for k in 0..180i64 {
        let open = close;
        close = open * (1.0 + 0.01 * (2.0 * unit(k, 0.618_033_988_749_895) - 1.0));
        let volume = 1_000.0 * (0.5 + unit(k, 0.414_213_562_373_095));
        let (high, low) = (open.max(close), open.min(close));
        prices.push(Bar::ohlcv(4 * k * HOUR, open, high, low, close, volume));
    }
    for m in 0..90i64 {
        let rate = if unit(m, 0.754_877_666_246_693) < 0.35 { 0.001 } else { 0.0001 };
        let at = Bar::ohlcv((8 * m + 2) * HOUR, 0.0, 0.0, 0.0, 0.0, 0.0);
        funding.push(Bar { funding: Some(rate), ..at });
    }
    store.append_bars(VENUE, SYMBOL, "4h", &prices, Some("fixture-prices")).unwrap();
    store.append_bars(VENUE, SYMBOL, "funding", &funding, Some("fixture-funding")).unwrap();
    (Arc::new(store), prices, TimestampRange::of(0, 720 * HOUR))
}

/// `funding_spike_effect` reads only bars CLOSED by a funding time (lookahead review 7): with each funding
/// inside a four-hour bar, an event's outcome starts at the close of the last bar closed by then, never at
/// the close of the bar that opened before the funding and closed after it.
#[test]
fn a_funding_events_outcome_starts_at_a_bar_closed_by_the_funding_time() {
    const STEP: i64 = 4 * 3_600_000;
    let (store, prices, window) = straddled_funding_store();
    let ctx = StudyContext::new(store, window, scratch());
    let recipe = params(&format!(
        "venue = \"{VENUE}\"\nsymbol = \"{SYMBOL}\"\ninterval = \"4h\"\nlookback = 6\nhorizon = 1\n\
         folds = 3\nnuisance = \"linear\""
    ));
    let out = user_funding_spike_effect::run(&ctx, &recipe).unwrap();
    let rows: Vec<&str> = out.artifacts()[0].1.lines().skip(1).collect();
    assert!(rows.len() > 60, "{} events", rows.len());
    for line in rows {
        let cells: Vec<&str> = line.split('\t').collect();
        let (time, next_return): (i64, f64) =
            (cells[0].parse().unwrap(), cells[5].parse().unwrap());
        let j = prices.partition_point(|b| b.timestamp + STEP <= time);
        let expected = prices[j].close / prices[j - 1].close - 1.0;
        assert_eq!(next_return.to_bits(), expected.to_bits(), "the event at {time}");
    }
}

/// `event_calibration` over synthetic event markets: the counts of its point-in-time split, the
/// same outcome both ways, and calibration texts that read back as models.
#[test]
fn event_calibration_fits_on_earlier_markets_and_scores_later_ones() {
    let (store, recipe) = user_event_calibration::synthetic::store_and_recipe(60);
    let recipe = params(&recipe);
    let ctx = || StudyContext::new(store.clone(), TimestampRange::all(), scratch());
    let said = both_ways(user_event_calibration::run, ctx, &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_event_calibration::run(&ctx(), &recipe).expect("it runs");
    let metric = |name: &str| out.metric_value(name).expect(name);
    let counts = ["markets", "unresolved", "without_forecast", "fitted", "evaluated", "left_out"];
    // The five unresolved markets last traded at 0.99: the outcome is the archive's, never that price.
    assert_eq!(counts.map(metric), [60.0, 5.0, 0.0, 27.0, 27.0, 1.0]);
    for series in ["raw", "platt", "isotonic"] {
        let brier = metric(&format!("brier_{series}"));
        assert!(brier > 0.0 && brier < 1.0, "{series}: {brier}");
    }
    let artifact = |name: &str| {
        let found = out.artifacts().iter().find(|(n, _)| n == name);
        found.map(|(_, body)| body.as_str()).expect(name)
    };
    assert!(artifact("chart_reliability.tsv").starts_with("x\tvalue\tseries\tkind\n"));
    for name in ["calibration_platt.txt", "calibration_isotonic.txt"] {
        let model = vike_ml_prediction::any_model::AnyModel::from_text(artifact(name)).expect(name);
        let p = vike_ml_prediction::Predictor::predict_one(&model, &[0.5]).expect(name);
        assert!((0.0..=1.0).contains(&p), "{name}: {p}");
    }
    assert_eq!(artifact("markets.tsv").lines().count(), 1 + 55);

    match user_event_calibration::run(&ctx(), &params("horizon_ms = 1")) {
        Err(StudyError::Study(m)) => assert!(m.contains("venue"), "{m}"),
        other => panic!("a recipe without `venue` must be refused by name, got {other:?}"),
    }

    // As of a window that ends before the first market resolved, no outcome is stored yet: refused,
    // naming the archive fetch.
    let first_end = user_event_calibration::synthetic::FIRST_END_MS;
    let early = TimestampRange { start: None, end: Some(first_end - 60_000) };
    match user_event_calibration::run(&StudyContext::new(store.clone(), early, scratch()), &recipe)
    {
        Err(StudyError::Study(m)) => {
            assert!(m.contains("60 listed") && m.contains("polymarket:archive:rules"), "{m}")
        }
        other => panic!("no stored outcome must be refused naming the fetch, got {other:?}"),
    }
}

/// A study of the two SEC tables at `decision_ms`: what it could know, counted, and the rows written
/// out, so both ways must agree on the ROWS.
fn sec_tables_study(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let decision_ms = params.get("decision_ms").and_then(toml::Value::as_integer).unwrap_or(0);
    let facts = ctx.financial_facts_as_of("sec", "0001234567", decision_ms, 3_600_000)?;
    let lines = ctx.institutional_holdings_as_of("sec", "0009876543", decision_ms, 3_600_000)?;
    let mut out = StudyOutcome::new();
    out.metric("facts", facts.len() as f64)?;
    out.metric("lines", lines.len() as f64)?;
    let rows: Vec<String> = facts
        .iter()
        .map(|f| format!("{}\t{}\t{}", f.filed_ms, f.concept, f.value))
        .chain(lines.iter().map(|l| format!("{}\t{}\t{}", l.filed_ms, l.cusip, l.value_usd)))
        .collect();
    out.artifact("sec.tsv", rows.join("\n"))?;
    Ok(out)
}

/// The two SEC table reads cross the study file's boundary as the point-in-time reads they are: the
/// same rows both ways, and a row knowable after the decision never comes back.
#[test]
fn the_point_in_time_sec_table_reads_answer_the_same_across_the_study_file_boundary() {
    let fact = |filed_ms: i64, value: f64| vike_data::FinancialFactRow {
        filed_ms,
        cik: "0001234567".into(),
        entity: "EXAMPLE CORP".into(),
        taxonomy: "us-gaap".into(),
        concept: "Revenues".into(),
        unit: "USD".into(),
        period_start: "2023-10-01".into(),
        period_end: "2024-09-30".into(),
        value,
        accession: "0001234567-24-000106".into(),
        fiscal_year: "2024".into(),
        fiscal_period: "FY".into(),
        form: "10-K".into(),
        frame: String::new(),
    };
    let line = |filed_ms: i64| vike_data::InstitutionalHoldingRow {
        filed_ms,
        accession: "0009876543-24-000002".into(),
        filer_cik: "0009876543".into(),
        filer_name: "EXAMPLE CAPITAL LLC".into(),
        period: "2024-09-30".into(),
        submission_type: "13F-HR".into(),
        amendment_type: String::new(),
        issuer: "EXAMPLE CORP".into(),
        class_title: "COM".into(),
        cusip: "123456789".into(),
        value_usd: 1_500_000_000,
        shares: 10_000_000,
        shares_type: "SH".into(),
        put_call: String::new(),
    };
    let store = || -> Arc<dyn HistoryStore + Send + Sync> {
        let store = HistoryStoreMemory::new();
        let facts = [fact(10_000, 1.0), fact(20_000, 2.0)];
        store.append_financial_facts("sec", "0001234567", &facts, Some("facts")).unwrap();
        store
            .append_institutional_holdings("sec", "0009876543", &[line(20_000)], Some("lines"))
            .unwrap();
        Arc::new(store)
    };
    let ctx = || StudyContext::new(store(), TimestampRange::all(), scratch());

    let said = both_ways(sec_tables_study, ctx, &params("decision_ms = 15000"));
    assert!(said.starts_with("ok"), "{said}");
    let direct = sec_tables_study(&ctx(), &params("decision_ms = 15000")).unwrap();
    assert_eq!(direct.metric_value("facts"), Some(1.0), "the restatement was not knowable yet");
    assert_eq!(direct.metric_value("lines"), Some(0.0));
    let across =
        across_the_boundary(sec_tables_study, ctx(), &params("decision_ms = 20000")).unwrap();
    assert_eq!(across.metric_value("facts"), Some(2.0));
    assert_eq!(across.metric_value("lines"), Some(1.0));
}

// ---- the point-in-time text-score read, across the boundary ------------------------------------

/// A study of the text scores: the positive probability of every score a decision could read, summed,
/// so both ways must agree on the NUMBERS and not only on how many rows.
fn scores_study(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let decision_ms = params.get("decision_ms").and_then(toml::Value::as_integer).unwrap_or(0);
    let rows = ctx.text_scores_as_of("news", "finbert", decision_ms, 3_600_000)?;
    let mut out = StudyOutcome::new();
    out.metric("scores", rows.len() as f64)?;
    let mut positive = 0.0;
    for row in &rows {
        positive += row.value_list().map_err(StudyError::Study)?[0];
    }
    out.metric("positive", positive)?;
    Ok(out)
}

/// The score verb crosses the study file's boundary as the point-in-time read it is: the same numbers
/// both ways, and the score of a text received after the decision never comes back.
#[test]
fn the_point_in_time_text_score_read_answers_the_same_across_the_study_file_boundary() {
    let score = |received_ms: i64, values: &[f64]| vike_data::TextScoreRow {
        received_ms,
        text_id: format!("news\u{1f}CoinDesk\u{1f}https://a.test/{received_ms}"),
        chunk: 0,
        scored_ms: 99_000,
        model: "finbert@0".to_string(),
        values: vike_data::TextScoreRow::joined_values(values),
    };
    let store = || -> Arc<dyn HistoryStore + Send + Sync> {
        let store = HistoryStoreMemory::new();
        let rows = [score(10_000, &[0.75, 0.125, 0.125]), score(20_000, &[0.25, 0.5, 0.25])];
        store.append_text_scores("news", "finbert", &rows, Some("fixture")).unwrap();
        Arc::new(store)
    };
    let ctx = || StudyContext::new(store(), TimestampRange::all(), scratch());

    let said = both_ways(scores_study, ctx, &params("decision_ms = 15000"));
    assert!(said.starts_with("ok"), "{said}");
    let direct = scores_study(&ctx(), &params("decision_ms = 15000")).unwrap();
    assert_eq!(direct.metric_value("scores"), Some(1.0), "the second text was not received yet");
    assert_eq!(direct.metric_value("positive"), Some(0.75));
    let across = across_the_boundary(scores_study, ctx(), &params("decision_ms = 20000")).unwrap();
    assert_eq!(across.metric_value("scores"), Some(2.0), "received at the decision: in");
    assert_eq!(across.metric_value("positive"), Some(1.0), "the numbers cross bit for bit");
}

// ---- `text_base_rate`, the retrieval study (W17), across the boundary ------------------------------

/// `text_base_rate`: the same base rates both ways over its fixture store, every query with
/// neighbours whose outcome was known, every scored query on its kind's side, the last query's
/// neighbours assembled under their citations; and the refusal of a recipe that names no venue.
#[test]
fn text_base_rate_answers_the_same_across_the_study_file_boundary() {
    use user_text_base_rate::fixture_store;
    let ctx = || StudyContext::new(fixture_store::store(), fixture_store::window(), scratch());
    let recipe = params(&fixture_store::recipe());
    let said = both_ways(user_text_base_rate::run, ctx, &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_text_base_rate::run(&ctx(), &recipe).unwrap();
    let metric = |m: &str| out.metric_value(m).unwrap();
    assert_eq!([metric("texts"), metric("chunks"), metric("embedded_chunks")], [25.0; 3]);
    assert_eq!([metric("queries"), metric("with_base_rate"), metric("scored")], [19.0; 3]);
    assert_eq!(metric("side_hit_rate"), 1.0, "each kind's neighbours predict its side");
    let context = &out.artifacts().iter().find(|(name, _)| name == "context.txt").unwrap().1;
    assert!(context.starts_with("query: Spot ETF inflows lift bitcoin\n\n[1] Wire, published "));
    assert_eq!(context.matches("\n[").count(), 3, "three neighbours cited: {context}");

    let refused = both_ways(user_text_base_rate::run, ctx, &params("symbol = \"BTCUSDT\""));
    assert!(refused.contains("must name `venue`"), "{refused}");
}

// ---- `covered_call` ----------------------------------------------------------------------------

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
/// 2024-01-05 00:00 UTC, a Friday.
const FRIDAY_0: i64 = 1_704_412_800_000;

/// A deribit-shaped store: fifteen days of a linear underlying's hourly bars (50,000 the first week,
/// 56,000 on the 12th, 51,000 after) and a coin-book chain's RAGGED rows, stamped at the hour's last
/// millisecond. At Friday the 5th (spot 50,000) the candidates are the 12th's 55,000 and 60,000 calls
/// (not the put, not the 19th's), and at their rows' 50% IV a week out their deltas are N(−1.342) ≈ 0.090
/// and N(−2.599) ≈ 0.005, so the 0.3 target takes the 55,000; at the 12th (spot 56,000) the 19th's 57,000,
/// the only call of that expiry seen in the last day; at the 19th there is no call of the 26th.
fn option_store() -> Arc<dyn HistoryStore + Send + Sync> {
    use vike_data::ChainRow;
    use vike_marketdata::OptionKind;
    let store = HistoryStoreMemory::new();
    let close = |open: i64| match open {
        o if o < FRIDAY_0 + 7 * DAY => 50_000.0,
        o if o < FRIDAY_0 + 8 * DAY => 56_000.0,
        _ => 51_000.0,
    };
    let bars: Vec<Bar> = (0..15 * 24)
        .map(|i| {
            let open = FRIDAY_0 + i * HOUR;
            vike_marketdata::test_support::flat_bar_unit_volume_on("BTCUSDC.P", open, close(open))
        })
        .collect();
    store.append_bars("deribit", "BTCUSDC.P", "1h", &bars, None).unwrap();
    let row = |instrument: &str, kind, strike, expiry_days: i64, ts, mark, iv| ChainRow {
        ts,
        underlying: "BTC".to_string(),
        instrument: instrument.to_string(),
        expiry_ms: FRIDAY_0 + expiry_days * DAY + 8 * HOUR,
        strike,
        kind,
        bid: None,
        ask: None,
        mark: Some(mark),
        iv,
        open_interest: None,
        volume: Some(1.0),
        delta: None,
        gamma: None,
        theta: None,
        vega: None,
    };
    let (call, put) = (OptionKind::Call, OptionKind::Put);
    let rows = [
        row("BTC-12JAN24-55000-C", call, 55_000.0, 7, FRIDAY_0 + 6 * HOUR - 1, 400.0, Some(0.5)),
        row("BTC-12JAN24-55000-P", put, 55_000.0, 7, FRIDAY_0 + 6 * HOUR - 1, 4_000.0, Some(0.5)),
        row("BTC-12JAN24-60000-C", call, 60_000.0, 7, FRIDAY_0 + 6 * HOUR - 1, 90.0, Some(0.5)),
        row("BTC-19JAN24-55000-C", call, 55_000.0, 14, FRIDAY_0 + 6 * HOUR - 1, 700.0, Some(0.5)),
        row("BTC-12JAN24-55000-C", call, 55_000.0, 7, FRIDAY_0 + 11 * HOUR - 1, 420.0, Some(0.5)),
        row(
            "BTC-19JAN24-57000-C",
            call,
            57_000.0,
            14,
            FRIDAY_0 + 7 * DAY + 2 * HOUR - 1,
            350.0,
            None,
        ),
        row(
            "BTC-12JAN24-55000-C",
            call,
            55_000.0,
            7,
            FRIDAY_0 + 7 * DAY + 7 * HOUR - 1,
            990.0,
            None,
        ),
        row(
            "BTC-19JAN24-57000-C",
            call,
            57_000.0,
            14,
            FRIDAY_0 + 7 * DAY + 10 * HOUR - 1,
            360.0,
            None,
        ),
    ];
    for r in &rows {
        store.append_chain_snapshot("deribit", "BTC", std::slice::from_ref(r), None).unwrap();
    }
    Arc::new(store)
}

/// A stand-in for the covered call's backtest that reads what the real one reads of the store the
/// study hands it (the underlying's bars, the chain), so both cross back into the study's own store,
/// and makes its numbers of them: the last close, the legs' row count, the leg count.
struct ChainReadingSim;

impl StudySim for ChainReadingSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistoryStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        let data = profile.get("data").ok_or("no [data]")?;
        let options = data.get("options").ok_or("no [data.options]")?;
        let text = |v: &toml::Value, k: &str| {
            v.get(k).and_then(toml::Value::as_str).map(str::to_string).ok_or(format!("no {k}"))
        };
        let series = data.get("series").and_then(|s| s.get(0)).ok_or("no series")?;
        let bound =
            |k: &str| text(data, k).and_then(|s| s.parse::<i64>().map_err(|e| e.to_string()));
        let range = TimestampRange::of(bound("from")?, bound("to")?);
        let legs: Vec<String> = options
            .get("legs")
            .and_then(toml::Value::as_array)
            .ok_or("no legs")?
            .iter()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect();
        let bars = store
            .load_bars(
                &text(series, "venue")?,
                &text(options, "underlying")?,
                &text(options, "underlying_interval")?,
                range,
            )
            .map_err(|e| e.to_string())?;
        let rows = store
            .scan_chain(&text(options, "venue")?, &text(options, "chain")?, range)
            .map_err(|e| e.to_string())?;
        Ok(SimOutcome {
            total_return: bars.last().map_or(f64::NAN, |b| b.close) / 100_000.0,
            n_trades: rows.iter().filter(|r| legs.contains(&r.instrument)).count() as u64,
            win_rate: legs.len() as f64 / 100.0,
        })
    }
}

/// `covered_call` across the boundary: its choice point in time (the right call each week, the IV and
/// delta from `vike_options`), its one backtest's reads of the bars AND the chain crossing back into
/// the study's store, the same bits both ways; the typed refusal of a run with no backtest; a recipe
/// naming no chain refused.
#[test]
fn covered_call_answers_the_same_across_the_study_file_boundary() {
    let recipe = params(
        "venue = \"deribit\"\nchain = \"BTC\"\nunderlying_venue = \"deribit\"\n\
         underlying = \"BTCUSDC.P\"\ninterval = \"1h\"\nassumed_half_spread = 0.053\n",
    );
    let window = TimestampRange::of(FRIDAY_0, FRIDAY_0 + 15 * DAY);
    let with_sim =
        || StudyContext::new(option_store(), window, scratch()).with_sim(Arc::new(ChainReadingSim));
    let said = both_ways(user_covered_call::run, with_sim, &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_covered_call::run(&with_sim(), &recipe).unwrap();
    let metric = |m: &str| out.metric_value(m).unwrap_or(f64::NAN);
    assert_eq!(metric("weeks"), 2.0);
    assert_eq!(metric("skipped"), 1.0, "no call of the 26th to choose on the 19th");
    assert_eq!(metric("premium"), 400.0 + 350.0, "the marks as they stood at each choice");
    assert_eq!(metric("payout"), 1_000.0, "56,000 − 55,000, then out of the money");
    assert!(metric("mean_iv").is_finite() && metric("mean_delta").is_finite());
    assert_eq!(metric("total_return"), 0.51, "the stand-in read the bars through the study");
    // The legs are every week's candidates: the 12th's 55,000 and 60,000 calls, the 19th's 57,000.
    assert_eq!(
        metric("trades"),
        6.0,
        "the stand-in read the three legs' six rows through the study"
    );
    let calls = &out.artifacts()[0].1;
    assert!(
        calls.contains("BTC-12JAN24-55000-C") && calls.contains("BTC-19JAN24-57000-C"),
        "{calls}"
    );
    assert_eq!(calls.lines().count(), 3, "{calls}");

    let without = || StudyContext::new(option_store(), window, scratch());
    assert!(both_ways(user_covered_call::run, without, &recipe).starts_with("refused"));
    let no_chain = params("venue = \"deribit\"\nunderlying = \"X\"\n");
    assert!(both_ways(user_covered_call::run, with_sim, &no_chain).contains("`chain`"));
}

// ---- the point-in-time read of the live forecasts, across the boundary --------------------------

/// A study of the live forecasts: what a decision at `decision_ms` could know (from `start_ms` when the
/// recipe names one), counted, the last value as a metric (so a `NaN` must cross as itself), and every
/// row written out to its bits, so both ways must agree on the ROWS.
fn forecasts_study(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let decision_ms = params.get("decision_ms").and_then(toml::Value::as_integer).unwrap_or(0);
    let start_ms = params.get("start_ms").and_then(toml::Value::as_integer);
    let rows = ctx.forecasts_as_of(VENUE, SYMBOL, decision_ms, start_ms)?;
    let mut out = StudyOutcome::new();
    out.metric("forecasts", rows.len() as f64)?;
    out.metric("last_value", rows.last().map_or(0.0, |r| r.value))?;
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            let confidence = r.confidence.map(f64::to_bits);
            format!(
                "{}\t{}\t{}\t{}\t{confidence:?}",
                r.ts,
                r.mount_id,
                r.value.to_bits(),
                r.horizon_ms
            )
        })
        .collect();
    out.artifact("forecasts.tsv", lines.join("\n"))?;
    Ok(out)
}

/// The forecast verb crosses the study file's boundary as the point-in-time read it is: the same rows
/// both ways, bit for bit (a `NaN` value included), only rows decided at or before the decision, from
/// the start when one is asked, and another symbol's rows never.
#[test]
fn the_point_in_time_forecast_read_answers_the_same_across_the_study_file_boundary() {
    let row =
        |ts: i64, mount_id: &str, value: f64, confidence: Option<f64>| vike_data::ExecForecastRow {
            ts,
            mount_id: mount_id.to_string(),
            value,
            horizon_ms: 3_600_000,
            confidence,
        };
    let store = || -> Arc<dyn HistoryStore + Send + Sync> {
        let store = HistoryStoreMemory::new();
        let rows = [
            row(1_000, "m1", 0.25, None),
            row(2_000, "m2", f64::NAN, Some(0.5)),
            row(3_000, "m1", -0.5, None),
        ];
        store.append_forecasts(VENUE, SYMBOL, &rows, Some("fixture")).unwrap();
        store.append_forecasts(VENUE, "ETHUSDT", &[row(1_500, "m1", 1.0, None)], None).unwrap();
        Arc::new(store)
    };
    let ctx = || StudyContext::new(store(), TimestampRange::all(), scratch());

    for recipe in ["decision_ms = 2000", "decision_ms = 2000\nstart_ms = 1500", "decision_ms = 500"]
    {
        let said = both_ways(forecasts_study, ctx, &params(recipe));
        assert!(said.starts_with("ok"), "{recipe}: {said}");
    }
    let across =
        across_the_boundary(forecasts_study, ctx(), &params("decision_ms = 2000")).unwrap();
    assert_eq!(across.metric_value("forecasts"), Some(2.0), "the third was decided after it");
    assert!(
        across.metric_value("last_value").is_some_and(f64::is_nan),
        "the NaN crossed as itself"
    );
    let from =
        across_the_boundary(forecasts_study, ctx(), &params("decision_ms = 2000\nstart_ms = 1500"));
    assert_eq!(from.unwrap().metric_value("forecasts"), Some(1.0), "the start bounds it below");
    let at = across_the_boundary(forecasts_study, ctx(), &params("decision_ms = 3000")).unwrap();
    assert_eq!(at.metric_value("forecasts"), Some(3.0), "decided at the decision: in");
}

const MINUTE: i64 = 60_000;
/// The `(base, gamma, eta)` the seeded journal's fills were priced at.
const PRICED: (f64, f64, f64) = (0.0002, 0.6, 0.2);

/// One stored account fill, as the journal materializer writes it.
fn journal_fill(
    n: usize,
    ts: i64,
    side: i32,
    qty: f64,
    price: f64,
    liquidity: &str,
) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{n}"),
        order_client_id: format!("c{n}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        px: price,
        commission: 0.0,
        mark_price: None,
        liquidity_side: liquidity.into(),
        commission_asset: String::new(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

/// Eighty 1m bars, a quote just before and 30 s after each of eight taker fills priced by the
/// engine's model at [`PRICED`] (in the context `window_stats_before` measures, window 5), plus a
/// maker fill and a taker fill with no quote around it.
fn impact_store() -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    use vike_fills::market_impact::{unit_impact_terms, window_stats_before};
    use vike_marketdata::test_support::quote;
    let held = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..80)
        .map(|i| {
            let close = 100.0 + 0.3 * (i % 4) as f64 + 0.01 * i as f64;
            Bar {
                timestamp: MINUTE * i,
                open: close,
                high: close,
                low: close,
                close,
                volume: 1_000.0 + 100.0 * (i % 5) as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    // One commit key per batch: the store skips a batch whose key it has already ingested.
    held.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("impact-bars")).unwrap();
    let (base, gamma, eta) = PRICED;
    let (mut quotes, mut fills) = (Vec::new(), Vec::new());
    for (k, qty) in [5.0, 40.0, 120.0, 15.0, 300.0, 60.0, 200.0, 10.0].into_iter().enumerate() {
        let ts = 5 * MINUTE * (k as i64 + 1) + 15_000;
        let side = if k % 2 == 0 { 1 } else { -1 };
        let s = f64::from(side);
        let context = window_stats_before(&bars, ts, 5).expect("five bars closed before");
        let (permanent, temporary) = unit_impact_terms(qty, &context, 1.0).expect("measurable");
        let arrival = 100.0 + k as f64;
        let after = arrival * (1.0 + s * gamma * permanent);
        quotes.push(quote(ts - 1_000, arrival - 0.05, arrival + 0.05));
        quotes.push(quote(ts + 30_000, after - 0.05, after + 0.05));
        let price = arrival * (1.0 + s * (base + gamma * permanent + eta * temporary));
        fills.push(journal_fill(k, ts, side, qty, price, "taker"));
    }
    fills.push(journal_fill(100, 70 * MINUTE, 1, 50.0, 100.0, "maker"));
    fills.push(journal_fill(101, 72 * MINUTE, 1, 50.0, 100.0, "taker"));
    assert_eq!(held.append_exec_fills(VENUE, SYMBOL, &fills, Some("impact-fills")).unwrap(), 10);
    (Arc::new(JournalStore { held, quotes }), TimestampRange::of(0, 80 * MINUTE))
}

/// The journal study's store: the bars and the account's fills held by the workspace's own
/// `HistoryStoreMemory`, plus the L1 quotes that double does not hold (its quote verbs are inert).
struct JournalStore {
    held: HistoryStoreMemory,
    quotes: Vec<TickQuote>,
}

impl HistoryStore for JournalStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.held.load_bars(venue, symbol, interval, range)
    }

    fn scan_quotes(
        &self,
        _: &str,
        _: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let inside =
            |ts: i64| range.start.is_none_or(|lo| ts >= lo) && range.end.is_none_or(|hi| ts <= hi);
        Ok(self.quotes.iter().filter(|q| inside(q.timestamp)).cloned().collect())
    }

    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        self.held.scan_exec_fills(venue, symbol)
    }

    vike_data::hist_store_stubs!(inert: writes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_orders);
}

/// `impact_fit` reads the journal through the context (`execution_fills`, then the quotes and bars
/// around each fill) and fits back the coefficients the fills were priced at, the same both ways;
/// the profile table it writes reads as TOML; a recipe without the profile's knobs is refused.
#[test]
fn impact_fit_recovers_the_coefficients_the_journal_was_priced_at() {
    let recipe = params(
        "venue = \"binance\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\nwindow = 5\nexec_time = 1",
    );
    let ctx = || {
        let (store, window) = impact_store();
        StudyContext::new(store, window, scratch())
    };
    let said = both_ways(user_impact_fit::run, ctx, &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_impact_fit::run(&ctx(), &recipe).expect("it fits");
    let (base, gamma, eta) = PRICED;
    let metric = |name: &str| out.metric_value(name).expect(name);
    assert!((metric("gamma") - gamma).abs() <= 1e-9 * gamma, "gamma {}", metric("gamma"));
    assert!((metric("eta") - eta).abs() <= 1e-9 * eta, "eta {}", metric("eta"));
    assert!((metric("base") - base).abs() < 1e-12, "base {}", metric("base"));
    let counts = ["fills", "fills_used", "fills_not_taker", "fills_without_mid", "profile_ready"];
    assert_eq!(counts.map(metric), [10.0, 8.0, 1.0, 1.0, 1.0]);
    assert_eq!(metric("fills_without_context"), 0.0);

    let table = out
        .artifacts()
        .iter()
        .find(|(name, _)| name == "stated_slippage.toml")
        .map(|(_, body)| toml::from_str::<toml::Table>(body).expect("the table is TOML"))
        .expect("a fit the table's check accepts writes its table");
    let slippage = &table;
    assert_eq!(slippage["model"].as_str(), Some("market_impact"));
    assert_eq!(slippage["gamma"].as_float(), Some(metric("gamma")));
    assert_eq!(slippage["window"].as_integer(), Some(5));
    assert_eq!(slippage["exec_time"].as_float(), Some(1.0));

    let no_knobs = params("venue = \"binance\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"");
    match user_impact_fit::run(&ctx(), &no_knobs) {
        Err(StudyError::Study(m)) => assert!(m.contains("window"), "{m}"),
        other => panic!("a recipe without `window` must be refused by name, got {other:?}"),
    }
}

/// `alpha_pool` (book W21), a pool over three saved runs, across the boundary: the runs read through the
/// parent (`Call::RunForecasts`), every number the same both ways. The run that knew the answer gets the
/// largest weight, the mean-variance weights sum to one, and the pool's information is positive where it
/// was about. Without saved runs, and with a run the host does not hold, the study refuses in its words.
#[test]
fn alpha_pool_answers_the_same_across_the_study_file_boundary() {
    use user_alpha_pool::fixture;
    let recipe = params(fixture::RECIPE);
    let said = both_ways(user_alpha_pool::run, || fixture::context(scratch()), &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_alpha_pool::run(&fixture::context(scratch()), &recipe).expect("the pool");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    assert_eq!((metric("runs"), metric("symbols"), metric("periods")), (3.0, 1.0, 12.0));
    let weights = [metric("weight_0"), metric("weight_1"), metric("weight_2")];
    assert!((weights.iter().fold(0.0, |t, w| t + w) - 1.0).abs() < 1e-12, "{weights:?}");
    assert!(weights[0] > weights[1] && weights[0] > weights[2], "trend knew: {weights:?}");
    assert!(metric("out_of_sample_information_lag_0") > 0.0, "{:?}", out.metrics());
    assert_eq!(metric("out_of_sample_periods"), 6.0);
    assert!(
        out.metric_value("pool_information_lag_0").is_none(),
        "no in-sample figure as a measure"
    );
    let tsv = &out.artifacts()[0];
    assert_eq!(tsv.0, "pool.tsv");
    assert_eq!(tsv.1.lines().count(), 13, "a header and one line a period: {}", tsv.1);

    // The live route (STOP 3): `model.txt` is the weights as a pool text, and a strategy that runs the
    // three members itself and combines them with it gets the pool's forecast, bit for bit, each period.
    let (name, text) = &out.artifacts()[1];
    assert_eq!(name, "model.txt");
    let pool = vike_portfolio::alpha_pool::PoolWeights::parse(text).expect("a pool text");
    let names: Vec<&str> = pool.members().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["trend", "fade", "hold"]);
    assert_eq!(pool.members()[0].1, weights[0]);
    use vike_user_research::StudyRuns;
    let members: Vec<_> =
        names.iter().map(|n| fixture::Runs.forecasts(n).expect("a fixture run")).collect();
    for line in tsv.1.lines().skip(1) {
        let cells: Vec<&str> = line.split('\t').collect();
        let (time, pooled): (i64, f64) = (cells[0].parse().unwrap(), cells[2].parse().unwrap());
        let combined = pool
            .combine(|member| {
                let at = names.iter().position(|n| *n == member)?;
                let row = members[at].iter().find(|r| r.timestamp == time)?;
                Some(row.forecast.value())
            })
            .unwrap_or(f64::NAN);
        assert_eq!(combined.to_bits(), pooled.to_bits(), "period {time}: {combined} vs {pooled}");
    }

    let (store, window) = fixture::store();
    let no_runs = || StudyContext::new(Arc::clone(&store), window, scratch());
    assert!(both_ways(user_alpha_pool::run, no_runs, &recipe).contains("no saved runs"));
    let unknown = params("runs = [\"trend\", \"gone\"]\n");
    let said = both_ways(user_alpha_pool::run, || fixture::context(scratch()), &unknown);
    assert!(said.contains("no run \"gone\""), "{said}");
}

/// `alpha_pool`'s information is OUT OF SAMPLE (lookahead review 3): it is the pool of the weights fit on the
/// window's first six periods alone, scored on the last six, and not the whole-window weights' pool, which
/// was fit on the very returns it would be scored on.
#[test]
fn alpha_pools_information_is_measured_on_periods_its_weights_never_saw() {
    use user_alpha_pool::fixture::{self, CLOSES};
    use vike_math::linear_algebra::Matrix;
    use vike_portfolio::alpha_pool::{
        alpha_returns, combined_forecast, information_decay, mean_variance_weights,
    };
    use vike_user_research::StudyRuns;
    let out = user_alpha_pool::run(&fixture::context(scratch()), &params(fixture::RECIPE)).unwrap();
    let column = |values: Vec<f64>| Matrix::from_row_major(values.len(), 1, values).unwrap();
    let realized: Vec<f64> = (0..CLOSES.len())
        .map(|t| CLOSES.get(t + 1).map_or(f64::NAN, |next| next / CLOSES[t] - 1.0))
        .collect();
    // Each run's forecast of period t, stamped at bar t's close.
    let panel = |run: &str, range: std::ops::Range<usize>| {
        let rows = fixture::Runs.forecasts(run).unwrap();
        column(
            range
                .map(|t| {
                    let close = fixture::BAR_MS * (t as i64 + 1);
                    rows.iter()
                        .find(|r| r.timestamp == close)
                        .map_or(f64::NAN, |r| r.forecast.value())
                })
                .collect(),
        )
    };
    let information = |fit: std::ops::Range<usize>, scored: std::ops::Range<usize>| {
        let fitted: Vec<Matrix> =
            ["trend", "fade", "hold"].iter().map(|run| panel(run, fit.clone())).collect();
        let truth = column(realized[fit].to_vec());
        let weights =
            mean_variance_weights(&alpha_returns(&fitted, &truth).unwrap(), false).unwrap();
        let tested: Vec<Matrix> =
            ["trend", "fade", "hold"].iter().map(|run| panel(run, scored.clone())).collect();
        let pool = combined_forecast(&tested, &weights).unwrap();
        information_decay(&pool, &column(realized[scored].to_vec()), 3).unwrap()[0]
    };
    let held_out = information(0..6, 6..12);
    let in_sample = information(0..12, 6..12);
    let reported = out.metric_value("out_of_sample_information_lag_0").unwrap();
    assert_eq!(reported.to_bits(), held_out.to_bits(), "{reported} vs {held_out}");
    assert_ne!(
        reported.to_bits(),
        in_sample.to_bits(),
        "the whole window's weights saw these returns"
    );
}

/// `forward_report` (book chapter 22): a five-day forward test held against its saved backtest, across the
/// boundary (the run's ledger read through the parent, `Call::RunTrades`; the fills through
/// `Call::ExecutionFills`), every number the same both ways. The pairing finds the missed signal and the
/// forward's own trade, the slippage is the fixture's by construction and drifts up in the second half,
/// and the forward record trails the backtest's. Without an `account` the other account's round trip
/// joins; a run with no ledger, a host with no runs and a window with no end are refused in words.
#[test]
fn forward_report_answers_the_same_across_the_study_file_boundary() {
    use user_forward_report::fixture;
    let recipe = params(fixture::RECIPE);
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &recipe);
    assert!(said.starts_with("ok"), "{said}");

    let out = user_forward_report::run(&fixture::context(scratch()), &recipe).expect("the report");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    let close = |name: &str, want: f64| {
        let got = metric(name);
        assert!((got - want).abs() < 1e-9, "{name}: {got} vs {want}");
    };
    assert_eq!(
        ["backtest_trades", "forward_trades", "matched_trades"].map(metric),
        [20.0, 20.0, 19.0]
    );
    assert_eq!(["backtest_only_trades", "forward_only_trades"].map(metric), [1.0, 1.0]);
    close("match_rate", 0.95);
    close("entry_delay_ms", 30_000.0);
    let matched: Vec<usize> = (0..fixture::SIGNALS).filter(|k| *k != fixture::MISSED).collect();
    let average = |ks: &[usize], cost: &dyn Fn((f64, f64)) -> f64| {
        ks.iter().map(|k| cost(fixture::slippage_bps(*k))).sum::<f64>() / ks.len() as f64
    };
    close("entry_slippage_bps", average(&matched, &|(entry, _)| entry));
    close("exit_slippage_bps", average(&matched, &|(_, exit)| exit));
    close("slippage_bps_first_half", average(&matched[..9], &|(entry, exit)| entry + exit));
    close("slippage_bps_second_half", average(&matched[9..], &|(entry, exit)| entry + exit));
    assert!(metric("slippage_t_statistic") > 3.0 && metric("slippage_p_value") < 0.01);
    assert!(metric("matched_return_difference") < 0.0);
    assert!(metric("return_difference") < 0.0, "{:?}", out.metrics());
    assert!(metric("sharpe_difference") < 0.0, "{:?}", out.metrics());
    assert_eq!(metric("days"), 5.0);
    assert!(metric("forward_probability_sharpe_above_backtest") < 0.5);
    for needed in [
        "days_to_tell_backtest_from_zero",
        "days_to_tell_forward_from_backtest",
        "trades_to_tell_backtest_from_zero",
        "trades_to_tell_forward_from_backtest",
    ] {
        assert!(metric(needed).is_finite() && metric(needed) > 1.0, "{needed}: {}", metric(needed));
    }

    let artifact = |name: &str| {
        let found = out.artifacts().iter().find(|(n, _)| n == name);
        found.unwrap_or_else(|| panic!("no {name}")).1.clone()
    };
    let lines = |name: &str| artifact(name).lines().count();
    assert_eq!(lines("matched_trades.tsv"), 1 + 19 + 1 + 1);
    assert_eq!(lines("power.tsv"), 1 + 6);
    assert_eq!(lines("chart_equity.tsv"), 1 + 2 * 6, "two records, the opening and five days each");
    assert_eq!(lines("chart_slippage.tsv"), 1 + 2 * 19);
    assert_eq!(lines("chart_confidence.tsv"), 1 + 2 * 4);
    for chart in ["chart_equity.tsv", "chart_slippage.tsv", "chart_confidence.tsv"] {
        assert!(artifact(chart).starts_with("time\tvalue\tseries\tkind\n"), "{chart}");
    }

    let every_account = params(&fixture::RECIPE.replace("account = \"paper-1\"\n", ""));
    let all = user_forward_report::run(&fixture::context(scratch()), &every_account).unwrap();
    assert_eq!(all.metric_value("forward_only_trades"), Some(2.0), "demo-2's round trip joins");

    let no_ledger = params(&fixture::RECIPE.replace("\"backtest\"", "\"no_ledger\""));
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &no_ledger);
    assert!(said.contains("kept no trade ledger"), "{said}");
    let no_runs =
        || fixture::valued(StudyContext::new(fixture::store(), fixture::window(), scratch()));
    assert!(both_ways(user_forward_report::run, no_runs, &recipe).contains("no saved runs"));
    let open_ended = || {
        let window = TimestampRange { start: Some(fixture::START), end: None };
        fixture::valued(
            StudyContext::new(fixture::store(), window, scratch())
                .with_runs(Arc::new(fixture::Runs)),
        )
    };
    assert!(both_ways(user_forward_report::run, open_ended, &recipe).contains("start and an end"));

    // Named by the forward run alone (book chapter 22): its span, its backtest, its venue, symbol and account
    // give the long recipe's report bit for bit.
    let by_forward = params(fixture::FORWARD_RECIPE);
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &by_forward);
    assert!(said.starts_with("ok"), "{said}");
    let short = user_forward_report::run(&fixture::context(scratch()), &by_forward).unwrap();
    let bits = |o: &vike_user_research::StudyOutcome| -> Vec<(String, u64)> {
        o.metrics().iter().map(|(n, v)| (n.clone(), v.to_bits())).collect()
    };
    // The forward run names its mount too, so its report is the long recipe naming that mount: the plain
    // report's metrics, then the two mount counts (every `paper-1` fill is the mount's own).
    let with_mount = params(&format!("{}mount_id = \"{}\"\n", fixture::RECIPE, fixture::MOUNT));
    let mounted = user_forward_report::run(&fixture::context(scratch()), &with_mount).unwrap();
    assert_eq!(bits(&short), bits(&mounted));
    assert_eq!(bits(&mounted)[..bits(&out).len()], bits(&out)[..]);
    assert_eq!(bits(&mounted).len(), bits(&out).len() + 2);
    assert_eq!(
        ["fills_of_other_mounts", "fills_without_mount"].map(|n| mounted.metric_value(n)),
        [Some(0.0), Some(0.0)]
    );
    assert_eq!(short.artifacts(), out.artifacts());
    // A forward run still open, naming no backtest: the recipe must name one, and its end is the window's.
    let open = params("forward_run = \"open\"\ncapital = 10000.0\n");
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &open);
    assert!(said.contains("names no backtest"), "{said}");
    let open = params("forward_run = \"open\"\nbacktest_run = \"backtest\"\ncapital = 10000.0\n");
    let still_open = user_forward_report::run(&fixture::context(scratch()), &open).unwrap();
    assert_eq!(bits(&still_open), bits(&mounted), "an open run ends where the window does");
    let not_forward = params("forward_run = \"backtest\"\ncapital = 10000.0\n");
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &not_forward);
    assert!(said.contains("not a `forward` run"), "{said}");

    // Two strategies on one account and symbol, told apart by the mount every fill names: another mount's
    // round trip is left out, a hand-placed one naming no mount is kept and counted.
    let other = || {
        fixture::valued(
            StudyContext::new(fixture::store_with_another_mount(), fixture::window(), scratch())
                .with_runs(Arc::new(fixture::Runs)),
        )
    };
    let said = both_ways(user_forward_report::run, other, &with_mount);
    assert!(said.starts_with("ok"), "{said}");
    let mixed = user_forward_report::run(&other(), &recipe).unwrap();
    assert_eq!(
        mixed.metric_value("forward_only_trades"),
        Some(3.0),
        "both join with no mount named"
    );
    let apart = user_forward_report::run(&other(), &with_mount).unwrap();
    assert_eq!(
        ["forward_only_trades", "fills_of_other_mounts", "fills_without_mount"]
            .map(|n| apart.metric_value(n)),
        [Some(2.0), Some(2.0), Some(2.0)]
    );
}

/// `forward_report`'s headline is ONE `TradingResultsComparison` of the two records, so a record whose equity
/// reached zero is INVALID there, as in every table: the `ruined` backtest (one round trip losing more than the
/// capital) reads NaN for its return and Sharpe and for both differences, where it read the numbers of an account
/// that no longer existed. The forward record's stay numbers. Both ways.
#[test]
fn forward_report_reads_a_record_whose_equity_reached_zero_as_invalid() {
    use user_forward_report::fixture;
    let ruined =
        params(&fixture::RECIPE.replace("\"backtest\"", "\"ruined\"").replace("10000.0", "10.0"));
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &ruined);
    assert!(said.starts_with("ok"), "{said}");
    let out = user_forward_report::run(&fixture::context(scratch()), &ruined).expect("the report");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    for invalid in ["backtest_return", "backtest_sharpe", "return_difference", "sharpe_difference"]
    {
        assert!(metric(invalid).is_nan(), "{invalid}: {}", metric(invalid));
    }
    for valid in ["forward_return", "forward_sharpe"] {
        assert!(metric(valid).is_finite(), "{valid}: {}", metric(valid));
    }
}

/// REVIEW-LIVE-MONEY M5: `forward_report` values both records linearly, so a symbol whose KEPT row is not linear is
/// refused in words, both ways: an inverse perpetual (+6,000,000 where the account booked +100) and a quanto. A
/// linear row answers the report it always did, its `contract_size` included (a crypto mount books base units).
#[test]
fn forward_report_refuses_a_contract_it_cannot_value_and_keeps_a_linear_one_identical() {
    use user_forward_report::fixture;
    use vike_model::{Contract, ContractPerp, Instrument, Margining};
    let recipe = params(fixture::RECIPE);
    let row = |margining, contract_size| {
        let perp = ContractPerp {
            margining,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        };
        let mut row =
            Instrument::new(fixture::VENUE, fixture::SYMBOL, "BTC", "USDT", Contract::Perp(perp));
        row.properties.contract_size = contract_size;
        row
    };
    let keeping = |row: Instrument| {
        move || {
            fixture::valued(
                StudyContext::new(
                    fixture::store_keeping(vec![row.clone()]),
                    fixture::window(),
                    scratch(),
                )
                .with_runs(Arc::new(fixture::Runs)),
            )
        }
    };
    for refused in [row(Margining::Inverse, 100.0), row(Margining::Quanto, 1.0)] {
        let said = both_ways(user_forward_report::run, keeping(refused), &recipe);
        assert!(said.contains("is not a linear contract"), "{said}");
    }
    let plain = user_forward_report::run(&fixture::context(scratch()), &recipe).unwrap();
    let bits = |o: &vike_user_research::StudyOutcome| -> Vec<(String, u64)> {
        o.metrics().iter().map(|(n, v)| (n.clone(), v.to_bits())).collect()
    };
    for linear in [row(Margining::Linear, 0.0), row(Margining::Linear, 0.01)] {
        let kept = user_forward_report::run(&keeping(linear)(), &recipe).unwrap();
        assert_eq!(bits(&kept), bits(&plain));
        assert_eq!(kept.artifacts(), plain.artifacts());
    }
}

/// Review M5's second half (DEFERRED 533f): the host values the kept row (`StudyContext::kept_contract_value`, the
/// catalog's `vike_catalog::kept_contract_value`, asked across the study file's boundary), and `forward_report`
/// never reads one unit as 1.0. A unit only the venue states is refused in words, both ways: an ibkr future (its
/// multiplier), an fxcm CFD (its `ContractMultiplier`) and an ig epic (its `contractSize`). So is a symbol with no
/// kept row, and a host that values no contract (a context built without the valuation).
#[test]
fn forward_report_refuses_a_unit_only_the_venue_states_and_a_symbol_the_host_cannot_value() {
    use user_forward_report::fixture;
    use vike_model::{
        Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
        ContractFutureTenor, Instrument, Margining,
    };
    let es = Contract::Future(ContractFuture {
        margining: Margining::Linear,
        settle: "USD".into(),
        value_currency: None,
        expiry_ms: 1_797_638_400_000,
        tenor: Some(ContractFutureTenor::Quarter),
        delivery: ContractFutureDelivery::Unknown,
        family: ContractFutureFamily::Traditional,
    });
    let cases = [
        Instrument::new("ibkr", "ES_261218", "ES", "USD", es),
        Instrument::new("fxcm", "XAUUSD", "XAU", "USD", Contract::Commodity),
        Instrument::new("ig", "IX.D.FTSE.DAILY.IP", "FTSE", "GBP", Contract::Index),
    ];
    let recipe_on = |venue: &str, symbol: &str| {
        params(
            &fixture::RECIPE
                .replace("venue = \"binance\"", &format!("venue = \"{venue}\""))
                .replace("symbol = \"BTCUSDT\"", &format!("symbol = \"{symbol}\"")),
        )
    };
    let keeping = |rows: Vec<Instrument>| {
        move || {
            fixture::valued(
                StudyContext::new(
                    fixture::store_keeping(rows.clone()),
                    fixture::window(),
                    scratch(),
                )
                .with_runs(Arc::new(fixture::Runs)),
            )
        }
    };
    for row in cases {
        assert_eq!(
            vike_catalog::kept_contract_value(&row.venue, &row.raw_symbol, Some(&row)),
            None
        );
        let recipe = recipe_on(&row.venue, &row.raw_symbol);
        let said = both_ways(user_forward_report::run, keeping(vec![row.clone()]), &recipe);
        assert!(said.contains("the catalog states no unit"), "{}: {said}", row.venue);
    }
    // No kept row of the symbol: refused, never read as 1.0.
    let said = both_ways(user_forward_report::run, keeping(Vec::new()), &params(fixture::RECIPE));
    assert!(said.contains("has no kept instrument row"), "{said}");
    // A host that values no contract.
    let unvalued = || {
        StudyContext::new(fixture::store(), fixture::window(), scratch())
            .with_runs(Arc::new(fixture::Runs))
    };
    let said = both_ways(user_forward_report::run, unvalued, &params(fixture::RECIPE));
    assert!(said.contains("values no contract"), "{said}");
    // A host older than the verb answers it with nothing (it cannot read the call): refused in words.
    let ctx: &'static StudyContext = Box::leak(Box::new(fixture::context(scratch())));
    let start = host::start(ctx, &params(fixture::RECIPE)).expect("the start message");
    let parent = host::Parent::new(ctx, Arc::new(guest::serve));
    let older: guest::Host = Arc::new(move |request: &[u8]| {
        let asks_the_value = request.windows(17).any(|w| w == b"KeptContractValue");
        if asks_the_value { Vec::new() } else { parent.answer(request) }
    });
    match host::finished(&guest::run(user_forward_report::run, &start, older)) {
        Err(e) => assert!(e.to_string().contains("older than this study file"), "{e}"),
        Ok(_) => panic!("an older host's silence must refuse the report, never value it at 1.0"),
    }
}

/// `forward_report`'s two opt-in readings (book chapter 22), across the study file boundary: the mount's live
/// forecasts read point in time (`Call::ForecastsAsOf`) against the backtest run's `forecasts.json`, and F1's
/// sequential alarms. CUSUM alarms on the second matched trade (the forward always pays more than the
/// backtest), Page–Hinkley only once the fills drift (signal 13, the fourth trade after signal 10), and the
/// return shortfall alarms on the first day.
#[test]
fn forward_report_compares_the_live_forecasts_and_raises_the_drift_alarms() {
    use user_forward_report::fixture;
    let every = params(fixture::EVERY_KEY_RECIPE);
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &every);
    assert!(said.starts_with("ok"), "{said}");
    let out = user_forward_report::run(&fixture::context(scratch()), &every).expect("the report");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    // Another mount's forecasts, one after the window and one of the backtest's before it are left out.
    assert_eq!(
        ["live_forecasts", "backtest_forecasts", "paired_forecasts"].map(metric),
        [20.0, 20.0, 20.0]
    );
    let half_shifted = fixture::LIVE_FORECAST_SHIFT / 2.0;
    for name in ["forecast_difference", "forecast_absolute_difference"] {
        assert!((metric(name) - half_shifted).abs() < 1e-12, "{name}: {}", metric(name));
    }
    assert!(metric("forecast_population_stability_index") >= 0.0);
    let (statistic, p) = (
        metric("forecast_kolmogorov_smirnov_statistic"),
        metric("forecast_kolmogorov_smirnov_p_value"),
    );
    assert!((0.0..=1.0).contains(&statistic) && (0.0..=1.0).contains(&p), "{statistic} {p}");

    assert!(metric("slippage_cusum_alarms") > metric("slippage_page_hinkley_alarms"));
    let first_day = metric("return_shortfall_cusum_first_alarm_ms");
    assert!(
        first_day > fixture::START as f64 && first_day <= (fixture::START + fixture::DAY) as f64
    );
    assert!(metric("return_shortfall_cusum_alarms") >= 1.0);

    // Without the mount named, the other mount's forecasts join.
    let any_mount = params(&format!("{}compare_forecasts = true\n", fixture::RECIPE));
    let all = user_forward_report::run(&fixture::context(scratch()), &any_mount).unwrap();
    assert_eq!(all.metric_value("live_forecasts"), Some(40.0));
    // A threshold that would alarm on every step is refused in words.
    let zero = params(&format!("{}[drift]\nslippage_threshold_bps = 0.0\n", fixture::RECIPE));
    let said = both_ways(user_forward_report::run, || fixture::context(scratch()), &zero);
    assert!(said.contains("slippage_threshold_bps"), "{said}");
}

/// `forward_report`'s slippage alarm is stamped where the number that tripped it became KNOWN (lookahead review
/// 8): a matched trade's slippage holds its exit slippage, so the series runs in exit order and an alarm is
/// stamped at the forward EXIT, never at the entry, hours before its exit was paid.
#[test]
fn a_slippage_alarm_is_stamped_at_the_exit_that_made_it_known() {
    use user_forward_report::fixture;
    let out =
        user_forward_report::run(&fixture::context(scratch()), &params(fixture::EVERY_KEY_RECIPE))
            .expect("the report");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no {name}"));
    assert_eq!(metric("slippage_cusum_first_alarm_ms"), fixture::forward_exit(1) as f64);
    assert_eq!(metric("slippage_page_hinkley_first_alarm_ms"), fixture::forward_exit(13) as f64);
}

/// The recipe `pairs_reversion` runs the fixture market under: every symbol named, the 1% level.
fn pairs_recipe(extra: &str) -> toml::Value {
    use user_pairs_reversion::fixture_store::SYMBOLS;
    let list = SYMBOLS.map(|s| format!("\"{s}\"")).join(", ");
    params(&format!(
        "venue = \"fixture\"\ninterval = \"1h\"\nsymbols = [{list}]\nsignificance = \"one_percent\"\n{extra}"
    ))
}

fn pairs_context() -> StudyContext {
    let (store, window) = user_pairs_reversion::fixture_store::store();
    StudyContext::new(store, window, scratch())
}

/// `pairs_reversion` on the synthetic market: the screen finds one of the two planted pairs inside a cluster, the
/// pair stays cointegrated on the bars it never saw, the model learns that the spread reverts (a negative
/// coefficient on the z-score, predictions that agree with what the spread did) and every number is the same
/// across the study file's boundary.
#[test]
fn pairs_reversion_finds_a_planted_pair_and_learns_its_reversion() {
    let recipe = pairs_recipe("");
    let out = user_pairs_reversion::run(&pairs_context(), &recipe).expect("the study runs");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no metric {name}"));
    // 1000 bars, three of one symbol missing: the alignment keeps the 997 every symbol has.
    assert_eq!(metric("bars"), 997.0);
    assert_eq!(metric("train_bars") + metric("test_bars"), 997.0);
    assert!(metric("pairs_found") >= 1.0, "{:?}", out.metrics());
    let pair = (metric("pair_first") as usize, metric("pair_second") as usize);
    assert!(matches!(pair, (0, 1) | (1, 0) | (2, 3) | (3, 2)), "{pair:?}");
    assert!(metric("engle_granger_statistic") < metric("engle_granger_critical_five_percent"));
    assert_eq!(metric("johansen_rank"), 1.0, "{:?}", out.metrics());
    assert!(metric("kalman_hedge_ratio_end_of_training").is_finite());
    assert!(metric("kalman_hedge_ratio_end").is_finite());
    // The pair keeps reverting on the later window, which the screen never saw.
    assert!(metric("test_reverting_share") > 0.5, "{:?}", out.metrics());
    assert!(metric("test_adf_statistic") < -2.0, "{:?}", out.metrics());
    // The model: a spread above its mean is expected to fall.
    assert!(metric("zscore_coefficient") < 0.0, "{:?}", out.metrics());
    assert!(metric("prediction_correlation") > 0.1, "{:?}", out.metrics());
    assert!(metric("side_hit_rate") > 0.5, "{:?}", out.metrics());
    assert!(metric("ml_trades") > 0.0 && metric("baseline_trades") > 0.0);
    for name in ["ml_total", "ml_sharpe", "baseline_total", "baseline_sharpe"] {
        assert!(metric(name).is_finite(), "{name}: {:?}", out.metrics());
    }
    let r_squared = metric("factor_mean_r_squared");
    assert!(r_squared > 0.0 && r_squared < 1.0, "{r_squared}");
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    for wanted in ["pairs.tsv", "model.txt", "chart_spread.tsv", "chart_profit.tsv"] {
        assert!(names.contains(&wanted), "{names:?}");
    }
    let artifact = |name: &str| &out.artifacts().iter().find(|(n, _)| n == name).unwrap().1;
    assert!(artifact("chart_profit.tsv").starts_with("time\tvalue\tseries\tkind\n"));
    assert!(artifact("model.txt").starts_with("vike-ml linear v1"), "{}", artifact("model.txt"));
    eprintln!("pairs_reversion: {:?}", out.metrics());
}

/// Both clusterings, a refusal and a universe with no planted pair: the same both ways.
#[test]
fn pairs_reversion_answers_the_same_across_the_study_file_boundary() {
    for extra in ["", "clustering = \"k_means\"\n", "window = 24\nhorizon = 2\ncommission = 0.05\n"]
    {
        let said = both_ways(user_pairs_reversion::run, pairs_context, &pairs_recipe(extra));
        assert!(said.starts_with("ok"), "[{extra}] {said}");
    }
    let said =
        both_ways(user_pairs_reversion::run, pairs_context, &pairs_recipe("cost = 0.0005\n"));
    assert!(said.starts_with("refused"), "the retired cost key: {said}");
    // No `symbols`, too few of them, and an unknown clustering are the study's refusals.
    for recipe in [
        "venue = \"fixture\"\ninterval = \"1h\"",
        "venue = \"fixture\"\ninterval = \"1h\"\nsymbols = [\"ALPHA\", \"BRAVO\"]",
        "venue = \"fixture\"\ninterval = \"1h\"\nsymbols = [\"ALPHA\", \"BRAVO\", \"ECHO\"]\nclustering = \"dbscan\"",
        "venue = \"fixture\"\ninterval = \"1h\"\nsymbols = [\"ALPHA\", \"BRAVO\", \"NOWHERE\"]",
    ] {
        let said = both_ways(user_pairs_reversion::run, pairs_context, &params(recipe));
        assert!(said.starts_with("refused"), "[{recipe}] {said}");
    }
}

fn carry_recipe(extra: &str) -> toml::Value {
    use user_funding_carry::fixture_store::{PERPETUAL, SPOT, VENUE};
    params(&format!(
        "venue = \"{VENUE}\"\nspot_symbol = \"{SPOT}\"\nperpetual_symbol = \"{PERPETUAL}\"\ninterval = \"1h\"\n{extra}"
    ))
}

fn carry_context() -> StudyContext {
    let (store, window) = user_funding_carry::fixture_store::store();
    StudyContext::new(store, window, scratch())
}

/// `funding_carry` on the synthetic perpetual: the funding is positive and paid, the carry is held part of the
/// time and pays the recipe's commission (the backtest's key), the profit is the three parts, and the basis model
/// learns that the basis reverts.
#[test]
fn funding_carry_splits_the_profit_and_learns_the_basis() {
    let out = user_funding_carry::run(&carry_context(), &carry_recipe("commission = 0.02\n"))
        .expect("the study runs");
    let metric = |name: &str| out.metric_value(name).unwrap_or_else(|| panic!("no metric {name}"));
    assert_eq!(metric("bars"), 1200.0);
    assert_eq!(metric("settlements"), 149.0, "one every eight bars from bar 8 to bar 1192");
    assert!(metric("mean_funding") > 0.0 && metric("mean_basis") > 0.0, "{:?}", out.metrics());
    // The settlements are eight hours apart: three a day, over the platform's 365-day year.
    let per_year = 365.0 * 3.0;
    assert!((metric("annualised_funding") - metric("mean_funding") * per_year).abs() < 1e-9);
    let share = metric("carry_share_in_market");
    assert!(share > 0.0 && share < 1.0, "{share}");
    assert!(metric("carry_funding") > 0.0 && metric("carry_cost") > 0.0);
    let parts = metric("carry_funding") + metric("carry_basis") - metric("carry_cost");
    assert!((metric("carry_net") - parts).abs() < 1e-9, "{:?}", out.metrics());
    assert!(metric("zscore_coefficient") < 0.0, "{:?}", out.metrics());
    assert!(metric("correlation_with_funding").is_finite());
    assert!(metric("correlation_without_funding") > 0.1, "{:?}", out.metrics());
    assert!(metric("funding_helps").is_finite());
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["chart_carry.tsv", "model.txt"]);
    eprintln!("funding_carry: {:?}", out.metrics());
}

#[test]
fn funding_carry_answers_the_same_across_the_study_file_boundary() {
    for extra in [
        "",
        "entry = 0.0002\nexit = 0.0001\ncommission = 0.0\n",
        "window = 24\nslippage = 2\nslippage_unit = \"ticks\"\nslippage_tick = 0.01\n",
    ] {
        let said = both_ways(user_funding_carry::run, carry_context, &carry_recipe(extra));
        assert!(said.starts_with("ok"), "[{extra}] {said}");
    }
    for recipe in [
        "venue = \"fixture\"\nspot_symbol = \"COIN\"\ninterval = \"1h\"",
        "venue = \"fixture\"\nspot_symbol = \"COIN\"\nperpetual_symbol = \"NOWHERE\"\ninterval = \"1h\"",
        "venue = \"fixture\"\nspot_symbol = \"COIN\"\nperpetual_symbol = \"COIN_PERP\"\ninterval = \"fortnight\"",
    ] {
        let said = both_ways(user_funding_carry::run, carry_context, &params(recipe));
        assert!(said.starts_with("refused"), "[{recipe}] {said}");
    }
    // The study's own cost key is retired, never read as none; a bad cost key is the backtest grammar's refusal.
    for extra in ["cost = 0.0004\n", "commission = -1\n"] {
        let said = both_ways(user_funding_carry::run, carry_context, &carry_recipe(extra));
        assert!(said.starts_with("refused"), "[{extra}] {said}");
    }
}

const HOUR_MS: i64 = 3_600_000;
const OPTION_VENUE: &str = "deribit";
const OPTION_CHAIN: &str = "U";
const FUTURE: &str = "U-PERP";

/// The options studies' store: `hours` hourly bars of a future whose close zigzags around 100, and from
/// the second hour a chain snapshot at each hour's last millisecond (calls and puts at 90, 100 and 110,
/// a week out), marked by Black–Scholes on a SMILE: the IV grows with the strike's distance from the
/// price, so the flat at-the-money price misses the wings.
fn options_store(hours: i64) -> (Arc<dyn HistoryStore + Send + Sync>, TimestampRange) {
    use vike_marketdata::OptionKind;
    let store = HistoryStoreMemory::new();
    let close = |h: i64| 100.0 + ((h * 7) % 11) as f64 - 5.0;
    let bars: Vec<Bar> = (0..hours)
        .map(|h| {
            vike_marketdata::test_support::flat_bar_unit_volume_on(FUTURE, h * HOUR_MS, close(h))
        })
        .collect();
    store.append_bars(OPTION_VENUE, FUTURE, "1h", &bars, Some("fixture")).unwrap();
    for h in 1..hours {
        let (ts, spot) = (h * HOUR_MS + HOUR_MS - 1, close(h - 1));
        let expiry_ms = ts + 7 * 24 * HOUR_MS;
        let years = vike_options::years_to_expiry_ms(expiry_ms, ts);
        let mut rows = Vec::new();
        for strike in [90.0, 100.0, 110.0] {
            for kind in [OptionKind::Call, OptionKind::Put] {
                let iv = 0.5 + 2.0 * (strike / spot - 1.0) * (strike / spot - 1.0);
                let mark = vike_options::black_scholes_price(spot, strike, years, iv, kind, 0.0);
                rows.push(vike_data::ChainRow {
                    ts,
                    underlying: OPTION_CHAIN.into(),
                    instrument: format!("{OPTION_CHAIN}-{strike}-{kind:?}"),
                    expiry_ms,
                    strike,
                    kind,
                    bid: None,
                    ask: None,
                    mark,
                    iv: Some(iv),
                    open_interest: None,
                    volume: None,
                    delta: None,
                    gamma: None,
                    theta: None,
                    vega: None,
                });
            }
        }
        store
            .append_chain_snapshot(OPTION_VENUE, OPTION_CHAIN, &rows, Some(&format!("chain-{h}")))
            .unwrap();
    }
    (Arc::new(store), TimestampRange::all())
}

/// The pricing network study (a scripted learner stands in for the network): eight snapshots of six
/// options, the first four train and the last four test; the flat Black–Scholes price misses only the
/// smile's wings; the same answer both ways, and no learner is the typed refusal.
#[test]
fn the_pricing_network_study_samples_the_chain_point_in_time_and_answers_both_ways() {
    let recipe = params(
        "venue = \"deribit\"\nchain = \"U\"\nunderlying_venue = \"deribit\"\nunderlying = \"U-PERP\"\n\
         interval = \"1h\"\ntrain_fraction = 0.5\n",
    );
    let with = || {
        let (store, window) = options_store(9);
        StudyContext::new(store, window, scratch())
            .with_learner(Arc::new(ScriptedLearner::constant(0.05)))
    };
    let out = user_option_pricing_network::run(&with(), &recipe).expect("the study runs");
    assert_eq!(out.metric_value("rows_train"), Some(24.0));
    assert_eq!(out.metric_value("rows_test"), Some(24.0));
    let flat = out.metric_value("rmse_black_scholes").expect("a metric");
    assert!(flat > 0.0 && flat < 0.01, "the flat price misses the wings, a little: {flat}");
    assert!(out.metric_value("rmse_network").expect("a metric") > flat, "a constant is no pricer");
    let chart = &out.artifacts()[0];
    assert_eq!(chart.0, "chart_pricing_error.tsv");
    assert_eq!(
        chart.1.lines().count(),
        1 + 2 * 4,
        "a line per test snapshot and pricer: {}",
        chart.1
    );
    assert!(both_ways(user_option_pricing_network::run, with, &recipe).starts_with("ok"));
    let without = || {
        let (store, window) = options_store(9);
        StudyContext::new(store, window, scratch())
    };
    assert!(both_ways(user_option_pricing_network::run, without, &recipe).starts_with("refused"));
}

/// The deep hedging study (a scripted learner stands in for the network, holding half a unit): twenty
/// bars train sixteen paths of five, twenty judge four episodes three ways, the same both ways; a
/// recipe whose price column is not the environment's is refused, and so is one whose learner is
/// trained on another fee than the engine charges.
#[test]
fn the_deep_hedging_study_judges_three_hedges_and_answers_both_ways() {
    let recipe = "venue = \"deribit\"\nsymbol = \"U-PERP\"\ninterval = \"1h\"\nvolatility = 0.5\n\
                  train_fraction = 0.5\ncommission = 0.1\n[learner]\nkind = \"neural\"\npath_steps = 5\n\
                  fee_rate = 0.001\n";
    let with = || {
        let (store, window) = options_store(40);
        StudyContext::new(store, window, scratch())
            .with_learner(Arc::new(ScriptedLearner::constant(0.5)))
    };
    let good = params(&format!("{recipe}price_column = 0\n"));
    let out = user_deep_hedging::run(&with(), &good).expect("the study runs");
    assert_eq!(out.metric_value("paths_train"), Some(16.0));
    assert_eq!(out.metric_value("episodes"), Some(4.0));
    for policy in ["none", "delta", "network"] {
        for stat in ["mean", "std", "q05"] {
            let value = out.metric_value(&format!("{policy}_pnl_{stat}")).expect("a metric");
            assert!(value.is_finite(), "{policy}_pnl_{stat} = {value}");
        }
    }
    assert_eq!(out.artifacts()[0].1.lines().count(), 1 + 3 * 4, "a point per episode and hedge");
    assert!(both_ways(user_deep_hedging::run, with, &good).starts_with("ok"));
    eprintln!("deep_hedging: {:?}", out.metrics());
    let wrong = params(&format!("{recipe}price_column = 2\n"));
    assert!(user_deep_hedging::run(&with(), &wrong).is_err(), "the environment's column or none");
    for (costs, why) in [
        ("commission = 0.05", "a learner trained on another fee than the engine charges"),
        ("commission = 0.1\nslippage = 0.01", "a slippage the hedging loss charges none of"),
        ("commission = 0.1\ncommission_unit = \"per_order\"", "a commission no fraction states"),
    ] {
        let other =
            params(&format!("{}price_column = 0\n", recipe.replace("commission = 0.1", costs)));
        assert!(user_deep_hedging::run(&with(), &other).is_err(), "{why}");
    }
}
