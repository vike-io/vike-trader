// Shared by `backtest_cli.rs`, `data_cli.rs` and `planted_binary_retry.rs`, which cargo compiles as
// INDEPENDENT test binaries. Each uses a subset of these items, so per-binary dead-code analysis
// flags the rest — expected, and the same rationale `crates/bridges/ctrader/tests/common/mod.rs`
// carries for the same shape.
#![allow(dead_code)]
//! Shared test-support for the `vike-cli` cases that PLANT AN EXECUTABLE and then cause it to be
//! exec'd — the stand-in `backtest` engines that let this crate assert "which binary, which argv,
//! whose exit code" without a DataFusion build in its test lane.
//!
//! # The race, and why it is nobody's fault locally
//!
//! `std::fs::write` closes its own descriptor before it returns, so the writing thread holds
//! nothing when it chmods and runs. But a test binary's cases run as parallel THREADS of ONE
//! process, and a `fork`/`posix_spawn` in ANY OTHER thread — every other case in these files runs
//! the shipped `vike-cli`, so there are always several — hands the forked child an inherited
//! duplicate of that still-open write descriptor. `O_CLOEXEC` closes the duplicate AT exec, not at
//! fork, so for the window between the two some process on the box holds the freshly planted file
//! open for WRITING, and exec'ing a file that is open for write is `ETXTBSY`.
//!
//! The window is microseconds; the loser is whichever case spawns inside it. That is why this has
//! always been a coin toss ACROSS the file rather than a property of any one case, and why curing
//! only the cases that have already lost is curing nothing: `CHANGELOG.md`'s `[0.1.10]` entry
//! wrapped two call sites for exactly this reason and said so — "the `select` twin has the
//! identical race and had simply not lost the coin toss yet".
//!
//! # Two faces, ONE errno
//!
//! The planted engine is exec'd from two different places, so the same errno arrives wearing two
//! shapes and a cure matching only one of them would look complete while covering half the file:
//!
//! 1. **This process spawns it.** `Command::output()` returns `Err`, and the errno is right there:
//!    [`spawn_error_is_etxtbsy`].
//! 2. **`vike-cli` spawns it.** Every case here plants an engine and points the SHIPPED binary at
//!    it, so the failing `execve` happens one process away. An `io::Error` cannot cross a process
//!    boundary as a number — all this process ever gets is the text that binary printed. So the
//!    errno is read back out of the child's diagnostic: [`child_reported_etxtbsy`].
//!
//! Face 2 is the one that hid: the child dies before producing anything, so the case fails on its
//! OWN assertion about missing output rather than on a spawn error. Measured 2026-09-14 as
//! `backtest_cli.rs`'s `the_project_bin_engine_is_the_one_that_runs` reading "the child's stdout is
//! INHERITED, not captured:" with an EMPTY value — the same race as `data_cli.rs`'s
//! `json_is_the_whole_of_stdout_and_the_engines_report_moves_to_stderr`, which showed errno 26
//! outright.
//!
//! # Why a new case joins the cure by construction
//!
//! Neither file spawns `vike-cli` anywhere but through its own single runner, and both runners now
//! go through [`output_retrying_etxtbsy`]. A case added to either file inherits the retry by using
//! the runner that is already there, and [`plant_engine`] is the only spelling of the plant left —
//! so there is no second idiom for a new case to copy.
//!
//! # ⚠ What this must NOT do
//!
//! A retry that swallowed a real spawn failure would be worse than the flake it cures: a genuinely
//! missing, unreadable or non-executable engine would be retried into a timeout and then reported
//! as this same race, and the case that exists to prove the CONNECT rung would prove nothing.
//! So the predicate is the errno and NOTHING else — see [`output_retrying_etxtbsy`]'s own
//! disposition table — and exhausting the budget PANICS rather than returning the busy outcome as
//! though it were an answer. `crates/vike-cli/tests/planted_binary_retry.rs` proves both halves,
//! including that a missing binary fails in a fraction of the retry budget.

use std::process::{Command, Output};
use std::time::Duration;

