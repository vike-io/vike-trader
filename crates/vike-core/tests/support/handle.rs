//! Helpers over a running core's handle and its published snapshot, and the guard that joins a
//! spawned core when a test unwinds past it.

use std::time::{Duration, Instant};

use vike_exec::BarUpdate;
use vike_model::Bar;

/// A spawned core that is shut down and JOINED when it leaves scope, on a failing run too.
///
/// `vike_core::CoreHandle` has no join-on-drop: dropping it closes the ingest lane and DETACHES the
/// `vt-core` thread, which then runs its whole teardown (the save-on-stop sidecar write among it)
/// with nobody waiting. So a test that panics before its own `shutdown_and_join` unwinds past the
/// core, its `crate::scratch::Scratch` guard removes the state directory, and the still-running
/// teardown re-creates it through `write_json_atomic`'s `create_dir_all`: a directory nothing
/// removes, left on the failure path only. This guard's `Drop` makes the same `shutdown_and_join`
/// call the test would have made, so the teardown has finished before the next local drops.
///
/// - DECLARE IT AFTER the `Scratch` the core writes into. Locals drop in reverse declaration order,
///   so the core is joined first and the directory removed second. Declared before the `Scratch`,
///   it would join only once the directory was already gone, and close nothing.
/// - `Deref<Target = vike_core::CoreHandle>`, so `handle.bar_sender()`, `handle.is_alive()` and
///   `&handle` passed as a `&CoreHandle` read exactly as they did on the bare handle.
/// - [`Self::shutdown_and_join`] is the test's own explicit shutdown: the same call, the same
///   effect, and the `Drop` after it finds nothing left to join. A passing test runs what it ran
///   before.
/// - Not for a test whose subject is what DROPPING the handle does (the every-sender-dropped exit):
///   this guard's drop delivers `Command::Shutdown` first, which is a different exit path.
/// - A core that never answers `Command::Shutdown` makes a failing test wait here rather than
///   report: the same wait the test's own `shutdown_and_join` would have hit had it got that far.
pub(crate) struct CoreJoinGuard {
    /// `Some` for the guard's whole life; taken once, by whichever of [`Self::shutdown_and_join`]
    /// and `Drop` runs first.
    handle: Option<vike_core::CoreHandle>,
}

impl CoreJoinGuard {
    /// Own `handle` from here on: it is shut down and joined when the guard drops.
    pub(crate) fn new(handle: vike_core::CoreHandle) -> Self {
        Self { handle: Some(handle) }
    }

    /// `vike_core::CoreHandle::shutdown_and_join`, run now; the guard's `Drop` is then a no-op.
    pub(crate) fn shutdown_and_join(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown_and_join();
        }
    }
}

impl std::ops::Deref for CoreJoinGuard {
    type Target = vike_core::CoreHandle;
    fn deref(&self) -> &vike_core::CoreHandle {
        // Only `shutdown_and_join(self)` and `Drop` take the handle, and both end the guard, so a
        // guard that can still be dereferenced still holds it.
        self.handle.as_ref().expect("a live CoreJoinGuard holds its handle")
    }
}

impl Drop for CoreJoinGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown_and_join();
        }
    }
}

/// Close `b` on the binance `symbol`/1m series through the handle's lossless bar lane.
pub(crate) fn close_binance_bar(handle: &vike_core::CoreHandle, symbol: &str, b: Bar) {
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "binance".into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
}

/// Poll the snapshot until `pred` holds, or fail after `secs`.
///
/// Fails by DEADLINE rather than hanging: a core that never reaches the state turns into an
/// assertion naming `what`, polled every 5 ms.
pub(crate) fn wait_for_snapshot(
    cell: &arc_swap::ArcSwap<vike_core::CoreSnapshot>,
    secs: u64,
    what: &str,
    pred: impl Fn(&vike_core::CoreSnapshot) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred(&cell.load_full()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
