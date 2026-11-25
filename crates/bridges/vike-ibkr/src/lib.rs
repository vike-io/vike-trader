//! Interactive Brokers venue bridge (Phase 1: socket exec). See
//! docs/superpowers/specs/2026-07-14-vike-ibkr-bridge-design.md.
//!
//! One crate, three switchable transports behind `transport::IbkrTransport`; Phase 1 wires only the
//! socket backend (feature `ibkr-socket`, Task 9) on the registry `ibapi` client. The bridge
//! is a thin adapter: all order-correctness logic is pure and fixture-tested
//! (config/error/contract/order/id_registry/event_mapper), and a `IbkrTransportFake` makes the whole
//! submit→accept→fill lifecycle testable without a running Gateway (see `tests/ibkr_lifecycle.rs`).

#![warn(unreachable_pub)]

/// The venue-adapter contract (`vike_bridge_core::venue_adapter::VenueAdapter`) for IBKR —
/// `vike-tradehub`'s registry row, under that crate's `ibkr` feature. Behind the `ibkr` umbrella
/// because the start reads both backends: the two reconcile clients and the socket grid pre-fetch.
#[cfg(feature = "ibkr")]
pub mod adapter;
pub mod config;
pub mod refusals;
// No `///` here: the module's own `//!` block is its doc. Ungated: the gatewayhub's `trade` build watches the gateway
// with no backend compiled.
pub mod gateway_watch;
#[cfg(feature = "ibkr-socket")]
pub mod symbol_properties;
pub mod transport;

pub mod account;
pub mod execution;
/// The one door to an IBKR data client, matching on the backend (decision 0140).
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
pub mod feed;
#[cfg(feature = "ibkr-socket")]
pub mod history;
pub mod instrument;
/// The two IBKR `ReconcileClient`s (recon breadth, decision 0140): `reconcile::cpapi` over the cpapi REST
/// backend, `reconcile::socket` over the SOCKET backend on its own API client id.
#[cfg(feature = "ibkr-cpapi")]
pub mod reconcile;

use std::sync::Arc;

use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_execution::EventSender;
use vike_venues::IbkrBackend;

pub use config::IbkrConfig;
#[cfg(feature = "ibkr-socket")]
pub use history::{HistWhat, HistoricalFetcher, Window};
#[cfg(feature = "ibkr-socket")]
pub use instrument::properties::fetch_ibkr_properties;
#[cfg(feature = "ibkr-cpapi")]
pub use reconcile::cpapi::ReconcileClientIbkrCpapi;
pub use transport::error_codes::IbkrError;
// ⚠ This crate re-exported `vike_secrets::credentials::Environment` and the two store loaders
// beside it (since deleted) until 2026-09-28, so the two start binaries that loaded an IBKR config
// — `vike-backfill`'s `ibkr_backfill` and `vike-run`'s `ibkr_mount` — could name them without a
// vike-bridge-core edge of their own. Both binaries are DELETED (measured unused), which left three
// second names with no seam behind them. `Environment` itself is gone since (decision 0156 item 6):
// the tier is `vike_model::AccountTier`, and the loader reads the started account's fields. Why there
// is still no `load_ibkr_config(env)` convenience: `config.rs`'s `load_ibkr_config_from`.

/// This adapter's DECLARED static capability row (audit br6 registry). See
/// `vike_venues::venue_caps::IBKR` for the field-by-field rationale.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::IBKR;

/// Why an account whose tier has `venue.ibkr.<tier>.extended_hours = 1` does not start on the `cpapi` backend.
const CPAPI_REFUSES_EXTENDED_HOURS: &str = "venue.ibkr.<tier>.extended_hours is 1 but the cpapi backend cannot \
     send an order outside the regular session (its order body has no outsideRTH), so this account is NOT \
     started: set the tier's backend to socket, or write extended_hours 0";

