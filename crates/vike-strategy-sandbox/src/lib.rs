//! `vike-backend strategy-sandbox`: the ONE process that loads a Rust strategy file.
//!
//! A hub never loads a strategy file (decisions 0082/0083: the order-signing binary loads no code, and the
//! backtesthub joined it). It spawns this verb instead, one child per backtest request or per live mount,
//! with `vike_sandbox::spawn_strategy_process` (an empty environment, working directory `/`, three pipes),
//! and the child jails itself BEFORE it loads the file, because a file can run code at load time
//! (`.init_array`).
//!
//! # The verbs
//!
//! - `strategy-sandbox backtest <builds-dir> <artifact>...` — a whole backtesthub request runs inside the jail:
//!   the request comes down the protocol, the history the run reads goes up it (answered from the hub's
//!   store, read verbs only), and the finished wire answer comes back, produced by the backtesthub's own
//!   verb code ([`BacktestServe`]), so its bytes are the in-process run's. Each `<artifact>` is the path the
//!   builder answered, `sdk-N/<folder>/<sha>.so` under the builds directory (decision 0296), checked here
//!   by its SHAPE ([`artifact_of`]): the folder is named by the SDK's marker, which the jailed child never
//!   reads. One artifact for a `[strategy]` run; a portfolio backtest names one per `.rs` mount, in mount
//!   order (decision 0281: a Rust strategy file runs sandboxed with its whole backtest, so every mount of
//!   one run loads in ONE child), and the jail admits exactly those files ("The artifact grant", below).
//! - `strategy-sandbox study <builds-dir> <artifact>` — one research STUDY file run (owner's rule, 2026-11-15:
//!   a study is a FILE, built, loaded and sandboxed exactly like a strategy file): the same jail and the
//!   same build folder, the study loaded by `vike_rust_strategy::loader::load_study`, and the run served
//!   by the backtesthub's own code ([`StudyServe`]): everything the study asks (its context, its learner,
//!   its one backtest) goes up the protocol to the process that holds the store and the trainer and can
//!   start a strategy file's own sandboxed backtest; that process's reads of the store the study handed
//!   its backtest come back down to the study file; and its finished answer comes back.
//! - `strategy-sandbox live <builds-dir> <artifact>` — one live mount's hooks (unit F5 of plan 2b), `<artifact>`
//!   the builder's answer under `<builds-dir>`, as the backtest verb is named its artifacts.
//! - `strategy-sandbox live-load <builds-dir> <artifact>` — the live verb up to its load, then exit (0 loaded, 3
//!   the jail refused, 4 the loader did): the deploy's pre-flight of a mounted file's new artifact (`live.rs`).
//!
//! # The order inside the child (`vike_sandbox::StrategyProcessPolicy::apply`'s rules)
//!
//! 1. **The jail, first**: before any thread starts, before any descriptor opens (it refuses a process
//!    holding one above stderr) and before logging starts. Then a console-only subscriber on stderr at
//!    `warn` (`vike_log::init`, no file layer: the jail grants no write), so this process's own warnings
//!    reach the reason pipe. ⚠ A strategy FILE's `tracing` events do not reach it: the artifact links its
//!    own copy of `tracing` with its own global dispatcher.
//! 2. **The protocol's output leaves fd 1, and fd 1 becomes stderr.** A strategy file links its OWN copy
//!    of std, so a `println!` in a user's `on_bar` writes fd 1 whatever this process does with its
//!    `stdout()` handle; on a stdout protocol that line would be a garbage frame and kill the run. After
//!    the move it lands in the reason pipe the parent drains. Done after the jail, because the copy of
//!    the protocol's descriptor is a descriptor above stderr.
//! 3. **The load**, inside the jail (`vike_rust_strategy::loader::load`), of each artifact in turn. A missing
//!    artifact (no entry at its path, or a "sha" that is not 64 hex digits, which names none) is the loader's
//!    own `Missing` refusal, built without a load, and nothing is jailed or loaded.
//! 4. **The verb**, with stdin as the protocol's input.
//!
//! Where the jail cannot be applied (not Linux, a kernel without Landlock ABI 4, a unit without
//! `@sandbox`, an artifact it refuses) nothing loads and the refusal, by name, is the run's strategy error.
//! No override.
//!
//! # The artifact grant
//!
//! The jail does not trust the builds directory's NAMES: `vike_sandbox::StrategyProcessPolicy::apply` opens
//! each artifact beneath its root with no symbolic link followed and requires a regular file, then grants READ
//! on the inode it opened (`crates/vike-sandbox/src/lib.rs`'s module doc, "The artifact grant"). The root is
//! `<builds-dir>` for every verb (backtest, study and live), so no component of `sdk-N/<folder>/<sha>.so` may
//! be a link and nothing outside `<builds-dir>` is reached. An artifact path that is a link (to `vike.db`, to a folder), passes through
//! one or is not a regular file REFUSES the whole run, naming it, before anything loads; a path swapped to a
//! link after the grant names an inode the jail never granted, so a read through it is refused.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use vike_model::Strategy;
use vike_rust_strategy::host::StrategyFileHost;
use vike_rust_strategy::loader::{self, LoadError};
use vike_rust_strategy::sdk::Sdk;
use vike_simulator::OrderBrokerSimulator;
use vike_utility::errors::CarriedRefusal;

