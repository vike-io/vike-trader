//! Interactive Brokers venue bridge (Phase 1: socket exec). See
//! docs/superpowers/specs/2026-07-14-vike-ibkr-bridge-design.md.
//!
//! One crate, three switchable transports behind `transport::IbkrTransport`; Phase 1 wires only the
//! socket backend (feature `ibkr-socket`, Task 9) on the registry `ibapi` client. The bridge
//! is a thin adapter: all order-correctness logic is pure and fixture-tested
//! (config/error/contract/order/id_registry/event_mapper), and a `FakeTransport` makes the whole
//! submit→accept→fill lifecycle testable without a running Gateway (see `tests/ibkr_lifecycle.rs`).

pub mod config;
pub mod contract;
pub mod error;
mod event_mapper;
mod exec;
#[cfg(feature = "ibkr-socket")]
pub mod historical;
mod id_registry;
#[cfg(feature = "ibkr-socket")]
pub mod market_feed;
/// The venue mount contract (`vike_bridge_core::venue_mount::VenueMount`) for IBKR —
/// `vike-tradehub`'s registry row, under that crate's `ibkr` feature. Behind the `ibkr` umbrella
/// because the mount reads both backends: the cpapi reconcile client and the socket grid pre-fetch.
#[cfg(feature = "ibkr")]
pub mod mount;
pub mod order;
/// Live-`RiskGate` pre-fetch: a best-effort `contractDetails` → `SymbolProperties` round-trip over a
/// dedicated throwaway socket connection, for `mount::IbkrVenueMount`'s mount. Behind
/// `ibkr-socket` (where ibapi exists — the socket connection it opens).
#[cfg(feature = "ibkr-socket")]
mod properties;
/// The IBKR `ReconClient` report-fetch seam over the cpapi REST backend (recon breadth). Behind the
/// `ibkr-cpapi` feature (the backend that provides its transport); a default build never sees it.
#[cfg(feature = "ibkr-cpapi")]
pub mod recon_client;
pub mod transport;

use vike_bridge_core::exec_actor::ExecActor;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;

pub use config::{IbkrBackend, IbkrConfig};
pub use error::IbkrError;
#[cfg(feature = "ibkr-socket")]
pub use historical::{HistWhat, HistoricalFetcher, Window};
#[cfg(feature = "ibkr-socket")]
pub use market_feed::IbkrFeeds;
#[cfg(feature = "ibkr-socket")]
pub use properties::fetch_ibkr_properties;
#[cfg(feature = "ibkr-cpapi")]
pub use recon_client::IbkrReconClient;
// ⚠ This crate re-exported `vike_bridge_core::credentials::{Environment, load_workspace_dotenv,
// load_workspace_dotenv_from}` until 2026-09-28, so the two mount binaries that loaded an IBKR config
// — `vike-backfill`'s `ibkr_backfill` and `vike-run`'s `ibkr_mount` — could name them without a
// vike-bridge-core edge of their own. Both binaries are DELETED (measured unused), which left three
// second names with no seam behind them, so every caller names `vike_bridge_core::credentials`
// now. Why there is still no `load_ibkr_config(env)` convenience: `config.rs`'s
// `load_ibkr_config_from`.

/// This adapter's DECLARED static capability row (audit br6 registry). See
/// `vike_model::venues::venue_caps::IBKR` for the field-by-field rationale.
pub const CAPS: vike_model::VenueCaps = vike_model::venues::venue_caps::IBKR;

/// The capability row for a specific backend. `CAPS` (the crate-level const consumed by the audit
/// registry) stays the conservative socket row; the cpapi backend advertises native modify.
pub fn caps_for_backend(backend: IbkrBackend) -> vike_model::VenueCaps {
    match backend {
        IbkrBackend::Socket | IbkrBackend::Oauth => vike_model::venues::venue_caps::IBKR,
        IbkrBackend::Cpapi => vike_model::venues::venue_caps::IBKR_CPAPI,
    }
}

/// The venue `ExecutionClient`, forwarding to the [`ExecActor`] that owns the exec loop + transport.
/// Phase 1: `connect` degrades to `Unavailable` until Task 9 lands the socket transport (the
/// `exec::connect_socket` stub) — the app root then keeps the venue paper.
pub struct IbkrExecutionClient {
    actor: ExecActor,
}

impl IbkrExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's mount passes
    /// `MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live mount calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self { actor: self.actor.with_halt_path(path) }
    }

    /// Build the client from a resolved [`IbkrConfig`] + event lane. Constructs the socket transport
    /// (`ibkr-socket` feature) and spawns the `ExecActor` around [`exec::run_exec`]. Without the
    /// feature — or if the Gateway is unreachable — `connect_socket` returns
    /// `Err(IbkrError::Unavailable)` and the venue stays paper. The FakeTransport path
    /// (`run_exec_for_test`, under `test-support`) drives the SAME `run_exec` loop.
    pub fn connect(
        cfg: &IbkrConfig,
        events: EventSender,
    ) -> Result<IbkrExecutionClient, IbkrError> {
        let transport = match cfg.backend {
            IbkrBackend::Cpapi => exec::connect_cpapi(cfg, &events)?,
            // Socket is the default; Oauth has no backend yet (deferred) → connect_socket returns
            // Unavailable when the ibkr-socket feature is off.
            IbkrBackend::Socket | IbkrBackend::Oauth => exec::connect_socket(cfg, &events)?,
        };
        let run_events = events.clone();
        let actor = ExecActor::spawn("ibkr-exec", events, move |rx| {
            exec::run_exec(rx, run_events, transport);
        });
        Ok(IbkrExecutionClient { actor })
    }
}