/// The venue exec client: the shared [`ExecutionClientActor`] that owns the exec loop + transport, built from a resolved
/// [`IbkrConfig`] + event lane, with the account its session trades. Constructs the socket transport
/// (`ibkr-socket` feature) and spawns the actor around [`execution::run_execution`]. Without the feature — or if the Gateway is
/// unreachable — `connect_socket` returns `Err(IbkrError::Unavailable)` and the account is NOT RUNNING. The
/// IbkrTransportFake path (`run_execution_for_test`, under `test-support`) drives the SAME `run_execution` loop.
///
/// The account is the one this client places every order in: `config.account`, or on the socket backend, for an account
/// that stores none, the one the gateway's login named at the connect ([`config::account_from_gateway`]). The HALT
/// sentinel the actor watches is handed in by [`ExecutionClientActor::with_halt_path`] (`AdapterInputs::process.halt_path`;
/// `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start calls it).
pub fn connect_execution_client(
    config: &IbkrConfig,
    events: EventSender,
) -> Result<(ExecutionClientActor, String), IbkrError> {
    // The cpapi order body carries no `outsideRTH`: a switch it cannot honour is refused here, before any
    // I/O, and never ignored (the host folds the refusal into the account's NOT RUNNING reason).
    if config.extended_hours && config.backend == IbkrBackend::Cpapi {
        return Err(IbkrError::Unsupported(CPAPI_REFUSES_EXTENDED_HOURS.to_string()));
    }
    // A new connection's path is not known until its watch says so; the last one's word is history.
    if let Some(key) = events.route_key {
        vike_bridge_core::venue_link::venue_link(&key).reset();
    }
    let (transport, account) = match config.backend {
        IbkrBackend::Cpapi => (execution::connect_cpapi(config, &events)?, config.account.clone()),
        // Socket is the default; Oauth has no backend yet (deferred) → connect_socket returns
        // Unavailable when the ibkr-socket feature is off.
        IbkrBackend::Socket | IbkrBackend::Oauth => execution::connect_socket(config, &events)?,
    };
    // Client ids are unique per gateway (`IbkrConfig::reconcile_client_id`), so this names one client's rebuilds.
    let identity = format!("{}:{}:{}", config.host, config.port, config.client_id);
    Ok((over("ibkr-exec", events, transport, execution::Taught::gated_as(identity)), account))
}

/// The client over an already-connected transport: the thread that runs the exec loop, and the hand-off the host's
/// teaching (`adopt_restored`) travels through. The ONE place both are built. `taught` is gated: the learning's
/// executions replay waits for the host's first `adopt_restored` (the handover).
///
/// The actor's hooks (`vike_bridge_core::execution_client::hooks`): the exec loop reads `ExecutionClientActorCommand::Modify`'s
/// trigger (`execution/mod.rs`'s `amend`), so a trigger amend is queued (`with_trigger_amend`); and `adopt_restored` hands the
/// orders a restart restored, each with the unified symbol the engine registered it under, to the exec loop, which
/// stamps that symbol on the fills of the order (`EventMapper::fill_symbol`: an order a rebuilt connection learns
/// from the venue's open orders carries no symbol of its own). Returns at once. The venue order id is NOT taught:
/// IB's numeric id belongs to one API client id, so a journaled one could name another order of this connection; the
/// open-orders replay rebinds each order by its `orderRef`. On the cpapi backend the journaled id is not the
/// gateway's either (an accept here carries our allocated numeric), so the exec loop asks the transport to find the
/// orders it does not hold in the gateway's order list by order client id (`IbkrTransport::locate_restored`), which is what a
/// cancel or amend of one then addresses.
fn over(
    name: &str,
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
    taught: execution::Taught,
) -> ExecutionClientActor {
    let run_events = events.clone();
    let run_taught = taught.clone();
    ExecutionClientActor::spawn(name, events, move |rx| {
        execution::run_execution(rx, run_events, transport, run_taught);
    })
    .with_trigger_amend()
    .with_adopt_restored(Arc::new(move |orders: &[vike_execution::RestoredOrder]| {
        let pairs: Vec<(String, String)> =
            orders.iter().map(|o| (o.order_client_id.clone(), o.symbol.clone())).collect();
        taught.teach(&pairs);
    }))
}

