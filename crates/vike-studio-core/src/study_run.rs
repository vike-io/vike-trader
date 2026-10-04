//! **Running a study, and leaving a run behind.** The half `crates/vike-user-research` cannot have.
//!
//! `crates/vike-user-research/src/contract.rs` states the split in its own words — *"the study
//! produces [`StudyOutcome`]; the caller mints the run, writes the artifacts and folds the metrics
//! into the manifest's `detail`"* — and then names the reason it cannot be that caller: run
//! persistence lives in `crates/vike-model/src/runs.rs`, at a layer above it. This module is
//! that caller.
//!
//! # Why this crate, stated as arithmetic
//!
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md` carries the argument and
//! the numbers. The short form: minting a run means naming `vike-backtest`, and the down-only
//! dependency rule (`crates/vike-ops/tests/layer_gate.rs`) puts anything that can do so at rank 50
//! or above — which rules out the study host, and ruled out `vike-research`, the crate whose name
//! made it the intuitive answer (rank 45; it has since been dissolved and deleted outright, so the
//! candidate is gone as well as disqualified). Of the crates that remain, this is the one that
//! also already owns
//! the listing those runs must appear in (`crates/vike-studio-core/src/listing.rs`'s `list_runs`).
//! Nothing else can see both ends.
//!
//! # What it takes, and what it constructs
//!
//! Everything, as parameters — [`StudyRunRequest`] has public fields and no constructor, the same
//! device `crates/vike-model/src/runs.rs`'s `RunManifest` uses and for the same reason: a struct
//! literal cannot omit a field, so a field added here stops every caller until it decides what to
//! put there.
//!
//! ⚠ **The store is a PARAMETER and this module constructs none**, which is not a preference but
//! the condition of a CI lane: a default build of this crate is DataFusion-free
//! (`scripts/ci_feature_suite.sh`'s `studio-standalone`, whose structural half greps the default
//! normal dep tree for a datafusion crate and fails if it finds one), and the concrete
//! `DataFusionHist` lives behind a feature this crate enables only in `[dev-dependencies]`. So the
//! request carries a [`StoreHandle`] the BINARY built. That is the same discipline
//! `crates/vike-user-research/src/contract.rs` applies one layer down, for a stronger reason there.
//!
//! The CLOCK is a parameter too (`vike_model::Clock`). `crates/vike-model/src/runs.rs` reads no
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
//! `crates/vike-backtest/src/backtest_cli.rs`'s `persist_run` does: a study that refuses leaves no
//! empty run directory for a listing to report as unfinished.
//!
//! **The artifacts are written before the manifest**, joining the report on the near side of the
//! completion marker. `crates/vike-model/src/runs.rs` writes the report first and the manifest
//! last precisely so a directory holding a manifest is a run that finished writing; artifacts
//! written afterwards would break that, and a listing would call a half-written run complete.
//!
//! **A persistence failure never costs the numbers.** `runs.rs` states the rule — *"Saving a run is
//! worth doing; it is not worth losing a run over"* — and here it has to be carried in a type,
//! because this module is the thing that computed them: [`StudyRunError::NotPersisted`] hands the
//! [`StudyOutcome`] back beside the reason it could not be stored.
//!
//! # How a study run is told apart from a backtest run — TWO ways, and the second is structural
//!
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` spends a ⚠ on this: a
//! study's Sharpe is vectorized (hysteresis, min-hold, slippage — no order book, no queue position,
//! no fee schedule) and a backtest's is event-driven with real fills. *"Showing both in one `runs/`
//! list is what makes the gap between them visible rather than assumed"* — visible, which means the
//! surface must not let them merge.
//!
//! * **[`STUDY_RUN_KIND`] as the manifest's `kind`.** The one top-level field `runs.rs` says a
//!   reader may branch on. It is what puts a study row and a backtest row in one list at all.
//! * **The metrics NEST under `detail.metrics` and are never hoisted beside the common fields.**
//!   This is the half that does the work. A top-level `sharpe` would have been renderable in a
//!   shared column by a listing that never asked which scorer produced it — the two numbers would
//!   have become one number, in a column, permanently. Nested, a reader must branch on `kind` and
//!   reach into a kind-specific subtree to get the number at all, which is exactly the moment it
//!   has to know which claim it is holding.
//! * …and [`STUDY_METRICS_NOTE`] rides in the same subtree, because a manifest is also read ALONE —
//!   copied out of its directory, opened by hand — where no listing is present to carry the
//!   distinction for it.
//!
//! ⚠ The note states only what this writer can HONESTLY assert, and it used to overclaim: it said
//! a study "cannot have reached the event-driven simulator", arguing from `vike-user-research`
//! declaring no `vike-backtest`/`vike-sim` dependency. That premise never covered the actual path —
//! `run_study` (below) hands EVERY study the backtest seam unconditionally
//! (`ctx.with_sim(Arc::new(harness_sim::HarnessSim))`), which drives the full event-driven harness
//! through an erased `StudySim` trait `vike-user-research` DOES declare. A study that calls
//! `ctx.sim()` reaches the real order book, queue position and fee schedule with no dependency
//! edge to show for it, and 0076's `poly_mm` study is exactly that case. So the note asserts only
//! what is actually knowable from here: the numbers were produced by a study rather than by a
//! `backtest` run, and whether the seam was called is not something this manifest records.
//!
//! # Non-finite metrics are RECORDED, and JSON is where that nearly went wrong
//!
//! `crates/vike-user-research/src/contract.rs`'s `StudyOutcome::metric` accepts a non-finite value
//! deliberately: a `NaN` Sharpe over a fold that never traded is an observation, and rounding it to
//! zero is the `unwrap_or(0.0)` that turns a gap into a number.
//!
//! ⚠ **`serde_json` writes a non-finite `f64` as `null`, silently** — `serde_json::Number::from_f64`
//! refuses one and the serializer maps the refusal to `Null`. That is the same defect arriving at
//! the last possible moment: three distinct observations (`NaN`, `inf`, `-inf`) and an absent value
//! all reaching disk as one token. So every metric is written as an OBJECT carrying both halves —
//! `value` (a number, or `null` when there is no number to write) and `nonfinite` (which one it
//! was, or `null`) — and [`metric_json`] is the one place that mapping is spelled.
//!
//! # …and why metrics are an ARRAY rather than an object
//!
//! `StudyOutcome` keeps metrics in EMISSION order on purpose — its own doc: *"a study puts its
//! headline first, and a `BTreeMap` would file `sharpe` after `n_trades` for no reason anyone
//! chose"*. This workspace pins `serde_json` without `preserve_order` (root `Cargo.toml`), so a
//! `serde_json::Map` IS a `BTreeMap` and an object would sort that order away at the last step. An
//! array preserves it, and it is the only shape that can.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use vike_data::TsRange;
use vike_model::Clock;
use vike_model::runs::{
    MANIFEST_SCHEMA, RESERVED_FILES, RunConfig, RunManifest, RunPersistError, create_run_dir,
    utc_rfc3339, write_run,
};
use vike_user_research::{
    StudyContext, StudyError, StudyFn, StudyLearner, StudyOutcome, valid_artifact_name,
};

