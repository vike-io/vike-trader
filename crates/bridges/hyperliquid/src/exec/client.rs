//! The public exec client: a newtype over the exec thread's command lane, with its own HALT copy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, JoinHandle};

use vike_bridge_core::exec_actor::{CancelOutcome, cancel_event};
use vike_bridge_core::ratelimit::RateGate;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};

use crate::exec::event_mapper::cloid_from_client_order_id;
use crate::feeds::instruments::HyperliquidInstruments;
use crate::signing::Signer;
use crate::signing::action::HlBuilderFee;

use super::DEFAULT_MARKET_SLIPPAGE;
use super::actor::{run, spawn_funding_poll};
use super::registry::CloidRegistry;
use super::state::HlCommand;

#[cfg(doc)]
use super::market_slippage_for;

/// Live Hyperliquid exec client. `submit`/`cancel`/`modify` (and their batch twins) enqueue onto ONE
/// dedicated OS thread that drives the signed `/exchange` REST off the single-writer core; every venue
/// event returns through the core ingest (the `/exchange` response synchronously, fills/cancels via the
/// WS pump). Dropping it (or `detach`) sends `Shutdown` and JOINS the thread — which itself tears down
/// the owned user-data pump — for deterministic teardown. Also owns the funding-poll background
/// thread (its OWN keyless transport, joined the same way).
pub struct HyperliquidExecutionClient {
    pub(super) tx: Sender<HlCommand>,
    pub(super) events: EventSender,
    pub(super) join: Option<JoinHandle<()>>,
    pub(super) funding_stop: Arc<AtomicBool>,
    pub(super) funding_join: Option<JoinHandle<()>>,
    /// A handle on the cloid registry the exec thread and the WS pump share, created BEFORE the exec
    /// thread spawns so `adopt_restored` can seed it synchronously from the fold thread (a restart
    /// empties it; the exec thread would otherwise be the only writer).
    pub(super) registry: CloidRegistry,
    /// The operator HALT sentinel this client watches — handed in by `crate::adapter` from
    /// `AdapterInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test, a tool) watches NO file: this client used to ask the process-wide resolver
    /// (`vike_tradehub_core::halt::halt_path_from_env`) at every submit, a bridge reading a
    /// process-global path at the moment an order arrived — the thing the start contract (decision
    /// 0096, rule 1) forbids and decision 0099 removed. A client with no path says so once at
    /// `error` (`vike_exec::halt::sentinel_engaged`), because a start that forgot to hand it
    /// one is a dead kill switch.
    pub(super) halt_path: Option<std::path::PathBuf>,
}

