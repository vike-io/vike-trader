//! **An IDENTICAL backtest answered from the run it saved** (decision 0078's amendment of 2026-11-20, the owner's "do"):
//! before a run request is run, this daemon computes its INPUT KEY ([`reusable`]); a FINISHED saved run with that key
//! whose recorded reads still hold answers it at once ([`answer`]), in the shape the request asks (a summary or the
//! whole report), saying so: a summary's `answered_from_saved_run`, a text report's last key [`ANSWERED_FROM_KEY`].
//! Otherwise the request runs as it always did, and the run it saves keeps its key, its answer and the witness of every
//! read it made ([`Reusable::kept`], [`keep_into`]; `vike_runs::KEPT_ANSWER_FILE`).
//!
//! **The key** is the sha256 of, and only of:
//! - the PROGRAM a sandboxed child runs, by the sha256 of its bytes ([`AnswersFromSaved`], `StudyDirs::answers_from_saved`:
//!   hashed once when the daemon starts, the program this executable is, so the engine, the simulator, the fee tables
//!   and every setting compiled in; a build line is no proof of the code, an incremental rebuild keeps it). The file is
//!   looked at again at every request, and once it changed on disk (a deploy, a rebuild) nothing more is keyed until the
//!   daemon restarts on it. Beside it, the daemon's commit (`StudyDirs::build`, a plain input) and the datahub its reads
//!   go to, as a FRESH dial's `Welcome` states it ([`datahub_identity`]: its `build=` entry, the sha256 of the program
//!   it runs; a datahub older than that entry, or still hashing at its start, by its advertised features);
//! - the strategy file's sha256 and the sha256 of its BUILT ARTIFACT (`sdk-N/<folder>/<sha256>.so`): the artifact is
//!   named by the text, so a new toolchain or SDK revision builds other bytes under the same name;
//! - the model texts its `type = "model"` keys read (`crate::strategy_sandbox::model_texts_of`);
//! - the REQUEST itself, as the wire carries it: the profile text (its window, `[engine]`, `[strategy]` and its
//!   `sdk`, `[data]`, `[paramscan]` grid), the rank and search method, whether it is summarized, a Studio slice, grid
//!   or walk. A profile's top-level `name` line is dropped first, the text otherwise as sent ([`without_name`]).
//!
//! So a report's SHAPE moves the key with it: a tick run's curve sampling and its `returns_interval_ms` (decision
//! 0078's amendment of 2026-11-20 on the curve kept by time) are the build's rule and the profile's
//! `engine.equity_sample_every` / `engine.seed_bar_interval_ms`, and a tick report from before that amendment never
//! answers one after it.
//!
//! **The data is checked, not keyed**: a `.rs` file's reads are known only once it ran, so the saved run keeps every
//! read its relay answered with that read's witness (`crate::strategy_sandbox::witness`: the store's day stamps of a bar
//! or tick read, the hash of any other answer, the kept instrument lists among them), and a hit asks the store again
//! (`witness::still_holds`). A run that read a window reaching today, a provisional day, an unstamped series or a read
//! the store refused keeps no key at all.
//!
//! **Left out of the key, and why:** the settings rows (decision 0111: none has an environment layer;
//! `preferences.sweep_threads` changes no byte of an answer, the paramscan determinism pin; `policy.*` guards the edge
//! surfaces only; the cache sizes and floors change no answer); the strategies folder's path and the runs root (the
//! file is keyed by its text, a model by its text); the client and its connection; the time of the request, beyond the
//! window rule (a closed window over pinned days answers the same tomorrow).
//!
//! **Never answered from a saved run** (each runs as before, and is saved as before):
//! - a request asked FRESH (`Request::Fresh`: `vike-cli backtest run --fresh`, the MCP tools' `fresh`, the Studio's
//!   Recompute): it does none of this module's work (no artifact hash, no datahub dial, no day stamps), and its run is
//!   saved with no key;
//! - a request whose WINDOW reaches today's UTC day ([`reaches_today`], read off the request before anything is hashed;
//!   a read that reaches it is refused its witness too);
//! - a strategy that is not a `.rs` FILE run in a sandboxed child (a `.rhai` file, an editor buffer): its reads go to
//!   the store from this process with no relay to witness them;
//! - a `[[mounts]]` PORTFOLIO (`runs_mounts`): the key hashes one strategy text and one artifact, so keying a portfolio
//!   on one of its files would answer a changed other mount with a stale run;
//! - a strategy whose CODE names a source of nondeterminism ([`nondeterministic`]: the wall clock, a random number,
//!   a hash map's per-process order, a thread): run twice it may answer twice differently, and a saved answer would
//!   hide that;
//! - a whole Studio verb (`vike_backtesthub_client::proto::may_answer_from_saved`; a profile's WHOLE report is answered
//!   from a saved run, up to [`KEPT_ANSWER_MAX_BYTES`]), a daemon that saves no run or answers nothing so (a test server, a
//!   child: `StudyDirs::answers_from_saved` is `None`), a daemon whose program changed on disk or whose datahub does not
//!   answer;
//! - a run whose reads with no day stamps pass `crate::strategy_sandbox::witness::HASHED_MAX_BYTES` (a big depth or
//!   chain run: its hashing stops there), whose answer is over [`KEPT_ANSWER_MAX_BYTES`] (a search's whole report can be
//!   gigabytes), whose saved
//!   report names another strategy text than the one keyed (the file changed under the request), or whose held
//!   points did not survive to be kept;
//! - a sweep is one request: it is answered from a saved run only when the WHOLE request is identical, and no saved
//!   point is ever mixed into a running sweep.
//!
//! **The lookup costs one manifest, not the runs root**: the keyed runs are indexed in memory per runs root ([`warm`]
//! reads every manifest once, when the daemon starts), kept in step by this process's saves ([`remember`]) and prunes
//! ([`forget`]); the one candidate's manifest is read again and must still carry the key. A hit HOLDS the saved run's
//! whole curve (or the event log it is rebuilt from) as a fresh run's is held, so its pages outlive the saved run; a
//! single run whose curve is gone under its lookup (the retention ran on another connection) is a miss.
//!
//! A saved run written before this module keeps no key and is never a hit; it reads as it always did. A run the
//! retention removed is gone, and so is its answer. Nothing here keeps market data: a witness is a stamp or a hash.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vike_backtesthub_client::proto::{Request, Response, may_answer_from_saved};
use vike_backtesthub_client::summary::{ParamscanRowSummaryWire, RunSummaryWire};
use vike_data::HistoryStore;
use vike_hub_protocol::hash::sha256_hex;
use vike_model::Trade;
use vike_model::time::clock::WALL_CLOCK_READERS;

