//! `daemon_spawn` — the tests that start the SHIPPED daemon in a throwaway project root (the graceful
//! stop under a real SIGTERM, the live feed splice): ONE test binary, so `tests/support/temp_project.rs`
//! compiles once instead of once per binary that `#[path]`-included it (owner, 2026-10-31). It cannot
//! compile into this crate's lib with the other shared helpers (`src/test_support.rs`): it uses
//! `tempfile`, which no member may take as a normal dependency
//! (`crates/vike-ops/tests/hygiene/system_temp_gate/ratchet.rs`'s `tempfile_is_never_a_normal_dependency`).
//!
//! Each former `tests/<name>.rs` is now `tests/daemon_spawn/<name>.rs`, included below as a plain module,
//! its tests and their bodies unchanged: `--test sigterm_stop` is now `--test daemon_spawn`, a test's
//! nextest name gains its module, and `-- <name>` filtering is untouched. `sigterm_stop` keeps its own
//! `#![cfg(unix)]`, which now gates that module (there is no POSIX signal on Windows); the live half of
//! `venue_feed_splice_smoke` stays `#[ignore]`d and runs as that file's doc says, with the module named as
//! the filter (`-- venue_feed_splice_smoke:: --ignored`).
//!
//! `unreadable_credential_store` (the third spawn test, both its tests `#[ignore]`d) joined on
//! 2026-10-31 for the link alone (owner: fewer test binaries): run it by hand as
//! `-- unreadable_credential_store:: --ignored`. `windows_stop` (the fourth, `sigterm_stop`'s Windows
//! twin) joined the same day: its own `#![cfg(windows)]` gates the module, so the `windows-cross` lane
//! compiles it and only the dev box runs it, as before.
//!
//! `daemon_spawn/daemon_profile_seed.rs` is no member: the profile-row seed every spawn test uses,
//! here and not in the lib because this binary is its one user, so an edit to it rebuilds this binary
//! alone.
//!
//! Each test spawns its own child in its own temp root and reads only that child's pipes: no environment
//! variable, working directory or `tracing` subscriber of the test process changes, so a plain
//! `cargo test` running the files' tests as threads of one process changes nothing they check.

#[cfg(test)]
#[path = "support/temp_project.rs"]
mod temp_project;

#[cfg(test)]
#[path = "daemon_spawn/daemon_profile_seed.rs"]
mod daemon_profile_seed;

#[cfg(test)]
#[path = "daemon_spawn/sigterm_stop.rs"]
mod sigterm_stop;
#[cfg(test)]
#[path = "daemon_spawn/unreadable_credential_store.rs"]
mod unreadable_credential_store;
#[cfg(test)]
#[path = "daemon_spawn/venue_feed_splice_smoke.rs"]
mod venue_feed_splice_smoke;
#[cfg(test)]
#[path = "daemon_spawn/windows_stop.rs"]
mod windows_stop;

/// The next protocol line a spawned daemon printed on stdout (`lines`), or a failure naming `what` was
/// awaited and what the daemon logged (`log`): a dead daemon closes the pipe, so this never waits past
/// `patience`. The stop and refusal members' readers (`sigterm_stop`, `windows_stop`,
/// `unreadable_credential_store`) all read through it.
#[cfg(test)]
fn next_protocol_line(
    lines: &std::sync::mpsc::Receiver<String>,
    patience: std::time::Duration,
    what: &str,
    log: impl FnOnce() -> String,
) -> String {
    lines
        .recv_timeout(patience)
        .unwrap_or_else(|e| panic!("no {what} within {patience:?} ({e}); stderr:\n{}", log()))
}
