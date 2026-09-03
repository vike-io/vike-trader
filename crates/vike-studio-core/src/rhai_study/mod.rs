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
//! `crates/vike-script` is layer 30 and would be the obvious home — it is where `RhaiStrategy`
//! lives. It cannot be: the contract types are in `vike-user-research` at layer 35, and layer 30
//! may not name layer 35 (`crates/vike-ops/tests/layer_gate.rs`). Moving the contract types DOWN to
//! reach it would undo the argument `crates/vike-user-research/src/lib.rs` makes for keeping them
//! in the crate whose whole purpose is the contract — *"When a second study exists and a lower
//! consumer needs the vocabulary without the host, that is the moment to move them down, not
//! before"*.
//!
//! This crate is layer 55 and already depends on `vike-script`, so it can see both. Nothing moved,
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
//!   REOPEN the verdict letting a script mount live. `crates/vike-script/src/engine.rs`'s
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
//!    folder name becomes a Rust module name (`crates/vike-user-research/src/gen.rs`'s
//!    `valid_name`). That is a property of Rust, not of studies, and imposing it here would refuse
//!    folders `crates/vike-studio-core/src/listing.rs`'s `list_studies` already lists — its own
//!    test uses `vol-clustering`.
//!
//! # What this module deliberately does NOT do
//!
//! It resolves no path, mints no run id, writes no artifact and consults no registry. [`RhaiStudy`]
//! is handed ONE study folder by a caller that already knows where `user_data` is (the binary, via
//! `vike_model::state_path::user_rhai_studies_dir`, or `list_studies`' `ListedStudy::dir`), and
//! hands back a `StudyOutcome` for that caller to persist. The split is
//! `crates/vike-user-research/src/contract.rs`'s, for its reasons: `crates/vike-backtest/src/runs.rs`
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

pub use bind::{StudyFit, FIT_PARAM_KEYS, MAX_CALL_LEVELS, MAX_OPERATIONS};

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
    /// `MissingEntry` ruling `crates/vike-user-research/src/gen.rs` makes for the compiled tier and
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
    /// `vike_model::state_path::user_rhai_studies_dir` (or takes `ListedStudy::dir` straight from
    /// `crates/vike-studio-core/src/listing.rs`'s `list_studies`) and hands the answer down — a
    /// library that resolved its own project root reads global state its caller can neither see nor
    /// override, which is what `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`
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
    /// [`tests::a_compiled_study_is_send_and_sync_so_run_may_be_called_concurrently`].
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
mod tests {
    use super::*;

    /// ⚠ `toml::from_str`, NOT `src.parse()`. Under the workspace's pinned `toml = "1.1"`,
    /// `FromStr for Value` parses a single VALUE rather than a DOCUMENT, so a recipe with more than
    /// one key comes back as `unexpected content, expected nothing` pointing at the first space.
    /// `crates/vike-studio-core/src/study_run.rs`'s own test helper already spells it this way; a
    /// second spelling here is what made this test red on the merged tree.
    fn params(src: &str) -> toml::Value {
        toml::from_str(src).expect("the test's own TOML")
    }

    fn empty_params() -> toml::Value {
        toml::Value::Table(toml::map::Map::new())
    }

    /// A context over the workspace's own store double — enough to compile, call and return, which
    /// is all these unit tests need. The seeded, learner-backed pipeline lives in
    /// `crates/vike-studio-core/tests/rhai_study_pipeline.rs`.
    fn bare_ctx() -> StudyContext {
        StudyContext::new(
            std::sync::Arc::new(vike_data::test_support::MemHistStore::new()),
            vike_data::TsRange::of(0, 1_000),
            // Nothing here ever CREATES this — the interpreted tier has no file verb at all, so a
            // study cannot write into scratch. Uniquified anyway, for the reason
            // `crates/vike-ops/tests/temp_path_gate.rs` gives.
            std::env::temp_dir().join(format!("vike-rhai-study-{}", std::process::id())),
        )
    }