use super::StudyDirs;
use crate::strategy_sandbox::RustRun;
use crate::strategy_sandbox::witness::{ReadWitnesses, WitnessedRead, still_holds};

/// The key under a saved run's `detail` its input key is kept under.
pub(crate) const INPUT_KEY: &str = "input_key";

/// The key under a saved run's `detail` that names its `vike_runs::KEPT_ANSWER_FILE`: the file's FNV-1a 64 and length.
pub(crate) const KEPT_ANSWER_KEY: &str = "kept_answer";

/// The key a TEXT report answered from a saved run carries last: the saved run's id. A summary carries the same words
/// as its field `answered_from_saved_run`.
pub(crate) const ANSWERED_FROM_KEY: &str = "answered_from_saved_run";

/// The input key's own format: a change to what the key hashes moves it, so no older key is ever equal to a newer one.
const KEY_FORMAT: u32 = 2;

/// The most bytes a kept answer may take (a summary is tens of kilobytes, a held scan's trades megabytes); a run whose
/// answer passes it is saved as before and answers no request again.
pub(crate) const KEPT_ANSWER_MAX_BYTES: usize = 64 << 20;

/// What a strategy's code may name that makes two runs of it answer differently, as whole identifiers ([`nondeterministic`]).
///
/// ⚠ A residual no word marks: a pointer cast to an integer with `as` (`&x as *const _ as usize`) reads an address,
/// which differs per process, and a text scan cannot tell that cast from arithmetic. `addr` and `expose_provenance` are
/// caught; the bare cast is not.
///
/// The SDK's own clock (`vike_model::time::clock`, re-exported to every strategy) is not listed here: its names are
/// `vike_model::time::clock::WALL_CLOCK_READERS`, which [`nondeterministic`] reads beside this list, so a clock
/// function added to the SDK is caught without an edit here.
const NONDETERMINISTIC: &[&str] = &[
    // The standard library's wall clock.
    "SystemTime",
    "Instant",
    "UNIX_EPOCH",
    // A hash map's iteration order is random per process.
    "HashMap",
    "HashSet",
    "RandomState",
    // Random numbers.
    "rand",
    "random",
    "thread_rng",
    "OsRng",
    "getrandom",
    "fastrand",
    // The SDK's order-id generator draws its session from the OS (`vike_model::OrderClientIdGenerator::new`, `new_v2`).
    "OrderClientIdGenerator",
    // Threads, whose interleaving is not fixed, and the machine they run on.
    "spawn",
    "spawn_scoped",
    "spawn_unchecked",
    "available_parallelism",
    "rayon",
    // The process itself (`std::process::id()`) and an address read as a number.
    "process",
    "addr",
    "expose_provenance",
];

