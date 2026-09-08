//! Finding and driving the STANDALONE BACKTEST ENGINE — the `backtest` binary that a release
//! attaches beside `vike-cli` on **Linux** (and that the container image carries as one of the
//! multicall `vike` binary's tool names).
//!
//! # ⚠ THE ENGINE IS A LINUX / CONTAINER ASSET, AND `vike-cli.exe` IS NOT
//!
//! This is the one asymmetry a reader has to carry, because it decides whether the four rungs below
//! can answer at all. `.github/workflows/release.yml` attaches `backtest` from its HOST (Linux)
//! lane only; its `windows` job cross-builds `vike-app.exe`, `vike-app-thin.exe` and
//! `vike-cli.exe`, and there is no `backtest.exe` in any published manifest. So on Windows, rungs 2
//! (`<project>/bin`), 3 (beside this executable) and 4 (`PATH`) find nothing that came from a
//! release, and `--local`, `sweep --local` and the whole `data` verb are unavailable until the user
//! supplies an engine themselves. [`MISSING_HINT`] says so in those words, per platform.
//!
//! Why the engine is not simply cross-built too, stated so it is a decision rather than a gap: that
//! binary is `required-features = ["datafusion-store"]` plus `venue-fetch` for the collectors, i.e.
//! the whole Arrow/DataFusion/Parquet closure, and nothing in this repository has ever compiled
//! that tree for `x86_64-pc-windows-gnu`. Adding an unproven cross-build of it to a release job is
//! the failure the `windows-cross` CI lane exists to prevent, not an instance of preventing it. The
//! condition that closes this: that closure proved to cross-compile in the `windows-cross` lane
//! first — at which point the `windows` job grows a fourth build and this paragraph shrinks to a
//! sentence.
//!
//! # ⚠ This SPAWNS rather than links, and that is not a shortcut
//!
//! `vike_backtest::backtest_cli::run` opens a concrete `DataFusionHist`, and **this crate's whole
//! identity is being DataFusion-free and transport-free** — argued edge by edge in
//! `crates/vike-cli/Cargo.toml`, and machine-checked by CI's `light-consumers` lane. Linking the
//! engine would end the property that makes this binary small enough to be the thing an agent
//! installs, in exchange for saving one process spawn on a command whose real cost is reading a
//! multi-gigabyte tape.
//!
//! So `--local` runs the engine as a CHILD: its stdout and stderr are inherited (the report reaches
//! the terminal or the pipe exactly as if the engine had been invoked directly — nothing is
//! captured, reformatted, or buffered by this process) and its exit status is folded onto this
//! crate's own ladder by [`fold_status`].
//!
//! # Where the engine is looked for, in order
//!
//! 1. `--engine PATH`, if given. The specific consumer's own knob, which is the grain
//!    `crates/vike-model/src/state_path.rs`'s `PROJECT_TMP_DIR` argues for — and the ONLY way to
//!    name it here, because a `$VIKE_BACKTEST_BIN` read under `src/cmd/` would be a new
//!    `Layer::Library` row on `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`, a
//!    ratchet that may shrink and never grow.
//! 2. `<project>/bin/backtest` — `vike_model::state_path::PROJECT_BIN_DIR`, the runtime home every
//!    project tool is installed into. The project root arrives as a PARAMETER from the dispatcher,
//!    which resolved it once for the whole process; nothing here walks anything.
//! 3. Beside THIS executable. `.github/workflows/release.yml`'s HOST lane attaches `vike-cli` and
//!    `backtest` to the same release and `scripts/release_container_image.sh` stages them into the
//!    same directory, so this is the rung that answers on an ordinary LINUX install — and the rung
//!    that cannot answer on a Windows one, per the asymmetry above.
//! 4. `PATH`, by bare name — the developer case, where `cargo build --bin backtest` put it in a
//!    directory that is already on the path.
//!
//! A miss on all four is a [`Exit::Connect`] failure naming what is missing and how to get it, for
//! the same reason an unreachable datahub is: the command line was right, the thing it needs is not
//! there, and a caller can fix that without editing the command.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::exit::{CliError, CmdResult};

/// The engine binary's name, without a platform suffix. It is the `[[bin]] name` of
/// `crates/vike-backtest`'s `backtest` target and the asset name `.github/workflows/release.yml`
/// attaches, so the three cannot be spelled differently without one of them failing to find it.
pub(crate) const ENGINE_BIN: &str = "backtest";

