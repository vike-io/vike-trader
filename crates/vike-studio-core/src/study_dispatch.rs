//! **Running a study the way a SURFACE has to run one**: address it the way
//! [`crate::listing::list_studies`] named it, and reach [`crate::study_run`]'s ONE persistence path
//! whichever tier it turned out to be.
//!
//! # Why this module exists at all
//!
//! `crates/vike-studio-core/src/study_run.rs` is the persistence half and
//! `crates/vike-studio-core/src/listing.rs` is the enumeration half, and until this file there was
//! nothing joining them. A surface — the Studio's Research pane, a headless caller — holds a
//! [`crate::listing::ListedStudy`]: a NAME, a FOLDER and a [`StudyTier`]. `run_study` takes a name
//! and resolves it through the compiled registry; [`crate::rhai_study::RhaiStudy::load`] takes a
//! folder. Neither is reachable from "the row the user clicked" without a `match` on the tier, and
//! a `match` on the tier written at every call site is the second copy of a rule that should have
//! one.
//!
//! So: one entry point, [`run_study_plan`], whose whole job is that `match` — and whose two arms
//! converge on the same persistence, so a run produced by an interpreted study and one produced by
//! a compiled study are indistinguishable on disk. That is R7's *"rhai OR rust, the author's
//! choice"* stated as behaviour rather than as intent.
//!
//! # ⚠ The interpreted tier reaches that persistence through a BRIDGE, and the bridge is a finding
//!
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study_with` says in its own doc that it is
//! separate *"because the registry is not the only way a study entry can arrive: the interpreted
//! tier resolves one at RUNTIME rather than at build time, and it must reach the same persistence
//! rather than growing a second copy of it."*
//!
//! **Its signature cannot honour that.** It takes `vike_user_research::StudyFn`, which is a bare
//! `fn` POINTER, and `crates/vike-studio-core/src/rhai_study/mod.rs`'s `RhaiStudy` is a VALUE — its
//! own `run` doc says so outright: *"It cannot BE a `StudyFn` — that type is a bare function
//! pointer, and an interpreted study is a value."* A compiled engine and AST cannot be coerced to a
//! function pointer in safe Rust, and this workspace forbids the unsafe kind
//! (`[workspace.lints.rust] unsafe_code = "forbid"`).
//!
//! [`interpreted`] is the bridge that closes the gap without touching either side: the study is
//! parked in a thread-local for exactly the duration of ONE synchronous call, and a plain `fn` item
//! reads it back. It is safe (an `Arc`, never a pointer), it is one call deep (`run_study_with`
//! invokes the entry on the calling thread and returns), and it disarms through a `Drop` guard so a
//! panicking study cannot leave the slot armed for the next one.
//!
//! ⚠ **It is still a workaround, and the real fix belongs to the other side.** `run_study_with`
//! taking `&dyn Fn(&StudyContext, &toml::Value) -> Result<StudyOutcome, StudyError>` instead of
//! `StudyFn` would delete this whole submodule and cost the compiled tier nothing (a `fn` item
//! coerces to `&dyn Fn` at the call site). That is a change to `study_run.rs`'s PUBLIC SHAPE, which
//! this change deliberately does not make — the shape is load-bearing for R6 and widening it in the
//! same breath as building the first consumer is how a contract gets bent to fit its first caller.
//! It is REPORTED instead, and this paragraph is the report.
//!
//! # What this module does NOT do
//!
//! It resolves no path — [`StudyRunPlan`] carries every directory as a field, filled in by the
//! BINARY (`crates/vike-model/src/state_path.rs`'s `user_data_dir_beside` and the `*_SUBDIR`
//! constants), for the reason `crates/vike-studio-core/src/listing.rs`'s module doc gives. It
//! constructs no store, for the reason `study_run.rs`'s does. And it supplies **no learner** — see
//! [`run_study_plan`], where that ceiling is stated rather than left to be discovered by a study
//! that wanted to fit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use vike_data::TsRange;
use vike_model::Clock;
use vike_model::runs::RunConfig;

use vike_ml::GbdtParams;

use crate::listing::StudyTier;
use crate::rhai_study::RhaiStudy;
use crate::run::{StoreHandle, spawn_outcome};
use crate::study_run::{StudyRun, StudyRunError, StudyRunRequest, run_study};
use vike_user_research::StudyLearner;

/// `YYYY-MM-DD`, `YYYY-MM-DDTHH` or bare unix SECONDS -> epoch MILLIseconds, which is what
/// [`TsRange`] speaks. Seconds are accepted because every anchor in the cohort study's own record
/// is written that way (`--end-anchor 1785906000`), and re-deriving one by hand is how a window
/// silently moves.
///
/// ⚠ **MOVED here from `crate::study_cli`, and it is a MOVE rather than a copy.** That module is
/// `#[cfg(feature = "study-cli")]` because it opens a concrete `DataFusionHist`, and the WIRE
/// runner (`crate::wire_run`'s `study_run_fn`) needs this grammar without needing DataFusion — the
/// daemon hands it an already-open store. A copy would be two answers to "what does
/// `--from 1785906000` mean", one of which only some builds compile.
pub fn boundary_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(n * 1_000);
    }
    if let Some((y, m, d, h)) = vike_model::time::parse_hour_label(s) {
        let secs = vike_model::time::days_from_civil(y, m, d) * 86_400 + i64::from(h) * 3_600;
        return Some(secs * 1_000);
    }
    let (y, m, d) = vike_model::parse_ymd(s).ok()?;
    Some(vike_model::time::days_from_civil(y, m, d) * 86_400 * 1_000)
}

