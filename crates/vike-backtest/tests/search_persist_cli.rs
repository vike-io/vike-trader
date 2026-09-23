//! `backtest <sweep.toml>` — the ARTIFACT a parameter search leaves behind, `backtest trials <id>`
//! which reads it, and `--resume <id>` which continues it.
//!
//! Stage 5 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` (§6.4, §13b).
//! Before it, the sweep branch of `crates/vike-backtest/src/backtest_cli.rs`'s `run` returned
//! `ExitCode::SUCCESS` one statement before the clock read that mints a run id — so a grid, euler,
//! TPE or genetic search, the runs that cost hundreds of backtests and carry a seed and a budget,
//! wrote nothing at all. That was documented behaviour, not an oversight, which is why every test
//! here asserts a FILE or a stderr COUNT rather than an exit code: every search below exits 0 with
//! and without the feature.
//!
//! ⚠ **The store is EMPTY on purpose in all but one test**, exactly as
//! `crates/vike-backtest/tests/optimizer_cli.rs` leaves it: every method still runs its whole loop,
//! and the ledger still records one line per evaluation, which is the mechanics under test. The
//! exception is `a_resume_after_the_stores_contents_moved_is_refused_and_names_the_data`, which
//! runs `data seed-demo` BETWEEN a search and its resume precisely because the thing under test
//! there is the store's contents CHANGING — and an empty store that stays empty cannot exhibit it.
//!
//! ⚠ **No test here asserts that a point FAILS**, and that is a correction rather than caution.
//! `optimizer_cli.rs`'s doc says an unseeded store makes every point carry an `error`; MEASURED on
//! the the CI box lane, these points instead complete as zero-trade backtests and carry a report. Both
//! are legitimate answers from a store with nothing in it, and the ledger's contract is the same
//! under either — `a_search_leaves_a_parent_run_with_a_ledger` asserts EXACTLY ONE of
//! `metrics`/`error`, which is `ParamscanRow`'s own never-both rule carried into the document.
//!
//! ⚠ **The runs directory cannot be redirected with `VIKE_USER_DATA_DIR` alone here**, because the
//! walk that finds `<project>` must succeed first. So each test plants a `settings/` DIRECTORY (the
//! self-describing project marker `vike_model::state_path::project_settings_dir` answers with at
//! the nearest level) in a `tempfile` scratch and sets the CHILD process's working directory to it.
//! That is `Command::current_dir`, never `std::env::set_current_dir` in the test process — which is
//! also why this file could not join `tests/parity.rs`'s group even without its feature gate
//! (`crates/vike-backtest/CLAUDE.md`'s grouping rule).
//!
//! ⚠ **The fixture's search section is `[sweep]`, and it stays that way ON PURPOSE now.** It was
//! written before the rename landed (this paragraph used to say so, and said the rename "has NOT
//! landed" — it has: `harness::profile`'s field is `paramscan`). Keeping the OLD spelling here is
//! what exercises `#[serde(default, alias = "sweep")]`, which that field's doc calls permanent and
//! whose removal would be a breaking change to every profile ever written. `BacktestProfile`
//! carries `#[serde(deny_unknown_fields)]`, so if the alias were ever dropped every test in this
//! file would fail at once with a message naming the section — which is exactly the warning this
//! fixture is worth.
//!
//! `#![cfg(feature = "datafusion-store")]` because the `backtest` bin carries
//! `required-features = ["datafusion-store"]`: without it the binary is not built and
//! `env!("CARGO_BIN_EXE_backtest")` would not COMPILE.
#![cfg(feature = "datafusion-store")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A search profile with a three-point grid — the smallest space in which ranking, a ledger and a
/// resume all mean something. Copied in shape from `optimizer_cli.rs`'s `PARAMSCAN_PROFILE`.
///
/// ⚠ The SECTION is spelled `[sweep]` — the permanent ALIAS, deliberately; see this file's
/// module doc. `[paramscan]` is the name the binary now PRINTS.
const PARAMSCAN_PROFILE: &str = r#"
name = "stage5-fixture"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"

[sweep]
size = [1.0, 2.0, 3.0]
"#;

/// The exact text Task 4's single-run fixture removes, so a `replace` that matched nothing is a
/// test failure rather than a silently-still-a-search profile.
const SWEEP_SECTION: &str = "[sweep]\nsize = [1.0, 2.0, 3.0]\n";

