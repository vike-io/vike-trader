use super::*;
use std::assert_matches;
use std::sync::Arc;
// ⚠ These two are TEST-ONLY now. `persist` checks the whole `RESERVED_FILES` roster and the
// refusal message joins it, so neither constant is named in the library any more — importing
// them at module level would be an `unused_imports` warning, which is `-D warnings` on the
// roster lane.
use vike_runs::{MANIFEST_FILE, REPORT_FILE};

use crate::listing::list_runs;
use serde_json::Value;
use vike_data::HistoryStore;
use vike_data::HistoryStoreMemory;
use vike_marketdata::Bar;
use vike_ml_training::seam::test_support::ScriptedLearner;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
/// A fixed instant, so a test asserts against an id rather than racing one.
const START_MS: i64 = 1_756_000_000_000;

/// The workspace's OWN store double, seeded — never a bespoke one (the `test-support` rule).
fn seeded_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..5)
        .map(|i| {
            vike_marketdata::test_support::flat_bar_unit_volume_on(
                SYMBOL,
                60_000 * i,
                100.0 + i as f64,
            )
        })
        .collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("fixture")).unwrap();
    Arc::new(store)
}

/// A study that reads the store through the context and reports what it found — the ordinary
/// shape, and the one that proves the context was built rather than merely passed.
fn reading_study(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or(SYMBOL);
    let bars = ctx.bars(VENUE, symbol, INTERVAL, ctx.window())?;
    let Some(last) = bars.last() else {
        return Err(StudyError::Study(format!("no bars for {symbol} in the window")));
    };
    let mut out = StudyOutcome::new();
    out.metric("bars", bars.len() as f64)?;
    out.metric("last_close", last.close)?;
    out.artifact("closes.tsv", "ts\tclose\n0\t100\n")?;
    // Whether the host handed a learner down is a fact about the RUN, and the manifest records
    // it separately; asserting it here proves the runner threaded it.
    out.metric("has_learner", if ctx.learner().is_some() { 1.0 } else { 0.0 })?;
    // The scratch directory must EXIST by the time a study is called — a study's first write
    // must not fail on a missing parent.
    out.metric("scratch_exists", if ctx.scratch().is_dir() { 1.0 } else { 0.0 })?;
    Ok(out)
}

/// A study whose numbers are honest gaps: a fold that never traded, and a degenerate slice.
fn nonfinite_study(
    _context: &StudyContext,
    _params: &toml::Value,
) -> Result<StudyOutcome, StudyError> {
    let mut out = StudyOutcome::new();
    out.metric("sharpe", f64::NAN)?;
    out.metric("best", f64::INFINITY)?;
    out.metric("worst", f64::NEG_INFINITY)?;
    out.metric("trades", 0.0)?;
    Ok(out)
}

/// A study that must fit and refuses when the host has no learner — the documented ceiling.
fn fitting_study(ctx: &StudyContext, _params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    if ctx.learner().is_none() {
        return Err(StudyError::NoLearner("the cohort model".to_string()));
    }
    let mut out = StudyOutcome::new();
    out.metric("fitted", 1.0)?;
    Ok(out)
}

/// A study naming an artifact that collides with the run directory's own documents.
fn colliding_study(
    _context: &StudyContext,
    _params: &toml::Value,
) -> Result<StudyOutcome, StudyError> {
    let mut out = StudyOutcome::new();
    out.metric("rows", 1.0)?;
    out.artifact(MANIFEST_FILE, "{}")?;
    Ok(out)
}

/// A study that keeps a NETWORK: `model.onnx` crosses as the base64 of the bytes (`AAECAw==` is
/// `[0, 1, 2, 3]`), as `vike_ml_training::NeuralLearner`'s captured text does.
fn network_study(
    _context: &StudyContext,
    params: &toml::Value,
) -> Result<StudyOutcome, StudyError> {
    let text = params.get("model").and_then(toml::Value::as_str).unwrap_or("AAECAw==");
    let mut out = StudyOutcome::new();
    out.artifact(vike_runs::MODEL_ONNX_ARTIFACT, text)?;
    Ok(out)
}

