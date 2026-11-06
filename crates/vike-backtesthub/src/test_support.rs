//! The loopback backtesthub this crate's tests (`backtest_plane.rs`, `backtesthub_profile_roundtrip.rs`),
//! vike-cli's backtest/search suites and vike-studio-core's wire-parity suite each spawned by hand until
//! 2026-10-31, and the research bins' `--json` provenance pin, compiled once here behind `test-support`
//! (only DEV edges turn it on: this crate's own, vike-cli's, vike-studio-core's).

use std::net::SocketAddr;
use std::sync::Arc;

use vike_data::{HistStore, MemHistStore};

use crate::backtesthub_server::serve;

/// Bind an ephemeral loopback listener, spawn the REAL backtesthub (`serve`) over a fresh in-memory store
/// on a detached thread, and return the assigned address. Every backtesthub serves the Studio runners and
/// the study runner too, so this one serves all of them.
pub fn spawn_server() -> SocketAddr {
    spawn_server_over(Arc::new(MemHistStore::new()))
}

/// [`spawn_server`] over the caller's `store`, which is MOVED into the serve thread (a parity test keeps
/// its own clone for the local run it compares against).
pub fn spawn_server_over(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-server", move |listener| {
        let _ = serve(listener, store);
    })
}

/// **A research bin's `--json` document SAYS which percentile convention produced its numbers**,
/// asserted against the bin's own SOURCE, `bin_src` (the caller reads its own file), because `main`
/// assembles that document out of a dozen locals and no unit test can call it — the device
/// `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` applies to the composition
/// roots, with that gate's rule: if this reddens because the emitting site was legitimately
/// re-spelled, UPDATE the needle, never delete the test.
///
/// The needle is ASSEMBLED rather than written out, so the joined string appears in no file and can
/// only match the emitting site — a literal would let a test pass on its own text, the
/// self-reference hazard `crates/vike-ops/tests/docs/unrun_command_gate.rs` declares for itself. And
/// EXACTLY one match is required, which fails a typo'd needle at zero (so the test cannot go vacuous)
/// and a marker duplicated onto a per-anchor lane or a per-entry row at two: the stamp belongs at RUN
/// level. The stamp must then name the percentile convention actually in force.
pub fn assert_json_stamps_its_percentile_once(bin_src: &str) {
    let key = "stats_provenance";
    let needle = format!("\"{key}\": binutil::{key}()");
    assert_eq!(
        bin_src.matches(&needle).count(),
        1,
        "the --json summary must stamp `{needle}` exactly once, at RUN level"
    );
    assert_eq!(
        crate::binutil::stats_provenance()["percentile_method"],
        serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
        "the stamp must name the percentile convention the bin's numbers come from"
    );
}