/// A scratch PROJECT: a `settings/` marker so the runs directory resolves inside it, a profile, and
/// a store path that starts out unseeded (one test seeds it on purpose — see the module doc).
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch project");
        std::fs::create_dir_all(dir.path().join("settings")).expect("the project marker");
        std::fs::write(dir.path().join("sweep.toml"), PARAMSCAN_PROFILE).expect("the profile");
        Project { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn runs_root(&self) -> PathBuf {
        self.dir.path().join("user_data").join("runs")
    }

    /// Every run directory under `user_data/runs`, sorted — the listing a Studio Research tab would
    /// build, and what a flood would show up in.
    fn run_dirs(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.runs_root())
            .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// The ONE run directory, asserting there is exactly one.
    fn only_run(&self) -> PathBuf {
        let dirs = self.run_dirs();
        assert_eq!(dirs.len(), 1, "exactly one run directory, got {dirs:?}");
        dirs.into_iter().next().expect("one")
    }

    fn only_run_id(&self) -> String {
        self.only_run().file_name().expect("a directory name").to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_backtest"));
        cmd.current_dir(self.path())
            .args(args)
            // The dev box's own environment must not relocate the project under the test.
            .env_remove("VIKE_SETTINGS_DIR")
            .env_remove("VIKE_USER_DATA_DIR")
            .env_remove("VIKE_HIST_STORE")
            // The file log layer defaults to `trace`; nothing here wants a JSON log in the scratch.
            .env("VIKE_LOG_FILE_LEVEL", "off");
        cmd.output().unwrap_or_else(|e| panic!("run backtest {args:?}: {e}"))
    }

    fn search(&self, extra: &[&str]) -> Output {
        let mut args = vec!["sweep.toml", "--store", "store"];
        args.extend_from_slice(extra);
        self.run(&args)
    }
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn ledger_lines(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("trials.jsonl"))
        .expect("the ledger")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a ledger line"))
        .collect()
}

// ───────────────────────────────────────────────────── §13b: the artifact

/// §13b, end to end: a search that wrote NOTHING now leaves a parent run holding four documents.
#[test]
fn a_search_leaves_a_parent_run_with_a_ledger() {
    let proj = Project::new();
    let out = proj.search(&[]);
    let stderr = stderr_of(&out);
    assert!(out.status.success(), "a search still exits 0; stderr: {stderr:?}");

    let dir = proj.only_run();
    for file in ["search.json", "trials.jsonl", "report.json", "manifest.json"] {
        assert!(dir.join(file).is_file(), "{file} must exist in {dir:?}");
    }
    assert!(
        stderr.contains("search saved to") && stderr.contains(&dir.display().to_string()),
        "…and the operator is TOLD where, on stderr so --json stdout stays a document: {stderr:?}"
    );

    let manifest = read_json(&dir.join("manifest.json"));
    assert_eq!(manifest["kind"], "search", "a search is its own kind, not a backtest");
    assert_eq!(manifest["produced_by"], "backtest");
    assert_eq!(
        manifest["detail"]["trials"]["file"], "trials.jsonl",
        "the manifest names the ledger so a reader never has to know the constant"
    );
    assert_eq!(
        manifest["detail"]["search"]["optimizer"], "grid",
        "…and what a LISTING needs to render a search row without opening the ledger"
    );

    let ledger = std::fs::read_to_string(dir.join("trials.jsonl")).unwrap();
    assert_eq!(ledger.lines().count(), 3, "one line per grid point: {ledger:?}");
    let first: serde_json::Value = serde_json::from_str(ledger.lines().next().unwrap()).unwrap();
    assert_eq!(first["n"], 0, "n is the 0-based EVALUATION index");
    assert!(first["overrides"].is_array(), "…and it records WHAT was tried: {first}");
    // ⚠ The invariant is EXACTLY ONE of the two, which is `ParamscanRow`'s own rule ("either its
    // BacktestReport (success) or the stringified HarnessError (failure) — never both") carried
    // into the ledger. This deliberately does NOT assert which: over an unseeded store a point runs
    // a zero-trade backtest on THIS box and fails outright on one whose store layer refuses an
    // absent series, and the mechanics under test are the same either way. An earlier draft
    // asserted the failing branch, and it was wrong here.
    assert_ne!(
        first["metrics"].is_object(),
        first["error"].is_string(),
        "exactly one of metrics/error, never both and never neither: {first}"
    );
    // ⚠ Asserted on the KEY SET, not with `first["score"]`. `serde_json`'s `Index<&str>` answers
    // `Null` for a MISSING key, so `first["score"].is_null()` passes on a record that dropped the
    // field entirely — the one thing this line claims to guard is the one thing that spelling
    // cannot see.
    let keys = first.as_object().expect("a ledger line is a JSON object");
    for key in ["n", "overrides", "score", "metrics", "error"] {
        assert!(keys.contains_key(key), "every ledger line carries `{key}`: {first}");
    }
    assert!(
        keys["score"].is_null() || keys["score"].is_number(),
        "…and `score` is a number or `null` for UNRANKABLE: {first}"
    );
}

