//! The client's background threads: the funding-payment poll and the exec command loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use vike_bridge_core::confirm::DispatchLedger;
use vike_bridge_core::exec_actor::funding_poll;
use vike_bridge_core::exec_actor::{ExecutionClientActorCommand, Sending, sending, serving_submit};
use vike_bridge_core::ratelimit::RateGate;
use vike_execution::{EventSender, RestoredOrder};
use vike_utility::poller::StopHandle;

use crate::config::HyperliquidUrls;
use crate::execution::user_data::spawn_hyperliquid_user_data;
use crate::history::funding::{HyperliquidFundingPoller, fetch_funding};
use crate::instrument::properties::SharedInstruments;
use crate::signing::action::HyperliquidBuilderFee;
use crate::signing::{NonceManager, Signer};
use crate::transport::HyperliquidTransport;
use crate::transport::consts::VENUE;
use vike_model::VenueNetwork;

use super::registry::OrderClientIdVenueRegistry;
use super::state::{ExecutionState, HyperliquidExchangeLive};
use crate::account::modes::MarginGuard;
use crate::instrument::universe::Universe;

#[cfg(doc)]
use super::client::spawn_with_market_slippage;
#[cfg(doc)]
use super::market_slippage_for;

/// Spawn the funding-payment background poll thread through
/// `vike_bridge_core::exec_actor::funding_poll::spawn_funding_poll` (every polled venue's cadence, spawn floor and
/// warn-once streak): its OWN keyless [`HyperliquidTransport`] (never shares the exec thread's signed transport — no
/// signer is needed for `/info` reads), polling until the stop fires or the core goes away.
///
/// ⚠ The transport is its own; the per-IP REST **weight budget** is NOT, and `ip_gate` is how it
/// stops being. That budget is metered per IP rather than per client (see [`crate::transport::ratelimit`]), so
/// the `/info` reads below and the exec thread's `/exchange` actions are spending the same minute
/// whether or not they are counting it together. The caller's handle is cloned in here; a
/// transport left on the fresh gate [`HyperliquidTransport::new`] mints would run a SECOND
/// full-budget window that neither side can see.
///
/// Mirrors the
/// bybit/okx REST-poll sourcing decision — see the module doc on why HL, despite exposing
/// `userFundings`-shaped ledger channels in principle, is polled here rather than added as a
/// third WS subscription: `crate::execution::user_data`'s pump only carries `orderUpdates` + `userFills`
/// today, and reusing the already-ported [`fetch_funding`] keeps this change scoped to the same
/// REST-poll shape every venue in this wave uses, instead of growing the WS protocol surface.
///
/// The poller is floored at THIS spawn instant (`FundingPollThread::floor_ms`) — NOT a trailing lookback, which
/// would re-fold up to that whole lookback of already-balance-embedded payments on every restart
/// (the double-count the restart law forbids; see [`HyperliquidFundingPoller`]'s doc).
///
/// A payment names its venue `coin`; it leaves under the coin's USER name (`BTC` is `BTCUSDC.P`, decision 0290)
/// through `instruments`, the universe the exec thread grows, so the engine's symbol filter sees its own name. A
/// coin the universe does not know keeps the venue's name (the engine then drops it, as it drops a foreign fill).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): `userFunding` is
/// ACCOUNT-WIDE (master-address-scoped, every coin — deliberate, so every engine coin's payment
/// reaches the fold). If several HL perp starts ever share one master account, EACH start's
/// poller would emit EVERY coin's payments → N-fold duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must dedupe this lane account-wide (one poller per
/// master address, not per start).
pub(super) fn spawn_funding_poll(
    network: VenueNetwork,
    rest: String,
    master_address: String,
    events: EventSender,
    ip_gate: RateGate,
    instruments: SharedInstruments,
) -> StopHandle {
    funding_poll::spawn_funding_poll("hyperliquid-funding-poll", events, move |thread| {
        // The account's OWN funding is a trade read: its network's `trade` row, never live data.
        let transport = HyperliquidTransport::new(network, &rest).with_rate_gate(ip_gate);
        // Floor = spawn instant: only payments landing WHILE started are folded.
        let mut poller = HyperliquidFundingPoller::new(
            |start, end| fetch_funding(&transport, &master_address, start, end),
            VENUE,
            thread.floor_ms(),
        );
        thread.run(
            |now_ms| {
                poller.poll(now_ms).map(|mut polled| {
                    let now = instruments.current();
                    for event in &mut polled {
                        if let vike_model::events::Event::Funding(funding) = event
                            && let Some(row) = now.symbology().by_coin(funding.symbol.as_str())
                        {
                            funding.symbol = row.symbol.as_str().into();
                        }
                    }
                    polled
                })
            },
            |e: &String| {
                tracing::warn!(
                    target: "vike_hyperliquid",
                    error = %e,
                    "funding poll fetch failed; retrying each cadence (warns once per streak)"
                );
            },
        );
    })
}

/// Refuse by name each of `reqs` that joined the lane too long ago to be signed, and hand back the rest: a batch's
/// close still goes (`vike_bridge_core::exec_actor`'s `closes_at_once`), and so does nothing else that waited. A
/// cancel has no such door: a late cancel is safe and a dropped one is not.
pub(super) fn refused_too_old(
    events: &EventSender,
    reqs: Vec<vike_model::OrderRequest>,
    queued_at: std::time::Instant,
) -> Vec<vike_model::OrderRequest> {
    let mut still = Vec::with_capacity(reqs.len());
    for req in reqs {
        match vike_bridge_core::exec_actor::too_old_to_send(&req, queued_at) {
            Some(why) => {
                for event in vike_bridge_core::rest::refused(&req, why) {
                    let _ = events.blocking_send(event);
                }
            }
            None => still.push(req),
        }
    }
    still
}

