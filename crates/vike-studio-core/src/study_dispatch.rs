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

use vike_backtest::runs::RunConfig;
use vike_data::TsRange;
use vike_model::Clock;

use crate::listing::StudyTier;
use crate::rhai_study::RhaiStudy;
use crate::run::{StoreHandle, spawn_outcome};
use crate::study_run::{StudyRun, StudyRunError, StudyRunRequest, run_study};
use vike_user_research::StudyLearner;

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
    /// label. `crates/vike-backtest/src/runs.rs`'s `RunConfig` carries the "not canonicalized" rule.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::list_runs;
    use crate::spec::empty_params;
    use crate::study_run::STUDY_RUN_KIND;
    use std::sync::Arc;
    use vike_backtest::runs::{MANIFEST_FILE, REPORT_FILE};
    use vike_data::HistStore;
    use vike_data::test_support::MemHistStore;
    use vike_model::Bar;
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
                .join(vike_model::state_path::RHAI_SUBDIR)
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
        let top: Vec<&str> =
            flat.as_object().expect("an object").keys().map(|k| k.as_str()).collect();
        for name in ["metrics", "n_bars", "last_close", "sharpe"] {
            assert!(
                !top.contains(&name),
                "{name:?} must not sit at the manifest's top level: {top:?}"
            );
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
}