impl ExecutionClient for IbkrExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.actor.submit(request);
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.actor.cancel(client_order_id);
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.actor.modify(order, new_qty, new_price);
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.actor.confirm(client_order_id);
    }
    /// Phase-one teardown seam — see `crates/vike-exec/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach`. ⚠ It MUST be delegated like every other method here: a
    /// wrapper that omits it inherits the trait's no-op and the core's raise-all phase skips this
    /// venue while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.actor.begin_detach();
    }
    fn detach(&mut self) {
        self.actor.detach();
    }
}

// ---------------------------------------------------------------------------------------------
// Test-support surface: drive the exec lifecycle with a FakeTransport, no Gateway. Behind
// `test`/`test-support` so it never ships in a default build. The integration test uses it under
// `--features test-support`; production `connect` uses the SAME `exec::run_exec` with the socket
// transport.
// ---------------------------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    //! No-Gateway test surface: the [`FakeTransport`]/[`ScriptedInbound`] double + a spawner that
    //! runs the real `exec::run_exec` loop over it on an `ExecActor` thread.
    pub use crate::transport::{FakeTransport, ScriptedInbound};
}

/// Spawn the real `exec::run_exec` loop over an arbitrary [`transport::IbkrTransport`] (typically a
/// [`testing::FakeTransport`]) on an [`ExecActor`] thread, returning the actor. The caller drives it
/// via the `ExecutionClient` impl (`submit`/`cancel`/`detach`). Test-only: the production path is
/// [`IbkrExecutionClient::connect`], which runs the identical loop over the socket transport.
#[cfg(any(test, feature = "test-support"))]
pub fn run_exec_for_test(
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> ExecActor {
    let run_events = events.clone();
    ExecActor::spawn("ibkr-exec-test", events, move |rx| {
        exec::run_exec(rx, run_events, transport);
    })
}

#[cfg(test)]
mod caps_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("ibkr");
        assert_eq!(super::CAPS, caps);
        // Phase 1: no native modify, no native batch; TIF set includes GTC/DAY/IOC/FOK/GTD.
        assert!(!caps.supports_modify);
        assert!(caps.supported_tifs.contains(&TimeInForce::Day));
        // Expanded axes (w2-task-5): `map_order_request` wires `"limit"` → LMT and `"stop"` → STP
        // (trigger_price ← aux_price); everything else — take_profit included — is the `_ => MKT`
        // coercion → unwired. `accepted_tifs` == the mapped five (Gtd is a stub — the good-till
        // date is never wired; TWS rejects it server-side, which is why it can stay accepted).
        // `margin_mode` never read (account-level Reg-T); no batch.
        use vike_model::{MarginMode, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs.len(), 5);
        assert_eq!(caps.accepted_tifs, caps.supported_tifs);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
        // The row's "Phase 3 / not yet" LIVE-DATA fields, which were stale for three weeks (see
        // the row's own doc): `market_feed::IbkrFeeds` (#306) serves all five live verbs.
        assert!(caps.live_data.bars);
        assert!(caps.live_data.quotes);
        assert!(caps.live_data.trades);
        assert!(caps.live_data.book);
        assert!(caps.live_data.depth);
        assert!(caps.has_live_data());
        // `backfill_bars` has its OWN, separate history (see the row's own doc): it followed
        // `live_data` from `false` to `true` on 2026-07-15 (`vike_backfill::venues::ibkr` + its
        // bin, #311, appended bars), then back to `false` on 2026-09-28 when
        // docs/decisions/0094 deleted that bin and its planner, measured unused — this crate's own
        // `HistoricalFetcher` stays the collector's target, but nothing in `vike-backfill` pages
        // it today.
        assert!(!caps.backfill_bars && !caps.backfill_ticks);
        // ⚠ Every assertion in this test compares fields of ONE constant to literals — it restates
        // the row, it does not execute the adapter. The NON-CIRCULAR evidence for `live_data` lives
        // outside this crate, and is named here so the next reader need not trust the prose:
        // `vike-bridge-core`'s `pump_spec.rs::venue_caps_cross_pin_the_pump_spec` checks
        // `has_live_data("ibkr")` against that table's independently-maintained
        // `"ibkr" => OwnPump { site: "vike-ibkr market_feed/mod.rs …" }` row — it contradicted this
        // row for three weeks and nothing failed, because until #306's era no test compared them.
        // `backfill_bars` is cross-pinned the same way by `vike-backfill`'s
        // `caps.rs::venue_caps_cross_pin_the_backfill_table`, which is what makes today's `false`
        // trustworthy rather than another silent three weeks.
    }

    #[test]
    fn caps_are_per_backend() {
        use crate::IbkrBackend;
        // Socket: no native modify; cpapi: native amend endpoint.
        assert!(!super::caps_for_backend(IbkrBackend::Socket).supports_modify);
        assert!(super::caps_for_backend(IbkrBackend::Cpapi).supports_modify);
        // The crate-level CAPS stays the socket (conservative) row.
        assert_eq!(super::CAPS, super::caps_for_backend(IbkrBackend::Socket));
    }
}