// ---------------------------------------------------------------------------------------------
// Test-support surface: drive the exec lifecycle with a IbkrTransportFake, no Gateway. Behind
// `test`/`test-support` so it never ships in a default build. The integration test uses it under
// `--features test-support`; production `connect` uses the SAME `execution::run_execution` with the socket
// transport.
// ---------------------------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    //! No-Gateway test surface: the [`IbkrTransportFake`]/[`ScriptedInbound`] double + a spawner that
    //! runs the real `execution::run_execution` loop over it on an `ExecutionClientActor` thread.
    pub use crate::transport::{
        AmendOutcome, FAKE_ACCOUNT, FAKE_CLIENT_ID, IbkrTransportFake, ScriptedInbound,
        SnapshotMode, SnapshotRequest,
    };

    /// **The operator's ibkr rows the tests of this crate trade** (decision 0290's O-10: IBKR lists nothing, so its
    /// kept list is the operator's rows), each built from its wire name as `vike-cli data instruments add` states
    /// one: the stocks `AAPL`, `MSFT`, `TSLA`, `NVDA` (directed to NASDAQ), `SAP` (EUR) and the class share `BRK.B`
    /// (IB's `BRK B`, its contract id pinned), the pairs
    /// `EURUSD` and `USDJPY`, the futures `ES_251219` and `ES_991218` (GLOBEX), `ES_261218` and `MES_261218` (CME);
    /// and `E07` and `E07SL`, the stocks of the delayed-mark tests, whose marks no other test may write (the table is
    /// process-wide). `ES_991218` is the one a SEND test may name: the delivery guard refuses every order on a contract
    /// whose last trading day has begun (`ES_251219`), and judges an opening one inside its last days by the wall clock.
    #[must_use]
    pub fn operator_rows() -> Vec<vike_model::Instrument> {
        let row = |wire: &str, base: &str, quote: &str| {
            let contract = crate::instrument::contract::parse_simplified(wire)
                .map_err(|e| e.to_string())
                .and_then(|c| c.to_contract().ok_or_else(|| format!("{wire}: no typed contract")))
                .unwrap_or_else(|why| panic!("a test row: {why}"));
            vike_model::Instrument::new("ibkr", wire, base, quote, contract)
        };
        // named as the datahub's write stores them
        vike_catalog::spelled(vec![
            row("AAPL.SMART.USD", "AAPL", "USD"),
            row("MSFT.SMART.USD", "MSFT", "USD"),
            row("TSLA.SMART.USD", "TSLA", "USD"),
            row("NVDA.NASDAQ.USD", "NVDA", "USD"),
            row("SAP.SMART.EUR", "SAP", "EUR"),
            row("BRK_B.SMART.USD.STK.72063691", "BRK.B", "USD"),
            row("EUR.USD.IDEALPRO", "EUR", "USD"),
            row("USD.JPY.IDEALPRO", "USD", "JPY"),
            row("ES.GLOBEX.USD.FUT.20251219", "ES", "USD"),
            row("ES.GLOBEX.USD.FUT.20991218", "ES", "USD"),
            row("ES.CME.USD.FUT.20261218", "ES", "USD"),
            row("MES.CME.USD.FUT.20261218.5", "MES", "USD"),
            row("E07.SMART.USD", "E07", "USD"),
            row("E07SL.SMART.USD", "E07SL", "USD"),
        ])
    }

    /// Hold [`operator_rows`] as this process's ibkr kept list (`vike_catalog::hold_kept_list`). Every test of a binary
    /// holds these same rows, so tests running at once never see another list.
    pub fn hold_operator_rows() {
        vike_catalog::hold_kept_list("ibkr", operator_rows());
    }

    /// Whether anything accepts a TCP connection on `127.0.0.1:port`. A dead-port test (the bridge's
    /// and the gatewayhub's) points a config at ports nothing should answer on; a box where one answers
    /// cannot say what a dead port does, so the test skips BEFORE starting. Deciding the skip from the
    /// start's own outcome instead would make the outcome it asserts on unfalsifiable.
    pub fn something_listens_on(port: u16) -> bool {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(250),
        )
        .is_ok()
    }
}