/// What a strategy's code may name as the START of an identifier with the same effect: the CPU's cycle counter and
/// random-number instructions (`_rdtsc`, `_rdrand64_step`, `_rdseed32_step`).
const NONDETERMINISTIC_PREFIXES: &[&str] = &["_rdtsc", "_rdrand", "_rdseed"];

/// **What this daemon answers identical requests from its saved runs WITH** (`StudyDirs::answers_from_saved`; `None`:
/// it answers nothing so and keys nothing, every test server's and the sandboxed child's): the program a sandboxed child
/// runs, hashed once, and the datahub its reads go to. `crate::backtest_cli`'s `--addr` arm builds it when the daemon
/// starts ([`Self::of_running_program`]).
#[derive(Clone)]
pub struct AnswersFromSaved {
    executable: Arc<Executable>,
    /// The datahub's description of itself, asked at every request (`None`: it did not answer, so nothing is keyed);
    /// absent for a daemon over a store that is no datahub (a test's).
    datahub: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
}

impl std::fmt::Debug for AnswersFromSaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnswersFromSaved")
            .field("executable", &self.executable.path)
            .field("sha256", &self.executable.sha256)
            .finish_non_exhaustive()
    }
}

/// A program file: its path, the sha256 of its bytes and the length and modification time they were read at.
#[derive(Debug)]
struct Executable {
    path: PathBuf,
    sha256: String,
    stamp: (u64, Option<std::time::SystemTime>),
    /// Whether the change of the file on disk was said already.
    changed_said: AtomicBool,
}

/// The programs hashed in this process, by path: a second daemon (a test's) over the same program hashes nothing again
/// while the file is as it was.
static HASHED: Mutex<BTreeMap<PathBuf, Arc<Executable>>> = Mutex::new(BTreeMap::new());

impl AnswersFromSaved {
    /// For the program this process's sandboxed children run (`crate::strategy_sandbox`'s `program`: this executable),
    /// its reads going to the datahub `datahub` describes. `None` (logged) when the program cannot be read.
    pub fn of_running_program(
        datahub: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    ) -> Option<Self> {
        let program = crate::strategy_sandbox::program().ok()?;
        Self::of_program(&program, datahub)
    }

    /// [`Self::of_running_program`] for the program at `program`.
    pub fn of_program(
        program: &Path,
        datahub: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    ) -> Option<Self> {
        let stamp = stamp_of(program)?;
        let mut hashed = HASHED.lock().unwrap_or_else(PoisonError::into_inner);
        let executable = match hashed.get(program).filter(|known| known.stamp == stamp) {
            Some(known) => Arc::clone(known),
            None => {
                let sha256 = match vike_hub_protocol::hash::sha256_hex_of_file(program) {
                    Ok(sha256) => sha256,
                    Err(e) => {
                        tracing::warn!(
                            "backtesthub: {} could not be read ({e}), so no run is answered from a saved run",
                            program.display()
                        );
                        return None;
                    }
                };
                tracing::info!(
                    "backtesthub: identical runs are answered from saved runs of program {} (sha256 {sha256})",
                    program.display()
                );
                let path = program.to_path_buf();
                let known = Arc::new(Executable {
                    path: path.clone(),
                    sha256,
                    stamp,
                    changed_said: AtomicBool::new(false),
                });
                hashed.insert(path, Arc::clone(&known));
                known
            }
        };
        Some(Self { executable, datahub })
    }

    /// The daemon's part of an input key now: the program's sha256 while its file is as it was hashed, the daemon's
    /// commit `build`, the datahub's description. `None` once the program changed on disk (said once, at `warn`), or
    /// when the datahub does not answer.
    fn identity(&self, build: Option<&str>) -> Option<String> {
        let executable = &self.executable;
        if stamp_of(&executable.path) != Some(executable.stamp) {
            if !executable.changed_said.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "backtesthub: {} changed on disk since this daemon started, so no run is answered from a saved \
                     run until it restarts on it",
                    executable.path.display()
                );
            }
            return None;
        }
        let datahub = match &self.datahub {
            Some(describe) => describe()?,
            None => "none".to_string(),
        };
        Some(format!(
            "program {}\nbuild {}\ndatahub {datahub}",
            executable.sha256,
            build.unwrap_or("none")
        ))
    }
}

