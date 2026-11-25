//! The exec client's start: the ONE shared `vike_bridge_core::exec_actor::ExecutionClientActor` over this crate's exec
//! thread (`super::actor`'s `run`), built with this venue's hooks and declarations, so no wrapper of ours forwards
//! a method by hand.

use std::sync::Arc;

use vike_bridge_core::confirm::DispatchLedger;
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_bridge_core::ratelimit::RateGate;
use vike_execution::{EventSender, RestoredOrder};

use crate::config::HyperliquidUrls;
use crate::execution::event_mapper::cloid_from_order_client_id;
use crate::instrument::properties::{HyperliquidInstruments, SharedInstruments};
use crate::signing::Signer;
use crate::signing::action::HyperliquidBuilderFee;

use super::DEFAULT_MARKET_SLIPPAGE;
use super::actor::{run, spawn_funding_poll};
use super::confirm::order_status_confirm;
use super::registry::OrderClientIdVenueRegistry;

#[cfg(doc)]
use super::market_slippage_for;

/// **The live Hyperliquid exec client**: the shared [`ExecutionClientActor`] over ONE dedicated OS thread that drives the signed
/// `/exchange` REST off the single-writer core; every venue event returns through the core ingest (the `/exchange`
/// response synchronously, fills/cancels via the WS pump). Dropping it (or `detach`) sends `Shutdown` and JOINS the
/// thread, which itself tears down the owned user-data pump, and the funding-poll thread (its OWN keyless transport),
/// attached as the actor's background. The start hands it the HALT sentinel with `ExecutionClientActor::with_halt_path`.
///
/// `signer` is the (agent or master) L1 signer: its network selects the hosts and the phantom-agent source;
/// `master_address` is the MASTER account address the WS pump subscribes with (agent-wallet queries return empty,
/// research §8); `instruments` is the loaded symbology and grid (shared read-only with the pump for coin⇄symbol
/// remapping). `reconcile_trigger` (when a reconcile driver is started) is fired by the pump once each WS reconnect's
/// `userFills` snapshot is decoded (`user_data::ReconnectNudge`) so a
/// pass re-syncs any order/fill state that drifted while the socket was down; `None` ⇒ no trigger. `builder` (task 7)
/// is the configured Builder Code, resolved once by the caller (this crate's own `adapter::live_runtime_for_account`,
/// via `attribution_code_from(vars, "hyperliquid")` + the configured fee): `None` (every non-configured start) rides
/// every submit byte-identically to before that field existed.
///
/// Prices emulated market orders at [`DEFAULT_MARKET_SLIPPAGE`], the historical literal; a caller with an operator band
/// calls [`spawn_with_market_slippage`] instead.
///
/// ⚠ It also opens its OWN per-IP REST weight window ([`crate::transport::ratelimit::ip_weight_gate`]), honest only for a
/// process whose sole Hyperliquid REST caller is this client. A live start has three (exec, funding poll, recon) and
/// must NOT reach the venue through three budgets it publishes one of, so it resolves the gate itself and calls
/// [`spawn_with_market_slippage`] with the handle.
pub fn spawn(
    signer: Signer,
    master_address: String,
    instruments: Arc<HyperliquidInstruments>,
    events: EventSender,
    reconcile_trigger: Option<std::sync::mpsc::Sender<()>>,
    builder: Option<HyperliquidBuilderFee>,
    urls: HyperliquidUrls,
) -> ExecutionClientActor {
    spawn_with_market_slippage(
        signer,
        master_address,
        SharedInstruments::new(instruments),
        events,
        reconcile_trigger,
        builder,
        DEFAULT_MARKET_SLIPPAGE,
        crate::transport::ratelimit::ip_weight_gate(),
        urls,
        OrderClientIdVenueRegistry::new(),
    )
}

