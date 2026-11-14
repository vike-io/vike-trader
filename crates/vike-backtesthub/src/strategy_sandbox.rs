//! **A Rust strategy file's backtest runs in a SANDBOXED child, never in this process** (plan 2b): the
//! backtesthub loads no strategy file and links no loader, as the order-signing daemon does not
//! (decisions 0082/0083).
//!
//! - **The route.** `backtesthub_server::dispatch`'s `handle_request` hands every request whose strategy is
//!   a `.rs` FILE ([`rust_file_of`]: a Studio verb's `StrategySpecWire::File` or a profile verb's `[strategy] file`)
//!   to [`run_in_child`] before anything runs, on a hub (`RustFileBuild::Sandboxed`). The hub READS the file
//!   first, as it reads every file (`ProfileStrategy::read_file`: the path, the link refusal, the header's
//!   checks over the params); a file that read refuses is not routed, and the run in this process refuses
//!   it in those same words. A `.rhai` file, an editor buffer and a test's compiled stand-in run here.
//! - **The child** is `vike-backend strategy-sandbox backtest <builds-dir> <sha256>`
//!   (`crates/vike-strategy-sandbox`, the one crate that links the loader, and so the one that can name the
//!   artifact's build folder). It jails itself to that one artifact, loads it, and answers with
//!   [`serve_child`]: the UNCHANGED dispatch (`dispatch::handle_request_here`) with
//!   `StudyDirs.strategies.rust = RustFileBuild::Loaded` (the loaded file, and the TEXT the hub read, over
//!   which the header's checks and the recorded sha256 run again) and a [`pipe_store::PipeHistStore`], so
//!   its answer is the answer this process would have written. A file never built has no artifact: the
//!   loader's refusal names the build.
//! - **The history** the run reads crosses the channel as the parent store's own answers, read verbs
//!   only (`wire`'s module doc). A sweep's rayon pool runs inside the one child.
//! - **The kept instrument rows** the run reads its per-symbol facts from (`vike_backtest::harness::KeptRows`) are
//!   asked of this daemon's store once and handed to the child in its start message (`kept_rows_of`); its piped
//!   store answers them back, so the child reads no folder and asks nothing more for them.
//! - **Cancel** is a SIGKILL of the child, which stops a run already inside the engine.
//!
//! ⚠ **Which series a child may read is NOT narrowed to the run's plan.** The plan's
//! `data_plan::planned_series_ids` leaves out the detail tape, the funding history and the instrument grid
//! a run also opens (`auxiliary_series`), and a strategy's own declared inputs (decision 0271) are known
//! only to the loaded file, which this process never loads. A list built here would refuse reads the
//! in-process run made; the child reads only, from this daemon's store, and writes nothing.

mod pipe_store;
mod relay;
mod wire;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_backtest::harness::strategy_builder::StrategyBuilder;
use vike_backtest::harness::{
    HarnessError, KeptRows, ProfileBacktest, ProfileStrategy, StrategySource,
};
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::HistoryStore;
use vike_strategy::file::scan::StrategyFileLanguage;
use vike_studio_core::{RunError, StrategySpec};

use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::handle_request_here;
use crate::named_run::NamedRunLane;
use crate::studio_run::{
    RustFileBuild, StrategyFileBuild, StrategyFiles, loaded_file_builder, strategy_files_dir,
};

/// The `[strategy]` table `request` runs, as the request names it (no file read): a Studio verb's
/// `StrategySpecWire::File`, a profile verb's `[strategy]`. `None` for every other request and spec.
fn strategy_of(request: &Request, files: &StrategyFiles) -> Option<ProfileStrategy> {
    match request {
        Request::RunSlice { spec, .. }
        | Request::RunParamscan { spec, .. }
        | Request::RunWalkforward { spec, .. } => {
            match vike_studio_core::wire_run::to_strategy_spec(spec).ok()? {
                StrategySpec::File { file, params } => Some(ProfileStrategy::new(file, params)),
                _ => None,
            }
        }
        Request::RunBacktest(profile_toml)
        | Request::RunParamscanProfile { profile_toml, .. }
        | Request::RunWalkforwardProfile { profile_toml } => {
            Some(ProfileBacktest::from_toml_str_in(profile_toml, &files.dir).ok()?.strategy)
        }
        _ => None,
    }
}

/// Whether `request` runs a `.rs` FILE that this process hands a sandboxed child (a hub's
/// `RustFileBuild::Sandboxed`). Reads no file: the connection's peer watch asks it.
pub(crate) fn names_rust_file(request: &Request, files: &StrategyFiles) -> bool {
    matches!(files.rust, RustFileBuild::Sandboxed)
        && strategy_of(request, files)
            .is_some_and(|config| matches!(config.language(), Ok(StrategyFileLanguage::Rust)))
}