/// The `[learner]` table -> the base parameter bag, and whether the recipe actually carried one.
///
/// An absent KEY keeps LightGBM's default, so a partial table is meaningful rather than an error:
/// a study that fixes two of the five says exactly that.
///
/// ⚠ MOVED here from `crate::study_cli` with [`boundary_ms`], for the same reason.
pub fn learner_params(recipe: &toml::Value) -> (GbdtParams, bool) {
    let d = GbdtParams::default();
    let Some(t) = recipe.get("learner") else { return (d, false) };
    let f = |k: &str, dv: f64| t.get(k).and_then(toml::Value::as_float).unwrap_or(dv);
    let u = |k: &str, dv: u32| {
        t.get(k).and_then(toml::Value::as_integer).and_then(|v| u32::try_from(v).ok()).unwrap_or(dv)
    };
    let p = GbdtParams {
        num_iterations: u("num_iterations", d.num_iterations),
        lambda_l1: f("lambda_l1", d.lambda_l1),
        lambda_l2: f("lambda_l2", d.lambda_l2),
        bagging_fraction: f("bagging_fraction", d.bagging_fraction),
        bagging_freq: u("bagging_freq", d.bagging_freq),
        ..d
    };
    (p, true)
}

/// The extension a study RECIPE carries — one named configuration of a study, sitting beside the
/// entry file in the study's own folder.
///
/// Both tiers document the same layout and neither RUNNER reads a recipe:
/// `crates/vike-studio-core/src/rhai_study/mod.rs`'s folder sketch calls them *"recipes: named
/// configurations, loaded by the CALLER, not by this runner"*, and
/// `crates/vike-user-research/src/codegen.rs` says the same of the compiled tier's scan. [`recipes`] is
/// that caller's half.
pub const RECIPE_EXT: &str = "toml";

/// Every recipe in a study folder, in NAME order.
///
/// Sorted for the reason `crates/vike-studio-core/src/listing.rs`'s module doc gives about
/// directory order: a picker whose entries move between two calls is one nobody can read.
///
/// An absent or unreadable folder is an EMPTY list rather than an error, and that is a narrower
/// claim than it looks: a study folder with no recipe is the ordinary state (both tiers run a study
/// with no configuration at all — see [`crate::spec::empty_params`]), so "no recipes" is not a
/// failure to report. A folder that cannot be READ is already reported by the listing that produced
/// the row this is called for; a second diagnostic here would be the same fact twice.
pub fn recipes(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case(RECIPE_EXT))
        })
        .collect();
    found.sort();
    found
}