/// A file's length and modification time: what tells a file rewritten under its name.
fn stamp_of(path: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// A request this daemon may answer from a saved run, keyed BEFORE it runs, and the reads its relay records as it runs.
pub(crate) struct Reusable {
    key: String,
    /// A profile request's `name` (its `None`: unnamed), which the key leaves out; `None` for a Studio request.
    name: Option<Option<String>>,
    /// The strategy text the key names: a run whose report names another one keeps nothing.
    strategy_sha256: String,
    pub(crate) reads: ReadWitnesses,
}

/// What a run keeps to answer again, handed to the saver (`super::saved_run`'s `SavedRunStart`).
#[derive(Debug)]
pub(crate) struct Kept {
    key: String,
    strategy_sha256: String,
    bytes: Vec<u8>,
}

impl Kept {
    /// The bytes of `vike_runs::KEPT_ANSWER_FILE`.
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// `vike_runs::KEPT_ANSWER_FILE`'s document (msgpack with field names, so a later build reads an earlier one's).
#[derive(Serialize, Deserialize)]
struct KeptAnswer {
    /// Every read the run made, with its witness.
    reads: Vec<WitnessedRead>,
    /// The answer as its client got it, its held run's name and its saved run's id blank.
    answer: Response,
    /// A summarized single run's one point with its trades, or a scan's points: what `Request::ReadTrades` and
    /// `Request::ReadPoint` page after it.
    points: Vec<(RunSummaryWire, Vec<Trade>)>,
    /// A summarized profile search's rows: what `Request::ReadParamscanRows` pages.
    rows: Vec<ParamscanRowSummaryWire>,
}

/// [`KeptAnswer`] over borrowed parts, the same fields in the same order, so its bytes read back as one: what a run is
/// kept as without copying the points and trades its summary holds.
#[derive(Serialize)]
struct KeptAnswerOf<'a> {
    reads: &'a [WitnessedRead],
    answer: &'a Response,
    points: &'a [(RunSummaryWire, Vec<Trade>)],
    rows: &'a [ParamscanRowSummaryWire],
}

/// A buffer that refuses to grow past [`KEPT_ANSWER_MAX_BYTES`]: an answer over the cap stops being encoded there.
struct Capped(Vec<u8>);

impl Write for Capped {
    fn write(&mut self, part: &[u8]) -> io::Result<usize> {
        if self.0.len() + part.len() > KEPT_ANSWER_MAX_BYTES {
            return Err(io::Error::other(format!("over {KEPT_ANSWER_MAX_BYTES} bytes")));
        }
        self.0.extend_from_slice(part);
        Ok(part.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// **`request`'s input key**, or `None` when it may not be answered from a saved run nor kept for one (this module's
/// doc). `run` is its `.rs` file as `crate::strategy_sandbox::resolve` read and built it (once, for the run too);
/// `now_ms` is when it arrived (the window rule). A request asked `fresh`, or one that may not be kept, does none of the
/// costly work: the cheap refusals come first, and only then the datahub's dial and the artifact's hash.
pub(crate) fn reusable(
    request: &Request,
    study: &StudyDirs,
    run: &RustRun,
    now_ms: i64,
    fresh: bool,
) -> Option<Reusable> {
    // shortcut: a `[[mounts]]` portfolio is never keyed, because the key hashes ONE strategy text and ONE
    // artifact, so a changed second mount would be answered by a stale run; key it once the key hashes every
    // mount's text and artifact.
    if fresh || !study.save_backtest_runs || !may_answer_from_saved(request) || runs_mounts(request)
    {
        return None;
    }
    let answers = study.answers_from_saved.as_ref()?;
    if let Some(named) = nondeterministic(&run.source.text) {
        tracing::info!(
            "backtesthub: {} names `{named}`, so a run of it is never answered from a saved run",
            run.artifact
        );
        return None;
    }
    let reads = ReadWitnesses::new(now_ms);
    if reaches_today(request, reads.today()) {
        return None;
    }
    let hub = answers.identity(study.build.as_deref())?;
    let built = std::fs::read(crate::studio_run::strategy_files_dir().join(&run.artifact)).ok()?;
    let (key, name) = input_key(&hub, &run.source.sha256, &built, &run.models, request)?;
    let strategy_sha256 = run.source.sha256.clone();
    Some(Reusable { key, name, strategy_sha256, reads })
}

/// Whether `request` runs a profile of `[[mounts]]` (a portfolio), which [`reusable`] never keys.
fn runs_mounts(request: &Request) -> bool {
    match request {
        Request::Summarized(inner) => runs_mounts(inner),
        Request::RunBacktest(text)
        | Request::RunParamscanProfile { profile_toml: text, .. }
        | Request::RunWalkforwardProfile { profile_toml: text } => {
            vike_backtest::run::profile::ProfileBacktest::from_toml_str(text)
                .is_ok_and(|profile| profile.is_portfolio())
        }
        _ => false,
    }
}

/// Whether `request`'s window reaches `today` (the first millisecond of today's UTC day): an open end, or an end in it
/// or after it, read off its profile's `[data]` or its Studio slice. Such a run is never kept, so nothing is hashed for
/// it. A profile that does not parse reaches nothing here (its run refuses it, or its reads are refused their witness).
pub(crate) fn reaches_today(request: &Request, today: i64) -> bool {
    let end = match request {
        Request::Summarized(inner) => return reaches_today(inner, today),
        Request::RunBacktest(text)
        | Request::RunParamscanProfile { profile_toml: text, .. }
        | Request::RunWalkforwardProfile { profile_toml: text } => {
            let profile = vike_backtest::run::profile::ProfileBacktest::from_toml_str(text);
            match profile.and_then(|profile| profile.range()) {
                Ok(range) => range.end,
                Err(_) => return false,
            }
        }
        Request::RunSlice { slice, .. }
        | Request::RunParamscan { slice, .. }
        | Request::RunWalkforward { slice, .. } => slice.end,
        _ => return false,
    };
    end.is_none_or(|end| end >= today)
}

/// **The datahub as the key names it**, from the features a FRESH dial's `Welcome` advertised: its build
/// (`vike_datahub_client::proto::advertised_build`, the sha256 of the program it runs) when it states one, else every
/// feature it advertised (a datahub older than the `build=` entry, or one still hashing itself at its start).
pub(crate) fn datahub_identity(features: &[String]) -> String {
    match vike_datahub_client::proto::advertised_build(features) {
        Some(build) => format!("build {build}"),
        None => format!("features {}", features.join(" ")),
    }
}

/// **The input key itself** (this module's doc): the sha256 over the daemon's identity `hub` ([`AnswersFromSaved`]'s:
/// its program, its commit, its datahub), the strategy text's sha256, the built artifact's bytes' sha256, the model texts
/// and `request` with its profile's top-level `name` dropped ([`drop_profile_name`]), and that name.
pub(crate) fn input_key(
    hub: &str,
    strategy_sha256: &str,
    artifact: &[u8],
    models: &BTreeMap<String, String>,
    request: &Request,
) -> Option<(String, Option<Option<String>>)> {
    let mut asked = request.clone();
    let name = drop_profile_name(&mut asked);
    let material = format!(
        "vike input key {KEY_FORMAT}\n{hub}\nstrategy {strategy_sha256}\nartifact {}\nmodels {}\nrequest {}\n",
        sha256_hex(artifact),
        serde_json::to_string(models).ok()?,
        serde_json::to_string(&asked).ok()?,
    );
    Some((sha256_hex(material.as_bytes()), name))
}

/// The first identifier of [`NONDETERMINISTIC`] `text`'s code names, its comment lines left out (a strategy file's
/// header and docs are comments).
pub(crate) fn nondeterministic(text: &str) -> Option<&'static str> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')))
        .find_map(|word| {
            let whole =
                WALL_CLOCK_READERS.iter().chain(NONDETERMINISTIC).find(|named| **named == word);
            whole.or_else(|| {
                NONDETERMINISTIC_PREFIXES.iter().find(|named| word.starts_with(**named))
            })
        })
        .copied()
}

