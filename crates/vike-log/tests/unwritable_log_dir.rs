//! **An unwritable log destination degrades; it never stops the process.** `tracing_appender`'s
//! `rolling::daily` ends in an `.expect`, so reaching it would PANIC the caller (exit 101) on a
//! directory it cannot open — and `resolve_log_dir`'s `<exe_dir>/logs` last resort is read-only
//! under `ProtectSystem=strict`, a read-only container layer or `/usr/local/bin`.
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
/// The self-cleaning is safe because no permission bit is involved: `TempDir::drop` ignores its own
/// errors, so a read-only tree would leak in silence, but this root stays writable and holds one
/// regular file.
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
    // Not vacuous by accident: `$VIKE_LOG_DIR` OUTRANKS `LogConfig::dir`, so an ambient value would
    // redirect this run to a good directory and prove nothing. Not removed here: `remove_var`
    // mutates process-global state under a threaded harness.
    assert!(
        std::env::var_os("VIKE_LOG_DIR").is_none(),
        "VIKE_LOG_DIR is set in this environment and outranks LogConfig::dir, so this test cannot \
         reach the failure it exists to check — unset it and re-run"
    );

    // `_base` is BOUND for the rest of the test: it owns the file that makes `dir` unopenable, and
    // it removes the whole tree when this test returns — pass or panic.
    let (_base, dir) = undirectory();
    assert!(!dir.exists(), "the case must start with an unopenable destination");

    // The whole assertion is that this RETURNS rather than panicking.
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