mod live;
pub mod refusals;

use refusals::SandboxRefusal;

/// The backtesthub's half of a sandboxed backtest (`vike_backtesthub::strategy_sandbox::serve_child`):
/// read ONE request from `input`, run it, write its answer to `output`, reading history through the
/// same channel. `builds` holds one entry per artifact of the argv, in its order (a portfolio's `.rs`
/// mounts in mount order): the loaded file — a factory of strategies over a params table (TOML text) and
/// the mount facts (JSON text), called once per run, sweep point and walk-forward window, answering the
/// file's own refusal of those params when it has one (its `refuse` export, in its words) — or the jail's
/// refusal (its id kept) or the load's, in the loader's words; beside it the numbered SDK it was built
/// against (`None`: SDK dev), read
/// off the artifact's path ([`artifact_of`]), which the run's results record. `identity` is this binary's
/// build, the first frame it states to its backtesthub ([`run`]'s).
///
/// A function handed in by the composition root (`crates/vike-backend/src/main.rs`), not a dependency: this crate
/// and `vike-backtesthub` are both layer 65, and the backtesthub must not link the loader.
pub type BacktestServe = fn(
    input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
    identity: &str,
    builds: Vec<(
        Result<
            Box<
                dyn Fn(&str, &str) -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, String>
                    + Send
                    + Sync,
            >,
            CarriedRefusal,
        >,
        Option<u32>,
    )>,
) -> ExitCode;

/// The backtesthub's half of a sandboxed research STUDY (`vike_backtesthub::study_sandbox::serve_child`):
/// read the start message from `input`, run the loaded study file over it, relay what the study asks of
/// its parent and answer the parent's reads of the store it handed its backtest, then write the finished
/// answer to `output`. The study
/// file is handed over as two closures over `vike_rust_strategy::study::StudyFileLoaded` (its run, and
/// its serve of the store it handed its backtest), or the jail's refusal (its id kept) or the load's, in the
/// loader's words: the backtesthub links no loader.
pub type StudyServe = fn(
    input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
    study: Result<
        (
            Box<dyn Fn(&[u8], &(dyn Fn(&[u8]) -> Vec<u8> + Sync)) -> Vec<u8> + Send + Sync>,
            Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>,
        ),
        CarriedRefusal,
    >,
) -> ExitCode;

/// The words `-h` prints. Nobody types this verb; a hub spawns it.
const USAGE: &str = "usage: vike-backend strategy-sandbox backtest <builds-dir> <artifact>...\n\
       vike-backend strategy-sandbox study <builds-dir> <artifact>\n\
       vike-backend strategy-sandbox live <builds-dir> <artifact>\n\
       vike-backend strategy-sandbox live-load <builds-dir> <artifact>\n\n\
A hub spawns this verb to run a backtest's Rust strategy files (one per mount), or ONE research study\n\
or live mount's file, jailed (no other file, no network, no other process), with the protocol on stdin\n\
and stdout; a deploy's pre-flight runs `live-load`, which jails and loads one live file, then exits.\n\
It is not run by hand.\n";