/// Drop the top-level `name` of the profile `request` carries (a profile verb's, or one a `Summarized` wraps) from its
/// text ([`without_name`]), answering that name; `None` for a request that carries no profile.
fn drop_profile_name(request: &mut Request) -> Option<Option<String>> {
    let text = match request {
        Request::RunBacktest(text)
        | Request::RunParamscanProfile { profile_toml: text, .. }
        | Request::RunWalkforwardProfile { profile_toml: text } => text,
        Request::Summarized(inner) => return drop_profile_name(inner),
        _ => return None,
    };
    let (unnamed, name) = without_name(text);
    *text = unnamed;
    Some(name)
}

/// `profile_toml` without its top-level `name = …` line, and that name. The line is dropped only when the text left
/// parses to exactly the profile less its `name` (no other line held a part of it); otherwise the text is kept whole,
/// so the key holds the name and a differently named profile only misses.
pub(crate) fn without_name(profile_toml: &str) -> (String, Option<String>) {
    let Ok(whole) = toml::from_str::<toml::Table>(profile_toml) else {
        return (profile_toml.to_string(), None);
    };
    let name = whole.get("name").and_then(toml::Value::as_str).map(str::to_string);
    let mut tables = false;
    let unnamed: String = profile_toml
        .split_inclusive('\n')
        .filter(|line| {
            let line = line.trim_start();
            tables |= line.starts_with('[');
            tables
                || !line.strip_prefix("name").is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .collect();
    let mut expected = whole;
    expected.remove("name");
    match toml::from_str::<toml::Table>(&unnamed) {
        Ok(table) if table == expected => (unnamed, name),
        _ => (profile_toml.to_string(), name),
    }
}

impl Reusable {
    /// **What the run that just answered `response` keeps to answer again**, or `None` (said in the daemon's log when
    /// it is a choice, not a refusal): its reads with their witnesses, the answer with its held names blank, and what a
    /// summary holds for its later reads.
    pub(crate) fn kept(&self, response: &Response) -> Option<Kept> {
        let unkept = |why: &str| {
            tracing::info!("backtesthub: this run will answer no request again: {why}");
            None
        };
        // A text over the cap is refused before it is copied: a search's whole report can be gigabytes.
        let text = match response {
            Response::Error(_) => return None,
            Response::Report(text)
            | Response::ParamscanReport(text)
            | Response::WalkforwardReport(text)
            | Response::CombinatorialWalkforwardReport(text)
            | Response::PermutationReport(text) => text.len(),
            _ => 0,
        };
        if text > KEPT_ANSWER_MAX_BYTES {
            return unkept(&format!("its answer is {text} bytes, over {KEPT_ANSWER_MAX_BYTES}"));
        }
        let reads = match self.reads.finish() {
            Ok(reads) if reads.is_empty() => {
                return unkept("it read nothing a witness could stand for");
            }
            Ok(reads) => reads,
            Err(why) => return unkept(&why),
        };
        let mut answer = response.clone();
        let held = held_name(&mut answer);
        // The held parts are SHARED, never copied (`crate::run_summary::held_parts`), and encoded from there.
        let (points, rows) =
            held.as_deref().map(crate::run_summary::held_parts).unwrap_or_default();
        let whole = match &answer {
            Response::RunSummary(_) => points.len() == 1,
            Response::ParamscanSummary(scan) => points.len() == scan.points.len(),
            Response::ParamscanProfileSummary(search) => rows.len() == search.total,
            _ => true,
        };
        if !whole {
            return unkept("its held run is gone");
        }
        let document =
            KeptAnswerOf { reads: &reads, answer: &answer, points: &points, rows: &rows };
        let mut bytes = Capped(Vec::new());
        if let Err(e) = rmp_serde::encode::write_named(&mut bytes, &document) {
            return unkept(&format!("its answer was not kept: {e}"));
        }
        let (key, strategy_sha256) = (self.key.clone(), self.strategy_sha256.clone());
        Some(Kept { key, strategy_sha256, bytes: bytes.0 })
    }
}

/// The held run name `answer` carries (a summary's), cleared with its saved run's id, so a kept answer names neither.
fn held_name(answer: &mut Response) -> Option<String> {
    match answer {
        Response::RunSummary(summary) => {
            summary.saved_run.clear();
            Some(std::mem::take(&mut summary.run))
        }
        Response::ParamscanSummary(scan) => {
            if let Some(best) = &mut scan.best {
                best.run.clear();
            }
            Some(std::mem::take(&mut scan.run))
        }
        Response::ParamscanProfileSummary(search) => {
            search.saved_run.clear();
            Some(std::mem::take(&mut search.run))
        }
        _ => None,
    }
}

/// **The saver's half** (`super::saved_run`): when `kept` is given and the saved report names the strategy text the key
/// names (`detail.strategy_sha256`), `detail` gains the input key and the kept file's name, and `kept` comes back to be
/// written beside the manifest ([`Kept::bytes`]) and [`remember`]ed once the run is finished; otherwise nothing is kept.
pub(super) fn keep_into<'k>(kept: Option<&'k Kept>, detail: &mut Value) -> Option<&'k Kept> {
    let kept = kept?;
    let named = detail.get("strategy_sha256").and_then(Value::as_str);
    if named != Some(kept.strategy_sha256.as_str()) {
        tracing::info!(
            "backtesthub: this run will answer no request again: its report names the strategy text {named:?}, \
             and its key {} (the file changed while it ran, or the report names none)",
            kept.strategy_sha256
        );
        return None;
    }
    detail[INPUT_KEY] = json!(kept.key);
    detail[KEPT_ANSWER_KEY] = json!({
        "fnv": vike_utility::fnv::fnv1a64_hex(&kept.bytes),
        "bytes": kept.bytes.len(),
    });
    Some(kept)
}