impl HyperliquidExecutionClient {
    /// Hand this client the HALT sentinel it watches: `crate::adapter` passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process.
    /// Mirrors `vike_bridge_core::exec_actor::ExecActor::with_halt_path`; without it the client
    /// watches nothing (see the field's doc). This is the constructor seam the halt arms below were
    /// declared untestable without: `exec_tests.rs` builds a client over a dead command channel and
    /// drives them with a path of its own.
    #[must_use]
    pub fn with_halt_path(mut self, path: std::path::PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// Spawn the exec thread. `signer` is the (agent or master) L1 signer — its network selects the
    /// hosts + phantom-agent source; `master_address` is the MASTER account address the WS pump
    /// subscribes with (agent-wallet queries return empty — research §8); `instruments` is the loaded
    /// symbology + grid (shared read-only with the pump for coin⇄symbol remapping). `recon_trigger`
    /// (when a reconcile driver is started) is fired by the pump on every WS reconnect so a pass
    /// re-syncs any order/fill state that drifted while the socket was down; `None` ⇒ no trigger. The
    /// thread spawns the private-WS pump, then drains commands until `Shutdown`. `builder` (task 7)
    /// is the configured Builder Code, resolved once by the caller (this crate's own
    /// `start::live_runtime_for_account`, via `attribution_code_from(vars, "hyperliquid")` + the configured
    /// fee) — `None` (every non-configured start) rides every submit byte-identically to before this
    /// field existed.
    ///
    /// Prices emulated market orders at [`DEFAULT_MARKET_SLIPPAGE`] — the historical literal. A
    /// caller with an operator band calls [`Self::spawn_with_market_slippage`] instead.
    ///
    /// ⚠ It also opens its OWN per-IP REST weight window ([`crate::base::ratelimit::ip_weight_gate`]),
    /// which is honest only for a process whose sole Hyperliquid REST caller is this client. A live
    /// start has three (exec, funding poll, recon) and must NOT reach the venue through three
    /// budgets it publishes one of — so it resolves the gate itself and calls
    /// [`Self::spawn_with_market_slippage`] with the handle.
    pub fn spawn(
        signer: Signer,
        master_address: String,
        instruments: Arc<HyperliquidInstruments>,
        events: EventSender,
        recon_trigger: Option<std::sync::mpsc::Sender<()>>,
        builder: Option<HlBuilderFee>,
    ) -> Self {
        Self::spawn_with_market_slippage(
            signer,
            master_address,
            instruments,
            events,
            recon_trigger,
            builder,
            DEFAULT_MARKET_SLIPPAGE,
            crate::base::ratelimit::ip_weight_gate(),
        )
    }

    /// [`Self::spawn`] with the emulated-market slippage band supplied by the caller.
    ///
    /// `market_slippage` is the band the composition root resolved ONCE via [`market_slippage_for`]
    /// — the same "resolve at the start, never re-read from a spawned thread" idiom the perp arms'
    /// leverage, the arming ceiling and the attribution codes already follow. It bounds BOTH
    /// aggressive-price sites: the emulated market order (off the mid) and the protective
    /// stop-MARKET limit (off the trigger).
    ///
    /// Pass [`DEFAULT_MARKET_SLIPPAGE`] — what [`Self::spawn`] does — for the historical behaviour.
    /// The value is used verbatim, so callers must resolve it through [`market_slippage_for`] rather
    /// than passing a raw number: that is where the bound lives.
    ///
    /// `ip_gate` is the venue's per-IP REST weight window, resolved ONCE by this crate's own
    /// live-mount seam (`crate::adapter::live_runtime_for_account`, itself called from `crate::adapter`'s
    /// `HyperliquidVenueAdapter::start`) and cloned into BOTH REST paths this client owns: the exec
    /// thread's signed `/exchange` + its `allMids` reads, and the funding poller's `/info`. The
    /// caller keeps a
    /// handle for the reconcile transport it builds beside them,
    /// so all three spend one budget. Hyperliquid meters that budget per IP — see
    /// [`crate::base::ratelimit`] — so a client that minted its own would not be conservative, it would
    /// be a second full window invisible to the first, on a row
    /// (`vike_venues::venue_rate_limits::HYPERLIQUID`'s `rest_ip_weight`) that admits exactly what
    /// the venue publishes and holds nothing back to absorb it.
    pub fn spawn_with_market_slippage(
        signer: Signer,
        master_address: String,
        instruments: Arc<HyperliquidInstruments>,
        events: EventSender,
        recon_trigger: Option<std::sync::mpsc::Sender<()>>,
        builder: Option<HlBuilderFee>,
        market_slippage: f64,
        ip_gate: RateGate,
    ) -> Self {
        let (tx, rx) = channel();
        let run_events = events.clone();
        // Read the network BEFORE `signer` moves into the exec-thread closure below (`Network` is
        // `Copy`) — the funding poll needs it for its own keyless transport, but not the signer
        // itself (an `/info` read carries no signature).
        let network = signer.network();
        // Both REST paths below ride the caller's ONE per-IP weight window: the poller takes a
        // clone, the exec thread takes the handle itself.
        let (funding_stop, funding_join) =
            spawn_funding_poll(network, master_address.clone(), events.clone(), ip_gate.clone());
        let registry = CloidRegistry::new();
        let run_registry = registry.clone();
        let join = thread::Builder::new()
            .name("hyperliquid-exec".to_string())
            .spawn(move || {
                // Opt-in HFT pinning (VIKE_PIN_CORES=exec:N): keep the venue exec thread on a fixed
                // core. No-op unless the env names the `exec` role — the default desktop path.
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::Exec,
                    "hyperliquid-exec",
                );
                run(
                    signer,
                    master_address,
                    instruments,
                    run_events,
                    rx,
                    recon_trigger,
                    builder,
                    market_slippage,
                    ip_gate,
                    run_registry,
                );
            })
            .expect("spawn hyperliquid exec thread");
        Self {
            tx,
            events,
            join: Some(join),
            funding_stop,
            funding_join: Some(funding_join),
            registry,
            halt_path: None,
        }
    }