/// The `.rs` file `request` runs, read under `files.dir` as the run itself would read it: its path and its
/// text and sha256, for the builder and the child. `None` when the request runs no `.rs` file this process
/// hands a child, or when the read refuses it (the run in this process then refuses it, in the read's own
/// words).
pub(crate) fn rust_file_of(
    request: &Request,
    files: &StrategyFiles,
) -> Option<(String, StrategySource)> {
    if !names_rust_file(request, files) {
        return None;
    }
    let mut config = strategy_of(request, files)?;
    config.read_file(&files.dir).ok()?;
    Some((config.file, config.source?))
}

/// How this process asks the strategy builder for a file's artifact: `(path, text)` in, the text's sha256
/// out, or the refusal in words. Blocking for as long as the build takes.
pub type FileBuilder = Box<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>;

/// The builder this process asks (plan 2b §3: the process that needs a file built asks the builder). Set
/// once by the composition root (`crates/vike/src/main.rs`'s `backtest_main`: the builder's client is a
/// crate this one cannot name); absent, a file is run only when its artifact is already built.
static FILE_BUILDER: std::sync::OnceLock<FileBuilder> = std::sync::OnceLock::new();

/// Ask `build` for every `.rs` file this process runs, before its child starts (the first call wins).
pub fn use_builder(build: FileBuilder) {
    let _ = FILE_BUILDER.set(build);
}

/// The file's artifact built (or found in the builder's cache: the same text is one artifact), when a
/// builder is set: the run WAITS for it. `Err` is the builder's refusal (the compiler's diagnostics, a
/// missing key, a builder that is down), which the run answers instead of starting a child.
pub(crate) fn ensure_built(file: &str, source: &StrategySource) -> Result<(), String> {
    let Some(build) = FILE_BUILDER.get() else { return Ok(()) };
    let sha = build(file, &source.text)
        .map_err(|e| format!("strategy.file {file:?} could not be built: {e}"))?;
    if sha != source.sha256 {
        return Err(format!(
            "strategy.file {file:?}: the builder built text hashing to {sha}, and this hub read {}",
            source.sha256
        ));
    }
    Ok(())
}

/// The mount facts a backtest of `request` states, as JSON text: its symbol and interval ALONE
/// (`vike_model::params::MountFacts`'s doc: a live mount's venue, grid and fees are not a backtest's). A
/// Studio slice's or a profile's FIRST symbol; nothing for a request that names neither.
pub(crate) fn facts_of(request: &Request) -> String {
    let (symbol, interval) = match request {
        Request::RunSlice { slice, .. }
        | Request::RunParamscan { slice, .. }
        | Request::RunWalkforward { slice, .. } => {
            (slice.symbols.first().cloned(), slice.interval.clone())
        }
        Request::RunBacktest(profile_toml)
        | Request::RunParamscanProfile { profile_toml, .. }
        | Request::RunWalkforwardProfile { profile_toml } => {
            match ProfileBacktest::from_toml_str_in(profile_toml, Path::new("")) {
                Ok(p) => (p.data.symbols.first().cloned(), p.data.interval.clone()),
                Err(_) => (None, String::new()),
            }
        }
        _ => (None, String::new()),
    };
    let facts = vike_model::params::MountFacts {
        symbol: symbol.unwrap_or_default(),
        interval,
        ..Default::default()
    };
    serde_json::to_string(&facts).unwrap_or_else(|_| "{}".to_string())
}

