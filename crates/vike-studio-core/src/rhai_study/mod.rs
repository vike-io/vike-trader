//! [`RhaiStudy`] — the INTERPRETED user-study tier, the twin of the compiled one in
//! `crates/vike-user-research`.
//!
//! ```text
//! <project>/user_data/research/studies/rhai/<name>/
//! ├─ <name>.rhai      entry file — stem MUST equal the folder name
//! └─ *.toml           recipes: named configurations, loaded by the CALLER, not by this runner
//! ```
//!
//! ```rhai
//! fn run(ctx, params) {
//!     let bars = ctx.bars("binance", "BTCUSDT", "1h");
//!     let out = outcome();
//!     out.metric("n_bars", bars.len());
//!     return out;
//! }
//! ```
//!
//! That is `crates/vike-user-research/src/contract.rs`'s `StudyFn` with the types erased — the same
//! two arguments in the same order, the same return value, the same three failure kinds. R7 says a
//! study may be written in Rhai OR in Rust; this is the half a shipped BINARY can run, and the
//! compiled half is the one that needs a checkout.
//!
//! # Why this lives in `vike-studio-core` and not beside its strategy twin
//!
//! `crates/vike-script` is layer 20 and would be the obvious home — it is where `RhaiStrategy`
//! lives. It cannot be: the contract types are in `vike-user-research` at layer 25, and layer 20
//! may not name layer 25 (`crates/vike-ops/tests/architecture/layer_gate.rs`). Moving the contract types DOWN to
//! reach it would undo the argument `crates/vike-user-research/src/lib.rs` makes for keeping them
//! in the crate whose whole purpose is the contract — *"When a second study exists and a lower
//! consumer needs the vocabulary without the host, that is the moment to move them down, not
//! before"*.
//!
//! This crate is layer 35 and already depends on `vike-script`, so it can see both. Nothing moved,
//! no user file changed, and `vike-script` is untouched.
//!
//! # ⚠ The enforcement asymmetry — the one thing worth carrying away from this file
//!
//! `contract.rs` is careful to say that the Rust tier's ADR-0029 guarantee is **the sanctioned
//! surface, not enforcement**: a user's `.rs` is ordinary Rust compiled into the operator's own
//! binary, and nothing stops it opening a socket — what the contract buys is that the cheapest path
//! is the correct one and an unsanctioned one has to be added to a manifest, in a diff.
//!
//! **In this tier that same rule IS enforcement.** A script can only call what `bind.rs` registers.
//! There is no `use`, no manifest, no FFI, and — after `bind::build_engine` replaces rhai's default
//! module resolver — no `import` either. Rhai's standard packages carry no filesystem and no
//! network surface at all, so the registration list is the complete set of things a study can
//! reach, and it is reviewable in one file.
//!
//! Three consequences worth stating rather than leaving to be discovered:
//!
//! * The Rust tier is the one to widen when a study needs something new; this tier is where a
//!   REFUSAL is actually a refusal. A capability added here is added deliberately.
//! * ⚠ The default `rhai::Engine` is NOT the safe object. It installs a `FileModuleResolver`, so
//!   `import "…" as m;` reads a file from disk — a capability no registration list would show.
//!   `bind::build_engine` disables it, and a test gates that. **`crates/vike-script`'s strategy
//!   and user-indicator engines now do the same** — that used to be an open finding recorded here,
//!   and it was closed on the same argument: a strategy is not under ADR 0029, but nobody had
//!   decided that a strategy script may read arbitrary files either, and
//!   `docs/decisions/0024-rhai-strategies-live.md` names filesystem access as a thing that would
//!   REOPEN the verdict letting a script mount live. `crates/vike-script/src/engine/host.rs`'s
//!   `build_engine` carries that crate's half of the argument.
//! * One DECLARED residual: rhai's `time` package registers `timestamp()`, so a study can read a
//!   wall clock and therefore be non-reproducible. It reaches no data and no network; it is left
//!   registered because removing it means naming each function of a package by hand, and the
//!   reproducibility question belongs to whatever eventually records a run's inputs.
//!
//! # What this tier can reach that the Rust tier can, and the three places they differ
//!
//! Same: every read verb (`bars`/`quotes`/`trades`/`book_updates`/`depth`), the run's `window`, the
//! learner, `StudyOutcome`'s metrics and text artifacts, and the three `StudyError` kinds —
//! including their TYPED payloads, which survive the interpreter through [`fault`].
//!
//! Different, each deliberately:
//!
//! 1. **`ctx.scratch()` is absent.** The Rust tier has it because a Rust study can write a file. A
//!    script cannot — there is no file verb in this tier at all — so a scratch PATH would be a
//!    string no other verb accepts. A verb whose value nothing can consume is worse than a missing
//!    one; that is the same rule `vike_config::CONSUMPTION` applies to a settings key nothing
//!    reads.
//! 2. **`fit_identity` is absent.** The third method of
//!    `vike_user_research::StudyLearner`, whose erasure that trait deliberately carries in full. It
//!    is the key half of a cross-run fit CACHE, and a study does not own that cache — its caller
//!    does. Exposing a 32-byte digest to a script that has nothing to key would be a surface with
//!    no consumer. ⚠ This is a real narrowing and it is stated here rather than implied: the day a
//!    study needs to key its own cache, this is the line to revisit.
//! 3. **The folder-name charset is WIDER.** The Rust tier requires `[a-z][a-z0-9_]*` because the
//!    folder name becomes a Rust module name (`crates/vike-user-research/src/codegen.rs`'s
//!    `valid_name`). That is a property of Rust, not of studies, and imposing it here would refuse
//!    folders `crates/vike-studio-core/src/listing.rs`'s `list_studies` already lists — its own
//!    test uses `vol-clustering`.
//!
//! # What this module deliberately does NOT do
//!
//! It resolves no path, mints no run id, writes no artifact and consults no registry. [`RhaiStudy`]
//! is handed ONE study folder by a caller that already knows where `user_data` is (the binary, via
//! `vike_model::paths::state_path::user_rhai_studies_dir`, or `list_studies`' `ListedStudy::dir`), and
//! hands back a `StudyOutcome` for that caller to persist. The split is
//! `crates/vike-user-research/src/contract.rs`'s, for its reasons: `crates/vike-model/src/runs.rs`
//! owns run persistence for every kind of run, and a second minting site would be a second copy of
//! a schema whose whole point is that there is one.
//!
//! # No `user_data/` ⇒ inert
//!
//! Structurally, not by arrangement: there is no build-time scan and no process-wide registry here,
//! so a checkout with no `user_data/` compiles and behaves byte-identically — nothing is
//! registered, because nothing is loaded until a caller names a folder. [`RhaiStudy::load`] on an
//! absent folder is a NAMED refusal rather than a panic or a silent empty study. The pipeline is
//! still CI-proven on every run through the committed
//! `crates/vike-studio-core/tests/fixture_user_data/` tree, the same way the compiled tier proves
//! its own.

