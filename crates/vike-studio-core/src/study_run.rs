//! **Running a study, and leaving a run behind.** The half `crates/vike-user-research` cannot have.
//!
//! `crates/vike-user-research/src/contract.rs` states the split in its own words — *"the study
//! produces [`StudyOutcome`]; the caller mints the run, writes the artifacts and folds the metrics
//! into the manifest's `detail`"* — and then names the reason it cannot be that caller: run
//! persistence lives in `crates/vike-backtest/src/runs.rs`, at a layer above it. This module is
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
//! device `crates/vike-backtest/src/runs.rs`'s `RunManifest` uses and for the same reason: a struct
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
//! The CLOCK is a parameter too (`vike_model::Clock`). `crates/vike-backtest/src/runs.rs` reads no
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
//! completion marker. `crates/vike-backtest/src/runs.rs` writes the report first and the manifest
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
//! ⚠ The note states only what this writer can HONESTLY assert. It does not claim the numbers came
//! from `vike_analytics::signal_backtest`, because a study may compute them any way it likes and
//! nothing here can check. What IS structurally true is the negative: `vike-user-research` declares
//! no `vike-backtest` dependency (its own `Cargo.toml` and `src/lib.rs` argue the absence), so a
//! study cannot have reached the event-driven simulator whatever else it did.
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
use vike_backtest::runs::{
    MANIFEST_FILE, REPORT_FILE, RunConfig, RunManifest, RunPersistError, create_run_dir,
    utc_rfc3339, write_run,
};
use vike_data::TsRange;
use vike_model::Clock;
use vike_user_research::{
    StudyContext, StudyError, StudyFn, StudyLearner, StudyOutcome, valid_artifact_name,
};

use crate::run::StoreHandle;

/// The `kind` every study run declares in its manifest — the one top-level field
/// `crates/vike-backtest/src/runs.rs`'s `RunManifest` says a reader may branch on.
///
/// A bare `"study"` rather than a tier-qualified name (`"study-rust"`): the tier is a property of
/// where the SOURCE lives (`crates/vike-studio-core/src/listing.rs`'s `StudyTier`), and the spec's
/// own words are that the tiers *"differ only in REACH"*. A run produced by an interpreted study and
/// one produced by a compiled study are the same kind of result and belong in the same column.
pub const STUDY_RUN_KIND: &str = "study";