struct Fixture {
    _tmp: tempfile::TempDir,
    runs_root: PathBuf,
    scratch: PathBuf,
    store: Arc<dyn HistoryStore + Send + Sync>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("user_data").join("runs");
    let scratch = tmp.path().join("tmp").join("research");
    Fixture { _tmp: tmp, runs_root, scratch, store: seeded_store() }
}

impl Fixture {
    fn request<'a>(
        &'a self,
        study: &'a str,
        params: &'a toml::Value,
        clock: &'a dyn Clock,
    ) -> StudyRunRequest<'a> {
        StudyRunRequest {
            study,
            params,
            store: Arc::clone(&self.store),
            learner: None,
            sim: None,
            window: TimestampRange::of(0, 60_000 * 4),
            scratch: self.scratch.clone(),
            runs_root: &self.runs_root,
            produced_by: "studio",
            git_sha: Some("abc1234".to_string()),
            config: RunConfig {
                path: Some("research/studies/rust/vol/baseline.toml".to_string()),
                name: Some("baseline".to_string()),
            },
            clock,
        }
    }
}

fn fixed_clock() -> impl Clock {
    || START_MS
}

fn params(src: &str) -> toml::Value {
    toml::from_str(src).unwrap()
}

/// A backtest run written by hand exactly as `crates/vike-runs/src/lib.rs`'s `write_run`
/// lays one out — the neighbour a study run has to appear beside.
fn a_backtest_run(runs_root: &Path, run_id: &str) {
    let dir = runs_root.join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(REPORT_FILE), "{\"sharpe\":1.25}\n").unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"{{
  "run_id": "{run_id}",
  "kind": "backtest",
  "produced_by": "backtest",
  "started_at": "2026-08-24T09:15:04Z",
  "finished_at": "2026-08-24T09:15:16Z",
  "git_sha": null,
  "config": {{ "path": "profiles/sma.toml", "name": "sma cross" }},
  "detail": {{ "strategy": "sma_cross" }}
}}
"#
        ),
    )
    .unwrap();
}

/// R6, end to end: a study run lands in the SAME listing as a strategy backtest, rendered off
/// the common fields by a function that knows nothing about studies — and the two rows are told
/// apart by `kind`, which is the one top-level field a reader may branch on.
#[test]
fn a_study_run_lists_beside_a_backtest_run_and_is_told_apart_by_kind() {
    let fx = fixture();
    let clock = fixed_clock();
    a_backtest_run(&fx.runs_root, "1755000000-1-0");

    let p = params("");
    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    let listing = list_runs(&fx.runs_root);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
    let kinds: Vec<&str> = listing.runs.iter().map(|r| r.manifest.kind.as_str()).collect();
    assert_eq!(kinds, ["backtest", STUDY_RUN_KIND], "one list, two kinds, in id order");

    let listed = listing.runs.iter().find(|r| r.run_id == run.run_id).unwrap();
    assert_eq!(listed.manifest.produced_by, "studio");
    assert_eq!(listed.manifest.git_sha.as_deref(), Some("abc1234"));
    assert_eq!(listed.manifest.config.name.as_deref(), Some("baseline"));
    // Stamped from the clock the CALLER supplied, in `vike_runs`'s one spelling — asserted
    // through that function rather than against a hand-written string, which would be a second
    // authority for the format of a common field.
    assert_eq!(listed.manifest.started_at, utc_rfc3339(START_MS / 1_000));
    assert_eq!(listed.manifest.detail["study"], json!("vol"));
    assert_eq!(
        listed.report,
        Some(run.dir.join(REPORT_FILE)),
        "the run's report is offerable from the listing"
    );
}