/// `ETXTBSY`, "Text file busy" — the ONE errno this module retries on, and the whole of its
/// matching rule. Every other failure is somebody's real bug and is handed straight back.
pub const ETXTBSY: i32 = 26;

/// How many times a command may be run before the helper gives up and panics.
pub const ATTEMPTS: u32 = 8;

/// The first inter-attempt sleep; each later one doubles. The race clears as soon as the forking
/// thread's child reaches `execve`, which is microseconds away — the doubling is there for a box
/// under CI load, not because the window is long.
pub const FIRST_BACKOFF: Duration = Duration::from_millis(5);

/// The total time [`output_retrying_etxtbsy`] can spend ASLEEP before it gives up: the sum of the
/// `ATTEMPTS - 1` sleeps between attempts.
///
/// Derived rather than written down, so `planted_binary_retry.rs` can state "promptly" and "it
/// really did retry" as fractions of the real budget instead of as magic numbers that rot the
/// first time a constant above is tuned.
pub fn retry_budget() -> Duration {
    let mut total = Duration::ZERO;
    let mut backoff = FIRST_BACKOFF;
    for _ in 1..ATTEMPTS {
        total += backoff;
        backoff *= 2;
    }
    total
}

/// Face 1: THIS process tried to exec the planted file and the kernel said `ETXTBSY`.
#[cfg(unix)]
pub fn spawn_error_is_etxtbsy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(ETXTBSY)
}

/// Non-unix: `ETXTBSY` is a POSIX rule about a file open for write, and Windows has no equivalent
/// for `Command::new` to hit — so nothing is ever retried there and the helper is a pass-through.
/// (These cases are all `#[cfg(unix)]` anyway; this exists so the RUNNERS, which are not, compile
/// and behave identically on the Windows compile witness.)
#[cfg(not(unix))]
pub fn spawn_error_is_etxtbsy(_e: &std::io::Error) -> bool {
    false
}

/// The needle face 2 matches: the rendering `std` gives errno [`ETXTBSY`], as the child printed it.
///
/// ⚠ This is still MATCHING ON THE ERRNO — it is not a message match.
/// `crates/vike-cli/src/cmd/engine.rs`'s `cannot_spawn` renders the `io::Error` with `Display`, and
/// std's unix `Display` ends every raw-os error with the literal `(os error <n>)`. The prose around
/// it ("cannot run the backtest engine …") is deliberately NOT part of the needle: that sentence is
/// production wording this file may not pin, whereas the parenthesised number is `std`'s and is the
/// errno itself.
///
/// The closing `)` is load-bearing — without it `(os error 2` (a MISSING engine, `ENOENT`, the very
/// failure the connect-rung cases exist to prove) is a prefix of `(os error 26)` and every one of
/// them would be retried into a panic. `planted_binary_retry.rs`'s
/// `the_needle_is_the_errno_std_renders_and_enoent_is_not_a_prefix_of_it` re-derives this string
/// from `io::Error::from_raw_os_error` on every run and holds both halves.
pub fn etxtbsy_needle() -> String {
    format!("(os error {ETXTBSY})")
}

/// Face 2: a CHILD tried to exec the planted file and printed the errno on its way out.
#[cfg(unix)]
pub fn child_reported_etxtbsy(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains(&etxtbsy_needle())
}

/// Non-unix twin of [`child_reported_etxtbsy`] — see [`spawn_error_is_etxtbsy`].
#[cfg(not(unix))]
pub fn child_reported_etxtbsy(_stderr: &[u8]) -> bool {
    false
}