/// ⚠ **A SEARCH RUN ADDRESSES ITS INPUTS, and until the `series_facts` merge it could not.**
///
/// A search's `manifest.json` carried `fingerprint: null` and a pid-form run id, so
/// `vike-cli runs diff` could attribute nothing about a search and `runs gate` could find no
/// comparable baseline for one — the two most expensive runs this engine produces were the two
/// nothing could be compared against. The cause was arithmetic rather than principle:
/// `collect_data_fingerprint` asked the store for coverage and for commit keys separately and each
/// call parsed the same manifest, which a search about to run hundreds of backtests would not pay.
/// `vike_data::DataFusionHist::series_facts` answers both from ONE parse — the parse the search was
/// already spending on its resume witness — so the address now costs it nothing new.
///
/// This asserts BOTH places the address lands: the manifest field and the run id, which is what a
/// human and a selector each read. It also asserts the RECORD under `detail.data.fingerprint`,
/// because the address is a hash and a hash alone tells an investigator nothing about which series
/// moved.
#[test]
fn a_search_run_carries_an_input_address_in_its_manifest_and_in_its_directory_name() {
    let proj = Project::new();
    let out = proj.search(&[]);
    assert!(out.status.success(), "a search still exits 0; stderr: {}", stderr_of(&out));

    let dir = proj.only_run();
    let manifest = read_json(&dir.join("manifest.json"));
    let addr = manifest["fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("a search run must carry an input address: {manifest}"))
        .to_string();
    assert_eq!(
        addr.len(),
        64,
        "lowercase hex SHA-256, as `input_fingerprint` renders it: {addr:?}"
    );
    assert!(
        addr.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "…and nothing but lowercase hex: {addr:?}"
    );

    // ⚠ The ID half, which is what makes the change visible to a person and to a selector.
    // `vike_model::runs::run_id_at` takes the first `ID_FINGERPRINT_LEN` ASCII-alphanumeric
    // characters of the address, and every character of a hex digest qualifies — so the id's middle
    // segment IS that prefix. The pid form this replaces could never contain it.
    let id = proj.only_run_id();
    assert!(
        id.contains(&addr[..16]),
        "the run id must carry the address, not the pid: id {id:?}, address {addr:?}"
    );

    // The RECORD beside the ADDRESS. `run_detail_data` nests it under `data`, and it names every
    // series the profile resolved to — one `bar` series here, which the empty store does not hold.
    let record = &manifest["detail"]["data"]["fingerprint"];
    assert!(
        record["series"].is_array(),
        "the manifest carries the data RECORD, not only the hash of it: {manifest}"
    );
    assert_eq!(
        record["series"].as_array().map(Vec::len),
        Some(1),
        "the fixture's bar profile resolves to exactly one series: {record}"
    );
}

/// ⚠ **The address must be a function of the INPUTS, and this is the pair of assertions that makes
/// that falsifiable.** Either half alone passes on a broken implementation: "two searches agree"
/// passes on a CONSTANT, and "a moved store disagrees" passes on a per-run nonce. Together they
/// pin the property — same inputs, same address; different data, different address.
///
/// ⚠ The two projects are separate temp directories with separate store paths, and they must STILL
/// agree. `run_fingerprint::DataFingerprint::canonical` deliberately excludes the store PATH (a
/// baseline that a move to another box orphans is not a baseline), so two boxes running one profile
/// over equivalent data address the same. A test that shared one directory would prove none of it.
#[test]
fn two_searches_over_the_same_inputs_address_the_same_and_a_moved_store_does_not() {
    fn address_of(proj: &Project) -> String {
        let out = proj.search(&[]);
        assert!(out.status.success(), "a search exits 0; stderr: {}", stderr_of(&out));
        read_json(&proj.only_run().join("manifest.json"))["fingerprint"]
            .as_str()
            .expect("an input address")
            .to_string()
    }

    // ⚠ Each project searches EXACTLY ONCE. `Project::only_run` asserts there is one run directory,
    // so calling `address_of` twice on one project would fail on the count rather than on the
    // address — a failure that says nothing about what this test claims.
    let a = Project::new();
    let unseeded = address_of(&a);
    let b = Project::new();
    assert_eq!(
        unseeded,
        address_of(&b),
        "one profile over one (empty) data slice is one address, whatever directory it ran in"
    );

    // …and now MOVE the data under a third project. `seed-demo` writes the very series this
    // profile resolves to, so the slice goes from ABSENT to held — the same state change
    // `a_resume_after_the_stores_contents_moved_is_refused_and_names_the_data` drives the witness
    // with.
    let c = Project::new();
    let seeded = c.run(&["data", "seed-demo", "--store", "store"]);
    assert!(seeded.status.success(), "seed-demo: {}", stderr_of(&seeded));
    assert_ne!(
        address_of(&c),
        unseeded,
        "a search over a store that HOLDS the slice must not address the same as one over a store \
         that does not — an address that cannot move is not an address"
    );
}

/// ⚠ The row that makes the layout decision falsifiable: 3 trials must be 1 run directory, not 4.
/// Sibling directories named `<id>#<n>` would have put one row per trial into
/// `crates/vike-studio-core/src/listing.rs`'s `list_runs`, which enumerates every folder in the
/// runs root and knows nothing about parenthood.
#[test]
fn trials_are_lines_in_one_directory_not_directories_of_their_own() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    assert_eq!(proj.run_dirs().len(), 1, "three trials, ONE directory: {:?}", proj.run_dirs());
    assert!(
        !proj.run_dirs()[0].join("trials").exists(),
        "and no per-trial subdirectory either — the ledger is a FILE"
    );
}