/// The structural half of the distinction: a study's numbers NEST. A top-level `sharpe` would
/// be renderable in a column shared with a backtest's, and the two claims would silently become
/// one number — which is exactly what the design's ⚠ says the surface must not hide.
#[test]
fn a_study_run_metrics_nest_under_detail_and_never_sit_beside_the_common_fields() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    let text = std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let obj = v.as_object().unwrap();
    for key in ["bars", "last_close", "metrics", "sharpe"] {
        assert!(!obj.contains_key(key), "`{key}` must not sit beside the common fields");
    }
    assert_eq!(v["detail"]["metrics"][0]["name"], json!("bars"));
    assert_eq!(v["detail"]["metrics"][0]["value"], json!(5.0));
    assert!(
        v["detail"]["metrics_note"].as_str().unwrap().contains("backtest"),
        "a manifest read ALONE must still carry the distinction"
    );
}

/// The metric order is the study's own emission order — its headline first. An object would
/// have sorted it away, because this workspace pins `serde_json` without `preserve_order`.
#[test]
fn metrics_keep_the_order_the_study_emitted_them_in() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    let names: Vec<&str> = v["detail"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bars", "last_close", "has_learner", "scratch_exists"]);
}

/// A non-finite metric is an OBSERVATION and reaches disk as one. `serde_json` would have
/// written all three as a bare `null`, indistinguishable from each other and from "no value" —
/// the `unwrap_or(0.0)` of the serialization layer.
#[test]
fn a_non_finite_metric_is_recorded_rather_than_rounded_or_flattened() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let run = run_study_with(&nonfinite_study, fx.request("degenerate", &p, &clock)).unwrap();

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    let m = v["detail"]["metrics"].as_array().unwrap();
    assert_eq!(m[0], json!({ "name": "sharpe", "value": null, "nonfinite": "NaN" }));
    assert_eq!(m[1], json!({ "name": "best", "value": null, "nonfinite": "inf" }));
    assert_eq!(m[2], json!({ "name": "worst", "value": null, "nonfinite": "-inf" }));
    assert_eq!(m[3], json!({ "name": "trades", "value": 0.0, "nonfinite": null }));
    // ...and the outcome handed back is untouched: no rounding happened anywhere on the path.
    assert!(run.outcome.metric_value("sharpe").unwrap().is_nan());
}

/// Artifacts land as FILES beside the manifest, under the names the study chose.
#[test]
fn an_artifact_lands_as_a_file_in_the_run_directory() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    assert_eq!(std::fs::read_to_string(run.dir.join("closes.tsv")).unwrap(), "ts\tclose\n0\t100\n");
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap())
            .unwrap();
    assert_eq!(v["detail"]["artifacts"], json!(["closes.tsv"]));
}

/// A network's `model.onnx` is written as the BYTES its base64 text spells (decision 0292), and text
/// that is not base64 is refused before a run directory exists.
#[test]
fn a_networks_model_onnx_is_written_as_its_bytes() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");
    let run = run_study_with(&network_study, fx.request("net", &p, &clock)).unwrap();
    assert_eq!(std::fs::read(run.dir.join(vike_runs::MODEL_ONNX_ARTIFACT)).unwrap(), [0, 1, 2, 3]);

    let p = params("model = \"not base64!\"\n");
    let err = run_study_with(&network_study, fx.request("net", &p, &clock)).expect_err("refused");
    assert_matches!(
        err,
        StudyRunError::NotPersisted { why: StudyPersistError::InvalidArtifact { .. }, .. }
    );
    assert_eq!(list_runs(&fx.runs_root).runs.len(), 1, "the refused run minted no directory");
}

/// The reserved names are the WRITER's business — `contract.rs` says so and cannot enforce it,
/// because `MANIFEST_FILE` lives at a layer that crate may not name. Refused BEFORE anything is
/// minted, so a colliding study costs no run id and leaves no directory behind.
#[test]
fn an_artifact_named_like_the_manifest_is_refused_and_nothing_is_written() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let err = run_study_with(&colliding_study, fx.request("vol", &p, &clock)).expect_err("refused");

    match &err {
        StudyRunError::NotPersisted { outcome, why } => {
            assert_matches!(
                why, StudyPersistError::ReservedArtifact { name } if name == MANIFEST_FILE
            );
            assert_eq!(outcome.metric_value("rows"), Some(1.0), "the result is not lost");
        }
        other => panic!("expected NotPersisted, got {other:?}"),
    }
    assert!(err.to_string().contains(MANIFEST_FILE), "the message names the collision: {err}");
    assert!(list_runs(&fx.runs_root).runs.is_empty(), "no run directory was minted");
    assert!(
        list_runs(&fx.runs_root).diagnostics.is_empty(),
        "and nothing half-written was left for a listing to report"
    );
}

