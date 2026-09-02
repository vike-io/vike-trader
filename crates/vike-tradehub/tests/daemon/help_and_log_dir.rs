//! Two shipped-binary properties of `vike-tradehub` that only the REAL process can show: what
//! `--help` costs a caller, and where the rolling trace log actually lands.
//!
//! Both are spawn tests rather than unit tests on purpose. `--help` short-circuited the parser with
//! `Err("help requested")` and `main` then treated it as a usage error — exit **1**, with that
//! internal token printed to stderr as though it were a diagnostic; a unit test of `parse_args`
//! sees the same `Err` in both worlds and can prove nothing about the status or the stream. The log
//! directory is the same shape of question: `vike_log::init` resolves it from configuration the
//! binary hands it, so only a run of the binary says which directory that turned out to be.

use std::path::Path;
use std::process::Command;

/// The internal short-circuit token. Control flow, never a diagnostic.
const SENTINEL: &str = "help requested";

/// A fresh empty project root for one case, OWNED by the caller: the returned `TempDir` removes
/// it and everything the daemon wrote under it when it drops.
///
/// ⚠ BIND the guard for the whole test — the child is started with this as its working directory
/// and its `VIKE_STATE_ROOT`, and the assertions below read the log files it lands there.
///
/// This used to be `env::temp_dir().join(format!("vike_tradehub_{tag}_{pid}_{nanos}"))` plus a
/// `remove_dir_all` at the end of each test — cleanup that runs only when the test PASSES, and
/// which the whole family around it did not have at all. The two defects and the numbers measured
/// on the CI box on 2026-08-25 are in `crates/vike-tradehub/src/config.rs`'s `own_script`: 44,840
/// leaked directories under `/tmp`, and a REUSED pid colliding across the box's two test users
/// (`the CI user`, `the operator`) so that `create_dir_all` succeeds on somebody else's directory and
/// the write into it fails `PermissionDenied`. `tempfile` answers both, and it also cleans up on
/// the PANIC path, which is exactly where a failing log-directory test used to leave its evidence
/// behind forever. The tag stays in the NAME so a directory seen mid-run is attributable.
fn temp_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("vike_tradehub_{tag}_"))
        .tempdir()
        .expect("create temp dir")
}