mod bind;
mod fault;

use std::path::{Path, PathBuf};

use vike_user_research::{StudyContext, StudyError, StudyOutcome};

pub use bind::{FIT_PARAM_KEYS, MAX_CALL_LEVELS, MAX_OPERATIONS, StudyFit};

/// The entry function every Rhai study defines: `fn run(ctx, params)`.
///
/// The same NAME the Rust tier's entry has, and the same two parameters in the same order — so a
/// study author porting between tiers moves a body, not a contract.
pub const ENTRY_FN: &str = "run";

/// The entry file's extension. Compared case-INSENSITIVELY by [`RhaiStudy::load`], the same rule
/// `crates/vike-studio-core/src/user_strategies/load.rs` uses and for the same reason: a Windows
/// editor that saved `MyStudy.RHAI` wrote a real study, and refusing it on case would be a mystery
/// rather than a message.
pub const RHAI_EXT: &str = "rhai";

/// A compiled Rhai study, ready to run any number of times.
///
/// Holds its own engine and AST (`crates/vike-script/src/strategy.rs`'s `RhaiStrategy` shape),
/// plus the scope the one-time top-level run produced and the fault log [`fault`] recovers typed
/// errors through.
pub struct RhaiStudy {
    name: String,
    engine: rhai::Engine,
    ast: rhai::AST,
    /// The AST's top level (anything outside a `fn`), evaluated EXACTLY ONCE by [`RhaiStudy::new`]
    /// — see its doc. [`RhaiStudy::run`] clones this per call so a run's own locals never
    /// accumulate here.
    scope: rhai::Scope<'static>,
    faults: fault::FaultLog,
}