/// ⚠ **The refusal is over the WHOLE roster, and the roster GREW.** It held two names when this
/// check was written and holds six now — `META_FILE` joined it when tagging shipped, because a
/// run directory is a namespace several producers write into and a study artifact called
/// `meta.json` would silently overwrite a user's tags.
///
/// ⚠ **What this covers and what it does not.** The END-TO-END refusal is proven by the test
/// directly above, which drives a real study whose artifact is named `MANIFEST_FILE`;
/// a study entry is a function of the context and the recipe, so that case cannot be parameterised
/// over a name without one free function per entry. What is asserted here instead is the pair that actually changed
/// and that a single-name case cannot see: every roster entry is a name an artifact may take at
/// all (so `persist`'s `RESERVED_FILES.contains` is reachable for each), and the rendered
/// message names the WHOLE roster rather than a hand-typed prefix of it.
#[test]
fn every_reserved_document_name_is_refused_and_the_message_names_them_all() {
    let meta = vike_runs::META_FILE;
    assert!(
        RESERVED_FILES.contains(&meta),
        "the tag sidecar must be reserved: a study artifact called {meta} would silently \
             overwrite a user's tags"
    );
    for reserved in RESERVED_FILES {
        // The name has to survive `StudyOutcome::artifact`'s own validation, or `persist`'s
        // reserved check would be unreachable for it and the collision would land as an
        // InvalidArtifact instead — a different message about a different problem.
        assert!(
            valid_artifact_name(reserved).is_ok(),
            "`{reserved}` is reserved but is not even a legal artifact name, so the reserved \
                 refusal can never fire for it"
        );
        let rendered =
            StudyPersistError::ReservedArtifact { name: (*reserved).to_string() }.to_string();
        assert!(rendered.contains(reserved), "the refusal names the collision: {rendered}");
        for other in RESERVED_FILES {
            assert!(
                rendered.contains(other),
                "the refusal for `{reserved}` does not name `{other}` — this message hand-typed \
                     two of the six once, and went four names stale: {rendered}"
            );
        }
    }
}

/// A study's own refusal comes back TYPED, and no directory is minted — a listing must not show
/// a run that produced nothing.
#[test]
fn a_study_that_refuses_leaves_no_run_directory() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params(r#"symbol = "ETHUSDT""#);

    let err = run_study_with(&reading_study, fx.request("vol", &p, &clock)).expect_err("no bars");

    match &err {
        StudyRunError::Study { name, why: StudyError::Study(m) } => {
            assert_eq!(name, "vol");
            assert!(m.contains("ETHUSDT"), "{m}");
        }
        other => panic!("expected a study refusal, got {other:?}"),
    }
    assert!(list_runs(&fx.runs_root).runs.is_empty());
    assert!(list_runs(&fx.runs_root).diagnostics.is_empty());
}

/// The learner reaches the study through the erasure, and the manifest records WHETHER one was
/// there — the fact that decides whether a fitting study could have run at all.
#[test]
fn the_learner_reaches_the_study_and_the_manifest_records_that_it_did() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");
    let mut req = fx.request("vol", &p, &clock);
    req.learner = Some(Arc::new(ScriptedLearner::constant(0.75)));

    let run = run_study_with(&reading_study, req).unwrap();

    assert_eq!(run.outcome.metric_value("has_learner"), Some(1.0));
    assert_eq!(run.manifest.detail["learner"], json!(true));
}