    /// Signal every thread this client owns to wind down, and JOIN NOTHING — the first half of
    /// [`Self::stop`], hoisted for the same reason `ExecActor::signal_stop` was
    /// (`crates/vike-bridge-core/src/exec_actor.rs`): it backs
    /// `ExecutionClient::begin_detach`, so a core holding several venues raises every flag before
    /// it joins the first. Idempotent — the send is `let _ =` and a flag store is a store.
    fn signal_stop(&mut self) {
        self.funding_stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(HlCommand::Shutdown);
    }

    fn stop(&mut self) {
        // Signal everything first, THEN join, so the command + background threads wind down
        // together (matches `ExecActor::stop`'s ordering).
        self.signal_stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let Some(j) = self.funding_join.take() {
            let _ = j.join();
        }
    }

    /// True when the HALT sentinel file this client was handed currently exists → new order
    /// placement must be refused. Consulted only at the submit/modify boundary (submits are
    /// infrequent), never on a hot path — the shared kill-switch an operator `touch`es over ssh even
    /// when the runtime is wedged.
    fn halt_engaged(&self) -> bool {
        vike_exec::halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// Send a submit(-batch) command; if the exec thread is dead, synthesize the terminal
    /// [`OrderRejected`] the contract requires for every order (never let an intent vanish).
    fn submit_or_reject(&self, cmd: HlCommand, coids: &[&str], ts: i64) {
        if self.tx.send(cmd).is_err() {
            for coid in coids {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: (*coid).to_string(),
                    reason: "venue exec thread unavailable".into(),
                    ts,
                }));
            }
        }
    }

    /// Synthesize a HALT reject for every coid (the submit boundary is closed while HALT is engaged).
    fn reject_halted(&self, coids: &[&str], ts: i64) {
        for coid in coids {
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                client_order_id: (*coid).to_string(),
                reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                ts,
            }));
        }
    }
}

