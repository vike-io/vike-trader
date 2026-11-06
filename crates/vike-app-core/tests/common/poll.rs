//! The ONE poll of this crate's threaded suites (`md_session_scripted`, `trade_cause_line`,
//! `trade_window_screens`): each carried its own copy of `until` and its `SETTLE`. Each user declares
//! it `#[path = "common/poll.rs"] mod poll;` (a directory test with `#[path = "../common/poll.rs"]`);
//! a `tests/` SUBDIRECTORY file is not a test target, so this builds no binary of its own.

use std::time::Duration;

/// How long a test waits for a background thread to do a thing before calling it a failure. The
/// session's own schedule is `feed_lifecycle::retry_backoff`, whose FIRST step is one second, so
/// anything this bound covers is a first-attempt event; nothing here waits out a backoff.
pub const SETTLE: Duration = Duration::from_secs(5);

/// Poll `f` until it answers `true`, or fail naming `what`. A spin rather than a sleep: every
/// property under test is produced by another thread, and a fixed sleep is either a flake or a
/// wasted second. `FnMut` so a poll may itself drive the frame thread's half (`Harness::sync`).
pub fn until(what: &str, f: impl FnMut() -> bool) {
    assert!(vike_log::poll::wait_until(SETTLE, f), "timed out waiting for: {what}");
}