/// The sentence a study run's `detail` carries about its own numbers.
///
/// Only the structurally-true negative — see this module's doc for why it does not name a scorer.
pub const STUDY_METRICS_NOTE: &str = "Produced by a STUDY, not by the event-driven simulator: \
     vike-user-research declares no vike-backtest dependency, so no order book, no queue position \
     and no fee schedule took part in any number below. Not comparable with a `backtest` run's \
     metrics.";

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
    /// `crates/vike-model/src/state_path.rs`'s `user_runs_dir`, never here.
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
    /// An artifact whose name is one of the run directory's own two documents.
    ///
    /// `crates/vike-user-research/src/contract.rs`'s `StudyOutcome::artifact` states that this
    /// check belongs HERE: `MANIFEST_FILE` and `REPORT_FILE` are `runs.rs`'s constants, at a layer
    /// that crate may not name, and restating the two literals down there would be a second
    /// authority for a fact that already has one.
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
            Self::ReservedArtifact { name } => write!(
                f,
                "the study returned an artifact called {name:?}, which is one of the run \
                 directory's own documents ({MANIFEST_FILE}, {REPORT_FILE}) — rename it in the \
                 study"
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
        if name == MANIFEST_FILE || name == REPORT_FILE {
            return Err(StudyPersistError::ReservedArtifact { name: name.clone() });
        }
        if let Err(why) = valid_artifact_name(name) {
            return Err(StudyPersistError::InvalidArtifact { name: name.clone(), why });
        }
    }

    let run = create_run_dir(facts.runs_root, facts.started_at).map_err(StudyPersistError::Run)?;

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
        run_id: run.run_id.clone(),
        kind: STUDY_RUN_KIND.to_string(),
        produced_by: facts.produced_by.to_string(),
        started_at: utc_rfc3339(facts.started_at),
        finished_at: utc_rfc3339(facts.finished_at),
        git_sha: facts.git_sha,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::list_runs;
    use vike_data::HistStore;
    use vike_data::test_support::MemHistStore;
    use vike_ml::test_support::ScriptedLearner;
    use vike_model::Bar;

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
        _ctx: &StudyContext,
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
    fn fitting_study(
        ctx: &StudyContext,
        _params: &toml::Value,
    ) -> Result<StudyOutcome, StudyError> {
        if ctx.learner().is_none() {
            return Err(StudyError::NoLearner("the cohort model".to_string()));
        }
        let mut out = StudyOutcome::new();
        out.metric("fitted", 1.0)?;
        Ok(out)
    }

    /// A study naming an artifact that collides with the run directory's own documents.
    fn colliding_study(
        _ctx: &StudyContext,
        _params: &toml::Value,
    ) -> Result<StudyOutcome, StudyError> {
        let mut out = StudyOutcome::new();
        out.metric("rows", 1.0)?;
        out.artifact(MANIFEST_FILE, "{}")?;
        Ok(out)
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        runs_root: PathBuf,
        scratch: PathBuf,
        store: StoreHandle,
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
                window: TsRange::of(0, 60_000 * 4),
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

    /// A backtest run written by hand exactly as `crates/vike-backtest/src/runs.rs`'s `write_run`
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
        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        let listing = list_runs(&fx.runs_root);
        assert!(listing.diagnostics.is_empty(), "unexpected: {:?}", listing.diagnostics);
        let kinds: Vec<&str> = listing.runs.iter().map(|r| r.manifest.kind.as_str()).collect();
        assert_eq!(kinds, ["backtest", STUDY_RUN_KIND], "one list, two kinds, in id order");

        let listed = listing.runs.iter().find(|r| r.run_id == run.run_id).unwrap();
        assert_eq!(listed.manifest.produced_by, "studio");
        assert_eq!(listed.manifest.git_sha.as_deref(), Some("abc1234"));
        assert_eq!(listed.manifest.config.name.as_deref(), Some("baseline"));
        // Stamped from the clock the CALLER supplied, in `runs.rs`'s one spelling — asserted
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

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        let text = std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let obj = v.as_object().unwrap();
        for key in ["bars", "last_close", "metrics", "sharpe"] {
            assert!(!obj.contains_key(key), "`{key}` must not sit beside the common fields");
        }
        assert_eq!(v["detail"]["metrics"][0]["name"], json!("bars"));
        assert_eq!(v["detail"]["metrics"][0]["value"], json!(5.0));
        assert!(
            v["detail"]["metrics_note"].as_str().unwrap().contains("vike-backtest"),
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

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

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

        let run = run_study_with(nonfinite_study, fx.request("degenerate", &p, &clock)).unwrap();

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

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        assert_eq!(
            std::fs::read_to_string(run.dir.join("closes.tsv")).unwrap(),
            "ts\tclose\n0\t100\n"
        );
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(run.dir.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(v["detail"]["artifacts"], json!(["closes.tsv"]));
    }

    /// The reserved names are the WRITER's business — `contract.rs` says so and cannot enforce it,
    /// because `MANIFEST_FILE` lives at a layer that crate may not name. Refused BEFORE anything is
    /// minted, so a colliding study costs no run id and leaves no directory behind.
    #[test]
    fn an_artifact_named_like_the_manifest_is_refused_and_nothing_is_written() {
        let fx = fixture();
        let clock = fixed_clock();
        let p = params("");

        let err =
            run_study_with(colliding_study, fx.request("vol", &p, &clock)).expect_err("refused");

        match &err {
            StudyRunError::NotPersisted { outcome, why } => {
                assert!(
                    matches!(why, StudyPersistError::ReservedArtifact { name } if name == MANIFEST_FILE)
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

    /// A study's own refusal comes back TYPED, and no directory is minted — a listing must not show
    /// a run that produced nothing.
    #[test]
    fn a_study_that_refuses_leaves_no_run_directory() {
        let fx = fixture();
        let clock = fixed_clock();
        let p = params(r#"symbol = "ETHUSDT""#);

        let err =
            run_study_with(reading_study, fx.request("vol", &p, &clock)).expect_err("no bars");

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

        let run = run_study_with(reading_study, req).unwrap();

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

        let err = run_study_with(fitting_study, fx.request("cohort", &p, &clock))
            .expect_err("no learner");

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

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

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

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(run.dir.join(REPORT_FILE)).unwrap())
                .unwrap();
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

        let err =
            run_study_with(reading_study, fx.request("vol", &p, &clock)).expect_err("blocked");

        match &err {
            StudyRunError::NotPersisted { outcome, why } => {
                assert!(matches!(why, StudyPersistError::Run(RunPersistError::Dir { .. })));
                assert_eq!(outcome.metric_value("bars"), Some(5.0), "the numbers survived");
                assert_eq!(outcome.artifacts().len(), 1);
            }
            other => panic!("expected NotPersisted, got {other:?}"),
        }
    }

    /// The id mint is `runs.rs`'s, not a second one: two runs in the SAME clock second get
    /// different directories, which a bare-seconds id (the shape the research producer used) does
    /// not.
    #[test]
    fn two_study_runs_in_one_clock_second_get_different_run_directories() {
        let fx = fixture();
        let clock = fixed_clock();
        let p = params("");

        let a = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();
        let b = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        assert_ne!(a.run_id, b.run_id);
        assert!(a.run_id.starts_with("1756000000-"), "{}", a.run_id);
        assert_eq!(list_runs(&fx.runs_root).runs.len(), 2);
    }

    /// A run that FINISHED writing holds all three kinds of file, and the listing agrees it is
    /// finished. The manifest is the completion marker (`runs.rs` writes it LAST), so an artifact
    /// written after it would make a half-written run look complete — this pins the state that
    /// ordering produces rather than the ordering itself, which no post-hoc reader can observe.
    #[test]
    fn a_finished_run_holds_every_artifact_beside_its_report_and_manifest() {
        let fx = fixture();
        let clock = fixed_clock();
        let p = params("");

        let run = run_study_with(reading_study, fx.request("vol", &p, &clock)).unwrap();

        assert!(run.dir.join(MANIFEST_FILE).is_file(), "the marker is there");
        for (name, _) in run.outcome.artifacts() {
            assert!(run.dir.join(name).is_file(), "{name} must exist once the marker does");
        }
        assert!(run.dir.join(REPORT_FILE).is_file());
        let listing = list_runs(&fx.runs_root);
        assert_eq!(listing.runs.len(), 1);
        assert!(listing.diagnostics.is_empty(), "not unfinished: {:?}", listing.diagnostics);
    }

    /// The resolving arm, on a checkout with no `user_data/`: the registry is EMPTY, every name
    /// misses, and the message says the roster is empty rather than listing nothing and leaving the
    /// reader to guess why. This is the CI state, and it must be a working one.
    #[test]
    fn an_unknown_study_names_the_roster_this_build_actually_has() {
        let fx = fixture();
        let clock = fixed_clock();
        let p = params("");

        let err = run_study(fx.request("definitely_not_a_study", &p, &clock))
            .expect_err("not in the registry");

        match &err {
            StudyRunError::UnknownStudy { name, known } => {
                assert_eq!(name, "definitely_not_a_study");
                assert_eq!(
                    known.len(),
                    vike_user_research::USER_STUDIES.len(),
                    "the whole roster is offered, whatever it holds"
                );
            }
            other => panic!("expected UnknownStudy, got {other:?}"),
        }
        assert!(list_runs(&fx.runs_root).runs.is_empty(), "resolution failed before anything ran");
    }

    /// The non-finite mapping, as a unit — the three tags are the values a reader matches on.
    #[test]
    fn the_non_finite_tags_are_the_three_ieee_values_and_nothing_else() {
        assert_eq!(nonfinite_tag(f64::NAN), Some("NaN"));
        assert_eq!(nonfinite_tag(f64::INFINITY), Some("inf"));
        assert_eq!(nonfinite_tag(f64::NEG_INFINITY), Some("-inf"));
        assert_eq!(nonfinite_tag(0.0), None);
        assert_eq!(nonfinite_tag(-0.0), None);
        assert_eq!(nonfinite_tag(f64::MAX), None);
        assert_eq!(metric_json("x", 1.5), json!({"name":"x","value":1.5,"nonfinite":null}));
    }
}