/// `--keep-trials none` keeps the parent and drops the ledger. The parent still records what the
/// search SPENT, so "this search kept no trials" is an answer rather than an absence.
#[test]
fn keep_trials_none_writes_the_parent_and_no_ledger() {
    let proj = Project::new();
    assert!(proj.search(&["--keep-trials", "none"]).status.success());

    let dir = proj.only_run();
    assert!(!dir.join("trials.jsonl").exists(), "no ledger");
    assert!(dir.join("manifest.json").is_file(), "…but the run is still recorded");

    let report = read_json(&dir.join("report.json"));
    assert_eq!(report["keep_trials"], "none");
    assert_eq!(report["evaluated"], 3, "…and it still says what the search cost");
    assert_eq!(report["trials"].as_array().map(|a| a.len()), Some(0));
}

/// ⚠ PERSISTENCE CHANGES NO OUTPUT — asserted against a search that persists NOTHING AT ALL.
///
/// ⚠ **This test used to compare `--json` against `--json --keep-trials none`, and that comparison
/// was weaker than its own name.** Both of those mint a parent, write a `search.json` and drive the
/// evaluator through `TrialRecorder`; they differ only in whether a ledger file is appended. So it
/// proved the two PERSISTENCE MODES agree, which is not the claim. The second run here is in a
/// scratch with NO project marker, where `open_search_run` fails, no parent is minted, no document
/// is written and the recorder holds no ledger path — the closest reachable thing to the
/// pre-stage-5 binary, and the strongest comparison available from outside the process.
///
/// ⚠ Both runs name the SAME ABSOLUTE store, so a failure string naming the store path cannot make
/// them differ for a reason that has nothing to do with persistence. The profile is spelled
/// identically (`sweep.toml`, relative) in both.
///
/// The remaining half — that the recorder itself is TRANSPARENT, handing the inner evaluator the
/// original batch verbatim — is `harness::trials`'s
/// `an_empty_cache_passes_the_batch_through_unchanged`, which can see the inner call and this
/// cannot.
#[test]
fn the_shipped_json_report_is_unchanged_by_persistence() {
    let shared = tempfile::tempdir().expect("a shared store");
    let store = shared.path().join("store");
    let store = store.to_str().expect("a UTF-8 store path");

    let persisting = Project::new();
    let with = persisting.run(&["sweep.toml", "--store", store, "--json"]);

    // No `settings/` marker: nothing is minted, nothing is written, no ledger path reaches the
    // recorder.
    let bare = tempfile::tempdir().expect("a scratch with no project");
    std::fs::write(bare.path().join("sweep.toml"), PARAMSCAN_PROFILE).expect("the profile");
    let without = Command::new(env!("CARGO_BIN_EXE_backtest"))
        .current_dir(bare.path())
        .args(["sweep.toml", "--store", store, "--json"])
        .env_remove("VIKE_SETTINGS_DIR")
        .env_remove("VIKE_USER_DATA_DIR")
        .env_remove("VIKE_HIST_STORE")
        .env("VIKE_LOG_FILE_LEVEL", "off")
        .output()
        .expect("run backtest");

    assert!(with.status.success() && without.status.success());
    assert!(!persisting.run_dirs().is_empty(), "the first run DID persist — precondition");
    assert!(
        stderr_of(&without).contains("search NOT saved"),
        "…and the second persisted nothing at all — precondition: {:?}",
        stderr_of(&without)
    );
    assert_eq!(
        stdout_of(&with),
        stdout_of(&without),
        "the search's own document must not depend on whether anything was saved"
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&with)).expect("valid JSON");
    assert!(doc["rows"].is_array() && doc["rank_by"].is_string(), "still a ParamscanReport: {doc}");
}