/// The real exec client (the one [`connect_execution_client`] builds, with its `adopt_restored` hand-off) over an
/// arbitrary [`transport::IbkrTransport`]. Test-only. Like every test client here it holds
/// [`testing::operator_rows`] first: the orders of a test name user names (`AAPL`), read through those rows.
#[cfg(any(test, feature = "test-support"))]
pub fn client_for_test(
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> ExecutionClientActor {
    testing::hold_operator_rows();
    over("ibkr-exec-test", events, transport, execution::Taught::gated())
}

/// [`client_for_test`], with what says whether the exec loop is holding the learning's executions replay for the
/// host's first `adopt_restored` (the handover): how a test knows the bridge is ready to learn and waiting for its
/// host. Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn client_for_test_with_handover(
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> (ExecutionClientActor, impl Fn() -> bool + Send + Sync) {
    testing::hold_operator_rows();
    let taught = execution::Taught::gated();
    let awaiting = taught.clone();
    (over("ibkr-exec-test", events, transport, taught), move || awaiting.is_awaiting())
}

/// [`client_for_test`] as `identity`, the key [`connect_execution_client`] builds from one exec client id on one
/// gateway: what the host taught one client of it after its handover, the next one knows. Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn client_for_test_as(
    identity: &str,
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> ExecutionClientActor {
    testing::hold_operator_rows();
    let taught = execution::Taught::gated_as(identity.to_string());
    over("ibkr-exec-test", events, transport, taught)
}

/// Spawn the real `execution::run_execution` loop over an arbitrary [`transport::IbkrTransport`] (typically a
/// [`testing::IbkrTransportFake`]) on an [`ExecutionClientActor`] thread, returning the actor. The caller drives it
/// via the `ExecutionClient` impl (`submit`/`cancel`/`detach`). Test-only: the production path is
/// [`connect_execution_client`], which runs the identical loop over the socket transport.
#[cfg(any(test, feature = "test-support"))]
pub fn run_execution_for_test(
    events: EventSender,
    transport: Box<dyn transport::IbkrTransport>,
) -> ExecutionClientActor {
    testing::hold_operator_rows();
    let run_events = events.clone();
    ExecutionClientActor::spawn("ibkr-exec-test", events, move |rx| {
        execution::run_execution(rx, run_events, transport, execution::Taught::default());
    })
    .with_trigger_amend()
}

#[cfg(test)]
mod extended_hours_refusal {
    use super::*;

    /// **The cpapi backend refuses an account that asks for extended hours**: its order body carries no
    /// `outsideRTH`, so honouring the switch is impossible and an ignored one would be the silent failure
    /// this repo forbids. The refusal comes BEFORE any I/O (the loopback port below is never dialled), so
    /// the account is NOT RUNNING with the words in the journal (`adapter.rs`'s exec-connect failure).
    #[test]
    fn the_cpapi_backend_refuses_the_switch_it_cannot_honour() {
        let (events, _rx) = vike_execution::event_channel(8);
        let config = IbkrConfig {
            tier: vike_model::AccountTier::Demo,
            backend: IbkrBackend::Cpapi,
            host: "127.0.0.1".into(),
            port: 1,
            client_id: 1,
            account: "TEST".into(),
            cpapi_url: "https://127.0.0.1:1".into(),
            market_data_type: config::MarketDataType::default(),
            data_client_id: 2,
            reconcile_client_id: 3,
            extended_hours: true,
        };
        match connect_execution_client(&config, events) {
            Err(IbkrError::Unsupported(why)) => assert!(
                why.contains("extended_hours") && why.contains("cpapi") && why.contains("socket"),
                "{why}"
            ),
            Err(other) => panic!("refused for the wrong reason: {other}"),
            Ok(_) => panic!("a cpapi account asking for extended hours must not start"),
        }
    }
}

#[cfg(test)]
mod capabilities_test {
    use vike_model::TimeInForce;

    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("ibkr");
        assert_eq!(super::CAPABILITIES, capabilities);
        // Modify on both backends (the socket re-places under the same id), no native batch; TIF set
        // is GTC/DAY/IOC/FOK, GTD and the two auctions (the socket wires each; the Client Portal
        // refuses a GTD and `MOC`/`LOC` by name).
        assert!(capabilities.supports_modify);
        assert!(capabilities.supported_tifs.contains(&TimeInForce::Day));
        // Expanded axes: `map_order_request` builds `"limit"` → LMT, `"stop"` → STP (trigger_price ←
        // aux_price) and `"stop_limit"` → STP LMT (price ← lmt_price, trigger_price ← aux_price); every
        // other kind, take_profit included, is refused by name (it was the `_ => MKT` coercion before
        // wave 1 of the new order kinds). `accepted_tifs` == the mapped seven: GTD (`goodTillDate`, the
        // attributes smoke) and the auctions (`OPG`, `MOC`/`LOC`, the w2 auction smoke, paper 2026-11-10)
        // joined the four; post-only stays refused by name (IB has no such order). `margin_mode` never
        // read (account-level Reg-T); no batch.
        use vike_model::MarginMode;
        use vike_venues::TriggerType;
        // `"trailing_take_profit"` is `TRAIL MIT` / `TRAIL LIT` (the attributes wave's paper smoke, green
        // 2026-11-09 for `TRAIL MIT`; `TRAIL LIT` failed there, IB's 321); `"pegged"` is
        // `map_order_request`'s pegs, listed after its paper smoke passed `PEG MID` (the other anchors are
        // refused by name in the bridge)
        assert_eq!(
            capabilities.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "trailing_take_profit", "pegged"]
        );
        // The bridge never reads `reduce_only` (a plain order goes out), so the reduce-only set is the kind set.
        assert_eq!(capabilities.reduce_only_kinds, capabilities.supported_order_kinds);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss]);
        // the attributes wave, smoke green 2026-11-09: `build_order`'s `allOrNone`, `sweepToFill`,
        // `discretionaryAmt` (the regular route; refused by name on the extended-hours one), `hidden`, the
        // adjustable stop's `adjusted*` fields, and a time-only release resting at IB as `goodAfterTime`
        assert_eq!(
            capabilities.order_attributes,
            &["all_or_none", "sweep_to_fill", "discretionary", "hidden", "adjust", "release"]
        );
        // a percent trail, `trailingPercent` in place of the `auxPrice`, where the market form passed
        // (`TRAIL`, `TRAIL MIT`); `TRAIL LIMIT` by a percent stays closed (IB's 321)
        assert_eq!(capabilities.percent_trail_kinds, &["trailing", "trailing_take_profit"]);
        // the iceberg, a display on the round lot (`ibkr_iceberg_lot_smoke`, paper 2026-11-10: AAPL display 40
        // accepted, 1 and 100 IB's lot-size [201], which the bridge's `lot_refusal` says first)
        assert!(capabilities.supports_display_quantity);
        assert_eq!(capabilities.accepted_tifs.len(), 7);
        for t in [TimeInForce::Gtd, TimeInForce::AtOpen, TimeInForce::AtClose] {
            assert!(capabilities.accepted_tifs.contains(&t), "{t:?}");
        }
        assert!(!capabilities.accepted_tifs.contains(&TimeInForce::PostOnly));
        assert_eq!(capabilities.accepted_tifs, capabilities.supported_tifs);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
        // The LIVE-DATA fields: bars, quotes and trades are served on both backends
        // (the price stream's socket backend over ibapi, its cpapi backend over the CP Gateway); book and
        // depth are NOT — the accounts hold no depth entitlement (decision 0140, Context 7), and
        // both backends refuse a book off this row (`data_door`'s `no_book_lane`).
        assert!(capabilities.live_data.bars);
        assert!(capabilities.live_data.quotes);
        assert!(capabilities.live_data.trades);
        assert!(!capabilities.live_data.book);
        assert!(!capabilities.live_data.depth);
        assert!(capabilities.has_live_data());
        // `backfill_bars` has its OWN, separate history (see the row's own doc): it followed
        // `live_data` from `false` to `true` on 2026-07-15 (`vike_backfill::venues::ibkr` + its
        // bin, #311, appended bars), then back to `false` on 2026-09-28 when
        // docs/decisions/0094 deleted that bin and its planner, measured unused — this crate's own
        // `HistoricalFetcher` stays the collector's target, but nothing in `vike-backfill` pages
        // it today.
        assert!(!capabilities.backfill_bars && !capabilities.backfill_ticks);
        // ⚠ Every assertion in this test compares fields of ONE constant to literals — it restates
        // the row, it does not execute the adapter. The NON-CIRCULAR evidence lives outside this
        // crate, and is named here so the next reader need not trust the prose: HOW this venue's
        // prices are connected is the market driver of `crates/vike-venues/src/venue_connection.rs`'s
        // `IBKR` row (decision 0298, the one class: the pump table that once restated it, and the weld
        // that compared them, are gone), which the gatewayhub's credentialed-venue invariant holds
        // `!= No`; and vike-backfill reads `backfill_bars` as its venue-direct row, so its existence
        // gate is what makes today's `false` trustworthy.
    }
}
