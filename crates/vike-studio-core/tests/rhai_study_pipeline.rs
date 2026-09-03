//! The full-pipeline gate for the INTERPRETED study tier: drive the COMMITTED fixture tree
//! (`tests/fixture_user_data/`) end to end, on every CI run, on a box with no LightGBM binary and
//! no DataFusion.
//!
//! The compiled tier proves itself the same way and for the same reason
//! (`crates/vike-user-research/tests/pipeline.rs`): CI checkouts carry no `user_data/`, so a tier
//! whose only proof was the real tree would be a mechanism nothing ever executed. ⚠ The difference
//! is that the compiled tier needs a SECOND generated registry to reach its fixtures, because its
//! scan happens in `build.rs`; this tier has no build-time scan at all — a caller names a folder
//! and [`vike_studio_core::RhaiStudy::load`] reads it — so the fixture tree is reached by naming it,
//! and "no `user_data/` ⇒ inert" is structural rather than arranged.
//!
//! Both doubles are the workspace's OWN shared ones — `vike_data::test_support::MemHistStore` and
//! `vike_ml::test_support::ScriptedLearner` — so what these tests exercise is the contract against
//! the same objects a study meets in a real run.

use std::path::PathBuf;
use std::sync::Arc;

use vike_data::test_support::MemHistStore;
use vike_data::{HistStore, TsRange};
use vike_ml::test_support::ScriptedLearner;
use vike_ml::{FitImportance, GridPoint};
use vike_model::state_path::{RESEARCH_SUBDIR, RHAI_SUBDIR, STUDIES_SUBDIR};
use vike_model::Bar;
use vike_studio_core::rhai_study::RhaiStudy;
use vike_user_research::{StudyContext, StudyError, StudyLearner};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
const N_BARS: i64 = 5;

/// One fixture study's folder. ⚠ Every path component below the fixture root is spelled through
/// `vike_model::state_path`'s constants and never as a literal — those constants are the layout's
/// one authority (`user_studies_dir` and `crates/vike-studio-core/src/listing.rs`'s `list_studies`
/// resolve the same way), and a second spelling here would rot the first time the layout moves.
fn study_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixture_user_data")
        .join(RESEARCH_SUBDIR)
        .join(STUDIES_SUBDIR)
        .join(RHAI_SUBDIR)
        .join(name)
}

fn bar(ts: i64, close: f64) -> Bar {
    // `Bar` derives no `Default` — spell every field (the struct is small and stable).
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(SYMBOL.to_string()),
    }
}

/// A seeded store plus the window that covers it: five one-minute bars closing 100..=104.
fn seeded_store() -> (Arc<dyn HistStore + Send + Sync>, TsRange) {
    let store = MemHistStore::new();
    let bars: Vec<Bar> = (0..N_BARS).map(|i| bar(60_000 * i, 100.0 + i as f64)).collect();
    store
        .append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("fixture"))
        .expect("MemHistStore accepts the fixture bars");
    (Arc::new(store), TsRange::of(0, 60_000 * (N_BARS - 1)))
}

fn ctx_without_learner() -> StudyContext {
    let (store, window) = seeded_store();
    StudyContext::new(store, window, scratch())
}

/// A scratch path nothing here ever CREATES — the interpreted tier has no file verb at all, so a
/// study cannot write into it. ⚠ Uniquified anyway: `crates/vike-ops/tests/temp_path_gate.rs`
/// refuses a fixed name under the system temp directory, because on a box where CI and agents run
/// as different users whichever creates it first owns it permanently.
fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("vike-rhai-study-{}", std::process::id()))
}

fn ctx_with(learner: ScriptedLearner) -> StudyContext {
    ctx_without_learner().with_learner(Arc::new(learner) as Arc<dyn StudyLearner>)
}

/// A three-feature importance table — deliberately NOT the fixture matrix's width, because
/// `ScriptedLearner::with_importance` hands its table back unchecked and this proves the numbers
/// travel verbatim rather than being reconstructed from the data.
fn scripted_importance() -> FitImportance {
    FitImportance { n_features: 3, gain: vec![1.5, 0.0, 4.25], splits: vec![2, 0, 7] }
}