/// Every runs root's keyed saved runs, run id to input key: read once from its manifests (the first lookup or save
/// under it, or [`warm`]), then kept in step by this process's saves ([`remember`]) and prunes ([`forget`]). A run
/// removed by anything else is found gone at its lookup, and dropped then.
static INDEX: Mutex<BTreeMap<PathBuf, BTreeMap<String, String>>> = Mutex::new(BTreeMap::new());

/// `f` over `runs_root`'s index, read from its manifests first when this process has none of it.
fn with_index<R>(runs_root: &Path, f: impl FnOnce(&mut BTreeMap<String, String>) -> R) -> R {
    let mut indexes = INDEX.lock().unwrap_or_else(PoisonError::into_inner);
    let index = indexes.entry(runs_root.to_path_buf()).or_insert_with(|| indexed(runs_root));
    f(index)
}

/// Every finished run under `runs_root` whose manifest keeps an input key (its directory's own: a manifest naming
/// another run is no entry), read from its manifest.
fn indexed(runs_root: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(runs_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let keyed = read_keyed(&entry.path())?;
            let key = keyed.detail.input_key?;
            (keyed.run_id == entry.file_name().to_string_lossy()).then_some((keyed.run_id, key))
        })
        .collect()
}

/// The fields a lookup reads of the manifest in `dir`.
fn read_keyed(dir: &Path) -> Option<Keyed> {
    let manifest = std::fs::read(dir.join(vike_runs::MANIFEST_FILE)).ok()?;
    serde_json::from_slice(&manifest).ok()
}