impl std::fmt::Debug for RhaiStudy {
    /// Hand-written because neither `rhai::Engine` nor `rhai::AST` is `Debug`. The name is the
    /// whole of what an operator reading a log needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhaiStudy").field("name", &self.name).finish()
    }
}

impl RhaiStudy {
    /// Compile `src` as the study called `name`.
    ///
    /// ⚠ **The top level runs ONCE, here** — `Engine::run_ast_with_scope` — and never again, which
    /// is why [`RhaiStudy::run`] calls with `eval_ast(false)`. `Engine::call_fn`'s default
    /// `eval_ast: true` RE-EVALUATES the AST's top-level statements into the scope before every
    /// call, which is how a bare top-level order verb came to resubmit an order per bar in the
    /// strategy tier (`crates/vike-script/src/strategy.rs`'s module doc carries the incident). It
    /// is less dangerous here — a study places no orders — but a top-level statement that
    /// re-executed per run would still make two runs of one study differ, and a top-level `const`
    /// must stay visible inside `run`'s body either way, which is exactly what the persisted scope
    /// buys.
    ///
    /// A script with no `fn run(ctx, params)` is refused HERE, naming the contract. That is the
    /// `MissingEntry` ruling `crates/vike-user-research/src/codegen.rs` makes for the compiled tier and
    /// the "no `on_bar`" ruling `crates/vike-script/src/load.rs` makes for an indicator: a file with
    /// the wrong entry is not an empty study, it is a study nothing will ever call, and silence is
    /// how the strategy tier spent months being mistaken for a working mechanism.
    pub fn new(name: &str, src: &str) -> Result<Self, StudyError> {
        let faults = fault::new_log();
        let engine = bind::build_engine(&faults);
        let ast = engine
            .compile(src)
            .map_err(|e| StudyError::Study(format!("{name}: did not compile — {e}")))?;
        if !ast.iter_functions().any(|f| f.name == ENTRY_FN && f.params.len() == 2) {
            return Err(StudyError::Study(format!(
                "{name}: no `fn {ENTRY_FN}(ctx, params)`. A study is called ONCE with the context \
                 and its recipe, and returns an outcome — the same entry the Rust tier has.",
            )));
        }
        let mut scope = rhai::Scope::new();
        engine.run_ast_with_scope(&mut scope, &ast).map_err(|e| {
            StudyError::Study(format!("{name}: its top-level statements failed — {e}"))
        })?;
        Ok(RhaiStudy { name: name.to_string(), engine, ast, scope, faults })
    }