/// The identity `--resume` compares is written BEFORE the search, which is the only reason an
/// interrupted run has one at all — `manifest.json` is written LAST as the completion marker.
#[test]
fn the_search_header_records_the_identity_the_resume_will_check() {
    let proj = Project::new();
    assert!(proj.search(&["--optimizer", "tpe", "--trials", "4", "--seed", "9"]).status.success());

    let header = read_json(&proj.only_run().join("search.json"));
    assert_eq!(header["identity"]["method"], "tpe");
    assert_eq!(header["identity"]["seed"], 9);
    assert_eq!(header["identity"]["budget"], 4);
    assert_eq!(header["identity"]["rank_by"], "sharpe", "the default rank label is recorded too");
    assert_eq!(header["keep_trials"], "scalars");
    assert!(header["schema"].is_number(), "a schema version shipped WITH the document, not after");
    let hash = header["identity"]["profile_fnv1a64"].as_str().expect("a profile hash");
    assert_eq!(hash.len(), 16, "FNV-1a 64 as hex: {hash}");
    assert_ne!(hash, "unreadable", "the profile was readable, so it was read");

    // ⚠ The DATA witness is part of the identity, not just the store path. A search over an unseeded
    // store witnesses its series as ABSENT — which is a real state, and the one a later seed or
    // backfill moves away from.
    let identity = header["identity"].as_object().expect("an identity object");
    let data = identity["store_data"].as_str().expect("a data witness");
    assert_ne!(data, "unreadable", "the store was listable, so it was witnessed");
    assert!(
        data.contains("bar demo BTCUSDT") && data.contains("absent"),
        "one line per resolved series, saying what the store held: {data:?}"
    );
    assert!(
        identity.contains_key("build"),
        "the BINARY is part of the identity — null from the standalone engine, which is its own \
         declared residual, but the KEY is always written: {identity:?}"
    );
    let store = identity["store"].as_str().expect("a store path");
    assert!(
        std::path::Path::new(store).is_absolute(),
        "the store path is CANONICALIZED, so `--store store` from two projects cannot compare \
         equal: {store:?}"
    );
}

/// A search that cannot persist still SEARCHES, and says why on stderr. Persisting is additive.
#[test]
fn a_search_outside_a_project_still_runs_and_names_the_failure() {
    // A scratch with NO `settings/` marker, so the project walk has nothing to answer with.
    let dir = tempfile::tempdir().expect("a scratch");
    std::fs::write(dir.path().join("sweep.toml"), PARAMSCAN_PROFILE).expect("the profile");
    let out = Command::new(env!("CARGO_BIN_EXE_backtest"))
        .current_dir(dir.path())
        .args(["sweep.toml", "--store", "store"])
        .env_remove("VIKE_SETTINGS_DIR")
        .env_remove("VIKE_USER_DATA_DIR")
        .env_remove("VIKE_HIST_STORE")
        .env("VIKE_LOG_FILE_LEVEL", "off")
        .output()
        .expect("run backtest");

    assert!(out.status.success(), "a filesystem that cannot take a run must not fail the search");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("search NOT saved"),
        "and it is NAMED rather than swallowed: {stderr:?}"
    );
    assert!(
        !stdout_of(&out).is_empty(),
        "…while the numbers the search computed still reach stdout"
    );
}

// ───────────────────────────────────────────────────── `backtest trials <id>`

/// The verb reads the ARTIFACT: no store, no profile, no network. `--store` is not even passed.
#[test]
fn trials_reads_a_finished_search_with_no_store() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let id = proj.only_run_id();

    let out = proj.run(&["trials", &id]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "exit 0; stderr: {}", stderr_of(&out));
    assert!(stdout.contains(&id), "the table names the search it read: {stdout:?}");
    assert!(stdout.contains("grid"), "…and the method: {stdout:?}");
    for addr in ["#0", "#1", "#2"] {
        assert!(stdout.contains(addr), "one row per trial, addressed {addr}: {stdout:?}");
    }
}

/// `--json` is a DOCUMENT — the same `TrialsDocument` the run persisted, so a consumer that reads
/// `report.json` and one that pipes this verb see one schema.
#[test]
fn trials_json_is_the_same_document_the_run_persisted() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();

    let out = proj.run(&["trials", &id, "--json"]);
    assert!(out.status.success());
    let printed: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    let stored = read_json(&dir.join("report.json"));

    assert_eq!(printed["schema"], stored["schema"]);
    assert_eq!(printed["run_id"], stored["run_id"]);
    assert_eq!(printed["identity"], stored["identity"]);
    assert_eq!(printed["evaluated"], stored["evaluated"]);
    assert_eq!(
        printed["trials"].as_array().map(|a| a.len()),
        stored["trials"].as_array().map(|a| a.len())
    );
}

