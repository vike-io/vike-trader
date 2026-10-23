//! Interactive Brokers venue bridge (Phase 1: socket exec). See
//! docs/superpowers/specs/2026-07-14-vike-ibkr-bridge-design.md.
//!
//! One crate, three switchable transports behind `transport::IbkrTransport`; Phase 1 wires only the
//! socket backend (feature `ibkr-socket`, Task 9) on the registry `ibapi` client. The bridge
//! is a thin adapter: all order-correctness logic is pure and fixture-tested
//! (config/error/contract/order/id_registry/event_mapper), and a `FakeTransport` makes the whole
//! submit→accept→fill lifecycle testable without a running Gateway (see `tests/ibkr_lifecycle.rs`).

#![warn(unreachable_pub)]

/// The venue-adapter contract (`vike_bridge_core::venue_adapter::VenueAdapter`) for IBKR —
/// `vike-tradehub`'s registry row, under that crate's `ibkr` feature. Behind the `ibkr` umbrella
/// because the start reads both backends: the two reconcile clients and the socket grid pre-fetch.
#[cfg(feature = "ibkr")]
pub mod adapter;
/// The shared bar aggregator: the socket feed folds its 5-second bars through it, the cpapi feed its
/// ticks (decision 0140). It sits at the crate root because both feeds use it, so it compiles with
/// either backend.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
mod agg;
// (Its docs are the module's own `//!` block: an outer `///` here would make rustdoc resolve that block's
// links from THIS scope.)
pub mod any_symbol;
/// The socket backend of [`any_symbol`]: the lookup over one data connection, and the source the
/// gatewayhub asks.
#[cfg(feature = "ibkr-socket")]
pub mod any_symbol_socket;
pub mod config;
pub mod contract;
/// IBKR market data over the cpapi (Client Portal) session — decision 0140's "Data on cpapi".
/// Public for its pure `decode` module; the client itself is reached through
/// [`connect_data_client`].
#[cfg(feature = "ibkr-cpapi")]
pub mod cpapi_feed;
/// The one door to an IBKR data client, matching on the backend (decision 0140).
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
mod data_door;
/// The ONE data connection per gateway session that the price stream, the any-symbol lookup and the grid
/// pre-fetch share (error 326 otherwise: IB refuses a second client under an id in use).
#[cfg(feature = "ibkr-socket")]
mod data_session;
pub mod error;
mod exec;
// No `///` here: an outer doc beside the file's own `//!` makes rustdoc resolve the module's links from this
// scope, where `AccountPerQuote` and `quote_rate` are not (the module doc says what it is).
pub mod fx_rate;
/// The socket reads behind [`fx_rate`]: a forex pair's top of book and the account's base currency, over the
/// session's one data connection.
#[cfg(feature = "ibkr-socket")]
mod fx_socket;
#[cfg(feature = "ibkr-socket")]
pub mod historical;
#[cfg(feature = "ibkr-socket")]
pub mod market_feed;
pub mod order;
/// Live-`RiskGate` pre-fetch: a best-effort `contractDetails` → `SymbolProperties` round-trip over a
/// dedicated throwaway socket connection, for `adapter::IbkrVenueAdapter`'s start. Behind
/// `ibkr-socket` (where ibapi exists — the socket connection it opens).
#[cfg(feature = "ibkr-socket")]
mod properties;
/// The price stream for symbols the engine was not mounted on: ONE lazily dialled data client for every symbol
/// (any-symbol, decision 0174). The gatewayhub's credentialed stream of ibkr is this client; the real-time rule
/// that turns its quotes into a price is the tradehub's price tap.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
pub mod quote_handle;
/// The two IBKR `ReconClient`s (recon breadth, decision 0140): `recon::cpapi` over the cpapi REST
/// backend, `recon::socket` over the SOCKET backend on its own API client id.
#[cfg(feature = "ibkr-cpapi")]
pub mod recon;
/// The POLICY of the price stream's supervised re-dial (pure, over a clock it is handed): when a dead data
/// connection is dialled again, the backoff, the daily logoff's quiet hint and when the outage is over.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
mod redial;
pub mod transport;

use vike_bridge_core::exec_actor::ExecActor;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_venues::IbkrBackend;

pub use config::IbkrConfig;
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
pub use data_door::connect_data_client;
pub use error::IbkrError;
#[cfg(feature = "ibkr-socket")]
pub use historical::{HistWhat, HistoricalFetcher, Window};
#[cfg(feature = "ibkr-socket")]
pub use market_feed::IbkrFeeds;
#[cfg(feature = "ibkr-socket")]
pub use properties::fetch_ibkr_properties;
#[cfg(feature = "ibkr-cpapi")]
pub use recon::cpapi::IbkrReconClient;
// ⚠ This crate re-exported `vike_secrets::credentials::Environment` and the two store loaders
// beside it (since deleted) until 2026-09-28, so the two start binaries that loaded an IBKR config
// — `vike-backfill`'s `ibkr_backfill` and `vike-run`'s `ibkr_mount` — could name them without a
// vike-bridge-core edge of their own. Both binaries are DELETED (measured unused), which left three
// second names with no seam behind them. `Environment` itself is gone since (decision 0156 item 6):
// the tier is `vike_model::AccountTier`, and the loader reads the started account's fields. Why there
// is still no `load_ibkr_config(env)` convenience: `config.rs`'s `load_ibkr_config_from`.