/// `--help` and `-h`: exit 0, usage on STDOUT, nothing internal anywhere.
///
/// **Stdout is the right stream even though this daemon's stdout is a PROTOCOL** (a periodic
/// one-line JSON snapshot summary; logs go to the vike-log file/stderr layer). The two never
/// coexist: `--help` is answered by `parse_args` and the process exits before the core is spawned,
/// before the first snapshot line is ever written — there is no protocol on that stdout to corrupt,
/// and a reader parsing snapshot lines is by definition reading a daemon that is RUNNING. Putting
/// help on stderr instead would make this the one binary in the workspace whose `--help | less`
/// shows an empty page, to protect a stream that is not in use yet.
#[test]
fn help_exits_zero_with_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg(flag)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .output()
            .expect("run vike-tradehub --help");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-tradehub {flag}` must exit 0 — a non-zero --help breaks `set -e` and every \
             packaging smoke test. status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains("usage:"),
            "`vike-tradehub {flag}` must print its usage to STDOUT; stdout: {stdout:?}, \
             stderr: {stderr:?}"
        );
        assert!(
            !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
            "`vike-tradehub {flag}` leaked the internal {SENTINEL:?} token; stdout: {stdout:?}, \
             stderr: {stderr:?}"
        );
    }
}

/// `--version`/`-V`: `<name> <version>` on STDOUT, exit 0.
///
/// It was UNRECOGNISED, so it hit `parse_args`'s catch-all and exited **1** with
/// `vike-tradehub: unknown argument: --version` on stderr — which is what a packaging probe or a
/// bug-report template reads as "this binary is broken". `-V`, never `-v`: lowercase is verbosity
/// everywhere else on the box.
#[test]
fn version_prints_the_crate_version_on_stdout() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg(flag)
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .output()
            .expect("run vike-tradehub --version");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`vike-tradehub {flag}` must exit 0; status: {:?}, stderr: {stderr}",
            out.status.code()
        );
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "`vike-tradehub {flag}` must print the version {:?} on stdout; stdout: {stdout:?}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !stderr.contains("unknown argument"),
            "…and must be RECOGNISED, not routed to the unknown-argument arm; stderr: {stderr:?}"
        );
    }
}

/// A real usage error still fails, still on stderr — so the fix above cannot have been bought by
/// making every parse outcome a success.
#[test]
fn an_unknown_flag_still_exits_non_zero_on_stderr() {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .arg("--not-a-flag")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-tradehub --not-a-flag");
    assert!(!out.status.success(), "an unknown flag must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage:"), "the usage error explains itself on stderr: {stderr:?}");
    assert!(!stderr.contains(SENTINEL), "and carries no internal token: {stderr:?}");
}

/// The rolling JSON trace log lands under the project's STATE directory, not beside the binary.
///
/// `<exe_dir>/logs` means `target/debug/logs/vike-tradehub.<date>` in a checkout — a directory
/// `cargo clean` deletes, and on a deployment a directory beside the binary that an operator has no
/// reason to look in. `VIKE_STATE_ROOT` names the state directory outright (the daemon already
/// reads it for `alerts.json`), so this pins the shape end to end without depending on where the
/// harness happens to run: logs go to `<state root>/logs`.
///
/// Driven with a config path that does not exist, so the daemon initialises logging, logs its
/// startup failure and exits — no core, no venue, no network.
#[test]
fn the_trace_log_lands_under_the_state_root_not_the_exe_dir() {
    let root = temp_dir("logdir");
    let project = root.path();
    let state_root = project.join("state");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .arg("--config")
        .arg(project.join("no-such-profile.toml"))
        .current_dir(project)
        .env("VIKE_STATE_ROOT", &state_root)
        .env_remove("VIKE_LOG_DIR")
        .env_remove("VIKE_LOG_FILE_LEVEL")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_LIVE")
        .env_remove("VIKE_TRADEHUB_ADDR")
        .output()
        .expect("run vike-tradehub with a missing profile");

    assert!(
        !out.status.success(),
        "a missing --config profile must fail; stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    let logs = state_root.join("logs");
    assert!(
        logs.is_dir(),
        "the trace log must default to <state root>/logs ({}), not <exe_dir>/logs; \
         stderr: {}",
        logs.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        log_file_names(&logs).iter().any(|n| n.starts_with("vike-tradehub.")),
        "…and the daemon's own rolling file must be in it; found: {:?}",
        log_file_names(&logs)
    );
    // No hand cleanup: `root` drops here and takes the whole tree with it, on the failure path too.
}

/// `VIKE_LOG_DIR` still outranks everything — the escape hatch an operator already has must not be
/// taken away by giving the default a better home.
#[test]
fn the_log_dir_env_var_still_wins_over_the_state_root() {
    let root = temp_dir("logenv");
    let project = root.path();
    let state_root = project.join("state");
    let explicit = project.join("elsewhere");

    let out = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .arg("--config")
        .arg(project.join("no-such-profile.toml"))
        .current_dir(project)
        .env("VIKE_STATE_ROOT", &state_root)
        .env("VIKE_LOG_DIR", &explicit)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-tradehub with VIKE_LOG_DIR set");
    assert!(!out.status.success(), "a missing --config profile must still fail");

    assert!(
        log_file_names(&explicit).iter().any(|n| n.starts_with("vike-tradehub.")),
        "$VIKE_LOG_DIR must still win; {} held: {:?}",
        explicit.display(),
        log_file_names(&explicit)
    );
    assert!(
        !state_root.join("logs").exists(),
        "…and the project default must not ALSO be created when it is overridden"
    );
    // No hand cleanup: `root` drops here and takes the whole tree with it, on the failure path too.
}

/// The file names in `dir`, or an empty list when it does not exist.
fn log_file_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}