/// `--top N` truncates and `--sort` reorders — and the ARRAY ORDER is the rank, so no `rank` key is
/// invented.
#[test]
fn trials_top_and_sort_shape_the_answer() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let id = proj.only_run_id();

    let out = proj.run(&["trials", &id, "--json", "--top", "2"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    assert_eq!(doc["trials"].as_array().unwrap().len(), 2, "--top truncates");
    assert_eq!(
        doc["evaluated"], 3,
        "…and the COUNTS still describe the whole search, not the page"
    );

    let out = proj.run(&["trials", &id, "--json", "--sort", "n"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    let ns: Vec<i64> =
        doc["trials"].as_array().unwrap().iter().map(|t| t["n"].as_i64().unwrap()).collect();
    assert_eq!(ns, vec![0, 1, 2], "--sort n is the ledger's own order");

    let out = proj.run(&["trials", &id, "--sort", "nonsense"]);
    assert_eq!(out.status.code(), Some(2), "an unknown sort field is a usage refusal");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("nonsense") && stderr.contains("score"), "names both: {stderr:?}");
}

/// The selector grammar is ONE implementation and it is not this verb's to invent, so a prefix or
/// an `@last` is refused by NAME rather than half-supported.
#[test]
fn trials_refuses_a_selector_it_does_not_own() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());

    for spelling in ["@last", "1756"] {
        let out = proj.run(&["trials", spelling]);
        assert_eq!(out.status.code(), Some(2), "{spelling} is refused");
        let stderr = stderr_of(&out);
        assert!(stderr.contains(spelling), "echoes what was typed: {stderr:?}");
        assert!(
            stderr.contains("full run id"),
            "…and says what this verb takes rather than failing on an open: {stderr:?}"
        );
    }

    let out = proj.run(&["trials", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(2), "a well-formed id that does not exist is also a 2");
    assert!(
        stderr_of(&out).contains("1756000000-1-0"),
        "naming the id, because that is the thing the operator can check"
    );
}

/// A run that is not a SEARCH is refused by name — `backtest trials` over a single backtest would
/// otherwise report an empty table, which reads as "the search found nothing".
#[test]
fn trials_refuses_a_run_that_is_not_a_search() {
    let proj = Project::new();
    // ⚠ The literal must match `PARAMSCAN_PROFILE`'s section BYTE FOR BYTE. A `replace` that matched
    // nothing would leave a SEARCH profile here and this test would then be asserting about the
    // wrong kind of run while still passing its exit-code check, so the replacement is VERIFIED.
    let single = PARAMSCAN_PROFILE.replace(SWEEP_SECTION, "");
    assert!(
        !single.contains("[sweep]") && single.len() < PARAMSCAN_PROFILE.len(),
        "the grid must actually be removed — this test is about a run that is NOT a search"
    );
    std::fs::write(proj.path().join("single.toml"), single).expect("a single-run profile");
    assert!(proj.run(&["single.toml", "--store", "store"]).status.success());

    let id = proj.only_run_id();
    let out = proj.run(&["trials", &id]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    // ⚠ The QUOTED kinds, not the bare words. Every diagnostic this binary prints is prefixed
    // `backtest: `, so `contains("backtest")` passes whatever kind was found — it cannot fail for
    // the reason it claims. The message renders both kinds with `{:?}`, so the quotes are what make
    // these two assertions about the KINDS rather than about the prefix.
    assert!(
        stderr.contains("is a \"backtest\" run"),
        "names the kind it FOUND, in the message's own rendering: {stderr:?}"
    );
    assert!(stderr.contains("not a \"search\""), "…and the kind it NEEDS: {stderr:?}");
}

/// `--export-params` closes the loop: the winning trial's overrides, as a TOML fragment a profile
/// can take.
#[test]
fn export_params_writes_the_winning_overrides_as_toml() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let id = proj.only_run_id();
    let out_path = proj.path().join("best.toml");

    let out = proj.run(&["trials", &id, "--export-params", out_path.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));

    let text = std::fs::read_to_string(&out_path).expect("the fragment");
    assert!(text.contains("[strategy.params]"), "the table a profile merges: {text:?}");
    assert!(text.contains("size = "), "the swept key, with its winning value: {text:?}");
    assert!(text.contains(&id), "…and a provenance comment naming the search: {text:?}");
    assert!(
        toml::from_str::<toml::Value>(&text).is_ok(),
        "and it PARSES as TOML — a fragment that does not is a paste that fails later: {text:?}"
    );
}

// ───────────────────────────────────────────────────── `--resume`

/// Keep the first `k` lines of a run's ledger and drop the rest, plus its report and manifest — a
/// hand-made INTERRUPTED search. Killing a real one mid-flight is not reproducible in a test, and
/// the thing under test is what resume does with the bytes on disk.
fn truncate_ledger(dir: &Path, k: usize) {
    let path = dir.join("trials.jsonl");
    let text = std::fs::read_to_string(&path).expect("the ledger");
    let kept: Vec<&str> = text.lines().take(k).collect();
    let mut out = kept.join("\n");
    out.push('\n');
    std::fs::write(&path, out).expect("truncate the ledger");
    // The manifest is the completion marker and the report is derived from the ledger; an
    // interrupted run has neither.
    let _ = std::fs::remove_file(dir.join("manifest.json"));
    let _ = std::fs::remove_file(dir.join("report.json"));
}

/// THE deliverable: an interrupted search picks up where it stopped, and the ledger it ends with is
/// the one it would have had.
#[test]
fn resume_evaluates_only_what_the_ledger_is_missing() {
    let proj = Project::new();
    assert!(proj.search(&["--optimizer", "tpe", "--trials", "5", "--seed", "3"]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();
    let full: Vec<String> = ledger_lines(&dir).iter().map(|l| l["overrides"].to_string()).collect();
    assert_eq!(full.len(), 5);

    truncate_ledger(&dir, 2);

    let out = proj.search(&["--optimizer", "tpe", "--trials", "5", "--seed", "3", "--resume", &id]);
    let stderr = stderr_of(&out);
    assert!(out.status.success(), "stderr: {stderr:?}");
    assert!(
        stderr.contains("resumed") && stderr.contains("2 reused") && stderr.contains("3 evaluated"),
        "the counts are REPORTED — without them the assertions below would pass on a binary that \
         ignored --resume entirely: {stderr:?}"
    );

    assert_eq!(proj.run_dirs().len(), 1, "a resume CONTINUES the run, it does not mint a second");
    let after: Vec<String> =
        ledger_lines(&dir).iter().map(|l| l["overrides"].to_string()).collect();
    assert_eq!(
        after, full,
        "the resumed ledger holds the same candidates in the same order as the uninterrupted one — \
         the searcher is seed-deterministic and the warm cache fed it the original scores"
    );
    let ns: Vec<i64> = ledger_lines(&dir).iter().map(|l| l["n"].as_i64().unwrap()).collect();
    assert_eq!(ns, vec![0, 1, 2, 3, 4], "and n stayed the EVALUATION index across the seam");
}

/// The negative control that makes the test above falsifiable: a ledger whose candidates do not
/// match reuses NOTHING, and says so.
#[test]
fn a_ledger_whose_candidates_do_not_match_reuses_nothing() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();

    // Same identity, different candidates — only the overrides are rewritten.
    let rewritten: Vec<String> = ledger_lines(&dir)
        .into_iter()
        .map(|mut l| {
            l["overrides"] = serde_json::json!([["size", 99.0]]);
            l.to_string()
        })
        .collect();
    std::fs::write(dir.join("trials.jsonl"), rewritten.join("\n") + "\n").unwrap();
    let _ = std::fs::remove_file(dir.join("manifest.json"));

    let out = proj.search(&["--resume", &id]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("0 reused") && stderr.contains("3 evaluated"),
        "reuse is keyed on the CANDIDATE, not on the line count: {stderr:?}"
    );
}

/// A resume whose inputs differ is REFUSED and names which one — reusing a score computed for a
/// different profile, store, method, seed or budget would be a wrong answer, silently.
#[test]
fn resume_refuses_a_search_that_is_not_the_same_search() {
    let proj = Project::new();
    assert!(proj.search(&["--optimizer", "tpe", "--trials", "4", "--seed", "1"]).status.success());
    let id = proj.only_run_id();

    let out = proj.search(&["--optimizer", "tpe", "--trials", "4", "--seed", "2", "--resume", &id]);
    assert_eq!(out.status.code(), Some(2), "a different seed is a refusal, not a fresh search");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("seed"), "names the field that differs: {stderr:?}");
    assert!(stderr.contains('1') && stderr.contains('2'), "and both values: {stderr:?}");

    // …and the profile itself.
    let mut text = std::fs::read_to_string(proj.path().join("sweep.toml")).unwrap();
    text.push_str("\n# an edit\n");
    std::fs::write(proj.path().join("sweep.toml"), text).unwrap();
    let out = proj.search(&["--optimizer", "tpe", "--trials", "4", "--seed", "1", "--resume", &id]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("profile"), "an edited profile is named: {}", stderr_of(&out));
}

/// A resume prints the LEDGER rather than the sweep report, because a reused row has no
/// `BacktestReport` to render and `ParamscanReport`'s Display would print it as `FAILED`.
#[test]
fn a_resumed_run_prints_the_trials_document() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();
    truncate_ledger(&dir, 1);

    let out = proj.search(&["--resume", &id, "--json"]);
    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("valid JSON");
    assert!(doc["trials"].is_array(), "a TrialsDocument: {doc}");
    assert!(doc["rank_by"].is_null(), "…and NOT a ParamscanReport, whose top-level key that is");
    assert_eq!(doc["reused"], 1);
    assert_eq!(doc["trials"].as_array().unwrap().len(), 3, "the WHOLE search, not this leg of it");
}