/// Read and parse one recipe, or say why not in one line an operator can act on.
///
/// ⚠ `toml::from_str`, NOT `text.parse()`. Under the workspace's pinned `toml = "1.1"`,
/// `FromStr for Value` parses a single VALUE rather than a DOCUMENT, so a recipe with more than one
/// key comes back as `unexpected content, expected nothing` pointing at the first space —
/// `crates/vike-studio-core/src/rhai_study/mod.rs`'s own test helper carries the same warning, from
/// having been written the other way once.
pub fn read_recipe(path: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: could not be read — {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: is not valid TOML — {e}", path.display()))
}

/// ONE study run, addressed as a listing addresses it and hosted as a binary hosts it.
///
/// Public fields and no constructor, the device
/// `crates/vike-studio-core/src/study_run.rs`'s `StudyRunRequest` uses and for its reason: a struct
/// literal cannot omit a field, so a field added here stops every caller until it decides what to
/// put there.
pub struct StudyRunPlan {
    /// The study's folder name — its address in both tiers.
    /// `crates/vike-studio-core/src/listing.rs`'s `ListedStudy::name`.
    pub name: String,
    /// The study's own folder. Used by the interpreted tier (which loads from it) and recorded by
    /// neither — the compiled tier resolves through a registry and never opens it.
    pub dir: PathBuf,
    /// Which tier the listing found it in. The ONE thing this module branches on.
    pub tier: StudyTier,
    /// The recipe, already parsed. [`crate::spec::empty_params`] is the "no configuration" value.
    pub params: toml::Value,
    /// Which recipe drove the run, for the manifest — the file as the operator sees it, and its
    /// label. `crates/vike-model/src/runs.rs`'s `RunConfig` carries the "not canonicalized" rule.
    pub config: RunConfig,
    /// The store the study reads. Built by the BINARY.
    pub store: StoreHandle,
    /// The window the run is ASKED about.
    pub window: TsRange,
    /// A directory the study may write scratch into. Created by `run_study_with`; never deleted.
    pub scratch: PathBuf,
    /// `<project>/user_data/runs`.
    pub runs_root: PathBuf,
    /// The BINARY doing this, spelled literally — `RunManifest::produced_by`'s rule.
    pub produced_by: String,
    /// The commit that binary was built from, or `None` when it cannot name one.
    /// The learner a FITTING study is handed, or `None` for a host that cannot fit.
    ///
    /// ⚠ Present-and-`Option` rather than absent, and that is the change this field IS. It used to
    /// be absent, with `run_study_plan` hardcoding `None`, because the only host was the Studio —
    /// whose ML surface is the INFERENCE half only, so a fitting study could answer nothing but
    /// `StudyError::NoLearner`. That doc set the condition for this field's arrival in as many
    /// words: *"the day a Studio host CAN fit, the field arrives here and every caller is stopped
    /// until it decides"*. `crates/vike-cli`'s `study run` verb is that host, so the field is here
    /// and every caller is stopped — the Studio now passes `None` explicitly, which says the same
    /// thing its hardcode did while letting a host that CAN fit say otherwise.
    pub learner: Option<Arc<dyn StudyLearner>>,
    pub git_sha: Option<String>,
}

/// Resolve the tier, run the study, and leave a run behind — the ONE entry point a surface needs.
///
/// ⚠ **The learner now comes from the PLAN, and that is a change of ceiling rather than of
/// plumbing.** This doc used to say no learner was supplied "on purpose", because the only host
/// was the Studio: `crates/vike-studio-core/src/ml.rs`'s module doc says that crate's ML surface is
/// *"the INFERENCE half only"*, so a study that must FIT could answer nothing but
/// `vike_user_research::StudyError::NoLearner`. It also set the condition for the change: *"the day
/// a Studio host CAN fit, the field arrives here and every caller is stopped until it decides"*.
///
/// `crates/vike-cli`'s `study run` verb is that host — it constructs a `vike_ml::GbdtLearner` over
/// the pinned LightGBM binary — so [`StudyRunPlan::learner`] exists and every caller was stopped.
/// The Studio passes `None` explicitly, which asserts exactly what its hardcode used to assume,
/// while leaving a fitting host able to say otherwise. `None` remains a working state, not a
/// degraded one: a run made with no learner is a different experiment, not a worse one, and
/// `run_study`'s manifest records which it was.
///
/// The window, the scratch directory and the runs root all come from the plan, so this function
/// resolves nothing and reads no environment.
pub fn run_study_plan(plan: StudyRunPlan, clock: &dyn Clock) -> Result<StudyRun, StudyRunError> {
    let StudyRunPlan {
        name,
        dir,
        tier,
        params,
        config,
        store,
        window,
        scratch,
        runs_root,
        produced_by,
        git_sha,
        learner,
    } = plan;
    let req = StudyRunRequest {
        study: &name,
        params: &params,
        store,
        learner,
        window,
        scratch,
        runs_root: &runs_root,
        produced_by: &produced_by,
        git_sha,
        config,
        clock,
    };
    match tier {
        // The generated registry's own lookup, refusal included: in a checkout with no
        // `user_data/` — every CI runner, every fresh clone — that registry is EMPTY and every name
        // answers `StudyRunError::UnknownStudy` naming the whole (empty) roster. `run_study`'s doc
        // calls that a working state rather than a degenerate one.
        StudyTier::Rust => run_study(req),
        // Compiled at RUN time from the folder, then through the same persistence. A load failure
        // is the study's own refusal — a folder with no entry file holds nothing that would ever
        // run, which `RhaiStudy::load` says in the words it fails with.
        StudyTier::Rhai => {
            let study = RhaiStudy::load(&dir)
                .map_err(|why| StudyRunError::Study { name: name.clone(), why })?;
            interpreted::run_persisted(study, req)
        }
    }
}

/// [`run_study_plan`] on a worker thread; the single result arrives on the returned receiver.
///
/// Through `crates/vike-studio-core/src/run.rs`'s `spawn_outcome`, which that function's doc calls
/// *"the ONE `channel()` + `thread::spawn` + `tx.send(f())` site behind every `spawn_*` twin"* — so
/// a study dispatch is the same shape a Run, a Sweep and a Walk-Forward already are, and a UI polls
/// it with the `try_recv()` it already writes.
///
/// The clock is the wall clock (`vike_model::now_ms`), because a real run is stamped with the real
/// instant it happened at. [`run_study_plan`] keeps the seam so a test mints against a fixed one.
pub fn spawn_study(plan: StudyRunPlan) -> Receiver<Result<StudyRun, StudyRunError>> {
    spawn_outcome(move || {
        let clock = vike_model::now_ms;
        run_study_plan(plan, &clock)
    })
}

/// The bridge from an interpreted study (a VALUE) to `run_study_with` (which takes a `fn` POINTER).
///
/// See this module's doc for why it exists, why it is safe, and what would delete it.
mod interpreted {
    use std::cell::RefCell;
    use std::sync::Arc;

    use vike_user_research::{StudyContext, StudyError, StudyOutcome};

    use crate::rhai_study::RhaiStudy;
    use crate::study_run::{StudyRun, StudyRunError, StudyRunRequest, run_study_with};

    /// What [`entry`] answers when it is reached with no study parked — a BUG in this module, and
    /// worded as one so it is never mistaken for a study's own refusal.
    pub(super) const UNARMED: &str = "the interpreted study bridge was reached with no study \
                                      parked — this is a bug in vike-studio-core's \
                                      study_dispatch, not in the study";

    thread_local! {
        /// The study the NEXT call to [`entry`] on THIS thread runs.
        ///
        /// An `Arc` rather than a raw pointer: this workspace forbids `unsafe`, and a shared owner
        /// costs one refcount bump per run against a study that has just been compiled from source.
        static PARKED: RefCell<Option<Arc<RhaiStudy>>> = const { RefCell::new(None) };
    }

    /// Clears [`PARKED`] on the way out, however the way out happens.
    ///
    /// A plain assignment after the call would be skipped by an unwinding panic, and the next study
    /// dispatched on this thread would then run the PREVIOUS study's code under the current
    /// study's name — a wrong ANSWER rather than a failure, which is the class of defect worth
    /// spending a `Drop` impl on.
    struct Disarm;

    impl Drop for Disarm {
        fn drop(&mut self) {
            PARKED.with(|p| *p.borrow_mut() = None);
        }
    }

    /// The `vike_user_research::StudyFn` the interpreted tier reaches persistence through.
    ///
    /// A `fn` ITEM (so it coerces to the pointer type) whose body is one thread-local read. It is
    /// called exactly once per [`run_persisted`], synchronously, on the thread that parked the
    /// study — `run_study_with` invokes the entry inline and returns.
    ///
    /// `pub(super)` so the parent module's tests can reach the unarmed path, which is the only way
    /// to prove that branch is a named bug report rather than a panic.
    pub(super) fn entry(
        ctx: &StudyContext,
        params: &toml::Value,
    ) -> Result<StudyOutcome, StudyError> {
        // Cloned out of the borrow before running: the study is free to do anything except
        // re-enter this function, and holding a `RefCell` borrow across a call it does not control
        // would turn a hypothetical re-entry into a panic instead of the named refusal above.
        let parked = PARKED.with(|p| p.borrow().clone());
        match parked {
            Some(study) => study.run(ctx, params),
            None => Err(StudyError::Study(UNARMED.to_string())),
        }
    }

    /// Park `study`, run it through `run_study_with`, and disarm.
    pub(super) fn run_persisted(
        study: RhaiStudy,
        req: StudyRunRequest<'_>,
    ) -> Result<StudyRun, StudyRunError> {
        PARKED.with(|p| *p.borrow_mut() = Some(Arc::new(study)));
        let _disarm = Disarm;
        run_study_with(entry, req)
    }
}

#[path = "study_dispatch_tests.rs"]
#[cfg(test)]
mod study_dispatch_tests;
