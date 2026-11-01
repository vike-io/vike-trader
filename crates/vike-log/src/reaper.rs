//! **The workspace's ONE kill-on-drop guard for a test's child process.** `test-support`-gated, so no
//! shipped build compiles it.
//!
//! A test that spawns a daemon holds its [`Reaper`], so a panicking assertion never leaves the process
//! (and its sockets) running on a shared box. Eight copies of this guard, in three crates, were folded
//! here; it lives in this crate for the reason `poll` does: a leaf every such test reaches with a dev
//! edge. Field order still matters to a holder: a struct's fields drop in declaration order, so a
//! `Reaper` declared before the temp directory the child runs in is reaped before that directory goes.

use std::process::Child;

/// Kills and reaps the child when dropped (a `Drop` runs on a panicking test too). Both results are
/// ignored: the child may have exited already.
pub struct Reaper(pub Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