/// A resume against a run that kept no trials is refused by name — there is nothing to resume, and
/// silently re-running the whole search is not what was asked for.
#[test]
fn resume_refuses_a_search_that_kept_no_trials() {
    let proj = Project::new();
    assert!(proj.search(&["--keep-trials", "none"]).status.success());
    let id = proj.only_run_id();

    let out = proj.search(&["--resume", &id]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("keep-trials"), "names the reason: {stderr:?}");
    assert!(stderr.contains(&id), "and the run: {stderr:?}");
}

/// ⚠ **THE REVERT-PROOF for the data witness, end to end.** Drop `store_data` from
/// `SearchIdentity` or from `differences()` and this test goes red: the resume below would SUCCEED
/// and reuse three scores computed when the store held nothing, against a store that now holds a
/// real tape.
///
/// The store is changed the cheap, decisive way — the search runs against an EMPTY store, then
/// `backtest data seed-demo` writes the demo tape the profile's own `demo/BTCUSDT/1h` series names,
/// then the resume is attempted. Same profile bytes, same store PATH, same method, same seed, same
/// budget; only what the store HELD moved. That is the coordinator's backfill scenario with the two
/// states swapped, and it needs no store internals and no second dataset.
#[test]
fn a_resume_after_the_stores_contents_moved_is_refused_and_names_the_data() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();
    truncate_ledger(&dir, 1);

    // The store's CONTENTS move; its path does not.
    let seeded = proj.run(&["data", "seed-demo", "--store", "store"]);
    assert!(seeded.status.success(), "seed-demo: {}", stderr_of(&seeded));
    assert!(
        stdout_of(&seeded).contains("demo"),
        "precondition: the demo tape was written: {}",
        stdout_of(&seeded)
    );

    let out = proj.search(&["--resume", &id]);
    let stderr = stderr_of(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a resume over data that MOVED must be refused, not resolved; stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("data:"),
        "…and the refusal must NAME the data, because 'the search changed' sends an operator to \
         the wrong knob: {stderr:?}"
    );
    assert!(
        stderr.contains("MOVED"),
        "the message says what happened rather than only that something differs: {stderr:?}"
    );
    assert!(
        !stderr.contains("store:"),
        "the store PATH did not change — naming it would be a second, false cause: {stderr:?}"
    );
    assert_eq!(
        ledger_lines(&dir).len(),
        1,
        "nothing was appended: the refusal happens before a single candidate is evaluated"
    );
}

