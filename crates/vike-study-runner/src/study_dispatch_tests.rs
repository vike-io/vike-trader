use super::*;
use crate::listing::list_runs;
use std::assert_matches;
use std::sync::Arc;
use vike_data::HistoryStore;
use vike_data::HistoryStoreMemory;
use vike_marketdata::Bar;
use vike_runs::{MANIFEST_FILE, REPORT_FILE};
use vike_studio_core::STUDY_RUN_KIND;
use vike_user_research::StudyContext;

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
            .join(vike_paths::state_path::RHAI_SUBDIR)
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
            params: empty_recipe(),
            config: RunConfig { path: None, name: None },
            store: seeded_store(),
            window: TimestampRange::of(0, 60_000 * 4),
            scratch: self.scratch.clone(),
            runs_root: self.runs_root.clone(),
            produced_by: "studio".to_string(),
            learner: None,
            sim: None,
            file_runner: None,
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

/// Two studies run back to back on one thread each report their OWN numbers: asserted on the
/// metric, not on the absence of an error (a stale study would be a wrong ANSWER under the second
/// study's name, which was the risk of the thread-local bridge this dispatch used until 2026-11-15).
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

/// A Rust study FILE on a host that starts no sandbox: refused by name before anything
/// runs, pointing at the server-side door, and no run directory is minted.
#[test]
fn a_rust_study_on_a_host_with_no_sandbox_is_refused_by_name() {
    let t = tree();
    let clock = fixed_clock();
    let dir = t.user_data.join("research/studies/rust/vol");
    let err = run_study_plan(t.plan("vol", dir, StudyTier::Rust), &clock)
        .expect_err("this host starts no study sandbox");
    assert_matches!(err, StudyRunError::NoSandbox { ref name } if name == "vol", "{err}");
    assert!(err.to_string().contains("vike-cli research study"), "{err}");
    assert!(!t.runs_root.exists(), "a refusal must not mint a run directory");
}