/// What to tell a user who does not have the engine. Kept beside the search order it describes, so
/// a rung added above is a rung this sentence can be updated for in the same edit.
///
/// ⚠ **Two spellings, because the honest answer differs by platform** (see this module's doc). On
/// Linux the engine is in the release the user already downloaded, so "install it into
/// `<project>/bin/`" is an instruction they can follow. On Windows no release publishes one, so
/// telling them to install it would name a file that does not exist anywhere — the message says
/// that outright and names the one command that produces one. `#[cfg]` rather than a runtime
/// branch: the platform is known at compile time, and the `windows-cross` CI lane compiles this
/// crate for Windows on every PR, so both arms are type-checked.
#[cfg(not(windows))]
const MISSING_HINT: &str = "the standalone engine is a separate binary attached to the same \
                            release as vike-cli; install it into <project>/bin/, put it on your \
                            PATH, or name it with --engine PATH";
#[cfg(windows)]
const MISSING_HINT: &str = "no vike release publishes a Windows build of the engine - it is a \
                            Linux/container asset today. Build one with `cargo build --release -p \
                            vike-backtest --bin backtest --features datafusion-store,venue-fetch`, \
                            then install it into <project>/bin/, put it on your PATH, or name it \
                            with --engine PATH. (The remote path needs no engine at all: \
                            `vike-cli backtest --addr HOST:PORT` runs on a vike-datahub server \
                            instead.)";

/// The program to spawn, resolved through the four rungs in this module's doc.
///
/// Returns a bare NAME rather than an absolute path when nothing was found on disk, so the OS gets
/// its own go at `PATH` — the last rung is deliberately not a file probe, because reimplementing
/// `PATH` lookup (`PATHEXT` on Windows, the executable bit on unix) is exactly the sort of second
/// copy this workspace keeps deleting.
pub(crate) fn locate(explicit: Option<&str>, project_root: Option<&Path>) -> PathBuf {
    if let Some(named) = explicit {
        // NOT probed for existence: a named path that is not there must fail with the name the
        // operator typed, never by falling through to a different engine they did not ask for.
        return PathBuf::from(named);
    }
    let file = format!("{ENGINE_BIN}{}", std::env::consts::EXE_SUFFIX);
    let candidates = [
        project_root.map(|r| r.join(vike_model::state_path::PROJECT_BIN_DIR).join(&file)),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&file))),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(ENGINE_BIN))
}

/// Whether the child gets THIS process's stdin, or nothing at all.
///
/// ⚠ **`Null` is the default and stays the default**, and the reason is the one this enum exists to
/// keep visible: a child holding an inherited stdin can READ from it, and every engine verb but one
/// reads nothing — so handing it the terminal buys nothing and costs the ability to say, in this
/// file, which verbs can consume a line the operator typed.
///
/// [`Stdin::Inherit`] has exactly one caller, `vike-cli data rm`, and it is what lets the ENGINE own
/// the typed confirmation. The alternative — the CLI reading the line itself — needs the matched
/// COUNT the confirmation is bound to, which only the plan knows, which means a second spawn and a
/// machine document parsed on this side. Passing the terminal through costs one enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stdin {
    /// The child reads EOF. Every verb but `rm`'s confirmation.
    Null,
    /// The child gets this process's stdin — a terminal, when there is one.
    Inherit,
}

impl Stdin {
    fn stdio(self) -> Stdio {
        match self {
            Stdin::Null => Stdio::null(),
            Stdin::Inherit => Stdio::inherit(),
        }
    }
}

/// The one message a spawn failure gives, so [`run`] and [`run_capturing_stdout`] cannot describe
/// the same failure differently.
fn cannot_spawn(program: &Path, e: std::io::Error) -> CliError {
    CliError::connect(format!(
        "cannot run the {ENGINE_BIN} engine ({}): {e}\n{MISSING_HINT}",
        program.display()
    ))
}

/// Spawn the engine with `args`, inherit its streams, and fold its exit status onto this crate's
/// ladder.
///
/// `verb` is the `vike-cli` subcommand that asked, so a failure message names the command the user
/// actually typed rather than the child.
pub(crate) fn run(program: &Path, args: &[impl AsRef<OsStr>], verb: &str) -> CmdResult<()> {
    run_with_stdin(program, args, verb, Stdin::Null)
}