/// The committed recipe, loaded by the CALLER and handed in as `params` — the split the compiled
/// tier makes too. Reading it here is what proves a `.toml` beside the entry file is a recipe and
/// not something the runner touches.
fn baseline_params() -> toml::Value {
    let path = study_dir("fixture_bars").join("baseline.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // ⚠ `toml::from_str`, not `text.parse()`: `FromStr for toml::Value` parses a bare VALUE
    // expression, so a DOCUMENT fails on its first `=`. The workspace idiom is this one.
    toml::from_str(&text).expect("the committed recipe is valid TOML")
}

/// THE read-half gate: a committed script, a real store, both read arities, the column projection,
/// a per-row getter, a metric and a text artifact.
#[test]
fn the_committed_bars_fixture_runs_end_to_end_against_a_real_store() {
    let study = RhaiStudy::load(&study_dir("fixture_bars")).expect("the fixture compiles");
    assert_eq!(study.name(), "fixture_bars");

    let out = study.run(&ctx_without_learner(), &baseline_params()).expect("it runs");

    assert_eq!(out.metric_value("n_bars"), Some(N_BARS as f64));
    assert_eq!(out.metric_value("total_close"), Some(100.0 + 101.0 + 102.0 + 103.0 + 104.0));
    assert_eq!(out.metric_value("first_close"), Some(100.0));
    assert_eq!(out.metric_value("first_ts"), Some(0.0));
    // The store's codec erases `Bar::symbol`, so this row's is absent — and a study can TELL,
    // which is the whole point of projecting an absent optional as something distinguishable.
    assert_eq!(out.metric_value("first_symbol_is_unit"), Some(1.0));

    // The run's own window is the short arity's range...
    assert_eq!(out.metric_value("window_start"), Some(0.0));
    assert_eq!(out.metric_value("window_end"), Some(240_000.0));
    // ...and the explicit arity genuinely narrows: `[0, 120000]` covers three of the five bars.
    assert_eq!(out.metric_value("n_narrowed"), Some(3.0));

    let (name, body) = &out.artifacts()[0];
    assert_eq!(name, "bars.tsv");
    assert_eq!(body.lines().next(), Some("ts\tclose"));
    assert_eq!(body.lines().count(), 1 + N_BARS as usize, "a header and one line per bar");
}

/// THE learner-half gate: a script that cannot construct a `TrainData` or a `GridPoint` still
/// fits, predicts and exports — which is the argument for the narrowed fitting surface, tested
/// rather than asserted.
#[test]
fn the_committed_fit_fixture_reaches_a_real_learner_and_exports_what_it_can() {
    let study = RhaiStudy::load(&study_dir("fixture_fit")).expect("the fixture compiles");
    let ctx = ctx_with(ScriptedLearner::constant(0.75).with_importance(scripted_importance()));
    let params: toml::Value = toml::from_str("seed = 11").unwrap();

    let out = study.run(&ctx, &params).expect("it runs");

    assert_eq!(out.metric_value("can_fit"), Some(1.0));
    assert_eq!(out.metric_value("rows"), Some(3.0));
    assert_eq!(out.metric_value("p_first_row"), Some(0.75), "the fitted model was reached");

    // The importance table travels verbatim — three rows, the scripted gains and splits.
    assert_eq!(out.metric_value("importance_features"), Some(3.0));
    let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["importance.tsv"], "the double reports no model TEXT, so none is written");
    let tsv = &out.artifacts()[0].1;
    assert_eq!(tsv.lines().next(), Some("feature\tgain\tsplits"));
    assert!(tsv.contains("f2\t4.25\t7"), "the scripted numbers reach the artifact: {tsv}");
}

/// ⚠ The property the whole fitting surface exists to have: a hyperparameter TYPED IN A SCRIPT
/// reaches the learner as a real `GridPoint` axis. Without this the map could be parsed, validated,
/// refused-on-typo and still never connected — a knob that looks armed and is not.
///
/// `ScriptedLearner::by_point` makes the fitted model's answer a pure function of the point it was
/// fitted at, so the prediction IS the assertion.
#[test]
fn a_hyperparameter_typed_in_a_script_reaches_the_learner() {
    fn learning_rate(p: &GridPoint) -> f64 {
        p.learning_rate
    }
    let study = RhaiStudy::new(
        "axes",
        r#"fn run(ctx, params) {
             let out = outcome();
             let f = ctx.fit([1.0, 2.0, 3.0, 4.0], [0.0, 1.0], 2, #{ learning_rate: 0.375 });
             out.metric("echoed_learning_rate", f.predict([1.0, 2.0]));
             return out;
           }"#,
    )
    .unwrap();

    let ctx = ctx_with(ScriptedLearner::by_point(learning_rate));
    let out = study.run(&ctx, &toml::Value::Table(toml::map::Map::new())).unwrap();
    assert_eq!(out.metric_value("echoed_learning_rate"), Some(0.375));
}

