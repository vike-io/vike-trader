//! **An unwritable log destination degrades; it never stops the process.**
//!
//! `vike_log::init` used to reach `tracing_appender::rolling::daily`, whose whole body is
//! `builder()…build(dir).expect("initializing rolling file appender failed")`. So a directory it
//! could not open PANICKED the caller — exit 101, from inside a dependency, with a backtrace that
//! names neither this crate nor `VIKE_LOG_DIR`.
//!
//! That reaches further than deployments. `resolve_log_dir`'s last resort is `<exe_dir>/logs`, so
//! any binary run with no project above its working directory and no `VIKE_LOG_DIR` writes beside
//! its own executable — measured, `backtest --help` from a neutral directory creates
//! `target/debug/logs/backtest.<date>` before it has decided it has any work to do. Install that
//! same binary where the exe directory is read-only (a unit with `ProtectSystem=strict`, a read-only
//! container layer, `/usr/local/bin`) and it died because it could not open a LOG file. Measured on
//! the CI box at exit 101 for both `ENOENT` (`VIKE_LOG_DIR` naming a path that cannot be created) and
//! `EACCES` (naming a mode-500 directory).
//!
//! Logging is instrumentation: it may degrade, it may not be the thing that stops a trading tool.
//!
//! ⚠ **Its own test binary, deliberately.** `init` installs the GLOBAL subscriber, which can only be
//! set once per process — a second `init` anywhere in the same binary is a no-op warning and would
//! make this assert nothing. `tests/writes_json.rs` owns the happy path; this file owns the failure
//! and contains exactly one test.

use std::path::PathBuf;

/// A path that cannot be a directory on any platform: its PARENT is a regular file. Both
/// `create_dir_all` and the appender's own file open fail, and neither needs a permission bit, so
/// this behaves identically on Linux and Windows (where the equivalent of a mode-500 directory is an
/// ACL question this workspace carries no crate for).
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path, and the caller must BIND it — dropping the
/// guard removes the blocking file, at which point the destination is merely absent rather than
/// UNOPENABLE and the whole case would go vacuous.
///
/// **The self-cleaning is safe here precisely because no permission bit is involved.** A test that
/// makes a directory read-only cannot be given a `TempDir` root naively — `TempDir::drop` ignores
/// its own errors, so a removal it cannot perform leaks in silence. This case never chmods
/// anything: the root stays fully writable and holds ONE regular file, which its `Drop` unlinks
/// like any other. The ENOTDIR construction was chosen for cross-platform reasons (see the
/// paragraph above) and it is what makes the cleanup unconditional as well.
///
/// This used to be `env::temp_dir().join(format!("vike_log_unwritable_{pid}_{nanos}"))` with
/// nothing ever deleting it. MEASURED on the CI box, 2026-08-25: **423 of these directories** were
/// sitting in `/tmp`, part of 44,840 leaked across the whole family — and a pid is REUSED, so on a
/// box that runs tests as two users (`the CI user` for CI, `the operator` for the verification lanes) a
/// collision makes `create_dir_all` succeed on the other user's directory while the write into it
/// fails `PermissionDenied`. `crates/vike-tradehub/src/config.rs`'s `own_script` carries the full
/// argument. `tempfile` is unique by construction and self-deleting, on the panic path too.
fn undirectory() -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::Builder::new()
        .prefix("vike_log_unwritable_")
        .tempdir()
        .expect("create the case's temp dir");
    let blocker = base.path().join("this-is-a-file");
    std::fs::write(&blocker, b"not a directory\n").expect("write the blocking file");
    // …so this can never be created.
    let path = blocker.join("logs");
    (base, path)
}

#[test]
fn an_unopenable_log_directory_degrades_to_console_instead_of_panicking() {
    // Not vacuous by accident: `$VIKE_LOG_DIR` OUTRANKS `LogConfig::dir`, so a value in the ambient
    // environment would silently redirect this run to a perfectly good directory and the test would
    // pass while proving nothing. It is not removed here on purpose — `std::env::set_var`/`remove_var`
    // mutate process-global state under a threaded harness, which this workspace's test files
    // deliberately avoid (see `crates/vike-cli/tests/secrets_cli.rs`).
    assert!(
        std::env::var_os("VIKE_LOG_DIR").is_none(),
        "VIKE_LOG_DIR is set in this environment and outranks LogConfig::dir, so this test cannot \
         reach the failure it exists to check — unset it and re-run"
    );

    // `_base` is BOUND for the rest of the test: it owns the file that makes `dir` unopenable, and
    // it removes the whole tree when this test returns — pass or panic.
    let (_base, dir) = undirectory();
    assert!(!dir.exists(), "the case must start with an unopenable destination");

    // The whole assertion is that this RETURNS. Before the fix it panicked here.
    let guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "unwritable-case".to_string(),
        dir: Some(dir.clone()),
        ..Default::default()
    });

    assert!(
        guards.is_empty(),
        "the FILE layer must be dropped when its destination cannot be opened — a guard here means \
         a writer was installed over a directory that does not exist"
    );
    assert!(!dir.exists(), "nothing may have been created at an unopenable destination");

    // …and the console half is live: the subscriber installed, so an event has somewhere to go.
    // (This would abort the process if `init` had left the registry in a broken state.)
    tracing::info!("the console layer still works with no file layer");
    assert!(
        tracing::dispatcher::has_been_set(),
        "the global subscriber must still be installed — the file layer is the only casualty"
    );
}