/// A Rust study FILE on a host that runs one: the host's runner is handed the study's FOLDER and the
/// context `run_study_with` built, and its outcome reaches the same persistence as any study's.
#[test]
fn a_rust_study_runs_through_its_hosts_runner_and_the_same_persistence() {
    let t = tree();
    let clock = fixed_clock();
    let dir = t.user_data.join("research/studies/rust/vol");
    let mut plan = t.plan("vol", dir.clone(), StudyTier::Rust);
    plan.file_runner = Some(Arc::new(
        move |folder: &Path,
              ctx: &StudyContext,
              _params: &toml::Value|
              -> Result<vike_user_research::StudyOutcome, vike_user_research::StudyError> {
            assert_eq!(folder, dir.as_path(), "the runner is handed the study's folder");
            let bars = ctx.bars(VENUE, SYMBOL, INTERVAL, ctx.window())?;
            let mut out = vike_user_research::StudyOutcome::new();
            out.metric("n_bars", bars.len() as f64)?;
            Ok(out)
        },
    ));
    let run = run_study_plan(plan, &clock).expect("the runner's outcome is persisted");
    assert_eq!(run.outcome.metric_value("n_bars"), Some(5.0));
    assert!(run.dir.join(MANIFEST_FILE).is_file());
    assert_eq!(run.manifest.kind, STUDY_RUN_KIND);
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

/// The `[learner]` text `toml` reads, as a recipe value.
fn learner(text: &str) -> toml::Value {
    toml::from_str(&format!("[learner]\n{text}")).expect("a recipe")
}

/// A `[learner]` FLOAT knob written as an integer is read as that number (`lambda_l1 = 1` is `1.0`), and
/// a value that is no number is REFUSED by name (B5, the owner's ruling): it once kept the default silently.
#[test]
fn learner_float_knobs_accept_an_integer_and_refuse_a_non_number() {
    let (p, from_recipe) =
        learner_params(&learner("lambda_l1 = 1\nlambda_l2 = 0.5\nbagging_fraction = 0\n"))
            .expect("numbers");
    assert!(from_recipe);
    assert_eq!(p.lambda_l1, 1.0, "an integer float-knob is widened, not dropped");
    assert_eq!(p.lambda_l2, 0.5);
    assert_eq!(p.bagging_fraction, 0.0, "default is 1.0, so the integer 0 was read");

    let why = learner_params(&learner("lambda_l1 = \"big\"\n")).expect_err("a non-number");
    assert!(why.starts_with("[learner]: ") && why.contains("lambda_l1"), "{why}");
}

/// The INTEGER knobs are STRICT and say so: a float there is refused by name (it was once absent and the
/// default stood), and so is a value that does not fit the knob.
#[test]
fn learner_integer_knobs_refuse_a_float_or_a_misfit() {
    let (p, _) = learner_params(&learner("num_iterations = 7\nbagging_freq = 3\n")).expect("ints");
    assert_eq!((p.iteration_count, p.bagging_freq), (7, 3));

    for (text, key) in [
        ("num_iterations = 7.0\n", "num_iterations"),
        ("bagging_freq = 3.0\n", "bagging_freq"),
        ("num_iterations = -1\n", "num_iterations"),
    ] {
        let why = learner_params(&learner(text)).expect_err(text);
        assert!(why.starts_with("[learner]: ") && why.contains(key), "{text}: {why}");
    }

    assert_eq!(
        learner_params(&empty_recipe()).map(|(_, from)| from),
        Ok(false),
        "no [learner] table"
    );
}

/// Every LightGBM parameter `GbdtParams::apply` knows is honoured from `[learner]`, the six the hand reader
/// once ignored among them, beside the learner's `kind`; a name it does not know is refused by name, and so is
/// a `learner` that is no table.
#[test]
fn every_learner_key_is_honoured_and_an_unknown_one_refused() {
    let (p, from_recipe) = learner_params(&learner(
        "kind = \"gbdt\"\nnum_leaves = 15\nmax_depth = 6\nmin_data_in_leaf = 40\nseed = 7\n\
         learning_rate = 0.05\nfeature_fraction = 0.8\n",
    ))
    .expect("every key is one apply knows");
    assert!(from_recipe);
    assert_eq!((p.num_leaves, p.max_depth, p.min_data_in_leaf, p.seed), (15, 6, 40, 7));
    assert_eq!((p.learning_rate, p.feature_fraction), (0.05, 0.8));

    let why = learner_params(&learner("learnig_rate = 0.05\n")).expect_err("a misspelt key");
    assert!(why.starts_with("[learner]: ") && why.contains("learnig_rate"), "{why}");

    let not_a_table: toml::Value = toml::from_str("learner = 5\n").expect("a recipe");
    assert!(learner_params(&not_a_table).is_err(), "a learner that is no table");
}

/// `kind = "neural"` hands the rest of `[learner]` to the network trainer verbatim; no `kind` or
/// `kind = "gbdt"` keeps the tree learner; any other kind is refused rather than guessed.
#[test]
fn a_neural_learner_table_becomes_the_trainers_spec() {
    let neural: toml::Value = toml::from_str(
        "[learner]\nkind = \"neural\"\nnetwork = \"lstm\"\nhidden = [8]\nsteps = 6\n",
    )
    .unwrap();
    let spec: toml::Value =
        toml::from_str(&neural_spec(&neural).unwrap().expect("a network")).unwrap();
    assert_eq!(spec.get("network").and_then(toml::Value::as_str), Some("lstm"));
    assert_eq!(spec.get("steps").and_then(toml::Value::as_integer), Some(6));
    assert!(spec.get("kind").is_none(), "`kind` is the host's word, not the trainer's");

    let tree: toml::Value = toml::from_str("[learner]\nkind = \"gbdt\"\nlambda_l1 = 1\n").unwrap();
    assert_eq!(neural_spec(&tree).unwrap(), None);
    assert_eq!(neural_spec(&empty_recipe()).unwrap(), None);
    let odd: toml::Value = toml::from_str("[learner]\nkind = \"forest\"\n").unwrap();
    assert!(neural_spec(&odd).unwrap_err().contains("forest"));
}