/// The documented host ceiling, through the SAME committed fixture: no learner is a typed
/// `NoLearner` a UI can render as "run this somewhere else", not a sentence and not a number
/// computed without a model.
#[test]
fn the_fit_fixture_refuses_with_the_typed_ceiling_when_the_host_has_no_learner() {
    let study = RhaiStudy::load(&study_dir("fixture_fit")).unwrap();
    let params: toml::Value = toml::from_str("seed = 11").unwrap();

    let e = study.run(&ctx_without_learner(), &params).unwrap_err();

    match e {
        StudyError::NoLearner(what) => assert!(what.contains("2-column"), "{what}"),
        other => panic!("expected NoLearner, got {other}"),
    }
}

/// A learner that cannot report an artifact answers none EVEN WHEN ASKED, and the accessor raises
/// rather than handing back an empty value — an empty importance table reads as "no feature
/// mattered", and an empty `model.txt` is a file somebody would later try to load.
#[test]
fn an_unavailable_fit_artifact_raises_rather_than_answering_empty() {
    let study = RhaiStudy::new(
        "wants_text",
        r#"fn run(ctx, params) {
             let out = outcome();
             let f = ctx.fit([1.0, 2.0], [0.0], 2, #{ capture_text: true });
             out.artifact("model.txt", f.text());
             return out;
           }"#,
    )
    .unwrap();

    let e = study.run(&ctx_with(ScriptedLearner::constant(0.5)), &empty()).unwrap_err();
    assert!(e.to_string().contains("has_text"), "the refusal names the check to make: {e}");
}

/// ⚠ **THE enforcement gate.** `rhai::Engine::new()` installs a `FileModuleResolver`, so a DEFAULT
/// engine lets a script READ A FILE FROM DISK through `import` — a capability no registration list
/// would show and one `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` refuses.
/// This is the test that keeps `bind::build_engine`'s `set_module_resolver` line from being tidied
/// away by somebody who reads it as boilerplate.
///
/// ⚠ **It plants a module that a `FileModuleResolver` genuinely WOULD resolve, and the control
/// assertion proves it does.** The first shape of this test imported a name no file backed, which
/// fails under BOTH resolvers — a vacuous gate that would have gone on passing after somebody
/// deleted the line it exists to protect. The path is ABSOLUTE so the answer cannot depend on the
/// process's working directory, which several tests in one binary share.
#[test]
fn a_study_cannot_import_a_module_because_that_would_be_a_file_verb() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("helper.rhai"), "fn answer() { 42 }\n").unwrap();
    // Forward slashes: a Windows `\` is an ESCAPE inside a rhai string literal, and `Path::join`
    // takes `/` on every platform this ships to.
    let module = tmp.path().join("helper").display().to_string().replace('\\', "/");

    // The CONTROL: a default engine reads that file off disk and calls into it. If this ever stops
    // holding, the hazard is gone and this whole test should be re-argued — which is why it is an
    // assertion rather than a comment.
    let answer: i64 = rhai::Engine::new()
        .eval(&format!(r#"import "{module}" as m; m::answer()"#))
        .expect("a DEFAULT rhai engine resolves a module from the filesystem");
    assert_eq!(answer, 42, "the planted module is one a FileModuleResolver really does load");

    // ...and the study engine refuses the very same import, at the top level, which is where an
    // author would write it.
    let e = RhaiStudy::new(
        "importer",
        &format!(r#"import "{module}" as m; fn run(ctx, params) {{ return outcome(); }}"#),
    )
    .expect_err("an import must not resolve in a study");
    assert!(e.to_string().contains("importer"), "{e}");

    // ...and inside the entry function, which the compile-time top-level run never touches.
    let study = RhaiStudy::new(
        "late_importer",
        &format!(r#"fn run(ctx, params) {{ import "{module}" as m; return outcome(); }}"#),
    )
    .unwrap();
    let e = study.run(&ctx_without_learner(), &empty()).unwrap_err();
    assert!(matches!(e, StudyError::Study(_)), "got {e}");
}

/// No `user_data/` is every CI checkout and every fresh clone. This tier is inert there by
/// CONSTRUCTION — it has no build-time scan and no process-wide registry, so nothing is loaded
/// until a caller names a folder, and a folder that is not there is a NAMED refusal rather than a
/// panic or a silently empty study.
#[test]
fn an_absent_user_data_tree_leaves_this_tier_inert() {
    let tmp = tempfile::tempdir().unwrap();
    let absent = tmp
        .path()
        .join("user_data")
        .join(RESEARCH_SUBDIR)
        .join(STUDIES_SUBDIR)
        .join(RHAI_SUBDIR)
        .join("nothing_here");
    assert!(!absent.exists(), "precondition");

    let e = RhaiStudy::load(&absent).unwrap_err();
    assert!(matches!(e, StudyError::Study(ref m) if m.contains("nothing_here")), "{e}");
}

fn empty() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}