/// Run a command to completion, retrying ONLY the [`ETXTBSY`] race and handing every other outcome
/// straight back on the first attempt.
///
/// `build` is called afresh per attempt because a `Command` is consumed by `output()`; `label` is
/// what a panic names, so it should say which invocation this was.
///
/// # Disposition — how a real failure is told apart from the race
///
/// | outcome | what it is | what happens |
/// |---|---|---|
/// | `Ok`, stderr carries no [`etxtbsy_needle`] | an answer — success OR an ordinary failure | **returned, attempt 1**, untouched |
/// | `Err` with another errno (`ENOENT` missing, `EACCES` unreadable, `ENOEXEC` not a program) | a real bug in the test or the tree | **PANICS immediately**, naming the errno |
/// | `Err` with errno [`ETXTBSY`] | face 1 | retried |
/// | `Ok` whose stderr carries the needle | face 2 | retried |
/// | either face, [`ATTEMPTS`] times over | no longer a microsecond window | **PANICS**, carrying every attempt |
///
/// The two properties that keep this honest: a genuinely missing binary is never retried even once
/// (its errno is not 26), and a PERSISTENT `ETXTBSY` — a real descriptor leak rather than a window
/// — is louder than it was before, because the helper refuses to return the busy outcome as if it
/// were the command's answer.
///
/// ⚠ Re-running is safe precisely BECAUSE of what the errno means: `ETXTBSY` is refused at
/// `execve`, so the planted engine never started and did nothing a second attempt could duplicate.
/// Nothing whose retry would repeat real work may reach for this helper.
pub fn output_retrying_etxtbsy(label: &str, build: impl Fn() -> Command) -> Output {
    let mut backoff = FIRST_BACKOFF;
    let mut seen: Vec<String> = Vec::new();

    for attempt in 1..=ATTEMPTS {
        match build().output() {
            Ok(out) if !child_reported_etxtbsy(&out.stderr) => return out,
            Ok(out) => seen.push(format!(
                "attempt {attempt}: the child reported it — {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) if spawn_error_is_etxtbsy(&e) => {
                seen.push(format!("attempt {attempt}: this process's own spawn — {e}"));
            }
            // NOT the race. A missing, unreadable or non-executable program is a real failure and
            // is reported NOW, with its errno, exactly as it was before this helper existed.
            Err(e) => panic!(
                "{label}: the spawn failed and it is NOT the ETXTBSY ({ETXTBSY}) race — \
                 errno {:?}: {e}\n\
                 Nothing was retried: this is a real failure to fix, not a window to wait out.",
                e.raw_os_error()
            ),
        }

        if attempt < ATTEMPTS {
            std::thread::sleep(backoff);
            backoff *= 2;
        }
    }

    panic!(
        "{label}: still ETXTBSY after {ATTEMPTS} attempts over {:?} of backoff.\n\
         The plant-then-spawn window is microseconds, so this is no longer that race — suspect a \
         descriptor genuinely held open for write on the planted file.\n{}",
        retry_budget(),
        seen.join("\n")
    );
}

/// A stand-in engine written to disk and made executable — the ONE spelling of the plant, so a new
/// case cannot reintroduce a bare `fs::write` + `chmod` pair that no runner knows about.
///
/// Unix-only, and that is inherent rather than incidental: a `#!` line is what makes a text file
/// executable, and Windows has no equivalent `Command::new` will run.
#[cfg(unix)]
pub struct PlantedEngine {
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl PlantedEngine {
    /// The planted file's path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The path as the `--engine` argument, which is how every case names it: the engine SEARCH's
    /// third rung looks beside this executable, and a lane that also built `-p vike-backtest
    /// --features datafusion-store` into the same `target/` really does leave a `backtest` binary
    /// there — so no case here may rely on the search.
    pub fn arg(&self) -> &str {
        self.path.to_str().expect("utf-8 temp path")
    }
}

/// Write `script` into `dir/name` and make it executable.
///
/// This does NOT close the race — nothing on this side can, since the descriptor that blocks the
/// exec belongs to another thread's forked child. It exists so the plant has one spelling, and so
/// that spelling sits next to [`output_retrying_etxtbsy`], which is the half that survives it.
#[cfg(unix)]
pub fn plant_engine(dir: &std::path::Path, name: &str, script: &str) -> PlantedEngine {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    std::fs::write(&path, script)
        .unwrap_or_else(|e| panic!("plant engine at {}: {e}", path.display()));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    PlantedEngine { path }
}
