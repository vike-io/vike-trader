//! **Running a study, and leaving a run behind.** The half `crates/vike-user-research` cannot have.
//!
//! `crates/vike-user-research/src/contract.rs` states the split in its own words — *"the study
//! produces [`StudyOutcome`]; the caller mints the run, writes the artifacts and folds the metrics
//! into the manifest's `detail`"* — and then names the reason it cannot be that caller: run
//! persistence lives in `crates/vike-runs/src/lib.rs`, at a layer above it. This module is
//! that caller. What it WRITES — the manifest's `kind`, the typed `detail`, a metric's shape and the
//! note — is `crates/vike-studio-core/src/study_run.rs`'s vocabulary, which the desktop shows a run with
//! and which carries why a study's numbers never share a column with a backtest's.
//!
//! # Why this crate
//!
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md` carries the argument: the
//! runner lives beside the listing its runs must appear in (`crates/vike-study-runner/src/listing.rs`'s
//! `list_runs`), so nothing else has to see both ends. Both moved here from `vike-studio-core` on
//! 2026-11-19, when studies came to run only on the backtest server (the owner's ruling of that day):
//! that crate is what the desktop links, and the desktop runs no study.
//!
//! # What it takes, and what it constructs
//!
//! Everything, as parameters — [`StudyRunRequest`] has public fields and no constructor, the same
//! device `crates/vike-runs/src/lib.rs`'s `RunManifest` uses and for the same reason: a struct
//! literal cannot omit a field, so a field added here stops every caller until it decides what to
//! put there.
//!
//! ⚠ **The store is a PARAMETER and this module constructs none**: the concrete
//! `HistoryStoreDataFusion` lives in `vike-store`, which this crate does not name (only the datahub
//! touches the store, decision 0084), so the request carries an `Arc<dyn HistoryStore + Send + Sync>`
//! the BINARY built — the backtest server hands its datahub route. That is the same discipline
//! `crates/vike-user-research/src/contract.rs` applies one layer down, for a stronger reason there.
//!
//! The CLOCK is a parameter too (`vike_model::Clock`). `crates/vike-runs/src/lib.rs` reads no
//! clock at all and takes the run's start second, so that the minted id and the manifest's
//! `started_at` name the same instant; this module needs a SECOND read — the finish — which the
//! caller cannot supply in advance, so it holds the seam instead of the two numbers. Both reads go
//! through the caller's own clock, which is what lets a test mint against a fixed instant rather
//! than racing one.
//!
//! # The order of operations, and what each ordering buys
//!
//! 1. read the clock — the run's start second;
//! 2. create the scratch directory, so a study's first write does not fail on a missing parent;
//! 3. build the [`StudyContext`] and CALL the study;
//! 4. read the clock again — the finish;
//! 5. VALIDATE the artifact names, before anything is on disk;
//! 6. mint the run directory (`create_run_dir`, whose atomic `create_dir` IS the id mint);
//! 7. write the artifacts, then the report, then the manifest LAST.
//!
//! **The study runs BEFORE the directory is minted**, exactly as
//! `crates/vike-backtesthub/src/backtest_cli.rs`'s `persist_run` does: a study that refuses leaves no
//! empty run directory for a listing to report as unfinished.
//!
//! **The artifacts are written before the manifest**, joining the report on the near side of the
//! completion marker. `crates/vike-runs/src/lib.rs` writes the report first and the manifest
//! last precisely so a directory holding a manifest is a run that finished writing; artifacts
//! written afterwards would break that, and a listing would call a half-written run complete.
//!
//! **A persistence failure never costs the numbers.** `vike_runs` states the rule — *"Saving a run is
//! worth doing; it is not worth losing a run over"* — and here it has to be carried in a type,
//! because this module is the thing that computed them: [`StudyRunError::NotPersisted`] hands the
//! [`StudyOutcome`] back beside the reason it could not be stored. The backtest server answers them to
//! a client that asked (`vike_backtesthub_client::study::WireStudyReport::not_saved`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use vike_data::HistoryStore;
use vike_data::TimestampRange;
use vike_model::Clock;
use vike_model::time::utc_rfc3339;
use vike_runs::{
    MANIFEST_SCHEMA, MODEL_ONNX_ARTIFACT, RESERVED_FILES, RunConfig, RunManifest, RunPersistError,
    create_run_dir, write_run,
};
use vike_studio_core::study_run::{STUDY_METRICS_NOTE, STUDY_RUN_KIND, StudyDetail, StudyMetric};
use vike_user_research::{
    StudyContext, StudyError, StudyLearner, StudyOutcome, StudyRuns, StudySim, valid_artifact_name,
};

/// Everything one study run needs. Public fields and no constructor, on purpose — see this
/// module's doc.
pub struct StudyRunRequest<'a> {
    /// The study's name, which is also its folder name under
    /// `<project>/user_data/research/studies/{rust,rhai}/`. Recorded as `detail.study`.
    pub study: &'a str,
    /// The recipe: one of the study folder's `.toml` files, already parsed. Recorded verbatim in
    /// the report, so a run says which configuration produced it rather than only which FILE did.
    pub params: &'a toml::Value,
    /// The store the study reads. Built by the BINARY — this module constructs none.
    pub store: Arc<dyn HistoryStore + Send + Sync>,
    /// The learner, when this host has one. `None` is the documented ceiling of a box with no
    /// LightGBM binary, and a study that must fit says so with `StudyError::NoLearner` rather than
    /// degrading into a number.
    pub learner: Option<Arc<dyn StudyLearner>>,
    /// The ONE backtest a study may ask for (`ctx.sim()`), when this host can run one. `None` is a
    /// host that runs no backtest, where a study that asks answers `StudyError::NoSim`, the learner's
    /// shape. The backtest server's `RunStudy` and `vike-study` hand
    /// `vike_backtesthub::harness_sim::HarnessSim`. Until 2026-11-10 the study host linked the harness
    /// and handed every study the seam unconditionally; the Studio's Research pane, which handed none,
    /// runs no study since 2026-11-19.
    pub sim: Option<Arc<dyn StudySim>>,
    /// The window the run is ASKED about. Recorded in the manifest so a listing can show it without
    /// parsing a study's own configuration.
    pub window: TimestampRange,
    /// A directory the study may write scratch into.
    ///
    /// Created here when absent, and deliberately NOT deleted afterwards:
    /// `crates/vike-user-research/src/contract/context.rs`'s `StudyContext::new` calls it *"a directory the
    /// caller owns and may delete afterwards"*, and the owner is whoever chose the path. Anything a
    /// run must KEEP is an artifact, which lands in the run directory instead.
    pub scratch: PathBuf,
    /// `<project>/user_data/runs` — resolved by the BINARY through
    /// `crates/vike-paths/src/state_path.rs`'s `user_runs_dir`, never here.
    pub runs_root: &'a Path,
    /// The BINARY that is running this, spelled literally — `vike_runs`'s `RunManifest::produced_by`
    /// carries why that is not `CARGO_PKG_NAME`.
    pub produced_by: &'a str,
    /// The commit the producing binary was built from, or `None` when it cannot name one. A
    /// parameter because only a binary depending on `vike-buildinfo` can answer, and this is a
    /// library.
    pub git_sha: Option<String>,
    /// Which config drove the run — the recipe file as the operator spelled it, and its label.
    pub config: RunConfig,
    /// The clock both timestamps are read from. See this module's doc for why the seam is here
    /// rather than two `i64`s.
    pub clock: &'a dyn Clock,
}

/// A study run that is now ON DISK.
#[derive(Debug)]
pub struct StudyRun {
    /// The minted id, which is also the run directory's name.
    pub run_id: String,
    /// The run directory: `<runs_root>/<run_id>`.
    pub dir: PathBuf,
    /// The manifest as written — including the `detail` subtree, so a caller can render the row it
    /// just produced without reading the file back.
    pub manifest: RunManifest,
    /// What the study returned, unchanged. Returned rather than only written, so a caller can print
    /// the numbers it was waiting for without opening a file.
    pub outcome: StudyOutcome,
}

/// Why a study run did not produce a run directory.
#[derive(Debug)]
pub enum StudyRunError {
    /// A Rust study FILE was asked of a host that cannot run one: a study file runs only in the
    /// sandboxed study process a backtest server starts (`crate::study_dispatch::StudyFileRunner`),
    /// and this host was handed none. Raised before anything runs.
    NoSandbox {
        /// The study that was asked for.
        name: String,
    },
    /// The scratch directory could not be created. Raised BEFORE the study runs, because a study
    /// whose first write fails on a missing parent reports it as its own failure.
    Scratch {
        /// The directory that could not be created.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// The study itself refused. Carries `vike-user-research`'s own error rather than a string, so
    /// a caller can act on `StudyError::NoLearner` — the one failure that is a property of the HOST
    /// rather than of the study — without parsing a sentence.
    Study {
        /// Which study refused.
        name: String,
        /// Its own refusal.
        why: StudyError,
    },
    /// The study RAN and its result could not be stored. The outcome is carried out, never
    /// discarded — this module's doc has the rule and `vike_runs` has the original.
    NotPersisted {
        /// What the study returned. Boxed to keep the `Ok` path's `Result` small.
        outcome: Box<StudyOutcome>,
        /// What went wrong while storing it.
        why: StudyPersistError,
    },
}

/// What can go wrong once a study has already produced a result.
#[derive(Debug)]
pub enum StudyPersistError {
    /// An artifact whose name is one of the run directory's own documents — `RESERVED_FILES`.
    ///
    /// `crates/vike-user-research/src/contract/outcome.rs`'s `StudyOutcome::artifact` states that this
    /// check belongs HERE: the names are `vike_runs`'s constants, at a layer that crate may not name,
    /// and restating the literals down there would be a second authority for a fact that already
    /// has one.
    ///
    /// ⚠ The roster GROWS — it was two names, then five, and `META_FILE` joined it when tagging
    /// shipped — so neither the check nor the message below may spell a list of its own.
    ReservedArtifact {
        /// The artifact the study asked to write.
        name: String,
    },
    /// An artifact name that is not usable as a file name. Re-checked here through
    /// `vike_user_research::valid_artifact_name` — the same function `StudyOutcome::artifact`
    /// applies, called rather than copied — because this is the code that turns the name into a
    /// path, and a writer that trusts its input is one refactor away from a traversal.
    InvalidArtifact {
        /// The artifact the study asked to write.
        name: String,
        /// The rule it broke, in that function's own words.
        why: &'static str,
    },
    /// The run directory, the report or the manifest could not be written.
    Run(RunPersistError),
    /// One artifact file could not be written.
    Artifact {
        /// The file that could not be written.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
}

impl std::fmt::Display for StudyPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // ⚠ The roster is JOINED, never re-typed: it was two names when this message named two,
            // and it is six now. A hand copy here would be a second authority that goes stale in
            // the one message an author reads when their artifact is refused.
            Self::ReservedArtifact { name } => write!(
                f,
                "the study returned an artifact called {name:?}, which is one of the run \
                 directory's own documents ({}) — rename it in the study",
                RESERVED_FILES.join(", ")
            ),
            Self::InvalidArtifact { name, why } => {
                write!(f, "artifact name {name:?}: {why}")
            }
            Self::Run(e) => write!(f, "{e}"),
            Self::Artifact { path, why } => {
                write!(f, "cannot write {}: {why}", path.display())
            }
        }
    }
}

impl std::error::Error for StudyPersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(e) => Some(e),
            _ => None,
        }
    }
}

impl std::fmt::Display for StudyRunError {
    /// One line each, every line naming what to do about it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSandbox { name } => write!(
                f,
                "{name} is a Rust study FILE, which runs only in the sandboxed study process a \
                 backtest server starts, and this host starts none: run it with `vike-cli research \
                 study --study {name} ...` — nothing was run"
            ),
            Self::Scratch { path, why } => write!(
                f,
                "cannot create the scratch directory {}: {why} — nothing was run",
                path.display()
            ),
            Self::Study { name, why } => write!(f, "{name}: {why}"),
            Self::NotPersisted { outcome, why } => write!(
                f,
                "the study ran and its result was NOT saved ({} metric(s) and {} artifact(s) are \
                 still in hand): {why}",
                outcome.metrics().len(),
                outcome.artifacts().len()
            ),
        }
    }
}

impl std::error::Error for StudyRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Study { why, .. } => Some(why),
            Self::NotPersisted { why, .. } => Some(why),
            _ => None,
        }
    }
}

/// The study as [`run_study_with`] calls it: the contract's signature (`vike_user_research::StudyFn`)
/// as a value, so an entry that is not a `fn` item reaches the same persistence — an interpreted study
/// (`crate::rhai_study::RhaiStudy::run`), and a Rust study FILE run in its sandbox by the host's
/// `crate::study_dispatch::StudyFileRunner`.
pub type StudyEntry<'a> =
    &'a dyn Fn(&StudyContext, &toml::Value) -> Result<StudyOutcome, StudyError>;

/// Run a study entry and leave a run behind: the ONE persistence path of every study, whichever way
/// it runs (a study file in its sandbox, an interpreted study in this process).
pub fn run_study_with(
    entry: StudyEntry<'_>,
    req: StudyRunRequest<'_>,
) -> Result<StudyRun, StudyRunError> {
    // Destructured rather than field-accessed: the store and the learner are MOVED into the
    // context below, and a partially-moved struct cannot then be borrowed for the manifest.
    let StudyRunRequest {
        study,
        params,
        store,
        learner,
        sim,
        window,
        scratch,
        runs_root,
        produced_by,
        git_sha,
        config,
        clock,
    } = req;

    let started_at = unix_secs(clock);

    std::fs::create_dir_all(&scratch)
        .map_err(|e| StudyRunError::Scratch { path: scratch.clone(), why: e.to_string() })?;

    let has_learner = learner.is_some();
    let mut ctx = StudyContext::new(store, window, scratch);
    if let Some(learner) = learner {
        ctx = ctx.with_learner(learner);
    }
    // The backtest seam is the HOST's, like the learner: a host that can run a backtest hands one,
    // and one that cannot hands none (`StudyRunRequest::sim`). A study that never asks pays nothing;
    // one that asks without it answers `StudyError::NoSim`.
    if let Some(sim) = sim {
        ctx = ctx.with_sim(sim);
    }
    // The saved runs a study may read (book W21), under the root this run is written into: every host
    // that persists a study offers them, read-only and by run id.
    ctx = ctx.with_runs(Arc::new(SavedRuns { root: runs_root.to_path_buf() }));
    // What one unit of a kept contract is worth, the catalog's answer (`StudyContext::kept_contract_value`): valued
    // here, on the host's side, because the study contract crate links no catalog.
    ctx = ctx.with_contract_valuation(vike_catalog::kept_contract_value);

    let outcome =
        entry(&ctx, params).map_err(|why| StudyRunError::Study { name: study.to_string(), why })?;

    // Read AFTER the work and before anything is written, so the pair measures the study rather
    // than the disk — `vike_runs`'s `RunManifest::finished_at` makes the same distinction.
    let finished_at = unix_secs(clock);

    let facts = RunInputs {
        study,
        params,
        runs_root,
        produced_by,
        git_sha,
        config,
        window,
        has_learner,
        started_at,
        finished_at,
    };

    // Everything below can only fail with a result already in hand, so the failure carries it out.
    match persist(&outcome, facts) {
        Ok((run_id, dir, manifest)) => Ok(StudyRun { run_id, dir, manifest, outcome }),
        Err(why) => Err(StudyRunError::NotPersisted { outcome: Box::new(outcome), why }),
    }
}

/// The clock, as the unix SECOND `vike_runs` mints ids and stamps manifests in.
///
/// `Clock::now_ms` is milliseconds; `div_euclid` floors rather than truncating toward zero, so a
/// pre-epoch instant lands on the second that CONTAINS it instead of the one after.
fn unix_secs(clock: &dyn Clock) -> i64 {
    clock.now_ms().div_euclid(1_000)
}

/// The request's facts, minus the store and the learner it moved into the context — everything
/// [`persist`] still needs, and nothing it does not.
struct RunInputs<'a> {
    study: &'a str,
    params: &'a toml::Value,
    runs_root: &'a Path,
    produced_by: &'a str,
    git_sha: Option<String>,
    config: RunConfig,
    window: TimestampRange,
    /// Recorded because it is the fact that decides whether a FITTING study could have run at all —
    /// a run made with no learner is a different experiment, not a worse one.
    has_learner: bool,
    started_at: i64,
    finished_at: i64,
}

/// Mint, write, and hand back what was written. Split out so [`run_study_with`] can attach the
/// outcome to every failure in one place rather than at each `?`.
fn persist(
    outcome: &StudyOutcome,
    facts: RunInputs<'_>,
) -> Result<(String, PathBuf, RunManifest), StudyPersistError> {
    // Validated BEFORE a directory exists: a study whose artifact name collides costs no run id and
    // leaves nothing on disk for a listing to report as unfinished.
    let mut bodies: Vec<(&str, Vec<u8>)> = Vec::with_capacity(outcome.artifacts().len());
    for (name, body) in outcome.artifacts() {
        // A NETWORK crosses as text, the STANDARD base64 of its ONNX bytes (the learner seam's captured
        // text, `vike_ml_training::NeuralLearner`), and is written as those bytes: the file every ONNX
        // tool and `vike_runs::read_model_text` reads (decision 0292).
        let bytes = if name == MODEL_ONNX_ARTIFACT {
            match STANDARD.decode(body.trim()) {
                Ok(bytes) => bytes,
                Err(_) => {
                    return Err(StudyPersistError::InvalidArtifact {
                        name: name.clone(),
                        why: "a network's model.onnx is the base64 of its bytes, and this is not",
                    });
                }
            }
        } else {
            body.as_bytes().to_vec()
        };
        bodies.push((name, bytes));
        // ONE roster, not a list maintained here: this check named two files when the run
        // directory held two, and a study artifact called `series.json` would now silently
        // overwrite a document the same `write_run_with` call writes.
        if RESERVED_FILES.contains(&name.as_str()) {
            return Err(StudyPersistError::ReservedArtifact { name: name.clone() });
        }
        if let Err(why) = valid_artifact_name(name) {
            return Err(StudyPersistError::InvalidArtifact { name: name.clone(), why });
        }
    }

    // ⚠ `None`, matching the manifest's own `fingerprint: None` and for the same reason: a
    // study's inputs are a recipe and a window rather than a config file and a data slice, so
    // no address has been designed for them. This producer keeps the pid form it always had.
    let run =
        create_run_dir(facts.runs_root, facts.started_at, None).map_err(StudyPersistError::Run)?;

    // Artifacts join the report on the near side of the completion marker — see this module's doc —
    // and are on disk (`write_atomic` syncs them) before the manifest can name them.
    for (name, body) in bodies {
        let path = run.path.join(name);
        if let Err(e) = vike_utility::atomic_file::write_atomic(&path, &body) {
            return Err(StudyPersistError::Artifact { path, why: e.to_string() });
        }
    }

    let detail = StudyDetail {
        study: facts.study.to_string(),
        window: facts.window.into(),
        learner: facts.has_learner,
        metrics: outcome.metrics().iter().map(|(n, v)| StudyMetric::of(n, *v)).collect(),
        artifacts: outcome.artifacts().iter().map(|(n, _)| n.clone()).collect(),
        metrics_note: STUDY_METRICS_NOTE.to_string(),
    };

    let manifest = RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: STUDY_RUN_KIND.to_string(),
        produced_by: facts.produced_by.to_string(),
        started_at: utc_rfc3339(facts.started_at),
        finished_at: utc_rfc3339(facts.finished_at),
        git_sha: facts.git_sha,
        strategy_sdk: None,
        // ⚠ `None`, and permanently so far. A study's inputs are a `toml::Value` recipe and a
        // window rather than a config file plus a data slice, so the backtest producer's
        // `input_fingerprint` is not the right function for them and no study-shaped one has
        // been designed. A study that wants an address states what its inputs ARE first — and
        // that is the `research` plane's question rather than this producer's, because a study
        // fits a MODEL from a recipe instead of computing a strategy over history.
        fingerprint: None,
        config: facts.config,
        // Everything below here is a STUDY's business and NESTS — the second, structural half of
        // telling a study run from a backtest run. This module's doc argues why the metrics may not
        // be hoisted beside the common fields.
        // A typed value of strings, bools, optional numbers and arrays cannot fail to serialize; a
        // failure would still be RECORDED rather than written as an empty detail.
        detail: serde_json::to_value(&detail)
            .unwrap_or_else(|e| json!({ "unrepresentable": e.to_string() })),
        // shortcut: a study records no kept instrument row (its host's sims read them per sim); record them here
        // if a study's result is ever compared across a list refresh.
        kept_rows: Vec::new(),
    };

    // The run's own document. It repeats the metrics the manifest carries and adds what a LISTING
    // has no use for — the recipe the run was driven by. Not a second authority: one call writes
    // both, from ONE value, so they cannot come to disagree.
    let report = json!({
        "study": detail.study,
        "window": detail.window,
        // A recipe that cannot be rendered is RECORDED as unrenderable rather than written as an
        // absent one — an empty `params` would read as "this run was driven by nothing".
        "params": serde_json::to_value(facts.params)
            .unwrap_or_else(|e| json!({ "unrepresentable": e.to_string() })),
        "metrics": detail.metrics,
        "artifacts": detail.artifacts,
        "metrics_note": STUDY_METRICS_NOTE,
    });

    write_run(&run.path, &manifest, &report).map_err(StudyPersistError::Run)?;
    Ok((run.run_id, run.path, manifest))
}

/// The project's saved runs a study reads (`vike_user_research::StudyRuns`, book W21): finished runs
/// under `root`, by run id, through `vike_runs`' own readers (`finished_run_dir` holds the id to one
/// directory name). A finished run that published no forecast answers none.
struct SavedRuns {
    root: PathBuf,
}

impl StudyRuns for SavedRuns {
    fn forecasts(&self, run_id: &str) -> Result<Vec<vike_model::ForecastRow>, String> {
        let dir = vike_runs::finished_run_dir(&self.root, run_id).map_err(|e| e.to_string())?;
        match vike_runs::read_forecasts(&dir) {
            Ok(document) => Ok(document.forecasts),
            Err(vike_runs::RunReadError::Missing { .. }) => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The ledger, or the reason there is none to compare: a run whose producer kept no ledger (a
    /// validation run, a study's run) and a ledger cut to `vike_runs::MAX_TRADES` are refused in words,
    /// since a study would read either as trades that were never made.
    fn trades(&self, run_id: &str) -> Result<Vec<vike_model::Trade>, String> {
        let dir = vike_runs::finished_run_dir(&self.root, run_id).map_err(|e| e.to_string())?;
        let ledger = match vike_runs::read_trades(&dir) {
            Ok(ledger) => ledger,
            Err(vike_runs::RunReadError::Missing { .. }) => {
                return Err(format!(
                    "run {run_id} kept no trade ledger ({}): its producer saves none",
                    vike_runs::TRADES_FILE
                ));
            }
            Err(e) => return Err(e.to_string()),
        };
        if ledger.source_len > ledger.trades.len() {
            return Err(format!(
                "run {run_id} kept only its first {} of {} trades",
                ledger.trades.len(),
                ledger.source_len
            ));
        }
        Ok(ledger.trades)
    }

    /// The forward run (book chapter 22), through `vike_runs::read_forward_run`, which refuses in words a run
    /// that is not of kind `forward`.
    fn forward(&self, run_id: &str) -> Result<vike_user_research::StudyForwardRun, String> {
        let dir = vike_runs::finished_run_dir(&self.root, run_id).map_err(|e| e.to_string())?;
        let (run, ended_ms) = vike_runs::read_forward_run(&dir).map_err(|e| e.to_string())?;
        Ok(vike_user_research::StudyForwardRun {
            mount_id: run.mount_id,
            venue: run.venue,
            symbol: run.symbol,
            interval: run.interval,
            account: run.account,
            strategy_file: run.strategy_file,
            strategy_sha256: run.strategy_sha256,
            started_ms: run.started_ms,
            ended_ms,
            reference_run: run.reference_run,
        })
    }
}

#[path = "study_run_tests.rs"]
#[cfg(test)]
mod study_run_tests;