/// This adapter's DECLARED static capability row (audit br6 registry). See
/// `vike_venues::venue_caps::IBKR` for the field-by-field rationale.
pub const CAPS: vike_venues::VenueCaps = vike_venues::venue_caps::IBKR;

/// The venue `ExecutionClient`, forwarding to the [`ExecActor`] that owns the exec loop + transport.
/// Phase 1: `connect` degrades to `Unavailable` until Task 9 lands the socket transport (the
/// `exec::connect_socket` stub) — the app root then keeps the venue paper.
pub struct IbkrExecutionClient {
    actor: ExecActor,
    /// What the host taught about restored orders (`adopt_restored`), drained by the exec loop.
    taught: exec::Taught,
}

impl IbkrExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self { actor: self.actor.with_halt_path(path), taught: self.taught }
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
        Ok(Self::over("ibkr-exec", events, transport))
    }

    /// Whether the exec loop is holding the learning's executions replay for the host's first `adopt_restored`
    /// (the handover). Test door: how a test knows the bridge is ready to learn and waiting for its host.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn awaiting_handover(&self) -> bool {
        self.taught.is_awaiting()
    }

    /// The client over an already-connected transport: the thread that runs the exec loop, and the hand-off the
    /// host's teaching (`adopt_restored`) travels through. The ONE place both are built.
    fn over(
        name: &str,
        events: EventSender,
        transport: Box<dyn transport::IbkrTransport>,
    ) -> IbkrExecutionClient {
        let run_events = events.clone();
        // Gated: the learning's executions replay waits for the host's first `adopt_restored` (the handover).
        let taught = exec::Taught::gated();
        let run_taught = taught.clone();
        let actor = ExecActor::spawn(name, events, move |rx| {
            exec::run_exec(rx, run_events, transport, run_taught);
        });
        IbkrExecutionClient { actor, taught }
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
    /// The host announces the orders a restart restored, each with the unified symbol the engine registered it
    /// under. The exec loop stamps that symbol on the fills of the order (`EventMapper::fill_symbol`): an order
    /// a rebuilt connection learns from the venue's open orders carries no symbol of its own. Returns at once.
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        self.taught.teach(orders);
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
    pub use crate::transport::{
        AmendOutcome, FAKE_ACCOUNT, FAKE_CLIENT_ID, FakeTransport, ScriptedInbound, Snapshot,
        SnapshotMode,
    };
}

/// The real [`IbkrExecutionClient`] (the one `connect` builds, with its `adopt_restored` hand-off) over an arbitrary
/// [`transport::IbkrTransport`]. Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn client_for_test(
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> IbkrExecutionClient {
    IbkrExecutionClient::over("ibkr-exec-test", events, transport)
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
        exec::run_exec(rx, run_events, transport, exec::Taught::default());
    })
}

#[cfg(test)]
mod caps_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_venues::caps_for("ibkr");
        assert_eq!(super::CAPS, caps);
        // Modify on both backends (the socket re-places under the same id), no native batch; TIF set
        // includes GTC/DAY/IOC/FOK/GTD.
        assert!(caps.supports_modify);
        assert!(caps.supported_tifs.contains(&TimeInForce::Day));
        // Expanded axes (w2-task-5): `map_order_request` wires `"limit"` → LMT and `"stop"` → STP
        // (trigger_price ← aux_price); everything else — take_profit included — is the `_ => MKT`
        // coercion → unwired. `accepted_tifs` == the mapped five (Gtd is a stub — the good-till
        // date is never wired; TWS rejects it server-side, which is why it can stay accepted).
        // `margin_mode` never read (account-level Reg-T); no batch.
        use vike_model::MarginMode;
        use vike_venues::TriggerType;
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs.len(), 5);
        assert_eq!(caps.accepted_tifs, caps.supported_tifs);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
        // The LIVE-DATA fields: bars, quotes and trades are served on both backends
        // (`market_feed::IbkrFeeds` over ibapi, `cpapi_feed`'s client over the CP Gateway); book and
        // depth are NOT — the accounts hold no depth entitlement (decision 0140, Context 7), and
        // both clients refuse the two verbs off this row (`data_door`'s `no_book_lane`).
        assert!(caps.live_data.bars);
        assert!(caps.live_data.quotes);
        assert!(caps.live_data.trades);
        assert!(!caps.live_data.book);
        assert!(!caps.live_data.depth);
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
}