impl ExecutionClient for HyperliquidExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        // A REDUCING submit is admitted so a halt never traps the operator in a position — the rule
        // itself lives in `vike_exec::halt::halt_admits_submit`, shared with the `ExecActor`
        // copy of this sentinel precisely so the two cannot disagree about what a halt lets out.
        if self.halt_engaged() && !vike_exec::halt::halt_admits_submit(request) {
            self.reject_halted(&[&request.client_order_id], request.ts);
            return;
        }
        self.submit_or_reject(
            HlCommand::Submit(Box::new(request.clone())),
            &[&request.client_order_id],
            request.ts,
        );
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        if requests.is_empty() {
            return;
        }
        let coids: Vec<&str> = requests.iter().map(|r| r.client_order_id.as_str()).collect();
        let ts = requests[0].ts;
        // ALL-OR-NOTHING, and stricter than the per-order arm above on purpose. This is ONE native
        // venue batch: it is submitted or refused as a unit, so a batch mixing reducing and opening
        // legs cannot be "partly admitted" without splitting it into a different request than the
        // caller built. Requiring EVERY leg to reduce keeps the guarantee that no opening order
        // reaches the venue under a halt, at the cost of refusing a mixed batch whole — which the
        // caller sees as a terminal rejection per coid, never as a silent partial send.
        if self.halt_engaged() && !requests.iter().all(vike_exec::halt::halt_admits_submit) {
            self.reject_halted(&coids, ts);
            return;
        }
        self.submit_or_reject(HlCommand::SubmitBatch(requests.to_vec()), &coids, ts);
    }

    fn cancel(&mut self, client_order_id: &str) {
        // Cancel is NEVER halt-gated — an operator must be able to reduce/exit under HALT.
        if self.tx.send(HlCommand::Cancel(client_order_id.to_string())).is_err() {
            let _ = self.events.blocking_send(cancel_event(
                client_order_id,
                CancelOutcome::Rejected("venue exec thread unavailable".to_string()),
            ));
        }
    }

    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        if client_order_ids.is_empty() {
            return;
        }
        if self.tx.send(HlCommand::CancelBatch(client_order_ids.to_vec())).is_err() {
            for c in client_order_ids {
                let _ = self.events.blocking_send(cancel_event(
                    c,
                    CancelOutcome::Rejected("venue exec thread unavailable".to_string()),
                ));
            }
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        // Conservative: HALT blocks modify (it can add size / chase price and reduce-only can't be
        // inferred here). Non-terminal like a dead channel — the resting order keeps its terms.
        //
        // ⚠ It used to return SILENTLY, which made a halted modify indistinguishable from a lost
        // one (`docs/ops/kill-switches.md` gap 5). The verdict is unchanged; the advisory is the
        // same non-terminal event, carrying the same shared reason, that this client already emits
        // for an unknown coid and a dead command channel — and that `ExecActor`, the paper book and
        // cTrader now all emit here too.
        //
        // This client's halt arms — this one and the two in `submit`/`submit_batch` — are driven by
        // `exec_tests.rs`'s `a_hyperliquid_client_watches_the_sentinel_it_was_handed`: the
        // `with_halt_path` seam made it constructible over a dead command channel, which closed the
        // residual this comment used to declare (no test, because a client could not be built
        // without a `Signer` and a dialled private-WS pump).
        if self.halt_engaged() {
            let _ = self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: vike_exec::halt::HALT_REJECT_REASON.into(),
                ts: order.ts,
            }));
            return;
        }
        let _ =
            self.tx.send(HlCommand::Modify { order: Box::new(order.clone()), new_qty, new_price });
    }

    /// Phase-one teardown seam — see `crates/vike-exec/src/execution_engine/client.rs`'s
    /// `ExecutionClient::begin_detach`. This client owns its threads directly rather than through
    /// an `ExecActor`, so it wires the seam itself.
    fn begin_detach(&mut self) {
        self.signal_stop();
    }
    fn detach(&mut self) {
        self.stop();
    }

    /// The restore seam's translation half (decision 0121): HL's reports carry `keccak(coid)` and
    /// this is the one function that computes it, so the core can read a report after a restart.
    /// Pure and stateless — a fresh client with an empty registry answers it.
    fn wire_id_for(&self, coid: &str) -> Option<String> {
        Some(cloid_from_client_order_id(coid))
    }

    /// The restore seam's state half: seed the shared registry NOW (a WS `orderUpdates` or fill of a
    /// restored order resolves the moment this returns) and queue an `Adopt` per order for the exec
    /// thread's asset map — on the SAME lane as `cancel`, so a cancel sent after this one finds it.
    /// `symbol` is the unified symbol (`"BTC"`, `"HYPE/USDC"`); the exec thread resolves the asset.
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        for (coid, symbol) in orders {
            self.registry.register_restored(coid);
            let _ = self.tx.send(HlCommand::Adopt { coid: coid.clone(), symbol: symbol.clone() });
        }
    }
}

impl Drop for HyperliquidExecutionClient {
    fn drop(&mut self) {
        self.stop();
    }
}