/// …and its absence is the documented ceiling of a host with no LightGBM binary: a TYPED
/// refusal a caller can act on, carried through unflattened.
#[test]
fn a_host_with_no_learner_surfaces_the_typed_refusal_rather_than_a_sentence() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let err =
        run_study_with(&fitting_study, fx.request("cohort", &p, &clock)).expect_err("no learner");

    match &err {
        StudyRunError::Study { why: StudyError::NoLearner(what), .. } => {
            assert_eq!(what, "the cohort model");
        }
        other => panic!("expected NoLearner, got {other:?}"),
    }
    assert!(err.to_string().contains("LightGBM"), "{err}");
}

/// The scratch directory is CREATED before the study is called (the study asserts it saw one),
/// and is deliberately still there afterwards: it belongs to whoever chose the path.
#[test]
fn the_scratch_directory_exists_when_the_study_runs_and_survives_the_run() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");
    assert!(!fx.scratch.exists(), "precondition");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    assert_eq!(run.outcome.metric_value("scratch_exists"), Some(1.0));
    assert!(fx.scratch.is_dir(), "the caller's directory is not deleted underneath it");
}

/// The recipe is recorded verbatim in the run's own document, so a run says which CONFIGURATION
/// produced it and not only which file was named on the command line.
#[test]
fn the_report_records_the_params_the_run_was_driven_by() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("period = 14\nsymbol = \"BTCUSDT\"\n");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(run.dir.join(REPORT_FILE)).unwrap()).unwrap();
    assert_eq!(v["params"]["period"], json!(14));
    assert_eq!(v["params"]["symbol"], json!("BTCUSDT"));
    assert_eq!(v["window"], json!({ "start": 0, "end": 240_000 }));
    assert_eq!(v["study"], json!("vol"));
}

/// Saving a run is worth doing; it is not worth LOSING a run over. A runs root that cannot be
/// created hands the numbers back beside the reason they are not on disk.
#[test]
fn a_runs_root_that_cannot_be_created_still_hands_back_what_the_study_computed() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");
    // A FILE where the runs directory belongs — unwritable as a directory on every platform
    // this ships to, without a test changing permissions.
    std::fs::create_dir_all(fx.runs_root.parent().unwrap()).unwrap();
    std::fs::write(&fx.runs_root, "not a directory").unwrap();

    let err = run_study_with(&reading_study, fx.request("vol", &p, &clock)).expect_err("blocked");

    match &err {
        StudyRunError::NotPersisted { outcome, why } => {
            assert_matches!(why, StudyPersistError::Run(RunPersistError::Dir { .. }));
            assert_eq!(outcome.metric_value("bars"), Some(5.0), "the numbers survived");
            assert_eq!(outcome.artifacts().len(), 1);
        }
        other => panic!("expected NotPersisted, got {other:?}"),
    }
}

/// The id mint is `vike_runs`'s, not a second one: two runs in the SAME clock second get
/// different directories, which a bare-seconds id (the shape the research producer used) does
/// not.
#[test]
fn two_study_runs_in_one_clock_second_get_different_run_directories() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let a = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();
    let b = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    assert_ne!(a.run_id, b.run_id);
    assert!(a.run_id.starts_with("1756000000-"), "{}", a.run_id);
    assert_eq!(list_runs(&fx.runs_root).runs.len(), 2);
}

/// A run that FINISHED writing holds all three kinds of file, and the listing agrees it is
/// finished. The manifest is the completion marker (`vike_runs` writes it LAST), so an artifact
/// written after it would make a half-written run look complete — this pins the state that
/// ordering produces rather than the ordering itself, which no post-hoc reader can observe.
#[test]
fn a_finished_run_holds_every_artifact_beside_its_report_and_manifest() {
    let fx = fixture();
    let clock = fixed_clock();
    let p = params("");

    let run = run_study_with(&reading_study, fx.request("vol", &p, &clock)).unwrap();

    assert!(run.dir.join(MANIFEST_FILE).is_file(), "the marker is there");
    for (name, _) in run.outcome.artifacts() {
        assert!(run.dir.join(name).is_file(), "{name} must exist once the marker does");
    }
    assert!(run.dir.join(REPORT_FILE).is_file());
    let listing = list_runs(&fx.runs_root);
    assert_eq!(listing.runs.len(), 1);
    assert!(listing.diagnostics.is_empty(), "not unfinished: {:?}", listing.diagnostics);
}

