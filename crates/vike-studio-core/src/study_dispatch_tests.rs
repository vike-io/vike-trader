use super::*;
use crate::listing::list_runs;
use crate::spec::empty_params;
use crate::study_run::STUDY_RUN_KIND;
use std::sync::Arc;
use vike_data::HistStore;
use vike_data::test_support::MemHistStore;
use vike_model::Bar;
use vike_model::runs::{MANIFEST_FILE, REPORT_FILE};
use vike_user_research::StudyContext;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";
/// A fixed instant, so a test asserts against an id rather than racing one.
const START_MS: i64 = 1_756_000_000_000;

fn bar(ts: i64, close: f64) -> Bar {
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

/// The workspace's OWN store double, seeded — never a bespoke one (the `test-support` rule).
fn seeded_store() -> StoreHandle {
    let store = MemHistStore::new();
    let bars: Vec<Bar> = (0..5).map(|i| bar(60_000 * i, 100.0 + i as f64)).collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("fixture")).unwrap();
    Arc::new(store)
}

struct Tree {
    _tmp: tempfile::TempDir,
    user_data: PathBuf,
    runs_root: PathBuf,
    scratch: PathBuf,
}

/// A `user_data/` tree with a rhai study tier in it, ready to have studies written into.
fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let user_data = tmp.path().join("user_data");
    let runs_root = user_data.join("runs");
    let scratch = tmp.path().join("tmp").join("study");
    Tree { _tmp: tmp, user_data, runs_root, scratch }
}

impl Tree {
    /// Write a rhai study folder and return it, the way a user's own tree holds one:
    /// `research/studies/rhai/<name>/<name>.rhai`.
    fn rhai_study(&self, name: &str, src: &str) -> PathBuf {
        let dir = self
            .user_data
            .join("research")
            .join("studies")
            .join(vike_model::paths::state_path::RHAI_SUBDIR)
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.rhai")), src).unwrap();
        dir
    }

    fn plan(&self, name: &str, dir: PathBuf, tier: StudyTier) -> StudyRunPlan {
        StudyRunPlan {
            name: name.to_string(),
            dir,
            tier,
            params: empty_params(),
            config: RunConfig { path: None, name: None },
            store: seeded_store(),
            window: TsRange::of(0, 60_000 * 4),
            scratch: self.scratch.clone(),
            runs_root: self.runs_root.clone(),
            produced_by: "studio".to_string(),
            learner: None,
            git_sha: None,
        }
    }
}

fn fixed_clock() -> impl Clock {
    || START_MS
}

/// A study that reads the store through the context and reports what it found — the ordinary
/// shape, and the one that proves the context reached the interpreter rather than merely being
/// constructed.
const READING_STUDY: &str = r#"
fn run(ctx, params) {
    let bars = ctx.bars("binance", "BTCUSDT", "1m");
    let out = outcome();
    out.metric("n_bars", bars.len());
    out.metric("last_close", bars[bars.len() - 1].close);
    out.artifact("closes.tsv", "ts\tclose\n");
    return out;
}
"#;

/// **The claim this module exists for.** An INTERPRETED study — a value, not a function pointer
/// — reaches `study_run`'s persistence and leaves a run that is indistinguishable in shape from
/// what a compiled study would have left: `kind` is the shared one, the metrics NEST under
/// `detail.metrics`, the artifact is on disk beside the run's own two documents, and the whole
/// thing lists through `list_runs`.
#[test]
fn an_interpreted_study_reaches_the_same_persistence_a_compiled_one_would() {
    let t = tree();
    let dir = t.rhai_study("vol", READING_STUDY);
    let clock = fixed_clock();

    let run = run_study_plan(t.plan("vol", dir, StudyTier::Rhai), &clock)
        .expect("the interpreted tier must reach persistence");

    assert_eq!(run.manifest.kind, STUDY_RUN_KIND, "one kind for both tiers");
    assert_eq!(run.manifest.detail["study"], serde_json::json!("vol"));
    // The structural half of telling a study run from a backtest run: the numbers are in a
    // kind-specific subtree, never hoisted beside the common fields.
    assert!(
        run.manifest.detail["metrics"].is_array(),
        "metrics must nest under detail.metrics: {:?}",
        run.manifest.detail
    );
    // ...and not ALSO at the top level, which is the half that does the work: a hoisted
    // `sharpe` would be renderable in a column shared with a backtest's by a reader that never
    // asked which scorer produced it. Asserted over the manifest as it SERIALIZES, because the
    // top level is exactly the key set a listing reads.
    let flat = serde_json::to_value(&run.manifest).expect("the manifest serializes");
    let top: Vec<&str> = flat.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
    for name in ["metrics", "n_bars", "last_close", "sharpe"] {
        assert!(!top.contains(&name), "{name:?} must not sit at the manifest's top level: {top:?}");
    }
    assert!(run.dir.join(MANIFEST_FILE).is_file(), "the manifest is on disk");
    assert!(run.dir.join(REPORT_FILE).is_file(), "so is the report");
    assert!(run.dir.join("closes.tsv").is_file(), "...and the study's own artifact");

    let listing = list_runs(&t.runs_root);
    assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
    let kinds: Vec<&str> = listing.runs.iter().map(|r| r.manifest.kind.as_str()).collect();
    assert_eq!(kinds, [STUDY_RUN_KIND]);
}

