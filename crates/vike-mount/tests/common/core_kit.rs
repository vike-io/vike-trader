//! `kit` — the few builders and doubles the reconcile-manager suites (`tests/recon.rs`) share with
//! the vike-core integration suites they came from (wave 5, P6a).
//!
//! These are COPIES, on purpose and only of what the moved tests call: `crates/vike-core/tests/support/`
//! stays vike-core's own kit, and nothing of it is a dependency of this crate (vike-mount depends on
//! vike-core, so the arrow never points back; a `#[path]` include of that kit would also
//! drag in every double vike-core's suites need). The bodies are the originals verbatim, in the same
//! `kit::{doubles, engines, events}` module layout, so a moved test names `crate::kit::engines::dyn_engine`
//! exactly as it did.
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

/// Engine and core-config builders.
pub(crate) mod engines {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use vike_core::CoreConfig;
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;

    /// An engine on `venue`/`symbol` driving `client` — the ONE spelling of the default engine shape
    /// every builder below goes through.
    ///
    /// DEFAULTS: contract multiplier `1.0` (`Account::new`'s first argument — NOT a seed: `spawn_core`
    /// sets the equity seed from `CoreConfig::seed_cash`), `BalanceMode::Delta`, `RiskLimits::new()`
    /// (no limits), and the account labelled with the engine's own `venue` (so `route_key` is `venue`
    /// too). A test whose subject is one of those — an Authoritative wallet, a risk ceiling, a
    /// multiplier, an overridden `route_key`, `extra_symbols` — builds its engine itself.
    pub(crate) fn engine_on<C: ExecutionClient>(
        venue: &str,
        symbol: &str,
        client: C,
    ) -> ExecutionEngine<C> {
        ExecutionEngine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            client,
            venue,
            symbol,
        )
    }

    /// [`engine_on`] over a BOXED client, so engines whose clients differ in type share one
    /// `ExecutionEngine<Box<dyn ExecutionClient + Send>>` and can ride one multi-engine core.
    ///
    /// DEFAULTS: those of [`engine_on`]. A separate builder rather than [`engine_on`] called with a box,
    /// because the parameter's type is what makes `Box::new(SomeClient)` coerce at the call site: a
    /// generic parameter would infer `Box<SomeClient>`, which is not an `ExecutionClient` — only the
    /// `dyn` box is.
    pub(crate) fn dyn_engine(
        venue: &str,
        symbol: &str,
        client: Box<dyn ExecutionClient + Send>,
    ) -> ExecutionEngine<Box<dyn ExecutionClient + Send>> {
        engine_on(venue, symbol, client)
    }

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

    /// A core config with a self-advancing clock: every call advances an `AtomicI64` by one, so message
    /// `k` dispatches at `now_ms == k` and no real wall-clock wait is needed to reach a future instant.
    ///
    /// DEFAULTS: every other field is `CoreConfig::default()` — no strategy mount, no journal. Built
    /// with struct-update rather than by reassigning fields of a `Default::default()` value, which the
    /// workspace clippy gate forbids.
    pub(crate) fn test_config(seed_cash: f64) -> CoreConfig {
        let t = Arc::new(AtomicI64::new(0));
        CoreConfig {
            seed_cash,
            clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
            ..CoreConfig::default()
        }
    }
}

/// Venue report builders.
pub(crate) mod events {
    use vike_model::FillReport;
    use vike_model::events::LiquiditySide;

    /// A venue fill report for an EXTERNAL order — one with no local coid — which a reconcile pass
    /// must classify against local state rather than match to an order it placed.
    ///
    /// DEFAULTS: venue `"binance"`, symbol `"BTCUSDT"`, trade id `"t1"`, venue order id `"v9"`,
    /// `side 1`, 1.0 @ 100.0, no commission in `"USDT"`, taker, `ts 5`.
    pub(crate) fn external_fill_report() -> FillReport {
        FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: "t1".into(),
            venue_order_id: "v9".into(),
            client_order_id: None, // external order — no local coid
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        }
    }
}
