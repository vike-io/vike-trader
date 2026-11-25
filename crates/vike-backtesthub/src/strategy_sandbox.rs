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
//! - **The child** is `vike-backend strategy-sandbox backtest <builds-dir> <artifact>...`
//!   (`crates/vike-strategy-sandbox`, the one crate that links the loader): each `<artifact>` is the path the
//!   builder answered (`sdk-N/<folder>/<sha256>.so`, decision 0296, [`ensure_built`]), which the child
//!   checks by its shape. It jails itself to those artifacts and nothing else new, loads each, and answers
//!   with [`serve_child`]: the UNCHANGED dispatch (`dispatch::answer_here`, `handle_request_here` with the saved run
//!   a Studio single run leaves, sent up in its own frame) with
//!   `StudyDirs.strategies.rust = RustFileBuild::Loaded` (each mount's loaded file, and the TEXT the hub read,
//!   over which the header's checks and the recorded sha256 run again) and a [`pipe_store::PipeHistStore`], so
//!   its answer is the answer this process would have written. A file never built has no artifact: the
//!   loader's refusal names the build.
//! - **A portfolio** (decision 0281: a Rust strategy file runs sandboxed with its whole backtest) runs its `.rs`
//!   mounts in ONE child: one [`RustRun`] per mount in mount order ([`resolve`]), one artifact on the argv per
//!   mount in that order, and the child's [`RustFileBuild::Loaded`] one builder per mount, each over its own mount
//!   facts ([`facts_of`]), looked up by mount (`StrategyFiles::prepare_mount`). A `[strategy]` run is the one-mount
//!   case, byte for byte what it was. A portfolio mixing `.rhai` and `.rs` files is refused ([`rust_files_of`]).
//! - **The history** the run reads crosses the channel as the parent store's own answers, read verbs
//!   only (`wire`'s module doc). A sweep's rayon pool runs inside the one child.
//! - **The kept instrument lists** the run reads its names and per-symbol facts from (`vike_backtest::run::KeptRows`)
//!   are asked by the child, for the venues the run reads (its series, the inputs the loaded file declares, its
//!   conversion pairs: `vike_backtest::run::profile::ProfileBacktest::read_kept_rows`), as one history read
//!   (`wire::StoreRead::KeptInstruments`) this daemon answers from its store, never handed whole.
//! - **The model texts** of the file's `type = "model"` keys are read here, from this daemon's runs root by run
//!   id, and handed over in the same start message ([`model_texts_of`]); the child puts each text in place of
//!   its id as it builds the file, so the run's own params (and every report of them) keep the id.
//! - **Cancel** is a SIGKILL of the child, which stops a run already inside the engine.
//!
//! ⚠ **Which series a child may read is NOT narrowed to the run's plan.** The plan's
//! `run::data_plan::planned_series_ids` leaves out the detail tape, the funding history and the instrument grid
//! a run also opens (`auxiliary_series`), and a strategy's own declared inputs (decision 0271) are known
//! only to the loaded file, which this process never loads. A list built here would refuse reads the
//! in-process run made; the child reads only, from this daemon's store, and writes nothing.

mod pipe_store;
// `pub(crate)`: the research study's sandboxed run (`crate::study_sandbox`) keeps a child's reason tail and
// reaps it with this relay's two functions.
pub(crate) mod relay;
// `pub(crate)`: a request that may be answered from its saved run hands the relay its `ReadWitnesses`
// (`crate::backtesthub_server`'s `answer_from_saved`).
mod wire;
pub(crate) mod witness;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_backtest::HarnessError;
use vike_backtest::run::refusals::ProfileRefusal;
use vike_backtest::run::strategy_builder::StrategyBuilder;
use vike_backtest::run::{
    StrategySource,
    profile::{ProfileBacktest, ProfileStrategy},
};
use vike_backtesthub_client::proto::{Request, Response};
use vike_data::HistoryStore;
use vike_hub_protocol::child_pipe;
use vike_model::params::Params;
use vike_strategy::file::StrategyFileHeader;
use vike_strategy::file::scan::StrategyFileLanguage;
use vike_studio_core::{RunError, StrategySpec};
use vike_utility::errors::CarriedRefusal;