/// Read `runs_root`'s keyed runs into memory now (the daemon's start, so no request pays it), answering how many there
/// are.
pub(crate) fn warm(runs_root: &Path) -> usize {
    with_index(runs_root, |index| index.len())
}

/// Saved run `run_id`, finished under `runs_root`, keeps `kept`'s key.
pub(super) fn remember(runs_root: &Path, run_id: &str, kept: &Kept) {
    with_index(runs_root, |index| index.insert(run_id.to_string(), kept.key.clone()));
}

/// The runs `run_ids` under `runs_root` are gone (the retention removed them).
pub(crate) fn forget(runs_root: &Path, run_ids: &[String]) {
    with_index(runs_root, |index| {
        for run_id in run_ids {
            index.remove(run_id);
        }
    });
}

/// The fields of a manifest a lookup reads; the rest is skipped.
#[derive(Deserialize)]
struct Keyed {
    run_id: String,
    #[serde(default)]
    config: KeyedConfig,
    #[serde(default)]
    detail: KeyedDetail,
}

#[derive(Default, Deserialize)]
struct KeyedConfig {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
struct KeyedDetail {
    #[serde(default)]
    input_key: Option<String>,
    #[serde(default)]
    kept_answer: Option<KeptNamed>,
}

/// What a manifest names of its kept answer.
#[derive(Deserialize)]
struct KeptNamed {
    fnv: String,
    bytes: usize,
}

/// **The answer of the newest FINISHED saved run under `runs_root` keyed as `reusable` is, when every read it recorded
/// still holds over `store`**: held again for the reads after it (`crate::run_summary::hold_again`), its saved run named
/// and the words "answered from saved run <id>" in it. `None` is a miss: no such run, or one whose answer does not read
/// back, whose name differs where its answer names it, or whose data moved (logged).
pub(crate) fn answer(
    reusable: &Reusable,
    runs_root: &Path,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Option<Response> {
    let (found, dir) = newest_keyed(runs_root, &reusable.key)?;
    let refused = |why: String| {
        tracing::info!("backtesthub: not answered from saved run {}: {why}", found.run_id);
        None
    };
    let bytes = match vike_runs::read_kept_answer(&dir) {
        Ok(bytes) => bytes,
        Err(e) => return refused(e.to_string()),
    };
    let named = found.detail.kept_answer.as_ref();
    let fnv = vike_utility::fnv::fnv1a64_hex(&bytes);
    if named.is_none_or(|named| named.fnv != fnv || named.bytes != bytes.len()) {
        return refused(format!(
            "its {} is not the one its manifest names",
            vike_runs::KEPT_ANSWER_FILE
        ));
    }
    let kept: KeptAnswer = match rmp_serde::from_slice(&bytes) {
        Ok(kept) => kept,
        Err(e) => return refused(format!("its kept answer does not read: {e}")),
    };
    // A summary of a single run names no profile; every other answer may, and then the name is part of it.
    let nameless = matches!(kept.answer, Response::RunSummary(_));
    if let Some(name) = &reusable.name
        && !nameless
        && *name != found.config.name
    {
        return refused("its answer carries another profile name".to_string());
    }
    if let Err(why) = still_holds(&kept.reads, store.as_ref()) {
        return refused(why);
    }
    // A single run's whole curve is held as a fresh run's is (its curve, or the event log it is rebuilt from), so its
    // pages do not need the saved run to outlive the retention. A run that holds neither was pruned under this
    // lookup (the retention runs on another connection's thread): a miss, never an answer whose curve is gone.
    let (curve, log) = match kept.answer {
        Response::RunSummary(_) => match crate::run_summary::held_of_saved(&dir) {
            (None, None) => {
                return refused("its whole curve is gone (the retention removed it)".into());
            }
            held => held,
        },
        _ => (None, None),
    };
    let answer = crate::run_summary::hold_again(
        kept.answer,
        kept.points,
        kept.rows,
        (curve, log),
        &found.run_id,
    );
    tracing::info!("backtesthub: answered from saved run {}", found.run_id);
    Some(marked(answer, &found.run_id))
}

/// The newest finished run under `runs_root` whose manifest keeps `key`, and its directory: the index's newest candidate
/// (run ids open with the second they started, so the greatest is the newest) whose manifest, read again, still names
/// that run and that key. A candidate whose directory is gone, or whose manifest says otherwise, is dropped from the
/// index and the next one tried.
fn newest_keyed(runs_root: &Path, key: &str) -> Option<(Keyed, PathBuf)> {
    let mut candidates: Vec<String> = with_index(runs_root, |index| {
        index.iter().filter(|(_, k)| k.as_str() == key).map(|(id, _)| id.clone()).collect()
    });
    while let Some(run_id) = candidates.pop() {
        let found = vike_runs::finished_run_dir(runs_root, &run_id).ok().and_then(|dir| {
            let keyed = read_keyed(&dir)?;
            (keyed.run_id == run_id && keyed.detail.input_key.as_deref() == Some(key))
                .then_some((keyed, dir))
        });
        match found {
            Some(found) => return Some(found),
            None => forget(runs_root, &[run_id]),
        }
    }
    None
}

/// `answer` saying it came from saved run `run_id`: a summary's `answered_from_saved_run`, a text report's last key.
fn marked(answer: Response, run_id: &str) -> Response {
    let text = |json: String| match json.strip_suffix('}') {
        Some(open) if open.len() > 1 => {
            format!("{open},\"{ANSWERED_FROM_KEY}\":{}}}", json!(run_id))
        }
        _ => json,
    };
    match answer {
        Response::RunSummary(mut summary) => {
            summary.answered_from_saved_run = run_id.to_string();
            Response::RunSummary(summary)
        }
        Response::ParamscanSummary(mut scan) => {
            scan.answered_from_saved_run = run_id.to_string();
            // The best point is what the Studio opens first, and says it too.
            if let Some(best) = &mut scan.best {
                best.answered_from_saved_run = run_id.to_string();
            }
            Response::ParamscanSummary(scan)
        }
        Response::WalkforwardSummary(mut walk) => {
            walk.answered_from_saved_run = run_id.to_string();
            Response::WalkforwardSummary(walk)
        }
        Response::ParamscanProfileSummary(mut search) => {
            search.answered_from_saved_run = run_id.to_string();
            Response::ParamscanProfileSummary(search)
        }
        Response::Report(json) => Response::Report(text(json)),
        Response::ParamscanReport(json) => Response::ParamscanReport(text(json)),
        Response::WalkforwardReport(json) => Response::WalkforwardReport(text(json)),
        Response::CombinatorialWalkforwardReport(json) => {
            Response::CombinatorialWalkforwardReport(text(json))
        }
        Response::PermutationReport(json) => Response::PermutationReport(text(json)),
        other => other,
    }
}

#[cfg(test)]
#[path = "answer_from_saved_tests.rs"]
mod answer_from_saved_tests;