/// The venue order id the core taught for a restored order, as HL's numeric `oid` (what a native modify needs); one
/// that is absent or not a number teaches none, and the modify is then refused in words.
pub(super) fn taught_oid(order: &RestoredOrder) -> Option<u64> {
    order.venue_order_id.as_deref().and_then(|v| v.parse().ok())
}

/// The exec thread body: build the live exchange + shared registry, start the private-WS pump, then
/// drain commands until `Shutdown`. ALL network I/O (signing, REST, WS) lives HERE, off the
/// single-writer core. On `Shutdown` the pump is stopped + joined before returning. `builder` (task
/// 7) is the configured Builder Code, carried into every submitted `order` action. `slippage` is the
/// mount-resolved emulated-market band ([`market_slippage_for`]), carried into [`ExecutionState`].
///
/// `ip_gate` is the caller's handle on the venue's ONE per-IP REST weight window, installed on this
/// thread's transport so the signed `/exchange` actions and the emulated-market `allMids` reads
/// charge the same budget as the funding poller and the start's reconcile transport. See
/// [`spawn_with_market_slippage`] for who resolves it.
#[expect(clippy::too_many_arguments)]
pub(super) fn run(
    signer: Signer,
    master_address: String,
    shared: SharedInstruments,
    events: EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    reconcile_trigger: Option<std::sync::mpsc::Sender<()>>,
    builder: Option<HyperliquidBuilderFee>,
    slippage: f64,
    ip_gate: RateGate,
    urls: HyperliquidUrls,
    registry: OrderClientIdVenueRegistry,
    ledger: Arc<DispatchLedger>,
) {
    let network = signer.network();
    let websocket_url = urls.user_websocket;
    // ONE universe (`shared`), three readers: the pump remaps through it, this thread signs by it and the
    // confirm names a fill through it, and it is replaced as a whole when an order names a coin listed
    // after the start (`Universe`).
    // The private user-data pump: authoritative fills/cancels push straight into the core ingest,
    // remapping cloid→order client id via the shared registry and coin→symbol via the shared instruments. On
    // reconnect it fires `reconcile_trigger` (when reconcile is started) to re-sync missed state, once the new
    // session's `userFills` snapshot is decoded.
    let pump = spawn_hyperliquid_user_data(
        websocket_url,
        master_address.clone(),
        shared.clone(),
        registry.clone(),
        events.clone(),
        reconcile_trigger,
    );
    let mut universe = Universe::new(shared);
    let mut state = ExecutionState {
        exchange: HyperliquidExchangeLive {
            transport: HyperliquidTransport::new(network, &urls.rest).with_rate_gate(ip_gate),
            signer,
            nonce: NonceManager::new(),
            master: master_address,
        },
        registry,
        events,
        orders: HashMap::new(),
        builder,
        slippage,
        margin: MarginGuard::default(),
        ledger,
    };
    loop {
        match rx.recv() {
            Ok(ExecutionClientActorCommand::Submit(req, queued_at)) => {
                if refused_too_old(&state.events, vec![(*req).clone()], queued_at).is_empty() {
                    continue;
                }
                serving_submit(queued_at, || {
                    let now = universe.for_symbols(&state.exchange, &[req.symbol.as_str()]);
                    state.submit(now.symbology(), std::slice::from_ref(&*req));
                });
            }
            Ok(ExecutionClientActorCommand::SubmitBatch(reqs, queued_at)) => {
                let reqs = refused_too_old(&state.events, reqs, queued_at);
                if reqs.is_empty() {
                    continue;
                }
                let symbols: Vec<&str> = reqs.iter().map(|r| r.symbol.as_str()).collect();
                serving_submit(queued_at, || {
                    let now = universe.for_symbols(&state.exchange, &symbols);
                    state.submit(now.symbology(), &reqs);
                });
            }
            // This venue meters no cancel budget, so the intent is read by nobody here.
            // Each cancel and amend is marked while it is sent (`sending`): a panic inside leaves it of unknown outcome,
            // which the dead exec thread answers.
            Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. }) => {
                sending(Sending::Cancels(vec![order_client_id.clone()]), || {
                    state.cancel_any(universe.current().symbology(), &order_client_id)
                });
            }
            // The native batch (`with_bulk_cancel`): ONE `cancelByCloid` action of N items; an empty one sends nothing.
            Ok(ExecutionClientActorCommand::CancelBatch { order_client_ids, .. }) => {
                if !order_client_ids.is_empty() {
                    sending(Sending::Cancels(order_client_ids.clone()), || {
                        state.cancel(&order_client_ids)
                    });
                }
            }
            // No trigger amend is declared, so `new_trigger_price` is never set here. The amend's own stamp rides to
            // the last point before the venue (`ExecutionState::modify`), which refuses it there once it is too old.
            Ok(ExecutionClientActorCommand::Modify {
                order,
                new_qty,
                new_price,
                queued_at,
                ..
            }) => {
                sending(Sending::Amend(order.order_client_id.clone()), || {
                    let now = universe.for_symbols(&state.exchange, &[order.symbol.as_str()]);
                    state.modify(now.symbology(), &order, new_qty, new_price, queued_at)
                });
            }
            Ok(ExecutionClientActorCommand::Adopt(order)) => {
                let now = universe.for_symbols(&state.exchange, &[order.symbol.as_str()]);
                state.adopt(
                    now.symbology(),
                    &order.order_client_id,
                    &order.symbol,
                    taught_oid(&order),
                );
            }
            Ok(ExecutionClientActorCommand::Shutdown) | Err(_) => break,
        }
    }
    let _ = pump.shutdown();
}
