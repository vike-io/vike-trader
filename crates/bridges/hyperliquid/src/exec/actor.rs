//! The client's background threads: the funding-payment poll and the exec command loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};

use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;
use vike_model::now_ms;

use crate::base::config::Network;
use crate::base::consts::VENUE;
use crate::base::transport::HyperliquidTransport;
use crate::exec::user_data::spawn_hyperliquid_user_data;
use crate::feeds::funding::{HlFundingPoller, fetch_funding};
use crate::feeds::instruments::{HyperliquidInstruments, SharedInstruments};
use crate::signing::action::HlBuilderFee;
use crate::signing::{NonceManager, Signer};

use super::FUNDING_POLL_INTERVAL;
use super::margin::MarginGuard;
use super::registry::CloidRegistry;
use super::state::{ExecState, HlCommand, LiveExchange};
use super::universe::Universe;

#[cfg(doc)]
use super::client::HyperliquidExecutionClient;
#[cfg(doc)]
use super::market_slippage_for;

/// Spawn the funding-payment background poll thread: its OWN keyless [`HyperliquidTransport`]
/// (never shares the exec thread's signed transport — no signer is needed for `/info` reads),
/// polling [`FUNDING_POLL_INTERVAL`] until `stop` fires or the core goes away.
///
/// ⚠ The transport is its own; the per-IP REST **weight budget** is NOT, and `ip_gate` is how it
/// stops being. That budget is metered per IP rather than per client (see [`crate::base::ratelimit`]), so
/// the `/info` reads below and the exec thread's `/exchange` actions are spending the same minute
/// whether or not they are counting it together. The caller's handle is cloned in here; a
/// transport left on the fresh gate [`HyperliquidTransport::new`] mints would run a SECOND
/// full-budget window that neither side can see.
///
/// Mirrors the
/// bybit/okx REST-poll sourcing decision — see the module doc on why HL, despite exposing
/// `userFundings`-shaped ledger channels in principle, is polled here rather than added as a
/// third WS subscription: `crate::exec::user_data`'s pump only carries `orderUpdates` + `userFills`
/// today, and reusing the already-ported [`fetch_funding`] keeps this change scoped to the same
/// REST-poll shape every venue in this wave uses, instead of growing the WS protocol surface.
///
/// The poller is floored at THIS spawn instant (`now_ms()`) — NOT a trailing lookback, which
/// would re-fold up to that whole lookback of already-balance-embedded payments on every restart
/// (the double-count the restart law forbids; see [`HlFundingPoller`]'s doc).
///
/// CROSS-MOUNT HAZARD (for a future multi-mount-per-venue-account world): `userFunding` is
/// ACCOUNT-WIDE (master-address-scoped, every coin — deliberate, so every engine coin's payment
/// reaches the fold). If several HL perp starts ever share one master account, EACH start's
/// poller would emit EVERY coin's payments → N-fold duplication through the non-idempotent
/// `apply_funding`. Whoever adds multi-mount must dedupe this lane account-wide (one poller per
/// master address, not per start).
pub(super) fn spawn_funding_poll(
    network: Network,
    master_address: String,
    events: EventSender,
    ip_gate: RateGate,
) -> (Arc<AtomicBool>, JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = thread::Builder::new()
        .name("hyperliquid-funding-poll".to_string())
        .spawn(move || {
            let transport = HyperliquidTransport::new(network).with_rate_gate(ip_gate);
            // Floor = spawn instant: only payments landing WHILE started are folded.
            let mut poller = HlFundingPoller::new(
                |start, end| fetch_funding(&transport, &master_address, start, end),
                VENUE,
                now_ms(),
            );
            // Fetch failures retry each cadence, warned ONCE per consecutive-failure streak (a
            // 60s background thread, never the hot fold, so one warn per streak is bounded).
            let mut fail_streak = 0u32;
            while !stop_thread.load(Ordering::Relaxed) {
                match poller.poll(now_ms()) {
                    Err(e) => {
                        fail_streak += 1;
                        if fail_streak == 1 {
                            tracing::warn!(
                                target: "vike_hyperliquid",
                                error = %e,
                                "funding poll fetch failed; retrying each cadence (warns once per streak)"
                            );
                        }
                    }
                    Ok(evs) => {
                        fail_streak = 0;
                        for ev in evs {
                            if events.blocking_send(ev).is_err() {
                                return; // core gone — self-exit like every other pump thread
                            }
                        }
                    }
                }
                sleep_unless_stopped(&stop_thread, FUNDING_POLL_INTERVAL);
            }
        })
        .expect("spawn hyperliquid-funding-poll thread");
    (stop, join)
}