    /// [`RhaiStudy::run`] takes `&self`, and that is only worth anything if a caller may actually
    /// hold a compiled study across threads. Written as a compile-time assertion rather than as a
    /// sentence in a doc comment, so the day some field stops being `Sync` the claim fails with it.
    #[test]
    fn a_compiled_study_is_send_and_sync_so_run_may_be_called_concurrently() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RhaiStudy>();
        assert_send_sync::<StudyFit>();
    }

    #[test]
    fn a_study_returns_the_outcome_it_built() {
        let s = RhaiStudy::new(
            "smoke",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("answer", 42);
                 out.artifact("note.txt", "hello");
                 return out;
               }"#,
        )
        .unwrap();
        let out = s.run(&bare_ctx(), &empty_params()).unwrap();
        assert_eq!(out.metric_value("answer"), Some(42.0));
        assert_eq!(out.artifacts(), [("note.txt".to_string(), "hello".to_string())]);
    }

    /// The window is the run's, handed in — not something a study invents. It is also the default
    /// range of every read verb, which is what keeps a study measuring the range it is filed under.
    #[test]
    fn the_window_reaches_the_script_and_reads_back_as_its_bounds() {
        let s = RhaiStudy::new(
            "win",
            r#"fn run(ctx, params) {
                 let w = ctx.window();
                 let out = outcome();
                 out.metric("start", w.start);
                 out.metric("end", w.end);
                 out.metric("all_start_is_unit", if ts_range_all().start == () { 1 } else { 0 });
                 return out;
               }"#,
        )
        .unwrap();
        let out = s.run(&bare_ctx(), &empty_params()).unwrap();
        assert_eq!(out.metric_value("start"), Some(0.0));
        assert_eq!(out.metric_value("end"), Some(1000.0));
        assert_eq!(out.metric_value("all_start_is_unit"), Some(1.0));
    }

    /// The recipe arrives as a document a script can index — every TOML shape, in one study.
    #[test]
    fn a_toml_recipe_arrives_as_an_indexable_rhai_value() {
        let s = RhaiStudy::new(
            "recipe",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("seed", params.seed);
                 out.metric("rate", params.rate);
                 out.metric("on", if params.on { 1 } else { 0 });
                 out.metric("first_symbol_len", params.symbols[0].len());
                 out.metric("nested", params.nested.depth);
                 out.artifact("tag.txt", params.tag);
                 return out;
               }"#,
        )
        .unwrap();
        let p = params(
            "seed = 7\nrate = 0.25\non = true\nsymbols = [\"BTCUSDT\"]\ntag = \"v1\"\n\
             [nested]\ndepth = 3\n",
        );
        let out = s.run(&bare_ctx(), &p).unwrap();
        assert_eq!(out.metric_value("seed"), Some(7.0));
        assert_eq!(out.metric_value("rate"), Some(0.25));
        assert_eq!(out.metric_value("on"), Some(1.0));
        assert_eq!(out.metric_value("first_symbol_len"), Some(7.0));
        assert_eq!(out.metric_value("nested"), Some(3.0));
        assert_eq!(out.artifacts()[0].1, "v1");
    }

    /// A file with the wrong entry is a study nothing will ever call, so it is refused at COMPILE
    /// naming the contract — never accepted as an empty one.
    #[test]
    fn a_script_with_no_run_entry_is_refused_naming_the_contract() {
        // ⚠ NOT `fn go()` — `go` is a RESERVED KEYWORD in Rhai, so that script fails at the
        // tokenizer and never reaches the missing-entry check this test exists for. It was written
        // that way and passed nothing: the refusal it asserted on said "'go' is a reserved
        // keyword", not the contract. Any ordinary identifier exercises the real path.
        let e = RhaiStudy::new("wrong", "fn compute() { 1 }").unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("fn run(ctx, params)")), "{e}");
        // ...and the arity is part of the contract: a one-argument `run` is not this entry.
        let e = RhaiStudy::new("arity", "fn run(ctx) { 1 }").unwrap_err();
        assert!(matches!(e, StudyError::Study(_)), "{e}");
    }

    #[test]
    fn a_compile_error_names_the_study_and_carries_rhai_s_own_words() {
        let e = RhaiStudy::new("broken", "fn run(ctx, params) { this. }").unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("broken")), "{e}");
    }

    /// A study that forgets to return an outcome gets a sentence naming what to do, not a cast
    /// panic and not an empty result that would list as a finished run with no metrics.
    #[test]
    fn a_study_that_returns_something_else_is_refused_naming_what_it_returned() {
        let s = RhaiStudy::new("wrongret", "fn run(ctx, params) { 1 }").unwrap();
        let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("outcome()")), "{e}");
    }

    /// THE property [`fault`] exists for, proven end to end through a real script: a store read
    /// that fails comes back as `StudyError::Data`, not as a sentence a caller would have to parse.
    ///
    /// `depth` is the verb used because `MemHistStore` holds no depth lane and inherits
    /// `HistStore`'s REFUSING default for it — a real refusal from the real double, rather than a
    /// bespoke failing store written to make this test pass.
    #[test]
    fn a_store_read_failure_comes_back_as_the_typed_data_variant() {
        let s =
            RhaiStudy::new("read", r#"fn run(ctx, params) { ctx.depth("binance", "BTCUSDT") }"#)
                .unwrap();
        let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
        assert!(matches!(e, StudyError::Data(_)), "got {e}");

        // ...and a read that SUCCEEDS is an ordinary array, so the fault path is not the only path
        // this store exercises.
        let s = RhaiStudy::new(
            "ok",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("n", ctx.bars("binance", "BTCUSDT", "1m").len());
                 return out;
               }"#,
        )
        .unwrap();
        assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("n"), Some(0.0));
    }

    /// The top level runs during [`RhaiStudy::new`], which is why a top-level failure is a COMPILE
    /// refusal rather than a surprise on the first run — and why it cannot recur per run.
    #[test]
    fn a_failing_top_level_statement_is_refused_at_compile_not_at_run() {
        let e =
            RhaiStudy::new("boom", r#"throw "no"; fn run(ctx, params) { outcome() }"#).unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("top-level")), "{e}");
    }

    /// ...and the host ceiling: no learner is `NoLearner`, which a UI renders as "run this
    /// somewhere else" rather than as a stack of prose.
    #[test]
    fn fitting_with_no_learner_comes_back_as_the_typed_no_learner_variant() {
        let s = RhaiStudy::new(
            "fitless",
            r#"fn run(ctx, params) { ctx.fit([1.0, 2.0], [0.0], 2, #{}) }"#,
        )
        .unwrap();
        let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
        assert!(matches!(e, StudyError::NoLearner(_)), "got {e}");
        // ...and a study can ASK first rather than failing, which is what makes the ceiling
        // something an author can branch on.
        let s = RhaiStudy::new(
            "asks",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("can_fit", if ctx.can_fit() { 1 } else { 0 });
                 return out;
               }"#,
        )
        .unwrap();
        assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("can_fit"), Some(0.0));
    }

    /// A refused metric or artifact STOPS the study. Returning the `Result` as a value would let a
    /// script that never checks a return file a half-recorded outcome.
    #[test]
    fn the_outcome_rules_are_enforced_inside_the_script() {
        let s = RhaiStudy::new(
            "dup",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("sharpe", 1.0);
                 out.metric("sharpe", 2.0);
                 return out;
               }"#,
        )
        .unwrap();
        let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
        assert!(e.to_string().contains("duplicate metric"), "{e}");

        let s = RhaiStudy::new(
            "escape",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.artifact("../../secrets.env", "x");
                 return out;
               }"#,
        )
        .unwrap();
        let e = s.run(&bare_ctx(), &empty_params()).unwrap_err();
        assert!(e.to_string().contains("secrets.env"), "{e}");
    }

    /// An integer metric and a float metric must both work — the `param`-shaped trap this
    /// workspace shipped once already, at the one call site an author writes most often.
    #[test]
    fn a_metric_takes_an_integer_and_a_float_alike_and_refuses_text() {
        let s = RhaiStudy::new(
            "nums",
            r#"fn run(ctx, params) {
                 let out = outcome();
                 out.metric("int", 3);
                 out.metric("float", 3.5);
                 return out;
               }"#,
        )
        .unwrap();
        let out = s.run(&bare_ctx(), &empty_params()).unwrap();
        assert_eq!(out.metric_value("int"), Some(3.0));
        assert_eq!(out.metric_value("float"), Some(3.5));

        let s = RhaiStudy::new(
            "text",
            r#"fn run(ctx, params) { let out = outcome(); out.metric("m", "nope"); return out; }"#,
        )
        .unwrap();
        assert!(s.run(&bare_ctx(), &empty_params()).unwrap_err().to_string().contains("NUMBER"));
    }

    /// The top level runs once, at compile. Two runs of one study must not differ, and a top-level
    /// `const` must still be visible inside `run` — the pair of properties the persisted scope and
    /// `eval_ast(false)` buy together.
    #[test]
    fn a_top_level_const_is_visible_and_two_runs_agree() {
        let s = RhaiStudy::new(
            "constant",
            r#"const K = 7.0;
               fn run(ctx, params) { let out = outcome(); out.metric("k", K); return out; }"#,
        )
        .unwrap();
        let first = s.run(&bare_ctx(), &empty_params()).unwrap();
        let second = s.run(&bare_ctx(), &empty_params()).unwrap();
        assert_eq!(first.metric_value("k"), Some(7.0));
        assert_eq!(first, second, "two runs of one study must not differ");
    }

    /// A run's own locals must not survive into the next run — the reason the scope is CLONED per
    /// call rather than mutated in place.
    #[test]
    fn a_runs_locals_do_not_leak_into_the_next_run() {
        let s = RhaiStudy::new(
            "locals",
            r#"fn run(ctx, params) {
                 let seen = is_def_var("leaked");
                 let leaked = 1;
                 let out = outcome();
                 out.metric("seen", if seen { 1 } else { 0 });
                 return out;
               }"#,
        )
        .unwrap();
        assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("seen"), Some(0.0));
        assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("seen"), Some(0.0));
    }

    /// The entry file's stem must equal the folder name, and the EXTENSION is matched
    /// case-insensitively — a Windows editor that saved `.RHAI` wrote a real study.
    #[test]
    fn the_entry_file_is_found_by_stem_with_a_case_insensitive_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vol_clustering");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vol_clustering.RHAI"),
            "fn run(ctx, params) { let o = outcome(); o.metric(\"n\", 1); return o; }",
        )
        .unwrap();
        let s = RhaiStudy::load(&dir).unwrap();
        assert_eq!(s.name(), "vol_clustering");
        assert_eq!(s.run(&bare_ctx(), &empty_params()).unwrap().metric_value("n"), Some(1.0));
    }

    /// The Rust tier's `[a-z][a-z0-9_]*` rule is a property of RUST MODULE NAMES. This tier has no
    /// such constraint, and imposing one would refuse folders `list_studies` already lists — its
    /// own test uses exactly this name.
    #[test]
    fn a_dashed_folder_name_loads_because_this_tier_generates_no_rust_module() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vol-clustering");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("vol-clustering.rhai"),
            "fn run(ctx, params) { let o = outcome(); o.metric(\"n\", 1); return o; }",
        )
        .unwrap();
        assert_eq!(RhaiStudy::load(&dir).unwrap().name(), "vol-clustering");
    }

    /// A folder without its entry file is a study nothing would ever run. It is refused NAMING the
    /// file it should have held — the `MissingEntry` ruling, mirrored.
    #[test]
    fn a_folder_with_no_matching_entry_file_is_refused_naming_the_file_it_wants() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("halfdone");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scratch.rhai"), "fn run(ctx, params) { () }").unwrap();
        let e = RhaiStudy::load(&dir).unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("halfdone.rhai")), "{e}");
    }

    /// No `user_data/` at all is the CI and fresh-clone state. Nothing is registered and nothing
    /// panics — an absent folder is a named refusal, which is the answer a caller can render.
    #[test]
    fn an_absent_study_folder_is_a_named_refusal_rather_than_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let e = RhaiStudy::load(&tmp.path().join("user_data").join("nope")).unwrap_err();
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("nope")), "{e}");
    }
}