/// [`run`], choosing what the child's stdin is. See [`Stdin`] for why that is a choice at all and
/// why exactly one caller makes the other one.
pub(crate) fn run_with_stdin(
    program: &Path,
    args: &[impl AsRef<OsStr>],
    verb: &str,
    stdin: Stdin,
) -> CmdResult<()> {
    let status = Command::new(program)
        .args(args)
        // INHERIT, never capture. The engine's report — a human table or a `--json` document — is
        // the product of this command, and putting a pipe in front of it would mean buffering a
        // whole run's output and re-emitting it, changing both the timing and (on a failure) which
        // stream carried what.
        .stdin(stdin.stdio())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| cannot_spawn(program, e))?;
    fold_status(status.code(), verb)
}

/// The same spawn, with the child's STDOUT read back instead of inherited — and echoed to THIS
/// process's stderr as it arrives, so a person watching still sees the engine's report in order.
///
/// # ⚠ The one exception to "INHERIT, never capture", and what buys it
///
/// This module's doc says the engine's report is the product of the command and that putting a pipe
/// in front of it would change the timing and the stream a line lands on. That is right for every
/// caller but one: under `vike-cli data --json` the product is a JSON DOCUMENT, and this crate's
/// rule (`crate::cmd::secrets`, `crate::cmd::init`) is that stdout under `--json` is the document
/// and nothing else. An engine writing its human lines onto the same stdout would leave a caller
/// parsing a stream that is not JSON. So the lines are MOVED to stderr — where every diagnostic in
/// this crate already goes — and returned, so the document can carry them verbatim.
///
/// ⚠ Verbatim, and NOT parsed. `backtest --fetch` and `--seed-demo` print the bar and row counts
/// they know as prose and have no `--json` mode of their own; reading numbers out of those
/// sentences would be a second implementation of the engine's output format, in a different crate,
/// that drifts the first time a word changes and reports a wrong count rather than failing. The
/// caller gets the lines; nothing here claims to understand them.
///
/// The child's stderr is still inherited: a failure diagnostic reaching the user unbuffered is the
/// behaviour [`run`] promises, and `--json` is no reason to hold it back.
pub(crate) fn run_capturing_stdout(
    program: &Path,
    args: &[impl AsRef<OsStr>],
    verb: &str,
) -> CmdResult<Vec<String>> {
    run_capturing_stdout_with_stdin(program, args, verb, Stdin::Null)
}

/// [`run_capturing_stdout`], choosing the child's stdin — the `--json` twin of
/// [`run_with_stdin`]. `data rm --json` without `--yes` needs both halves at once: the document
/// comes back on the pipe while the confirmation the child prompts for is typed on the terminal it
/// inherited.
pub(crate) fn run_capturing_stdout_with_stdin(
    program: &Path,
    args: &[impl AsRef<OsStr>],
    verb: &str,
    stdin: Stdin,
) -> CmdResult<Vec<String>> {
    use std::io::{BufRead, BufReader};

    let mut child = Command::new(program)
        .args(args)
        .stdin(stdin.stdio())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| cannot_spawn(program, e))?;

    let mut report = Vec::new();
    // `take()` so the pipe is CLOSED when this scope ends: holding the read end open while waiting
    // is how a child that fills the pipe buffer and a parent that has stopped reading deadlock.
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let line = line.map_err(|e| {
                CliError::failed(format!("reading the {ENGINE_BIN} engine's output failed: {e}"))
            })?;
            eprintln!("{line}");
            report.push(line);
        }
    }
    let status = child.wait().map_err(|e| {
        CliError::failed(format!("waiting for the {ENGINE_BIN} engine failed: {e}"))
    })?;
    fold_status(status.code(), verb)?;
    Ok(report)
}