/// The exec thread body: build the live exchange + shared registry, start the private-WS pump, then
/// drain commands until `Shutdown`. ALL network I/O (signing, REST, WS) lives HERE, off the
/// single-writer core. On `Shutdown` the pump is stopped + joined before returning. `builder` (task
/// 7) is the configured Builder Code, carried into every submitted `order` action. `slippage` is the
/// mount-resolved emulated-market band ([`market_slippage_for`]), carried into [`ExecState`].
///
/// `ip_gate` is the caller's handle on the venue's ONE per-IP REST weight window, installed on this
/// thread's transport so the signed `/exchange` actions and the emulated-market `allMids` reads
/// charge the same budget as the funding poller and the start's reconcile transport. See
/// [`HyperliquidExecutionClient::spawn_with_market_slippage`] for who resolves it.
pub(super) fn run(
    signer: Signer,
    master_address: String,
    instruments: Arc<HyperliquidInstruments>,
    events: EventSender,
    rx: Receiver<HlCommand>,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    builder: Option<HlBuilderFee>,
    slippage: f64,
    ip_gate: RateGate,
    registry: CloidRegistry,
) {
    let network = signer.network();
    let ws_url = network.urls().2;
    // ONE universe, two readers: the pump remaps through it and this thread signs by it, and it is
    // replaced as a whole when an order names a coin listed after the start (`Universe`).
    let shared = SharedInstruments::new(instruments);
    // The private user-data pump: authoritative fills/cancels push straight into the core ingest,
    // remapping cloid→coid via the shared registry and coin→symbol via the shared instruments. On
    // reconnect it fires `recon_trigger` (when reconcile is started) to re-sync missed state.
    let pump = spawn_hyperliquid_user_data(
        ws_url.to_string(),
        master_address.clone(),
        shared.clone(),
        registry.clone(),
        events.clone(),
        recon_trigger,
    );
    let mut universe = Universe::new(shared);
    let mut state = ExecState {
        exchange: LiveExchange {
            transport: HyperliquidTransport::new(network).with_rate_gate(ip_gate),
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
    };
    loop {
        match rx.recv() {
            Ok(HlCommand::Submit(req)) => {
                let now = universe.for_symbols(&state.exchange, &[req.symbol.as_str()]);
                state.submit(now.symbology(), std::slice::from_ref(&*req));
            }
            Ok(HlCommand::SubmitBatch(reqs)) => {
                let symbols: Vec<&str> = reqs.iter().map(|r| r.symbol.as_str()).collect();
                let now = universe.for_symbols(&state.exchange, &symbols);
                state.submit(now.symbology(), &reqs);
            }
            Ok(HlCommand::Cancel(coid)) => {
                state.cancel_any(universe.current().symbology(), &coid);
            }
            Ok(HlCommand::CancelBatch(coids)) => state.cancel(&coids),
            Ok(HlCommand::Modify { order, new_qty, new_price }) => {
                let now = universe.for_symbols(&state.exchange, &[order.symbol.as_str()]);
                state.modify(now.symbology(), &order, new_qty, new_price);
            }
            Ok(HlCommand::Adopt { coid, symbol }) => {
                let now = universe.for_symbols(&state.exchange, &[symbol.as_str()]);
                state.adopt(now.symbology(), &coid, &symbol);
            }
            Ok(HlCommand::Shutdown) | Err(_) => break,
        }
    }
    let _ = pump.shutdown();
}