/// …and the other half of the same gate: an UNCHANGED store still resumes. Without this,
/// `a_resume_after_the_stores_contents_moved_is_refused_and_names_the_data` would also pass on a
/// build whose data witness refused EVERYTHING, which would make `--resume` useless rather than
/// sound.
#[test]
fn an_unchanged_store_still_resumes() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();
    truncate_ledger(&dir, 1);

    let out = proj.search(&["--resume", &id]);
    let stderr = stderr_of(&out);
    assert!(out.status.success(), "an unchanged store is not a difference; stderr: {stderr:?}");
    assert!(stderr.contains("1 reused"), "…and the cached trial was reused: {stderr:?}");
}

/// ⚠ `--resume` with `--keep-trials none` REFUSED BY NAME, before any I/O. The combination would
/// overwrite the resumed run's `search.json` to say it kept no ledger while `trials.jsonl` sat
/// intact beside it — after which every later `--resume` of that id is refused for a reason that is
/// not true, and the only stated remedy is re-running the whole search.
#[test]
fn resume_with_keep_trials_none_is_refused_and_the_artifact_survives() {
    let proj = Project::new();
    assert!(proj.search(&[]).status.success());
    let dir = proj.only_run();
    let id = proj.only_run_id();
    let header_before = std::fs::read_to_string(dir.join("search.json")).expect("the header");

    let out = proj.search(&["--resume", &id, "--keep-trials", "none"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("--resume") && stderr.contains("--keep-trials"),
        "names both: {stderr:?}"
    );

    assert_eq!(
        std::fs::read_to_string(dir.join("search.json")).expect("the header"),
        header_before,
        "⚠ THE DAMAGE THIS PREVENTS: the header must be byte-identical. A rewritten one saying \
         `keep_trials: none` makes every later --resume of this id refuse with a reason that is false"
    );
    assert_eq!(ledger_lines(&dir).len(), 3, "and the ledger is untouched");

    // …and the artifact is still resumable, which is the property the refusal exists to protect.
    truncate_ledger(&dir, 1);
    let after = proj.search(&["--resume", &id]);
    assert!(after.status.success(), "stderr: {}", stderr_of(&after));
    assert!(stderr_of(&after).contains("1 reused"), "{}", stderr_of(&after));
}

/// A resume naming a run that does not exist is a usage refusal that names it — never a fresh
/// search under a new id, which would look like success and quietly redo the work.
#[test]
fn resume_refuses_an_id_that_names_nothing() {
    let proj = Project::new();
    let out = proj.search(&["--resume", "1756000000-1-0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("1756000000-1-0"));
    assert!(proj.run_dirs().is_empty(), "…and nothing was minted");
}