use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::{
    answer_here, answer_whole_here, is_paramscan_profile_summary, paramscan_summary_parts,
};
use crate::backtesthub_server::saved_run::StudioRun;
use crate::named_run::NamedRunLane;
use crate::studio_run::{
    LoadedMount, RustFileBuild, StrategyFileBuild, StrategyFiles, loaded_file_builder,
    strategy_files_dir,
};

/// **Every mount `profile` runs, in mount order**: its strategy table and the symbol it trades first (its mount
/// facts' `symbol`, [`facts_of`]). A `[strategy]` profile is ONE mount over its `[data] symbols`. The one place this
/// module reads the profile's strategy shape.
fn profile_mounts(profile: ProfileBacktest) -> Vec<(ProfileStrategy, Option<String>)> {
    match profile.strategy {
        Some(s) => vec![(s, profile.data.symbols.first().cloned())],
        None => profile
            .mounts
            .into_iter()
            .map(|m| {
                let x = m.symbols.first().cloned();
                (m.strategy, x)
            })
            .collect(),
    }
}

/// The strategy tables `request` runs, in mount order, as the request names them (no file read): a Studio verb's
/// `StrategySpecWire::File` (one), a profile verb's mounts ([`profile_mounts`]). Empty for every other request and
/// spec.
fn strategies_of(request: &Request, files: &StrategyFiles) -> Vec<ProfileStrategy> {
    match request {
        // A summarized run runs the request it wraps, so the peer watch arms for it as for that one.
        Request::Summarized(inner) => strategies_of(inner, files),
        Request::RunSlice { spec, .. }
        | Request::RunParamscan { spec, .. }
        | Request::RunWalkforward { spec, .. } => {
            match vike_studio_core::wire_run::to_strategy_spec(spec) {
                Ok(StrategySpec::File { file, params, sdk }) => {
                    vec![ProfileStrategy { sdk, ..ProfileStrategy::new(file, params) }]
                }
                _ => Vec::new(),
            }
        }
        Request::RunBacktest(profile_toml)
        | Request::RunParamscanProfile { profile_toml, .. }
        | Request::RunWalkforwardProfile { profile_toml } => {
            ProfileBacktest::from_toml_str_in(profile_toml, &files.dir)
                .map(|profile| {
                    profile_mounts(profile).into_iter().map(|(table, _)| table).collect()
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// Whether `request` runs a `.rs` FILE (any mount's) that this process hands a sandboxed child (a hub's
/// `RustFileBuild::Sandboxed`). Reads no file: the connection's peer watch asks it.
pub(crate) fn names_rust_file(request: &Request, files: &StrategyFiles) -> bool {
    matches!(files.rust, RustFileBuild::Sandboxed)
        && strategies_of(request, files)
            .iter()
            .any(|config| matches!(config.language(), Ok(StrategyFileLanguage::Rust)))
}

/// The `.rs` files `request`'s mounts run, in mount order, each read under `files.dir` as the run itself would read
/// it: its table (its path, its params, and the SDK a profile's `sdk` key asks it built against: `None`, as the file
/// says) and its text and sha256, for the builder and the child. `None` when the request runs no `.rs` file this
/// process hands a child, or when a read refuses one (the run in this process then refuses it, in the read's own
/// words). `Err`: a portfolio whose mounts mix `.rhai` and `.rs` files.
///
/// shortcut: a mixed portfolio is refused, because its child reads no file and so has no `.rhai` mount's text; hand
/// the child each `.rhai` mount's text too when one is wanted.
pub(crate) fn rust_files_of(
    request: &Request,
    files: &StrategyFiles,
) -> Option<Result<Vec<(ProfileStrategy, StrategySource)>, String>> {
    if !names_rust_file(request, files) {
        return None;
    }
    let mut read = Vec::new();
    for mut table in strategies_of(request, files) {
        match table.language() {
            Ok(StrategyFileLanguage::Rust) => {}
            Ok(StrategyFileLanguage::Rhai) => {
                return Some(Err(format!(
                    "strategy.file {:?}: a portfolio whose mounts mix `.rhai` and `.rs` files is not \
                     run, because its `.rs` mounts run in a sandboxed child that reads no file. Run the \
                     `.rhai` mounts and the `.rs` mounts as two portfolios",
                    table.file
                )));
            }
            Err(_) => return None,
        }
        table.read_file(&files.dir).ok()?;
        let source = table.source.take()?;
        read.push((table, source));
    }
    Some(Ok(read))
}

/// **The model texts a run hands a mount's `.rs` file** (`source`, as the hub read it), by run id: each
/// `type = "model"` key the file's header declares, as the mount's own `params` name it, read
/// under `runs_root` (`vike_runs::read_model_text`: one run directory of this daemon's runs root, its fixed
/// `model.txt`, never a path the request spells). The jailed child reads no file, so the texts cross in its
/// start message and are put in place of the ids as the file is built (`loaded_file_builder`). Empty for a
/// file with no model key.
///
/// shortcut: only the request's own params are read, so a sweep axis over a model key names runs the child was
/// not handed and is refused by name there; read the axis values here when a model sweep is wanted.
///
/// # Errors
///
/// A model run that cannot be read, naming the key, the run and why: the run's strategy error.
pub(crate) fn model_texts_of(
    params: &Params,
    source: &StrategySource,
    runs_root: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let mut texts = BTreeMap::new();
    let Ok(header) = StrategyFileHeader::parse(&source.text) else {
        return Ok(texts);
    };
    header.with_model_texts(params, |run| {
        let text = vike_runs::read_model_text(runs_root, run).map_err(|e| e.to_string())?;
        texts.insert(run.to_string(), text.clone());
        Ok(text)
    })?;
    Ok(texts)
}

/// **One `.rs` mount's run, resolved ONCE before it runs** ([`resolve`]): the text the hub read, the artifact the
/// builder answered for it (its path under [`strategy_files_dir`]) and the model texts its keys read, which the child
/// is handed. Its mount facts are not here: the child states them from the request ([`facts_of`]).
#[derive(Debug)]
pub(crate) struct RustRun {
    pub(crate) source: StrategySource,
    pub(crate) artifact: String,
    pub(crate) models: BTreeMap<String, String>,
}

/// `request`'s `.rs` files read ([`rust_files_of`]), each built ([`ensure_built`]) and its model texts read
/// ([`model_texts_of`]) ONCE, one [`RustRun`] per mount in mount order, for the run and for the key an identical
/// request is answered by (`crate::backtesthub_server`'s `answer_from_saved`): `None` for a request this process runs
/// itself, `Err` the refusal its run answers instead (a mixed portfolio, a file that does not build, a builder that is
/// down, a model run that does not read: the first mount's that fails).
pub(crate) fn resolve(
    request: &Request,
    study: &StudyDirs,
) -> Option<Result<Vec<RustRun>, String>> {
    let tables = rust_files_of(request, &study.strategies)?;
    Some(tables.and_then(|tables| {
        tables
            .into_iter()
            .map(|(table, source)| {
                let artifact = ensure_built(&table.file, &source, table.sdk)?;
                let models = model_texts_of(&table.params, &source, &study.runs_root)?;
                Ok(RustRun { source, artifact, models })
            })
            .collect()
    }))
}

/// What the builder answered for a file: the sha256 of its text, and its artifact's path under the builds
/// directory, `sdk-N/<folder>/<sha256>.so` (decision 0296: the SDK it built against and its build folder),
/// which the sandboxed child checks by its shape (`vike_strategy_sandbox::artifact_of`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltFile {
    pub sha256: String,
    pub artifact: String,
}

/// How this process asks the strategy builder for a file's artifact: `(path, text, sdk)` in, what it
/// built ([`BuiltFile`]) out, or the refusal with its id (the builder's own, a compile's
/// `strategy.build.compile_failed`), which this process renders once where a person reads it. `sdk` is a
/// backtest profile's `[strategy] sdk` override, or a Studio's pick (decision 0296); `None` builds as the
/// file says. Blocking for as long as the build takes.
pub type FileBuilder =
    Box<dyn Fn(&str, &str, Option<u32>) -> Result<BuiltFile, CarriedRefusal> + Send + Sync>;

/// The builder this process asks (plan 2b §3: the process that needs a file built asks the builder). Set
/// once by the composition root (`crates/vike-backend/src/main.rs`'s `backtesthub_main`: the builder's client is a
/// crate this one cannot name); absent, a file is run only when its artifact is already built.
static FILE_BUILDER: std::sync::OnceLock<FileBuilder> = std::sync::OnceLock::new();

/// Ask `build` for every `.rs` file this process runs, before its child starts (the first call wins).
pub fn use_builder(build: FileBuilder) {
    let _ = FILE_BUILDER.set(build);
}

/// This process's build, the first frame of every sandboxed child's pipe (`wire`'s module doc), which the child
/// must match. Set once by the composition root (`crates/vike-backend/src/main.rs`'s `backtesthub_main`, with
/// `vike_buildinfo::summary`): a library links no `vike-buildinfo`. Absent, a `.rs` file's run is refused in words.
static PIPE_BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// State `build` first on every sandboxed child's pipe this process starts (the first call wins).
pub fn use_build(build: String) {
    let _ = PIPE_BUILD.set(build);
}

/// **A Studio's Build** (`Request::BuildStrategyFile`, decision 0288's amendment of 2026-11-16): `text` built
/// as the file `file` by the builder this process asks, the text's sha256 out, or the builder's refusal as it
/// came, rendered with its one id (the compiler's diagnostics verbatim under `strategy.build.compile_failed`:
/// the `Error` text is the last carrier, so Studio and the CLI show it as it is), against `sdk` when the Studio
/// picked one. Blocking for as long
/// as the build takes. Refused in words when no builder is set: a daemon started outside `vike-backend`
/// reaches none.
pub(crate) fn build_text(file: &str, text: &str, sdk: Option<u32>) -> Result<String, String> {
    let Some(build) = FILE_BUILDER.get() else {
        return Err(
            "BuildStrategyFile refused: this backtesthub reaches no strategy builder (it was \
                    started outside `vike-backend`, which hands it the builder's client). Run \
                    `vike-backend backtesthub --addr`."
                .to_string(),
        );
    };
    build(file, text, sdk).map(|built| built.sha256).map_err(|refusal| refusal.rendered())
}

/// The file's artifact built against `sdk` (`None`: as the file says), or found in the builder's cache
/// (the same text is one artifact), when a builder is set: the run WAITS for it. `Ok` is the artifact's
/// path under the builds directory ([`BuiltFile::artifact`]), the child's argument. `Err` is the builder's
/// refusal (the compiler's diagnostics, an SDK the file cannot build against, a missing key, a builder
/// that is down), which the run answers instead of starting a child. With no builder set, the artifact
/// already built from the text is the one run ([`already_built`]), and a named SDK is refused: only the
/// builder can say which SDK an artifact came from.
pub(crate) fn ensure_built(
    file: &str,
    source: &StrategySource,
    sdk: Option<u32>,
) -> Result<String, String> {
    let Some(build) = FILE_BUILDER.get() else {
        if let Some(sdk) = sdk {
            return Err(format!(
                "strategy.sdk = {sdk} asks for a build against that SDK, and this backtesthub \
                 reaches no strategy builder (it was started outside `vike-backend`, which hands it \
                 the builder's client)"
            ));
        }
        return already_built(&strategy_files_dir(), &source.sha256).ok_or_else(|| {
            format!(
                "strategy.file {file:?} is not built, and this backtesthub reaches no strategy \
                 builder (it was started outside `vike-backend`, which hands it the builder's client)"
            )
        });
    };
    let built = build(file, &source.text, sdk)
        .map_err(|e| format!("strategy.file {file:?} could not be built: {}", e.rendered()))?;
    if built.sha256 != source.sha256 {
        return Err(format!(
            "strategy.file {file:?}: the builder built text hashing to {}, and this hub read {}",
            built.sha256, source.sha256
        ));
    }
    Ok(built.artifact)
}

/// The artifact already built from text hashing to `sha256` under `builds`, by its path there
/// (`sdk-N/<folder>/<sha256>.so`, two folders down: a folder of the layout before SDKs is never run),
/// the last in name order. `None` when none holds one.
///
/// shortcut: with several SDKs holding the text the last path in name order wins, not the SDK the file
/// would resolve to; a process with no builder is a test's, and one SDK's folder is all it holds.
fn already_built(builds: &Path, sha256: &str) -> Option<String> {
    let names = |dir: &Path| -> Vec<String> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    };
    names(builds)
        .into_iter()
        .flat_map(|sdk| {
            names(&builds.join(&sdk))
                .into_iter()
                .map(move |folder| format!("{sdk}/{folder}/{sha256}.so"))
        })
        .filter(|relative| builds.join(relative).is_file())
        .max()
}

/// The mount facts mount `mount` of a backtest of `request` states, as JSON text: its symbol and interval ALONE
/// (`vike_model::params::MountFacts`'s doc: a live mount's venue, grid and fees are not a backtest's). A
/// Studio slice's FIRST symbol, or the profile mount's ([`profile_mounts`]); nothing for a request that names
/// neither.
pub(crate) fn facts_of(request: &Request, mount: usize) -> String {
    let (symbol, interval) = match request {
        Request::Summarized(inner) => return facts_of(inner, mount),
        Request::RunSlice { slice, .. }
        | Request::RunParamscan { slice, .. }
        | Request::RunWalkforward { slice, .. } => {
            (slice.symbols.first().cloned(), slice.interval.clone())
        }
        Request::RunBacktest(profile_toml)
        | Request::RunParamscanProfile { profile_toml, .. }
        | Request::RunWalkforwardProfile { profile_toml } => {
            match ProfileBacktest::from_toml_str_in(profile_toml, Path::new("")) {
                Ok(p) => {
                    let interval = p.data.interval.clone();
                    (
                        profile_mounts(p).into_iter().nth(mount).and_then(|(_, symbol)| symbol),
                        interval,
                    )
                }
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

/// The strategy files of the process that LOADED the files, one per mount in mount order
/// ([`RustFileBuild::Loaded`]), `builds` and `mounts` side by side: each mount's `build` (or its load's refusal,
/// which every build of that mount then answers) and the SDK its artifact was built against, which the run's results
/// record, over THAT mount's facts ([`facts_of`]), the text the hub read and the model texts it read for that text's
/// model keys ([`model_texts_of`]).
pub(crate) fn loaded_strategy_files(
    request: &Request,
    builds: Vec<(Result<StrategyFileBuild, CarriedRefusal>, Option<u32>)>,
    mounts: Vec<(String, BTreeMap<String, String>)>,
) -> StrategyFiles {
    let mounts = builds
        .into_iter()
        .zip(mounts)
        .enumerate()
        .map(|(mount, ((build, sdk), (text, models)))| {
            let builder = match build {
                Ok(build) => {
                    // A file with no model key crosses its params untouched.
                    let header = StrategyFileHeader::parse(&text)
                        .ok()
                        .filter(|header| header.model_keys().next().is_some());
                    loaded_file_builder(build, facts_of(request, mount), header, models)
                }
                Err(refusal) => StrategyBuilder::new(move |_| {
                    Err(HarnessError::from(ProfileRefusal::StrategyFileLoadRefused {
                        refusal: refusal.clone(),
                    }))
                }),
            };
            LoadedMount { builder, text, sdk }
        })
        .collect();
    StrategyFiles { dir: PathBuf::new(), rust: RustFileBuild::Loaded(mounts) }
}

/// The program a child runs: this executable unless a test pointed it elsewhere (a research study's
/// child too, `crate::study_sandbox::runner`).
pub(crate) fn program() -> std::io::Result<PathBuf> {
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

/// **The parent's half over two streams a test holds** (`test-support` only): `request` handed to the child at the
/// far end of `to_child`/`from_child`, its history answered from `store` and no model text, exactly as
/// [`run_in_child`]'s relay serves a spawned child's pipes. The answer, with the trades and the whole curve a
/// summarized single run held (empty when the child sent its event log in the curve's place, `pinned_log`: a store
/// that stamps no day, as a memory store, never pins one), or why the relay stopped. What
/// `tests/sandbox_child_memory.rs` drives. The test owns
/// the far end, so nothing is killed when the relay ends with an answer unwritten: the far end must read them all.
#[cfg(feature = "test-support")]
pub fn relay_over(
    to_child: &mut (impl Write + Send),
    from_child: &mut impl Read,
    build: &str,
    request: &Request,
    text: &str,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<(Response, Vec<vike_model::Trade>, vike_runs::RunEquityCurve), String> {
    let answered = relay::relay(
        to_child,
        from_child,
        build,
        request,
        text,
        &BTreeMap::new(),
        store,
        &|| {},
        None,
    )
    .map_err(|stopped| stopped.why)?;
    Ok((answered.response, answered.trades, answered.curve))
}

/// Run `request` in ONE sandboxed child loading every mount's artifact (`runs`, one per `.rs` mount in mount order, as
/// [`resolve`] answered them: each the artifact built from its `source`, its path under [`strategy_files_dir`], and its
/// model keys' texts, [`model_texts_of`]), its history read from `store`; `client_gone` set kills the child. A child that
/// fails is a strategy error naming why, its exit and its last words. Where the jail cannot be applied
/// (not Linux, an older kernel, a unit without `@sandbox`) the child says so by name and loads nothing: the
/// run's strategy error carries that sentence. Beside the answer: the saved run a Studio single run of the file left
/// (`crate::backtesthub_server::saved_run::StudioRun`), sent up in its own frame. `witness`, when given, records every
/// read the relay answers with its witness ([`witness::ReadWitnesses`]: a request that may be answered from its saved
/// run later).
pub(crate) fn run_in_child(
    request: &Request,
    runs: &[RustRun],
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
    witness: Option<&witness::ReadWitnesses>,
) -> (Response, Option<StudioRun>) {
    match child_answer(request, runs, store, client_gone, None, witness) {
        Ok(answered) => (answered.response, answered.studio_run),
        Err(why) => (Response::Error(why), None),
    }
}

/// **A `Request::Summarized` run of a `.rs` file** (`summarized`, the request itself): the child runs the
/// wrapped request and summarizes it with `crate::run_summary`'s one summarizer, streaming each point's
/// summary and trades ([`serve_child`]); THIS process holds them (`crate::run_summary::hold_summary`), as
/// it holds an in-process summarized run's, a single run's event log in its curve's place when the child sent one
/// ([`pinned_log`]), stamped with `build`, this daemon's. `runs`, `witness` and the saved run beside the answer as
/// [`run_in_child`]'s.
pub(crate) fn run_summarized_in_child(
    summarized: &Request,
    runs: &[RustRun],
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
    build: Option<&str>,
    witness: Option<&witness::ReadWitnesses>,
) -> (Response, Option<StudioRun>) {
    match child_answer(summarized, runs, store, client_gone, None, witness) {
        Ok(answered) => hold_from_child(answered, summarized, build),
        Err(why) => (Response::Error(why), None),
    }
}

/// **A summarized PROFILE search of a `.rs` file** (`summarized`, `Request::Summarized` around
/// `Request::RunParamscanProfile`, decision 0078's amendment of 2026-11-19): the child runs the search and hands up each
/// row, best first, one a frame (its summary and its whole JSON, [`serve_child`]); `save` writes each row's JSON into the
/// saved run as it arrives, and THIS process holds the summaries (`crate::run_summary::hold_paramscan`). No frame
/// carries more than one point, so the search has no point-count ceiling of the frame cap's. `runs` and `witness` as
/// [`run_in_child`]'s.
pub(crate) fn run_paramscan_summary_in_child(
    summarized: &Request,
    runs: &[RustRun],
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
    save: &mut crate::backtesthub_server::saved_run::ParamscanSave<'_>,
    witness: Option<&witness::ReadWitnesses>,
) -> Response {
    match child_answer(summarized, runs, store, client_gone, Some(save), witness) {
        Ok(answered) => match answered.response {
            Response::ParamscanProfileSummary(summary) => {
                crate::run_summary::hold_paramscan(*summary, answered.rows)
            }
            other => other,
        },
        Err(why) => Response::Error(why),
    }
}

/// [`run_summarized_in_child`]'s hold of what its child `answered` to `summarized`: the summary the client gets, its
/// run held here, and the saved run beside it.
fn hold_from_child(
    answered: relay::Answered,
    summarized: &Request,
    build: Option<&str>,
) -> (Response, Option<StudioRun>) {
    let relay::Answered { response, points, trades, curve, mut log, studio_run, .. } = answered;
    // The child stamped the days; the build is this daemon's, as `equity_log::stamp` records it here.
    if let Some(held) = &mut log {
        held.log.recipe.build = build.map(str::to_string);
        // The profile this process SENT, not the child's copy, where it holds one: a `RunBacktest`'s text is the
        // profile itself (a Studio slice's is built in the child, and a rebuild refuses one that names a file).
        if let Request::Summarized(inner) = summarized
            && let Request::RunBacktest(sent) = &**inner
        {
            held.profile_toml.clone_from(sent);
        }
    }
    let held = crate::run_summary::hold_summary(crate::run_summary::summary_from_child(
        response, points, trades, curve, log,
    ));
    (held, studio_run)
}

/// `request` answered by a sandboxed child (see [`run_in_child`]), or the error line it ended with; a summarized
/// search's rows written by `save` as they come up, each read recorded with its witness in `witness` when given.
fn child_answer(
    request: &Request,
    runs: &[RustRun],
    store: &Arc<dyn HistoryStore + Send + Sync>,
    client_gone: Option<&AtomicBool>,
    save: Option<&mut crate::backtesthub_server::saved_run::ParamscanSave<'_>>,
    witness: Option<&witness::ReadWitnesses>,
) -> Result<relay::Answered, String> {
    let Some(build) = PIPE_BUILD.get() else {
        return Err(RunError::Strategy(
            "the strategy sandbox was not started: this process was handed no build to state to its \
             child (it was started outside `vike-backend`, which hands it in)"
                .to_string(),
        )
        .wire_line());
    };
    let outcome = match program() {
        Ok(program) => relay::run(
            &program,
            build,
            request,
            &strategy_files_dir(),
            runs,
            store.as_ref(),
            client_gone,
            save,
            witness,
        ),
        Err(e) => relay::Outcome::Failed(format!(
            "the strategy sandbox could not be started: this program's path is unknown ({e})"
        )),
    };
    match outcome {
        relay::Outcome::Answer(answered) => Ok(*answered),
        relay::Outcome::Cancelled => Err("cancelled: the client closed its connection, so its \
                                          strategy sandbox was killed and no report was built"
            .to_string()),
        relay::Outcome::Failed(why) => Err(RunError::Strategy(why).wire_line()),
    }
}

/// **The sandboxed child's half**: its build out and the parent's checked (another build is refused in words on
/// stderr, exit 3: `wire`'s module doc), then read ONE request from `input`, answer it with this crate's own dispatch
/// over the history piped from the parent, and write the answer to `output` (a `Request::Summarized` run
/// summarized here, its held parts written first: [`run_summarized_in_child`]). `builds` holds, per artifact the
/// child was named (one per `.rs` mount, in mount order), the strategy file it loaded, or the jail's or the load's
/// refusal, the jail's id kept (it becomes the run's strategy error, at the point the run builds that mount's
/// strategy: `vike_backtest::run::refusals::ProfileRefusal::StrategyFileLoadRefused`), and the
/// numbered SDK its artifact was built against (`None` for a development tree's), which the run's results record; a
/// start message naming another count of mounts is refused on stderr, exit 3. `identity` is this binary's build
/// (`vike_buildinfo::summary`, handed in by the composition root).
///
/// The multicall dispatcher hands this to `vike_strategy_sandbox::run` as its `BacktestServe`.
pub fn serve_child(
    mut input: Box<dyn Read + Send>,
    mut output: Box<dyn Write + Send>,
    identity: &str,
    builds: Vec<(Result<StrategyFileBuild, CarriedRefusal>, Option<u32>)>,
) -> ExitCode {
    // This build first, then the parent's checked: the frames after it are positional (`wire`'s module doc).
    let checked = child_pipe::send_build(&mut output, identity)
        .and_then(|()| child_pipe::check_build(&mut input, identity, "the backtesthub"));
    if let Err(e) = checked {
        eprintln!("strategy-sandbox: {e}");
        return ExitCode::from(3);
    }
    let first: wire::ChildRequest<Request, String, BTreeMap<String, String>> =
        match child_pipe::read_message(&mut input) {
            Ok(first) => first,
            Err(e) => {
                eprintln!("strategy-sandbox: no request on the protocol channel: {e}");
                return ExitCode::from(3);
            }
        };
    if first.mounts.len() != builds.len() {
        eprintln!(
            "strategy-sandbox: the backtesthub sent {} mount(s) and this child was named {} artifact(s)",
            first.mounts.len(),
            builds.len()
        );
        return ExitCode::from(3);
    }
    vike_backtest::search::paramscan::threads::install_paramscan_threads(Some(
        first.paramscan_threads,
    ));
    let study = StudyDirs {
        strategies: loaded_strategy_files(&first.request, builds, first.mounts),
        ..StudyDirs::working_directory()
    };
    let pipe = Arc::new(pipe_store::PipeHistStore::serving(&first.request, input, output));
    let store: Arc<dyn HistoryStore + Send + Sync> = pipe.clone();
    let answer = |request| answer_here(request, &store, &study, NamedRunLane::DISARMED, None);
    let (response, studio_run) = match first.request {
        // A summarized PROFILE search: each row goes up in its own frame, best first, its whole JSON beside its
        // summary, then the report's tail, then the header as the answer (`run_paramscan_summary_in_child`).
        request if is_paramscan_profile_summary(&request) => {
            let parts = paramscan_summary_parts(request, &store, &study, None, |row, json| {
                pipe.send_paramscan_row(row, json)
            });
            match parts {
                Ok((summary, tail)) => match pipe.send_paramscan_tail(tail) {
                    Ok(()) => (Response::ParamscanProfileSummary(Box::new(summary)), None),
                    Err(e) => {
                        eprintln!("strategy-sandbox: the search's tail could not be written: {e}");
                        return ExitCode::from(3);
                    }
                },
                Err(answer) => (*answer, None),
            }
        }
        // A SUMMARIZED run is summarized HERE, by `crate::run_summary`'s one summarizer, and what the parent
        // holds streams to it point by point ahead of the answer (`run_summarized_in_child`). The wrapped
        // request answers whole (`answer_whole_here`: a `RunBacktest` its run's result, not its report).
        Request::Summarized(inner) => {
            match crate::run_summary::periods_of_request(&inner, &study.strategies.dir) {
                Ok(periods_per_year) => {
                    // Summarized over the full records the run holds HERE, so nothing more crosses the pipe.
                    let (whole, mut studio_run, held) =
                        answer_whole_here(*inner, &store, &study, NamedRunLane::DISARMED, None);
                    let mut summary =
                        crate::run_summary::summarize_answer(whole, held, periods_per_year);
                    pinned_log(&mut summary, studio_run.as_mut(), store.as_ref());
                    match pipe.send_summary(summary) {
                        Ok(response) => (response, studio_run),
                        Err(e) => {
                            eprintln!("strategy-sandbox: the summary could not be written: {e}");
                            return ExitCode::from(3);
                        }
                    }
                }
                Err(why) => (Response::Error(why), None),
            }
        }
        request => answer(request),
    };
    // The saved run a Studio single run left goes up ahead of the answer; one that cannot be written (over the
    // frame cap, which writes nothing) costs the answer nothing, and the parent saves no run.
    if let Some(run) = studio_run
        && let Err(e) = pipe.send_studio_run(run)
    {
        eprintln!("strategy-sandbox: the Studio run will not be saved: {e}");
    }
    match pipe.send_answer(response) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("strategy-sandbox: the answer could not be written: {e}");
            ExitCode::from(3)
        }
    }
}

/// **The pin check, made BEFORE the child chooses what to send** (decision 0078's amendment of 2026-11-19 on what the
/// hub holds). A summarized SINGLE run's event log (`run`'s, the saved run it left) is stamped with the parent store's
/// day stamps of the bars the run marked, asked through the pipe (`vike_backtest::run::equity_log::stamp`, the check
/// the hub made after the child until then, over `wire::StoreRead::BarDays`), and when the store PINS them all (every
/// day stamped, none provisional) it moves into `summary` ([`crate::run_summary::HeldLog`]) to go up in the whole
/// curve's place. Otherwise it is dropped and the curve goes up, which the saved run keeps, as when the hub dropped it:
/// a log that came up is pinned, so the hub never drops one with no curve beside it. Any other summary, or a run that
/// left no log (a tick run, a file's bars, a log its own check refused), sends what it always sent.
fn pinned_log(
    summary: &mut crate::run_summary::Summary,
    run: Option<&mut StudioRun>,
    store: &dyn HistoryStore,
) {
    let (crate::run_summary::Summary::Run(_, _, _, held), Some(run)) = (summary, run) else {
        return;
    };
    let Some(mut log) = run.equity_log.take() else { return };
    if vike_backtest::run::equity_log::stamp(&mut log.recipe, store, None) {
        let log = crate::run_summary::HeldLog::new(run.profile_toml.clone(), log);
        *held = Some(Box::new(log));
    }
}

#[cfg(test)]
#[path = "strategy_sandbox_tests.rs"]
mod strategy_sandbox_tests;

#[cfg(test)]
#[path = "strategy_sandbox/log_not_curve_tests.rs"]
mod log_not_curve_tests;

#[cfg(test)]
#[path = "strategy_sandbox/in_flight_tests.rs"]
mod in_flight_tests;

#[cfg(test)]
#[path = "strategy_sandbox/tick_columns_tests.rs"]
mod tick_columns_tests;

#[cfg(test)]
#[path = "strategy_sandbox/tick_run_tests.rs"]
mod tick_run_tests;