/// [`spawn`] with the emulated-market slippage band, the per-IP weight window and the cloid map supplied by the
/// caller.
///
/// `market_slippage` is the band the composition root resolved ONCE via [`market_slippage_for`], the same "resolve at
/// the start, never re-read from a spawned thread" idiom the perp arms' leverage, the arming ceiling and the
/// attribution codes follow. It bounds BOTH aggressive-price sites: the emulated market order (off the mid) and the
/// protective stop-MARKET limit (off the trigger). The value is used verbatim, so callers resolve it through
/// [`market_slippage_for`] rather than passing a raw number: that is where the bound lives.
///
/// `ip_gate` is the venue's per-IP REST weight window, resolved ONCE by this crate's own live-mount seam
/// (`crate::adapter::live_runtime_for_account`) and cloned into BOTH REST paths this client owns: the exec thread's
/// signed `/exchange` and its `allMids` reads, and the funding poller's `/info`. The caller keeps a handle for the
/// reconcile transport it builds beside them, so all three spend one budget. Hyperliquid meters that budget per IP
/// ([`crate::transport::ratelimit`]), so a client that minted its own would be a second full window invisible to the first,
/// on a row (`vike_venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`) that admits exactly what the venue
/// publishes.
///
/// `urls` are the signer's network's addresses (`trade` REST, `user` ws), looked up by the start at that network's
/// type ([`HyperliquidUrls::of`]): every transport and socket this client opens dials them.
///
/// `registry` is the `cloid -> order_client_id` map this client's submits and adoptions write: the start hands the SAME map to
/// the reconcile client that reads the account (`ReconcileClientHyperliquid::with_cloids`). `shared` is the universe the
/// exec thread grows (a coin listed after the start), which the start hands the reconcile client too
/// (`ReconcileClientHyperliquid::with_instruments`), so both name a coin by the same user name.
pub fn spawn_with_market_slippage(
    signer: Signer,
    master_address: String,
    shared: SharedInstruments,
    events: EventSender,
    reconcile_trigger: Option<std::sync::mpsc::Sender<()>>,
    builder: Option<HyperliquidBuilderFee>,
    market_slippage: f64,
    ip_gate: RateGate,
    urls: HyperliquidUrls,
    registry: OrderClientIdVenueRegistry,
) -> ExecutionClientActor {
    // Read the network BEFORE `signer` moves into the exec-thread closure (`VenueNetwork` is `Copy`): the funding poll
    // needs it for its own keyless transport, not the signer (an `/info` read carries no signature).
    let network = signer.network();
    // Both REST paths ride the caller's ONE per-IP weight window: the poller takes a clone, the exec thread the handle.
    let funding = spawn_funding_poll(
        network,
        urls.rest.clone(),
        master_address.clone(),
        events.clone(),
        ip_gate.clone(),
        shared.clone(),
    );
    // ONE ledger: the exec thread marks what the confirm reads.
    let ledger = Arc::new(DispatchLedger::default());
    let confirm = order_status_confirm(
        network,
        &urls.rest,
        master_address.clone(),
        ip_gate.clone(),
        shared.clone(),
        ledger.clone(),
    );
    let run_registry = registry.clone();
    let run_events = events.clone();
    let actor = ExecutionClientActor::spawn("hyperliquid-exec", events, move |rx| {
        run(
            signer,
            master_address,
            shared,
            run_events,
            rx,
            reconcile_trigger,
            builder,
            market_slippage,
            ip_gate,
            urls,
            run_registry,
            ledger,
        );
    });
    with_venue_hooks(actor.with_poller(funding), registry).with_confirm(confirm)
}

/// `actor` with this venue's declarations and hooks over `registry`:
/// - [`ExecutionClientActor::with_bulk_submit`] and [`ExecutionClientActor::with_bulk_cancel`]: the `/exchange` order and cancel actions are
///   batch-first, so a batch is ONE signed action with N items (`vike_venues::venue_caps::HYPERLIQUID`'s
///   `supports_native_batch`); under a HALT a batch goes whole only when every leg reduces;
/// - [`ExecutionClientActor::with_wire_id_for`]: the reports name an order by `keccak(order client id)`, a pure function, so a fresh client
///   answers it (decision 0121);
/// - [`ExecutionClientActor::with_adopt_restored`] + [`ExecutionClientActor::with_adopt_queued`]: an order a previous session left resting
///   is seeded into the SHARED `registry` at once on the caller's thread (a WS frame or fill of it resolves the moment
///   the call returns), then taught to the exec thread's asset map by an `Adopt` on the lane `Cancel` rides, so a
///   cancel sent after it finds the asset. Restored, not "ours" for the first-snapshot floor
///   ([`OrderClientIdVenueRegistry::register_restored`]).
///
/// `pub(crate)` only so the tests can mount a recording command loop under the REAL hooks; [`spawn_with_market_slippage`]
/// is the one production caller.
pub(crate) fn with_venue_hooks(
    actor: ExecutionClientActor,
    registry: OrderClientIdVenueRegistry,
) -> ExecutionClientActor {
    actor
        .with_bulk_submit()
        .with_bulk_cancel()
        .with_wire_id_for(Arc::new(|order_client_id: &str| {
            Some(cloid_from_order_client_id(order_client_id))
        }))
        .with_adopt_restored(Arc::new(move |orders: &[RestoredOrder]| {
            for order in orders {
                registry.register_restored(&order.order_client_id);
            }
        }))
        .with_adopt_queued()
}