/// Book W21: a study reads a saved run's forecasts through its context (`StudyContext::runs`), by run
/// id, from the runs root this run is written into; a finished run that published none answers none,
/// and an id that names no run is the host's refusal in words.
#[test]
fn a_study_reads_a_saved_runs_forecasts_by_run_id() {
    fn pooling_study(ctx: &StudyContext, _: &toml::Value) -> Result<StudyOutcome, StudyError> {
        let runs = ctx.runs().ok_or_else(|| StudyError::Study("no saved runs".into()))?;
        let forecasts = runs.forecasts("1755000000-1-0").map_err(StudyError::Study)?;
        let quiet = runs.forecasts("1755000001-1-0").map_err(StudyError::Study)?;
        let missing = runs.forecasts("1755000002-1-0").expect_err("no such run");
        let mut out = StudyOutcome::new();
        out.metric("forecasts", forecasts.len() as f64)?;
        out.metric("first_value", forecasts[0].forecast.value())?;
        out.metric("quiet", quiet.len() as f64)?;
        out.metric("missing_said_so", if missing.contains("1755000002-1-0") { 1.0 } else { 0.0 })?;
        Ok(out)
    }
    let fx = fixture();
    let clock = fixed_clock();
    a_backtest_run(&fx.runs_root, "1755000000-1-0");
    a_backtest_run(&fx.runs_root, "1755000001-1-0");
    let row = |t: i64, v: f64| vike_model::ForecastRow {
        timestamp: t,
        symbol: SYMBOL.to_string(),
        forecast: vike_model::Forecast::new(v, 60_000, None).unwrap(),
    };
    let document = vike_runs::RunForecasts {
        schema: vike_runs::FORECASTS_SCHEMA,
        forecasts: vec![row(0, 0.25), row(60_000, -0.5)],
        source_len: 2,
    };
    let path = fx.runs_root.join("1755000000-1-0").join(vike_runs::FORECASTS_FILE);
    vike_runs::write_json(&path, vike_runs::FORECASTS_FILE, &document).unwrap();

    let p = params("");
    let run = run_study_with(&pooling_study, fx.request("pool", &p, &clock)).unwrap();
    let metric = |name: &str| run.outcome.metric_value(name);
    assert_eq!(metric("forecasts"), Some(2.0));
    assert_eq!(metric("first_value"), Some(0.25));
    assert_eq!(metric("quiet"), Some(0.0), "a finished run with no forecasts answers none");
    assert_eq!(metric("missing_said_so"), Some(1.0));
}

