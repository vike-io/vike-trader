//! End-to-end tests for what `vike-cli config bootstrap-daemon` TELLS the operator about the
//! `[daemon]` knobs it carries, driving the SHIPPED binary (`CARGO_BIN_EXE_vike-cli`).
//!
//! The unit tests beside the verb cover the argv grammar and what lands in the stored rows. These
//! cover the two things only the binary's streams can show: the one-line stderr WARNING when
//! `--shutdown-deadline-ms` is not under the shipped unit's stop timeout, and the usage text.
//!
//! # Why the warning exists
//!
//! The OANDA recipe needs a 10 s teardown deadline, and the shipped unit's `TimeoutStopSec=` is
//! 10 s. `crates/vike-tradehub/src/tradehub_cli/tests/stop_and_deadlines.rs`'s
//! `the_default_shutdown_deadline_fits_inside_the_units_stop_timeout` requires the deadline
//! STRICTLY under the stop timeout (the observe publisher's stop sits outside the capped teardown
//! and rides the difference), but it reads the DEFAULT deadline, so a deadline raised through this
//! verb is checked by nothing. A deadline that is not under the stop timeout lets SIGKILL win the
//! race and cut the teardown in half. The verb cannot change the unit, so it says so.
//!
//! The number the verb compares against is read here from the unit text itself, so the constant in
//! the verb and the line in `deploy/vike-tradehub.service` cannot drift apart unnoticed. The unit
//! ships in the public mirror (`deploy/*.service` is allowlisted), so reading it is allowed.
//!
//! ⚠ Every invocation sets `VIKE_SETTINGS_DIR` on the CHILD to a throwaway directory and clears the
//! rest of the environment. All cases are `--dry-run` — nothing is written, and no database is
//! needed.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_vike-cli");

/// The shipped unit, as text. `TimeoutStopSec=` is read out of it, skipping commented lines (the
/// unit discusses the directive in prose right above setting it).
const SHIPPED_UNIT: &str = include_str!("../../../deploy/vike-tradehub.service");

fn unit_stop_timeout_ms() -> i64 {
    let secs: i64 = SHIPPED_UNIT
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("TimeoutStopSec="))
        .expect("the shipped unit sets TimeoutStopSec= explicitly")
        .trim()
        .parse()
        .expect("TimeoutStopSec= is a plain number of seconds");
    secs * 1_000
}

fn run(dir: &Path, extra: &[&str]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["config", "bootstrap-daemon", "oanda-live"]);
    cmd.args(["--venue", "oanda", "--asset-class", "Fx", "--symbol", "EURUSD", "--dry-run"]);
    cmd.args(extra);
    cmd.env_clear();
    cmd.env("VIKE_SETTINGS_DIR", dir);
    cmd.stdin(Stdio::null()).output().expect("the vike-cli binary must run")
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("vike-cli-bootstrap-daemon-").tempdir().expect("tempdir")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// **A deadline that is not under the unit's stop timeout is WARNED about, on stderr, in one line,
/// and the verb still succeeds.** Both edges: the stop timeout itself (equal is not under) and
/// something well above it.
#[test]
fn a_deadline_not_under_the_stop_timeout_warns_on_stderr_and_still_succeeds() {
    let dir = scratch();
    let at = unit_stop_timeout_ms();
    for ms in [at, at + 5_000] {
        let out = run(dir.path(), &["--shutdown-deadline-ms", &ms.to_string()]);
        assert!(out.status.success(), "a warning is not a refusal: {out:?}");
        let err = stderr(&out);
        assert_eq!(err.trim().lines().count(), 1, "ONE line, not a paragraph: {err:?}");
        assert!(
            err.contains("warning") && err.contains("TimeoutStopSec"),
            "names the unit directive that must be raised: {err:?}"
        );
        assert!(err.contains(&ms.to_string()), "echoes the deadline it judged: {err:?}");
        assert!(err.contains("--stop-timeout"), "names the container twin too: {err:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("NOTHING WAS WRITTEN"),
            "the dry run still reports on stdout: {out:?}"
        );
    }
}

/// **A deadline under the stop timeout, and no deadline at all, print nothing on stderr.** The
/// guard against a warning that fires on every run and so means nothing.
#[test]
fn a_deadline_under_the_stop_timeout_or_none_prints_no_warning() {
    let dir = scratch();
    let under = (unit_stop_timeout_ms() - 1).to_string();
    for extra in [&["--shutdown-deadline-ms", under.as_str()][..], &[][..]] {
        let out = run(dir.path(), extra);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(stderr(&out), "", "nothing to warn about: {extra:?}");
    }
}

/// **The usage says what a re-run does and when the bar width is checked** — the two facts an
/// operator cannot read off the flags. Re-running under the same name REPLACES the stored body, so
/// a knob left off the second time returns to its default; and `--interval-ms` is compared with
/// `--interval` only when both are given.
#[test]
fn the_usage_says_what_a_rerun_replaces_and_when_the_bar_width_is_checked() {
    let out = Command::new(BIN)
        .args(["config", "bootstrap-daemon", "--help"])
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("the vike-cli binary must run");
    assert!(out.status.success(), "{out:?}");
    let usage = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        usage.contains("replaces") && usage.contains("returns to its default"),
        "a re-run under the same name replaces the body, so a knob left off goes back to its \
         default: {usage}"
    );
    assert!(
        usage.contains("only when --interval is given too"),
        "--interval-ms is cross-checked only against an --interval given in the same run: {usage}"
    );
}
