//! `kit` — the few builders and doubles the reconcile-manager suites (`tests/recon.rs`) share with
//! the vike-core integration suites they came from (wave 5, P6a).
//!
//! These are COPIES, on purpose and only of what the moved tests call: `crates/vike-core/tests/support/`
//! stays vike-core's own kit, and nothing of it is a dependency of this crate (vike-tradehub-core depends on
//! vike-core, so the arrow never points back; a `#[path]` include of that kit would also
//! drag in every double vike-core's suites need). The bodies are the originals verbatim, in the same
//! `kit::{doubles, engines, events}` module layout, so a moved test names `crate::kit::engines::test_config`
//! exactly as it did. What both kits needed and vike-exec could hold is `vike_exec::testing`'s
//! (`dyn_engine`).
//!
//! Included by `tests/recon.rs` as `#[path = "common/core_kit.rs"] mod kit;`. Cargo builds no target
//! from this file: it auto-discovers only `tests/*.rs` and `tests/<dir>/main.rs`, and `tests/common/mod.rs`
//! does not declare it (that module is the HALT-sentinel harness the other binaries share).
//!
//! `#![allow(dead_code)]` is required, not tidiness: the binary uses a subset of the kit.

#![allow(dead_code)]

/// Reconcile-client doubles.
pub(crate) mod doubles {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use vike_exec::recon::ReconClient;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

    /// Counts `fetch_order_status_reports` calls — one per reconcile pass — and reports nothing at all,
    /// so a test proves a pass RAN (or was suppressed) without depending on what a pass would find.
    pub(crate) struct CountingReconClient {
        pub(crate) calls: Arc<AtomicU64>,
    }

    impl ReconClient for CountingReconClient {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(Vec::new())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            Ok(Vec::new())
        }
    }
}

/// Engine builders.
pub(crate) mod engines {
    use vike_exec::testing::{RecordingClient, engine_on};
    use vike_exec::{ExecutionClient, ExecutionEngine};

    /// [`engine_on`] on the sim venue, for a test that brings its own client.
    ///
    /// DEFAULTS: those of [`engine_on`], with venue `"sim"` and symbol `"BTCUSDT"`.
    pub(crate) fn sim_engine_with<C: ExecutionClient>(client: C) -> ExecutionEngine<C> {
        engine_on("sim", "BTCUSDT", client)
    }

    /// [`sim_engine_with`] a fresh `RecordingClient`.
    ///
    /// DEFAULTS: those of [`sim_engine_with`], and the client. `RecordingClient` records but emits
    /// NOTHING of its own, which is what makes it the right client for most core tests: an order it
    /// accepted stays non-terminal until a test says otherwise, i.e. it RESTS.
    pub(crate) fn sim_engine() -> ExecutionEngine<RecordingClient> {
        sim_engine_with(RecordingClient::default())
    }
}