/// The `(venue, symbol)` series a run of `request` loads, as the run resolves them: a Studio slice's picks, a
/// profile's `resolved_series`; none for any other request.
fn series_of(request: &Request) -> Vec<(String, String)> {
    match request {
        Request::RunSlice { slice, .. }
        | Request::RunParamscan { slice, .. }
        | Request::RunWalkforward { slice, .. } => {
            slice.symbols.iter().map(|s| (slice.venue.clone(), s.clone())).collect()
        }
        Request::RunBacktest(profile_toml)
        | Request::RunParamscanProfile { profile_toml, .. }
        | Request::RunWalkforwardProfile { profile_toml } => {
            match ProfileBacktest::from_toml_str_in(profile_toml, Path::new("")) {
                Ok(p) => {
                    p.data.resolved_series().into_iter().map(|s| (s.venue, s.symbol)).collect()
                }
                Err(_) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// **The KEPT instrument rows a run of `request` reads, as `store` answers them** (`KeptRows::of` over its
/// series): what the child is handed in its start message and its piped store answers back, so the profile the
/// child resolves reads every fact this process's would (`KeptRows::of` over its own rows is those rows).
pub(crate) fn kept_rows_of(
    request: &Request,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<Option<Vec<vike_model::Instrument>>, vike_data::HistoryStoreError> {
    let series = series_of(request);
    Ok(store.kept_instruments()?.map(|all| KeptRows::of(&all, &series).rows))
}

/// The strategy files of the process that LOADED the file: `build` (or the load's refusal, which every
/// build of the run then answers), over the backtest's mount facts and the text the hub read.
pub(crate) fn loaded_strategy_files(
    request: &Request,
    build: Result<StrategyFileBuild, String>,
    text: String,
) -> StrategyFiles {
    let builder = match build {
        Ok(build) => loaded_file_builder(build, facts_of(request)),
        Err(refusal) => {
            StrategyBuilder::new(move |_| Err(HarnessError::Validation(refusal.clone())))
        }
    };
    StrategyFiles { dir: PathBuf::new(), rust: RustFileBuild::Loaded { builder, text } }
}

/// The program a child runs: this executable unless a test pointed it elsewhere.
fn program() -> std::io::Result<PathBuf> {
    #[cfg(feature = "test-support")]
    if let Some(program) = TEST_PROGRAM.get() {
        return Ok(program.clone());
    }
    std::env::current_exe()
}

#[cfg(feature = "test-support")]
static TEST_PROGRAM: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Run every sandboxed child of this process as `program` (a test hands it `vike-backend`'s binary; in
/// production the child is this executable). `test-support` only; the first call wins.
#[cfg(feature = "test-support")]
pub fn use_program(program: PathBuf) {
    let _ = TEST_PROGRAM.set(program);
}

/// Run `request` in a sandboxed child loading the artifact built from `source` (its sha256 names it under
/// [`strategy_files_dir`]; the child computes the path), its history read from `store`; `client_gone` set
/// kills the child. A child that fails is a strategy error naming why, its exit and its last words. Where
/// the jail cannot be applied (not Linux, an older kernel, a unit without `@sandbox`) the child says so by
/// name and loads nothing: the run's strategy error carries that sentence.
pub(crate) fn run_in_child(
    request: &Request,
    source: &StrategySource,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
) -> Response {
    let outcome = match program() {
        Ok(program) => relay::run(
            &program,
            request,
            &strategy_files_dir(),
            source,
            store.as_ref(),
            client_gone,
        ),
        Err(e) => relay::Outcome::Failed(format!(
            "the strategy sandbox could not be started: this program's path is unknown ({e})"
        )),
    };
    match outcome {
        relay::Outcome::Answer(response) => *response,
        relay::Outcome::Cancelled => Response::Error(
            "cancelled: the client closed its connection, so its strategy sandbox was killed and \
             no report was built"
                .to_string(),
        ),
        relay::Outcome::Failed(why) => Response::Error(RunError::Strategy(why).wire_line()),
    }
}

/// **The sandboxed child's half**: read ONE request from `input`, answer it with this crate's own dispatch
/// over the history piped from the parent, and write the answer to `output`. `build` is the strategy file
/// the child loaded, or the load's refusal in the loader's words (it becomes the run's strategy error, at
/// the point the run builds its strategy).
///
/// The multicall dispatcher hands this to `vike_strategy_sandbox::run` as its `BacktestServe`.
#[must_use]
pub fn serve_child(
    mut input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
    build: Result<StrategyFileBuild, String>,
) -> ExitCode {
    let first: wire::ChildRequest<Request, String> = match wire::read_message(&mut input) {
        Ok(first) => first,
        Err(e) => {
            eprintln!("strategy-sandbox: no request on the protocol channel: {e}");
            return ExitCode::from(3);
        }
    };
    vike_backtest::harness::paramscan::threads::install_paramscan_threads(Some(
        first.paramscan_threads,
    ));
    let study = StudyDirs {
        strategies: loaded_strategy_files(&first.request, build, first.text),
        ..StudyDirs::working_directory()
    };
    let pipe = Arc::new(pipe_store::PipeHistStore::new(input, output, first.kept));
    let store: Arc<dyn HistoryStore + Send + Sync> = pipe.clone();
    let response = handle_request_here(first.request, &store, &study, NamedRunLane::DISARMED, None);
    match pipe.send_answer(response) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("strategy-sandbox: the answer could not be written: {e}");
            ExitCode::from(3)
        }
    }
}

#[cfg(test)]
#[path = "strategy_sandbox_tests.rs"]
mod strategy_sandbox_tests;