/// **The bridge re-arms, and it re-arms with the RIGHT study.** Two studies run back to back on
/// one thread must each report their OWN numbers.
///
/// This is the assertion a thread-local costs: a slot that was set once and never updated, or
/// updated but read stale, produces a wrong ANSWER under the second study's name rather than a
/// failure — so it is asserted on the metric, not on the absence of an error.
#[test]
fn two_studies_run_back_to_back_each_report_their_own_numbers() {
    let t = tree();
    let clock = fixed_clock();
    let five = t.rhai_study(
        "five",
        "fn run(ctx, params) { let o = outcome(); o.metric(\"answer\", 5); return o; }",
    );
    let seven = t.rhai_study(
        "seven",
        "fn run(ctx, params) { let o = outcome(); o.metric(\"answer\", 7); return o; }",
    );

    let a = run_study_plan(t.plan("five", five, StudyTier::Rhai), &clock).unwrap();
    let b = run_study_plan(t.plan("seven", seven, StudyTier::Rhai), &clock).unwrap();

    assert_eq!(a.outcome.metric_value("answer"), Some(5.0));
    assert_eq!(b.outcome.metric_value("answer"), Some(7.0), "the slot must re-arm, not stick");
}

/// **The unarmed path is a NAMED bug report, not a panic and not a plausible number.**
///
/// Reachable only from inside this crate — nothing outside can call the entry directly — which
/// is exactly why it is worth pinning here: it is the branch a future refactor of
/// [`interpreted::run_persisted`] would silently start taking.
#[test]
fn the_bridge_refuses_by_name_when_it_is_reached_with_no_study_parked() {
    let ctx = StudyContext::new(seeded_store(), TsRange::all(), PathBuf::from("unused"));
    let err = interpreted::entry(&ctx, &empty_params()).expect_err("nothing is parked");
    assert!(
        format!("{err}").contains(interpreted::UNARMED),
        "the message must blame this module, not the study: {err}"
    );
}

/// A folder with no entry file is the study's own refusal, and it costs NO run directory — the
/// same rule `study_run.rs` applies to a refusing compiled study.
#[test]
fn a_study_folder_with_no_entry_file_refuses_and_leaves_nothing_behind() {
    let t = tree();
    let dir = t.rhai_study("vol", READING_STUDY);
    std::fs::remove_file(dir.join("vol.rhai")).unwrap();
    let clock = fixed_clock();

    let err = run_study_plan(t.plan("vol", dir, StudyTier::Rhai), &clock)
        .expect_err("a folder holding no entry file runs nothing");
    assert!(format!("{err}").contains("vol.rhai"), "the message must NAME the file: {err}");
    assert!(!t.runs_root.exists(), "a refusal must not mint a run directory");
}

/// The compiled tier through the same entry point, in the state every CI runner is in: a build
/// with no `user_data/` has an EMPTY registry, so any name answers `UnknownStudy` — a working
/// state, and the one a Studio on a shipped binary is in.
#[test]
fn the_compiled_tier_answers_unknown_study_when_the_registry_is_empty() {
    let t = tree();
    let clock = fixed_clock();
    let dir = t.user_data.join("research/studies/rust/vol");
    let err = run_study_plan(t.plan("vol", dir, StudyTier::Rust), &clock)
        .expect_err("no study of that name is compiled into this test binary");
    assert!(matches!(err, StudyRunError::UnknownStudy { .. }), "{err}");
}

/// Recipes are the CALLER's to load: listed in name order, filtered to `.toml`, and an absent
/// folder is empty rather than an error.
#[test]
fn recipes_lists_only_toml_in_name_order_and_an_absent_folder_is_empty() {
    let t = tree();
    let dir = t.rhai_study("vol", READING_STUDY);
    std::fs::write(dir.join("wide.toml"), "n = 2\n").unwrap();
    std::fs::write(dir.join("baseline.toml"), "n = 1\n").unwrap();
    std::fs::write(dir.join("notes.md"), "not a recipe\n").unwrap();

    let names: Vec<String> = recipes(&dir)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["baseline.toml", "wide.toml"]);
    assert!(recipes(&t.user_data.join("nope")).is_empty());
}

/// A recipe is parsed as a DOCUMENT — the `toml = "1.1"` trap [`read_recipe`] warns about —
/// and a broken one says which file and why.
#[test]
fn read_recipe_parses_a_multi_key_document_and_names_a_broken_one() {
    let t = tree();
    let dir = t.rhai_study("vol", READING_STUDY);
    let ok = dir.join("baseline.toml");
    std::fs::write(&ok, "venue = \"binance\"\nsymbol = \"BTCUSDT\"\n").unwrap();
    let parsed = read_recipe(&ok).expect("a two-key recipe is a document, not a value");
    assert_eq!(parsed.get("symbol").and_then(|v| v.as_str()), Some("BTCUSDT"));

    let bad = dir.join("broken.toml");
    std::fs::write(&bad, "venue = \n").unwrap();
    let err = read_recipe(&bad).expect_err("half a key/value pair is not TOML");
    assert!(err.contains("broken.toml"), "the message must name the file: {err}");
}