    /// Load and compile the study whose folder this is: `<dir>/<dir-name>.rhai`.
    ///
    /// ⚠ **`dir` is a PARAMETER and nothing here resolves one.** The BINARY calls
    /// `vike_model::paths::state_path::user_rhai_studies_dir` (or takes `ListedStudy::dir` straight from
    /// `crates/vike-studio-core/src/listing.rs`'s `list_studies`) and hands the answer down — a
    /// library that resolved its own project root reads global state its caller can neither see nor
    /// override, which is what `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`
    /// ratchets down.
    ///
    /// The stem-equals-folder rule is the same one the compiled tier applies, and `gen.rs` names it
    /// "the rhai tier's rule" — this is where that rule actually lives. A folder that does not
    /// carry its entry file is refused NAMING the file it should have held, because the alternative
    /// (an empty study, or a silent skip) is the state a user cannot diagnose from inside their own
    /// script.
    pub fn load(dir: &Path) -> Result<Self, StudyError> {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                StudyError::Study(format!("{}: folder name is not valid UTF-8", dir.display()))
            })?
            .to_string();
        let entry = entry_file(dir, &name).ok_or_else(|| {
            StudyError::Study(format!(
                "{}: holds no `{name}.{RHAI_EXT}` — a study's entry file's stem must equal its \
                 folder name, so nothing in this folder would ever run",
                dir.display()
            ))
        })?;
        let src = std::fs::read_to_string(&entry).map_err(|e| {
            StudyError::Study(format!("{}: could not be read — {e}", entry.display()))
        })?;
        Self::new(&name, &src)
    }

    /// The study's name — its folder name, which is how a caller addresses it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run it: the [`vike_user_research::StudyFn`] contract, with `self` in front.
    ///
    /// ⚠ It cannot BE a `StudyFn` — that type is a bare function pointer, and an interpreted study
    /// is a value. The signature is identical either side of the receiver, which is what lets a
    /// dispatcher hold `Rust(StudyFn)` and `Rhai(RhaiStudy)` in one enum and answer the same
    /// `Result<StudyOutcome, StudyError>` from both arms without either caller knowing which tier
    /// it got.
    ///
    /// `&self` rather than `&mut self`: the engine and the AST are only read, and the fault log has
    /// its own lock. That means a caller may run one compiled study concurrently — a claim that is
    /// GATED rather than asserted, by
    /// `rhai_study_tests::a_compiled_study_is_send_and_sync_so_run_may_be_called_concurrently`.
    pub fn run(
        &self,
        ctx: &StudyContext,
        params: &toml::Value,
    ) -> Result<StudyOutcome, StudyError> {
        self.faults.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        // A FRESH clone of the persisted scope per call, never `&mut self.scope`: a run's own
        // `let`-declared locals then live exactly as long as the call, while whatever the one-time
        // top-level run declared (a `const`, typically) is visible because it was already in the
        // scope before this clone was taken. `crates/vike-script/src/strategy.rs`'s `run_hook`
        // carries the full argument, including the `rewind_scope` pitfall that made the naive
        // spelling fail.
        let mut scope = self.scope.clone();
        let options = rhai::CallFnOptions::default().eval_ast(false).rewind_scope(false);
        let args = (rhai::Dynamic::from(ctx.clone()), toml_to_dynamic(params));
        let value = self
            .engine
            .call_fn_with_options::<rhai::Dynamic>(options, &mut scope, &self.ast, ENTRY_FN, args)
            .map_err(|e| fault::recover(&self.faults, format!("{}: {e}", self.name)))?;

        let got = value.type_name();
        value.try_cast::<StudyOutcome>().ok_or_else(|| {
            StudyError::Study(format!(
                "{}: `{ENTRY_FN}` returned a `{got}`, not an outcome. Build one with `outcome()`, \
                 record into it, and return it — a study that legitimately found nothing returns \
                 an outcome with a metric saying so.",
                self.name
            ))
        })
    }
}

/// `<dir>/<name>.rhai`, matched case-insensitively on the EXTENSION only.
///
/// The stem is compared exactly: `MyStudy/mystudy.rhai` is a different claim from
/// `MyStudy/MyStudy.rhai` and only the second one is this study's entry. The extension is not,
/// for the reason [`RHAI_EXT`] gives.
fn entry_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(format!("{name}.{RHAI_EXT}"));
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.is_file()
            && p.file_stem().and_then(|s| s.to_str()) == Some(name)
            && p.extension().is_some_and(|e| e.eq_ignore_ascii_case(RHAI_EXT))
    })
}

/// A study's recipe, as a Rhai value.
///
/// The Rust tier is handed `&toml::Value` and reads it leniently (`params.get("seed")`); this is
/// the same document in the shape a script can index — a table becomes a map, an array an array,
/// and a scalar its Rhai twin. `params.seed` and `params["seed"]` then both work.
///
/// ⚠ A TOML datetime becomes its own STRING form. Rhai has no date type, and rendering one as an
/// epoch would require choosing a timezone interpretation this function has no business choosing —
/// the string is what the author wrote, and a study that wants a number can parse it or the recipe
/// can carry one.
fn toml_to_dynamic(v: &toml::Value) -> rhai::Dynamic {
    match v {
        toml::Value::String(s) => rhai::Dynamic::from(s.clone()),
        toml::Value::Integer(i) => rhai::Dynamic::from(*i),
        toml::Value::Float(f) => rhai::Dynamic::from(*f),
        toml::Value::Boolean(b) => rhai::Dynamic::from(*b),
        toml::Value::Datetime(d) => rhai::Dynamic::from(d.to_string()),
        toml::Value::Array(a) => {
            rhai::Dynamic::from(a.iter().map(toml_to_dynamic).collect::<rhai::Array>())
        }
        toml::Value::Table(t) => {
            let mut m = rhai::Map::new();
            for (k, val) in t {
                m.insert(k.as_str().into(), toml_to_dynamic(val));
            }
            rhai::Dynamic::from(m)
        }
    }
}

#[cfg(test)]
mod rhai_study_tests;