/// Route `args` (the tail after `strategy-sandbox`) to its verb. `identity` is this binary's build
/// (`vike_buildinfo::summary`, handed in by the composition root: a library links no `vike-buildinfo`), the
/// first frame the backtest and live verbs state to their parent (`vike_hub_protocol::child_pipe::check_build`).
pub fn run(
    args: &[String],
    backtest: BacktestServe,
    study: StudyServe,
    identity: &str,
) -> ExitCode {
    match args.split_first() {
        Some((verb, _)) if verb == "-h" || verb == "--help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some((verb, [builds, artifacts @ ..])) if verb == "backtest" && !artifacts.is_empty() => {
            let builds = Path::new(builds);
            let artifacts: Vec<(PathBuf, Option<u32>)> =
                artifacts.iter().map(|artifact| artifact_of(builds, artifact)).collect();
            backtest_verb(builds, &artifacts, backtest, identity)
        }
        Some((verb, [builds, artifact])) if verb == "study" => {
            let builds = Path::new(builds);
            study_verb(builds, &artifact_of(builds, artifact).0, study)
        }
        Some((verb, rest)) if verb == "live" => live::run(rest, identity, true),
        Some((verb, rest)) if verb == "live-load" => live::run(rest, identity, false),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The artifact `artifact` names under `builds`, and the numbered SDK it was built against (`None`: SDK
/// dev, a checkout's). `artifact` is the path the builder answered, relative to `builds`
/// (`sdk-N/<folder>/<sha256>.so`, `vike_rust_strategy::artifact::artifact_path`), and it is checked by its
/// SHAPE, never recomputed (decision 0296): this child jails itself before it opens any file, so it
/// cannot read the SDK's marker the folder is named from. Exactly three plain parts: an SDK's folder
/// (`vike_rust_strategy::sdk::Sdk::of_directory_name`), eight lowercase hex digits and 64 hex digits with
/// `.so`. Anything else names nothing (`<builds>/not-an-artifact`), so no `..` can reach outside the
/// builds directory, and the loader's own refusal names the miss.
#[must_use]
pub fn artifact_of(builds: &Path, artifact: &str) -> (PathBuf, Option<u32>) {
    let parts: Vec<&str> = artifact.split('/').collect();
    let hex = |s: &str, len: usize| {
        s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let shaped = match parts.as_slice() {
        [sdk, folder, file] => Sdk::of_directory_name(sdk)
            .filter(|_| hex(folder, 8))
            .filter(|_| file.strip_suffix(".so").is_some_and(|sha| hex(sha, 64))),
        _ => None,
    };
    match shaped {
        Some(sdk) => {
            let number = match sdk {
                Sdk::Number(number) => Some(number),
                Sdk::Dev => None,
            };
            (builds.join(parts[0]).join(parts[1]).join(parts[2]), number)
        }
        None => (builds.join("not-an-artifact"), None),
    }
}

/// Steps 1 and 2 of the module doc's order, shared by the backtest and the study verb: the jail over
/// `artifacts` (each under `builds`, opened beneath it: the module doc's "The artifact grant") and nothing else
/// new (its refusal, by name, is handed on to be answered rather than acted on; the first artifact with no entry
/// at its path is the loader's `Missing` refusal, and nothing is jailed), a console-only log (kept for the whole
/// of `then`), and the protocol's output moved off fd 1; then `then`, with the two.
fn in_jail(
    builds: &Path,
    artifacts: &[PathBuf],
    then: impl FnOnce(Result<(), CarriedRefusal>, Box<dyn Write + Send>) -> ExitCode,
) -> ExitCode {
    // `symlink_metadata`, not `is_file`: it only words a missing build; what an entry may be is the jail's.
    let jailed = match artifacts.iter().find(|artifact| artifact.symlink_metadata().is_err()) {
        None => {
            // `artifact_of` joined each to `builds`; were one not under it, the jail refuses it as absolute.
            let beneath: Vec<PathBuf> = artifacts
                .iter()
                .map(|artifact| artifact.strip_prefix(builds).unwrap_or(artifact).to_path_buf())
                .collect();
            let mut policy = vike_sandbox::strategy_process_policy(builds, &beneath);
            // A whole run and its history live inside: the unit's cgroup bounds it, not the live cap.
            policy.address_space_bytes = None;
            policy.apply().map_err(|why| SandboxRefusal::Unjailed { why }.carried())
        }
        Some(missing) => Err(LoadError::Missing(missing.clone()).to_string().into()),
    };
    // After the jail (it refuses a process that started logging), console only: the jail grants no write.
    let _log = vike_log::init(vike_log::LogConfig {
        console_level: "warn".to_string(),
        file_enabled: false,
        ..vike_log::LogConfig::default()
    });
    match protocol_output() {
        Ok(output) => then(jailed, output),
        Err(e) => {
            eprintln!("strategy-sandbox: the protocol could not leave stdout: {e}");
            ExitCode::from(3)
        }
    }
}

/// The study verb: the jail, then the research study file loaded (`vike_rust_strategy::loader::load_study`)
/// and handed to the backtesthub's half as two closures.
fn study_verb(builds: &Path, artifact: &Path, serve: StudyServe) -> ExitCode {
    in_jail(builds, &[artifact.to_path_buf()], |jailed, output| {
        let input = Box::new(std::io::stdin());
        match jailed.and_then(|()| loader::load_study(artifact).map_err(|e| e.to_string().into())) {
            Ok(loaded) => {
                let loaded = Arc::new(loaded);
                let serving = Arc::clone(&loaded);
                serve(
                    input,
                    output,
                    Ok((
                        Box::new(move |start: &[u8], host: &(dyn Fn(&[u8]) -> Vec<u8> + Sync)| {
                            loaded.run(start, host)
                        }),
                        Arc::new(move |request: &[u8]| serving.serve(request)),
                    )),
                )
            }
            Err(refusal) => serve(input, output, Err(refusal)),
        }
    })
}

/// The backtest verb, in the module doc's order: `artifacts` (under `builds`) each with the SDK it was built
/// against, which the run's results record.
fn backtest_verb(
    builds: &Path,
    artifacts: &[(PathBuf, Option<u32>)],
    serve: BacktestServe,
    identity: &str,
) -> ExitCode {
    let paths: Vec<PathBuf> = artifacts.iter().map(|(artifact, _)| artifact.clone()).collect();
    in_jail(builds, &paths, |jailed, output| {
        // Step 3, each artifact in turn: loaded, or the jail's or the load's refusal in its words.
        let builds = artifacts
            .iter()
            .map(|(artifact, sdk)| {
                let loaded = load_for_run(&jailed, artifact);
                let build = loaded.map(|loaded| {
                    let vtable = loaded.vtable;
                    Box::new(move |params_toml: &str, facts_json: &str| {
                        // The file's own refusal of its params first, in its words, as a live mount asks it
                        // (`live.rs`): its build may panic on them, and the run then plans and reads nothing.
                        if let Some(why) = vike_rust_strategy::host::refusal(&vtable, params_toml) {
                            return Err(why);
                        }
                        Ok(Box::new(StrategyFileHost::<OrderBrokerSimulator>::new(
                            vtable,
                            params_toml,
                            facts_json,
                        )) as Box<dyn Strategy<OrderBrokerSimulator>>)
                    })
                        as Box<
                            dyn Fn(
                                    &str,
                                    &str,
                                )
                                    -> Result<Box<dyn Strategy<OrderBrokerSimulator>>, String>
                                + Send
                                + Sync,
                        >
                });
                (build, *sdk)
            })
            .collect();
        serve(Box::new(std::io::stdin()), output, identity, builds)
    })
}

/// Step 3 of the module doc's order for ONE artifact of a backtest: the jail's refusal (`jailed`, its id kept), else
/// the load's (the loader's words, no id). Either is handed on BARE, never `rendered()`: the backtest's carrier
/// (`vike_backtest::run::refusals::ProfileRefusal::StrategyFileLoadRefused`) shows it with the one id a person reads.
fn load_for_run(
    jailed: &Result<(), CarriedRefusal>,
    artifact: &Path,
) -> Result<loader::StrategyFileLoaded, CarriedRefusal> {
    jailed.clone().and_then(|()| loader::load(artifact).map_err(|e| e.to_string().into()))
}

/// The protocol's output, moved off fd 1, and fd 1 pointed at stderr (the module doc's step 2).
#[cfg(target_os = "linux")]
fn protocol_output() -> std::io::Result<Box<dyn Write + Send>> {
    use std::os::fd::AsFd;
    let protocol = std::io::stdout().as_fd().try_clone_to_owned()?;
    rustix::stdio::dup2_stdout(std::io::stderr().as_fd())?;
    Ok(Box::new(std::fs::File::from(protocol)))
}

/// Elsewhere the jail refuses and nothing loads, so stdout stays the protocol.
#[cfg(not(target_os = "linux"))]
fn protocol_output() -> std::io::Result<Box<dyn Write + Send>> {
    Ok(Box::new(std::io::stdout()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The artifact is the builder's answer checked by its SHAPE (decision 0296): an SDK's folder, eight
    /// hex digits, a sha256 and `.so`; the SDK it names reaches the run. Every other shape names nothing
    /// inside the builds directory.
    #[test]
    fn an_artifact_is_named_by_its_shape_and_names_its_sdk() {
        let builds = Path::new("/b");
        let sha = "ab".repeat(32);
        let at = |sdk: &str, folder: &str| artifact_of(builds, &format!("{sdk}/{folder}/{sha}.so"));
        assert_eq!(
            at("sdk-3", "0123abcd"),
            (builds.join(format!("sdk-3/0123abcd/{sha}.so")), Some(3))
        );
        assert_eq!(at("sdk-dev", "0123abcd").1, None);
        assert_eq!(at("sdk-dev", "0123abcd").0, builds.join(format!("sdk-dev/0123abcd/{sha}.so")));
        let nothing = (builds.join("not-an-artifact"), None);
        for bad in [
            format!("sdk-03/0123abcd/{sha}.so"),
            format!("sdk-3/0123ABCD/{sha}.so"),
            format!("sdk-3/0123abc/{sha}.so"),
            format!("sdk-3/0123abcd/{sha}.dll"),
            format!("sdk-3/0123abcd/{}.so", &sha[1..]),
            format!("../sdk-3/0123abcd/{sha}.so"),
            format!("sdk-3/../0123abcd/{sha}.so"),
            format!("/sdk-3/0123abcd/{sha}.so"),
            format!("0123abcd/{sha}.so"),
            sha.clone(),
        ] {
            assert_eq!(artifact_of(builds, &bad), nothing, "{bad}");
        }
    }

    /// What a person reads for a study this child could not load: the backtesthub's real study half
    /// (`vike_backtesthub::study_sandbox::serve_child`) handed `refusal`, after one START frame with no bytes (the
    /// channel's 14-byte header, `crates/vike-backtesthub/src/study_sandbox.rs`'s `HEADER`), its one DONE frame read
    /// back as the study's error.
    fn study_refused(refusal: CarriedRefusal) -> String {
        let start = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let (mut said, output) = std::io::pipe().expect("pipe");
        let _ = vike_backtesthub::study_sandbox::serve_child(
            Box::new(std::io::Cursor::new(start.to_vec())),
            Box::new(output),
            Err(refusal),
        );
        let mut done = Vec::new();
        said.read_to_end(&mut done).expect("the study's answer");
        match vike_user_research::file::host::finished(&done[14..]) {
            Err(study) => study.to_string(),
            Ok(_) => panic!("a study that could not load finished"),
        }
    }

    /// A refused load reaches a backtest or a study with exactly ONE id: this child hands the loader's bare words
    /// on (no id), and the backtest's carrier, the one surface that shows them, adds its own `error [<id>]: ` once;
    /// a study's path adds none, so its loader words stay bare. A jail's refusal is handed on as it came, its own
    /// id kept, and both show that one, once.
    #[test]
    fn a_refused_load_reaches_a_backtest_with_one_id() {
        let shown = |handed: CarriedRefusal| {
            let refusal = vike_backtest::run::refusals::ProfileRefusal::StrategyFileLoadRefused {
                refusal: handed,
            };
            let shown = vike_backtest::HarnessError::from(refusal).message();
            assert_eq!(shown.matches("error [").count(), 1, "ONE id: {shown}");
            shown
        };
        let missing = Path::new("/no/such/builds/sdk-dev/0123abcd/artifact.so");
        let handed = load_for_run(&Ok(()), missing).map(drop).expect_err("nothing to load");
        assert_eq!(
            vike_utility::errors::Declared::declared_id(&handed),
            None,
            "the loader's words: {handed}"
        );
        let loader = shown(handed);
        assert!(
            loader.starts_with(
                "error [profile.strategy.file.load_refused]: your Rust strategy's build was not found"
            ),
            "{loader}"
        );
        let unjailed = SandboxRefusal::Unjailed { why: "x".to_string() };
        let jail = load_for_run(&Err(unjailed.carried()), missing).map(drop);
        assert_eq!(jail, Err(unjailed.carried()));
        assert_eq!(
            shown(unjailed.carried()),
            "error [strategy.sandbox.unjailed]: no jail, so no strategy: x"
        );
        let study = study_refused(unjailed.carried());
        assert_eq!(study.matches("error [").count(), 1, "ONE id: {study}");
        assert_eq!(
            study,
            "study: the study file could not be loaded: error [strategy.sandbox.unjailed]: no jail, so no \
             strategy: x"
        );
        assert_eq!(
            study_refused("the loader's words".into()),
            "study: the study file could not be loaded: the loader's words"
        );
    }
}