/// The child's exit status, as one of ours: `0` is success and EVERY other outcome is
/// [`crate::exit::Exit::Failed`], the pre-existing catch-all this crate reserves for a failure it
/// has not classified.
///
/// # ⚠ Why the child's `2` is NOT folded onto the usage rung
///
/// It looks like it should be — both binaries call `2` "usage" — and that reading is wrong,
/// because the engine's `2` is OVERLOADED. `crates/vike-backtest/src/backtest_cli.rs` returns
/// `ExitCode::from(2)` from `run_fetch` when `fetch::fetch_into` fails (its own comment beside
/// that line says "a geoblock, a delisted symbol and a bad interval all arrive here"), when
/// `DataFusionHist::open` cannot open the store, and when `demo_tape::seed` fails — none of which
/// is a command line. [`crate::exit::Exit::Usage`] promises the opposite of all three: "nothing was
/// attempted, and re-running unchanged cannot succeed". Folding a failed venue fetch onto it would
/// tell a wrapper to STOP RETRYING a transient outage and tell an agent to report a typo to its
/// user — the exact retry-vs-fix inversion the ladder exists to remove, on this feature's own
/// headline verb (`vike-cli data fetch`).
///
/// So an unclassifiable child status lands where every unclassified failure in this crate lands,
/// which is the rule [`crate::exit::CliError`]'s `From<String>` impl states: `Failed`. The RUNG is
/// honest (it ran and failed) and the child's own diagnostic — already on the stderr this process
/// inherited — is what says which of the two it was.
///
/// ⚠ Nothing is LOST by that, because this crate does not need the child to catch usage errors: the
/// argument shapes are checked HERE before a process is started (`crate::cmd::data`'s spec and
/// window checks, `crate::cmd::backtest`'s and `crate::cmd::sweep`'s mode-exclusivity checks), and
/// the argv handed to the child is built by this crate rather than typed by the operator. What
/// would reopen this: a distinct pre-flight/runtime code on the engine side, at which point its
/// genuine argument errors could fold onto `Usage` without carrying a network failure with them.
///
/// ⚠ `None` — the child was SIGNALLED rather than exiting — gets its own sentence rather than the
/// numeric one, because there is no number to print. It cannot happen on Windows, where every
/// termination carries a code, which is exactly why it is stated here rather than left to a
/// platform's behaviour.
fn fold_status(code: Option<i32>, verb: &str) -> CmdResult<()> {
    match code {
        Some(0) => Ok(()),
        // The engine printed its own diagnostic on the stderr this process inherited, so a second
        // sentence here would only repeat it — and, as the doc above argues, this process cannot
        // tell WHICH failure a given code meant. What is added is the code itself and the RUNG.
        Some(other) => Err(CliError::failed(format!(
            "the {ENGINE_BIN} engine exited {other} (see its output above)"
        ))),
        None => Err(CliError::failed(format!(
            "the {ENGINE_BIN} engine was terminated by a signal during `vike-cli {verb}`"
        ))),
    }
}