use crate::run::StoreHandle;

/// The `kind` every study run declares in its manifest — the one top-level field
/// `crates/vike-model/src/runs.rs`'s `RunManifest` says a reader may branch on.
///
/// A bare `"study"` rather than a tier-qualified name (`"study-rust"`): the tier is a property of
/// where the SOURCE lives (`crates/vike-studio-core/src/listing.rs`'s `StudyTier`), and the spec's
/// own words are that the tiers *"differ only in REACH"*. A run produced by an interpreted study and
/// one produced by a compiled study are the same kind of result and belong in the same column.
pub const STUDY_RUN_KIND: &str = "study";

/// The sentence a study run's `detail` carries about its own numbers.
///
/// Only the structurally-true negative — see this module's doc for why it does not name a scorer.
pub const STUDY_METRICS_NOTE: &str = "Produced by a STUDY, not by a `backtest` run: the study \
     computed these numbers its own way, and may have called the event-driven simulator through \
     the host's backtest seam, which this manifest does not record. Not comparable with a \
     `backtest` run's metrics.";

/// Everything one study run needs. Public fields and no constructor, on purpose — see this
/// module's doc.
pub struct StudyRunRequest<'a> {
    /// The study's registry name, which is also its folder name under
    /// `<project>/user_data/research/studies/rust/`. Recorded as `detail.study`.
    pub study: &'a str,
    /// The recipe: one of the study folder's `.toml` files, already parsed. Recorded verbatim in
    /// the report, so a run says which configuration produced it rather than only which FILE did.
    pub params: &'a toml::Value,
    /// The store the study reads. Built by the BINARY — this module constructs none.
    pub store: StoreHandle,
    /// The learner, when this host has one. `None` is the documented ceiling of a box with no
    /// LightGBM binary, and a study that must fit says so with `StudyError::NoLearner` rather than
    /// degrading into a number.
    pub learner: Option<Arc<dyn StudyLearner>>,
    /// The window the run is ASKED about. Recorded in the manifest so a listing can show it without
    /// parsing a study's own configuration.
    pub window: TsRange,
    /// A directory the study may write scratch into.
    ///
    /// Created here when absent, and deliberately NOT deleted afterwards:
    /// `crates/vike-user-research/src/contract.rs`'s `StudyContext::new` calls it *"a directory the
    /// caller owns and may delete afterwards"*, and the owner is whoever chose the path. Anything a
    /// run must KEEP is an artifact, which lands in the run directory instead.
    pub scratch: PathBuf,
    /// `<project>/user_data/runs` — resolved by the BINARY through
    /// `crates/vike-model/src/paths/state_path.rs`'s `user_runs_dir`, never here.
    pub runs_root: &'a Path,
    /// The BINARY that is running this, spelled literally — `runs.rs`'s `RunManifest::produced_by`
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
    /// No study of that name is in the generated registry. `known` is the whole roster, because the
    /// overwhelmingly likely cause is a typo or a checkout with no `user_data/` — and in the second
    /// case an EMPTY roster is the answer, which a message naming it makes obvious.
    UnknownStudy {
        /// The name that was asked for.
        name: String,
        /// Every study the registry does hold.
        known: Vec<String>,
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
    /// discarded — this module's doc has the rule and `runs.rs` has the original.
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
    /// `crates/vike-user-research/src/contract.rs`'s `StudyOutcome::artifact` states that this
    /// check belongs HERE: the names are `runs.rs`'s constants, at a layer that crate may not name,
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
            Self::UnknownStudy { name, known } if known.is_empty() => write!(
                f,
                "no study called {name:?}: this build's study registry is EMPTY. A compiled study \
                 lives at <user_data>/research/studies/rust/<name>/<name>.rs and is scanned at \
                 BUILD time, so a study added since this binary was compiled is not in it"
            ),
            Self::UnknownStudy { name, known } => {
                write!(f, "no study called {name:?} — this build knows: {}", known.join(", "))
            }
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

/// Resolve a study BY NAME through `vike-user-research`'s generated registry, run it, and leave a
/// run behind.
///
/// ⚠ In a checkout with no `user_data/` — every CI runner, every fresh clone — that registry is
/// EMPTY by construction and every name answers [`StudyRunError::UnknownStudy`]. That is a working
/// state rather than a degenerate one, and it is why the running half lives in
/// [`run_study_with`]: the resolver is one lookup, the runner is everything else, and only the
/// second can be proven on a box that hosts no user code.
pub fn run_study(req: StudyRunRequest<'_>) -> Result<StudyRun, StudyRunError> {
    let Some(entry) = vike_user_research::user_study_entry(req.study) else {
        return Err(StudyRunError::UnknownStudy {
            name: req.study.to_string(),
            known: vike_user_research::USER_STUDIES.iter().map(|s| s.to_string()).collect(),
        });
    };
    run_study_with(entry, req)
}

/// Run an ALREADY-RESOLVED study entry and leave a run behind — everything [`run_study`] does
/// except the name lookup.
///
/// Separate for the reason [`run_study`] states, and also because the registry is not the only way
/// a study entry can arrive: the interpreted tier resolves one at RUNTIME rather than at build
/// time, and it must reach the same persistence rather than growing a second copy of it.
pub fn run_study_with(entry: StudyFn, req: StudyRunRequest<'_>) -> Result<StudyRun, StudyRunError> {
    // Destructured rather than field-accessed: the store and the learner are MOVED into the
    // context below, and a partially-moved struct cannot then be borrowed for the manifest.
    let StudyRunRequest {
        study,
        params,
        store,
        learner,
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
    // Every study gets the backtest seam, unconditionally: this crate links the simulator
    // already, so there is no host here that cannot supply one — unlike the learner, whose
    // absence on a box with no LightGBM binary is the ordinary case. A study that never asks
    // pays nothing; the seam is one `Arc` in a context it already builds.
    ctx = ctx.with_sim(std::sync::Arc::new(crate::harness_sim::HarnessSim));

    let outcome =
        entry(&ctx, params).map_err(|why| StudyRunError::Study { name: study.to_string(), why })?;

    // Read AFTER the work and before anything is written, so the pair measures the study rather
    // than the disk — `runs.rs`'s `RunManifest::finished_at` makes the same distinction.
    let finished_at = unix_secs(clock);

    let facts = RunFacts {
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

/// The clock, as the unix SECOND `runs.rs` mints ids and stamps manifests in.
///
/// `Clock::now_ms` is milliseconds; `div_euclid` floors rather than truncating toward zero, so a
/// pre-epoch instant lands on the second that CONTAINS it instead of the one after.
fn unix_secs(clock: &dyn Clock) -> i64 {
    clock.now_ms().div_euclid(1_000)
}

/// The request's facts, minus the store and the learner it moved into the context — everything
/// [`persist`] still needs, and nothing it does not.
struct RunFacts<'a> {
    study: &'a str,
    params: &'a toml::Value,
    runs_root: &'a Path,
    produced_by: &'a str,
    git_sha: Option<String>,
    config: RunConfig,
    window: TsRange,
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
    facts: RunFacts<'_>,
) -> Result<(String, PathBuf, RunManifest), StudyPersistError> {
    // Validated BEFORE a directory exists: a study whose artifact name collides costs no run id and
    // leaves nothing on disk for a listing to report as unfinished.
    for (name, _) in outcome.artifacts() {
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

    // Artifacts join the report on the near side of the completion marker — see this module's doc.
    for (name, body) in outcome.artifacts() {
        let path = run.path.join(name);
        if let Err(e) = std::fs::write(&path, body) {
            return Err(StudyPersistError::Artifact { path, why: e.to_string() });
        }
    }

    let metrics = metrics_json(outcome);
    let artifacts: Vec<&str> = outcome.artifacts().iter().map(|(n, _)| n.as_str()).collect();

    let manifest = RunManifest {
        schema: MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: STUDY_RUN_KIND.to_string(),
        produced_by: facts.produced_by.to_string(),
        started_at: utc_rfc3339(facts.started_at),
        finished_at: utc_rfc3339(facts.finished_at),
        git_sha: facts.git_sha,
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
        detail: json!({
            "study": facts.study,
            "window": window_json(facts.window),
            "learner": facts.has_learner,
            "metrics": metrics.clone(),
            "artifacts": artifacts.clone(),
            "metrics_note": STUDY_METRICS_NOTE,
        }),
    };

    // The run's own document. It repeats the metrics the manifest carries and adds what a LISTING
    // has no use for — the recipe the run was driven by. Not a second authority: one call writes
    // both, from ONE value (cloned, never recomputed), so they cannot come to disagree.
    let report = json!({
        "study": facts.study,
        "window": window_json(facts.window),
        // A recipe that cannot be rendered is RECORDED as unrenderable rather than written as an
        // absent one — an empty `params` would read as "this run was driven by nothing".
        "params": serde_json::to_value(facts.params)
            .unwrap_or_else(|e| json!({ "unrepresentable": e.to_string() })),
        "metrics": metrics,
        "artifacts": artifacts,
        "metrics_note": STUDY_METRICS_NOTE,
    });

    write_run(&run.path, &manifest, &report).map_err(StudyPersistError::Run)?;
    Ok((run.run_id, run.path, manifest))
}

/// The window, as the two nullable bounds `vike_data::TsRange` actually is. `null` is "unbounded",
/// which is a different answer from "zero" and must not be written as one.
fn window_json(w: TsRange) -> Value {
    json!({ "start": w.start, "end": w.end })
}

/// Every metric, in EMISSION order — see this module's doc for why an array.
fn metrics_json(outcome: &StudyOutcome) -> Value {
    Value::Array(outcome.metrics().iter().map(|(n, v)| metric_json(n, *v)).collect())
}

/// One metric as it lands on disk: `{ "name": …, "value": <number|null>, "nonfinite": <tag|null> }`.
///
/// The ONE place the non-finite mapping is spelled. See this module's doc for why `serde_json`'s own
/// treatment of a non-finite `f64` is the thing this shape exists to refuse.
pub fn metric_json(name: &str, value: f64) -> Value {
    json!({
        "name": name,
        "value": serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        "nonfinite": nonfinite_tag(value),
    })
}

/// Which non-finite value this is, or `None` for an ordinary number.
///
/// The three tags are IEEE-754's own names, so a reader that has to reconstruct the value has an
/// unambiguous token to match on rather than a rendering that varies by language.
pub fn nonfinite_tag(value: f64) -> Option<&'static str> {
    if value.is_nan() {
        Some("NaN")
    } else if value == f64::INFINITY {
        Some("inf")
    } else if value == f64::NEG_INFINITY {
        Some("-inf")
    } else {
        None
    }
}

#[path = "study_run_tests.rs"]
#[cfg(test)]
mod study_run_tests;