/// Book chapter 22: a study reads a saved run's closed trades by run id. A run that kept no ledger and a
/// ledger cut to a prefix are the host's refusals in words, never an empty or short list a forward
/// report would read as trades the backtest did not make.
#[test]
fn a_study_reads_a_saved_runs_trades_and_refuses_a_ledger_it_cannot_trust() {
    fn comparing_study(ctx: &StudyContext, _: &toml::Value) -> Result<StudyOutcome, StudyError> {
        let runs = ctx.runs().ok_or_else(|| StudyError::Study("no saved runs".into()))?;
        let trades = runs.trades("1755000000-1-0").map_err(StudyError::Study)?;
        let said = |id: &str, words: &str| {
            let why = runs.trades(id).expect_err("refused");
            if why.contains(id) && why.contains(words) { 1.0 } else { 0.0 }
        };
        let mut out = StudyOutcome::new();
        out.metric("trades", trades.len() as f64)?;
        out.metric("first_pnl", trades[0].pnl)?;
        out.metric("no_ledger_said_so", said("1755000001-1-0", vike_runs::TRADES_FILE))?;
        out.metric("prefix_said_so", said("1755000002-1-0", "first 1 of 3"))?;
        Ok(out)
    }
    let fx = fixture();
    let clock = fixed_clock();
    let trade = |pnl: f64| vike_model::Trade {
        entry_price: 100.0,
        exit_price: 100.0 + pnl,
        size: 1.0,
        pnl,
        fees: 0.0,
        entry_timestamp: 0,
        exit_timestamp: 60_000,
        symbol: String::new(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    };
    for (run_id, ledger) in [
        ("1755000000-1-0", Some((vec![trade(2.5), trade(-1.0)], 2))),
        ("1755000001-1-0", None),
        ("1755000002-1-0", Some((vec![trade(1.0)], 3))),
    ] {
        a_backtest_run(&fx.runs_root, run_id);
        if let Some((trades, source_len)) = ledger {
            let document =
                vike_runs::RunTrades { schema: vike_runs::TRADES_SCHEMA, trades, source_len };
            let path = fx.runs_root.join(run_id).join(vike_runs::TRADES_FILE);
            vike_runs::write_json(&path, vike_runs::TRADES_FILE, &document).unwrap();
        }
    }

    let p = params("");
    let run = run_study_with(&comparing_study, fx.request("compare", &p, &clock)).unwrap();
    let metric = |name: &str| run.outcome.metric_value(name);
    assert_eq!(metric("trades"), Some(2.0));
    assert_eq!(metric("first_pnl"), Some(2.5));
    assert_eq!(metric("no_ledger_said_so"), Some(1.0));
    assert_eq!(metric("prefix_said_so"), Some(1.0));
}

/// **A study reads a FORWARD run** (book chapter 22): what the trade daemon recorded at the mount and, once
/// written, its end; a backtest run is refused in words.
#[test]
fn a_study_reads_a_forward_run_and_refuses_a_run_of_another_kind() {
    fn reading_study(ctx: &StudyContext, p: &toml::Value) -> Result<StudyOutcome, StudyError> {
        let runs = ctx.runs().ok_or_else(|| StudyError::Study("no saved runs".into()))?;
        let id = p.get("run").and_then(|v| v.as_str()).unwrap_or_default();
        let forward = runs.forward(id).map_err(StudyError::Study)?;
        let refused = runs.forward("1755000000-1-0").expect_err("a backtest run");
        let mut out = StudyOutcome::new();
        out.metric("started_ms", forward.started_ms as f64)?;
        out.metric("ended_ms", forward.ended_ms.map_or(-1.0, |e| e as f64))?;
        out.metric(
            "refused_in_words",
            f64::from(u8::from(refused.contains("not a `forward` run"))),
        )?;
        Ok(out)
    }
    let fx = fixture();
    let clock = fixed_clock();
    a_backtest_run(&fx.runs_root, "1755000000-1-0");
    let recorded = vike_runs::ForwardRun {
        schema: vike_runs::FORWARD_RUN_SCHEMA,
        mount_id: "binance__BTCUSDT__1m".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        account: "binance/DEFAULT".to_string(),
        strategy_file: "trend/ema.rhai".to_string(),
        strategy_sha256: "ab".repeat(32),
        params: vike_model::params::Params::default(),
        reference_run: None,
        started_ms: 1_755_100_000_000,
    };
    let dir = vike_runs::write_forward_run(&fx.runs_root, &recorded, None).unwrap();
    let p = params(&format!("run = \"{}\"", dir.run_id));
    let metric = |name: &str, run: &StudyRun| run.outcome.metric_value(name);

    let open = run_study_with(&reading_study, fx.request("forward", &p, &clock)).unwrap();
    assert_eq!(metric("started_ms", &open), Some(1_755_100_000_000.0));
    assert_eq!(metric("ended_ms", &open), Some(-1.0), "still running");
    assert_eq!(metric("refused_in_words", &open), Some(1.0));

    vike_runs::end_forward_run(&dir.path, 1_755_200_000_000).unwrap();
    let ended = run_study_with(&reading_study, fx.request("forward", &p, &clock)).unwrap();
    assert_eq!(metric("ended_ms", &ended), Some(1_755_200_000_000.0));
}