/// `<project>/tmp`, where a REWRITTEN profile is staged before it is handed to the child.
///
/// ⚠ Not the system temp directory, and that is a rule rather than a preference:
/// `crates/vike-ops/tests/system_temp_gate.rs` is a ratchet over exactly this class, and
/// `crates/vike-model/src/state_path.rs`'s `PROJECT_TMP_DIR` argues why — inside the container this
/// project is moving to, the system temp directory is not the host's, does not survive a restart,
/// and cannot be mounted beside the project folder.
///
/// `None` when no project sits above the working directory. The caller REFUSES rather than falling
/// back, because the only remaining place to write is beside the user's own profile.
///
/// # ⚠ What this call also DOES, deliberately: it SWEEPS
///
/// `vike_model::scratch`'s own contract is that "ownership bounds the ordinary path and the sweep
/// bounds the abort path, and neither substitutes for the other". `ScratchDir`'s `Drop` is the
/// first half and it does not run on a `SIGKILL`, an OOM kill, a power loss — or a Ctrl-C, which
/// terminates without unwinding and is the *ordinary* way somebody stops a long `--local` run. So
/// without the second half the leftover population under `<project>/tmp` is unbounded, which is
/// exactly how the 26,851 directories / 215 GB measured on the build box happened.
///
/// Folded into the RESOLVER rather than left as a second call, for the reason
/// `crates/vike-backfill/src/cli.rs`'s `scratch_root` folds it into its own: no consumer can
/// resolve the root and forget the retention. ⚠ Unlike that one there is no `tracing` line — this
/// crate links no logging, by design — so the sweep's result is dropped rather than reported.
pub(crate) fn scratch_root(project_root: Option<&Path>) -> Option<PathBuf> {
    let root = project_root.map(|r| r.join(vike_model::state_path::PROJECT_TMP_DIR))?;
    // Best-effort and total: an absent or unreadable root is not an error (a project that has
    // staged nothing has no `tmp/`), and every removal is counted rather than propagated.
    vike_model::scratch::sweep(&root, Some(vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES));
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::Exit;

    /// A named engine is taken VERBATIM and never probed — an operator who typed a path must fail
    /// on that path, not silently get a different binary from a lower rung.
    #[test]
    fn an_explicit_engine_wins_and_is_not_probed() {
        let named = locate(Some("/nowhere/at/all/backtest"), Some(Path::new("/some/project")));
        assert_eq!(named, PathBuf::from("/nowhere/at/all/backtest"));
    }

    /// With nothing on disk, the answer is the BARE NAME, so the OS resolves it on `PATH` rather
    /// than this module reimplementing that lookup.
    #[test]
    fn nothing_on_disk_falls_through_to_the_bare_name() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let found = locate(None, Some(scratch.path()));
        assert_eq!(found, PathBuf::from(ENGINE_BIN), "no probe may invent an absolute path");
    }

    /// …and a real file under `<project>/bin/` is found there, ahead of the `PATH` fallback.
    #[test]
    fn the_project_bin_directory_answers_when_it_holds_an_engine() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let bin = scratch.path().join(vike_model::state_path::PROJECT_BIN_DIR);
        std::fs::create_dir_all(&bin).expect("create bin");
        let engine = bin.join(format!("{ENGINE_BIN}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&engine, b"not really an engine").expect("plant");

        assert_eq!(locate(None, Some(scratch.path())), engine);
    }

    /// The fold, rung by rung. ⚠ `2` is deliberately NOT a usage error here even though the engine
    /// calls it one: that code is overloaded on the engine side (a failed venue fetch and an
    /// unopenable store both return it), and [`fold_status`]'s doc carries the whole argument. Only
    /// `0` is a success; everything else, a signal included, is the unclassified-failure rung.
    #[test]
    fn the_childs_status_folds_onto_this_crates_ladder() {
        assert!(fold_status(Some(0), "backtest").is_ok());
        assert_eq!(fold_status(Some(2), "backtest").unwrap_err().exit, Exit::Failed);
        assert_eq!(fold_status(Some(1), "backtest").unwrap_err().exit, Exit::Failed);
        assert_eq!(fold_status(Some(101), "backtest").unwrap_err().exit, Exit::Failed);
        assert!(
            fold_status(Some(2), "data").unwrap_err().msg.contains("exited 2"),
            "the code is NAMED rather than asserted a cause for"
        );
        let signalled = fold_status(None, "sweep").unwrap_err();
        assert_eq!(signalled.exit, Exit::Failed);
        assert!(
            signalled.msg.contains("sweep"),
            "names the verb the user typed: {}",
            signalled.msg
        );
    }

    /// A missing engine is a CONNECT-class failure whose message names the binary and says how to
    /// get one — the same disposition as an unreachable datahub, and for the same reason.
    #[test]
    fn a_missing_engine_is_a_connect_failure_that_says_what_to_do() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let absent = scratch.path().join("definitely-not-an-engine");
        let e = run(&absent, &["--profile", "x.toml"], "backtest").unwrap_err();
        assert_eq!(e.exit, Exit::Connect);
        assert!(e.msg.contains(ENGINE_BIN), "names the engine: {}", e.msg);
        assert!(e.msg.contains("--engine"), "names the way out: {}", e.msg);
    }

    /// No project above the working directory means no project scratch — the caller must refuse
    /// rather than reach for the system temp directory.
    #[test]
    fn scratch_needs_a_project() {
        assert!(scratch_root(None).is_none());
        // A root that does not exist is answered, not created and not an error: resolving a path is
        // not the same as staging into it, and the sweep folded into this call is best-effort.
        let absent = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            scratch_root(Some(absent.path())),
            Some(absent.path().join(vike_model::state_path::PROJECT_TMP_DIR))
        );
    }

    /// …and resolving the root PRUNES it. The property is the one `vike_model::scratch`'s module
    /// doc insists on: `ScratchDir`'s `Drop` cannot bound a population it never created, so a run
    /// killed rather than unwound (Ctrl-C, an OOM kill) leaves an entry nothing else would ever
    /// remove. Folding the sweep into the resolver is what makes forgetting it impossible.
    #[test]
    fn resolving_the_scratch_root_bounds_what_earlier_runs_abandoned() {
        let project = tempfile::tempdir().expect("tempdir");
        let tmp = project.path().join(vike_model::state_path::PROJECT_TMP_DIR);
        std::fs::create_dir_all(&tmp).expect("create tmp");
        let planted = vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES + 5;
        for i in 0..planted {
            std::fs::create_dir(tmp.join(format!("abandoned-{i}"))).expect("plant");
        }

        let root = scratch_root(Some(project.path())).expect("a project resolves a scratch root");
        assert_eq!(root, tmp);
        let left = std::fs::read_dir(&tmp).expect("read tmp").count();
        assert_eq!(
            left,
            vike_model::scratch::DEFAULT_MAX_SCRATCH_ENTRIES,
            "{planted} abandoned entries must be pruned to the retention, not left to grow"
        );
    }
}
