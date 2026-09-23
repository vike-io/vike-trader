//! Alpaca exec — `ExecutionClient` over `/v1/trading/accounts/{id}/orders`. One dedicated REST
//! thread (`ExecActor`); `submit`/`cancel` enqueue. `OrderSubmitted` is emitted synchronously; the
//! POST returns Accepted (fills come back async on the SSE stream — a 2nd reader thread, see
//! `crate::stream`). Alpaca cancels by venue order id (not client id), so `cancel` resolves the
//! venue id with a fresh `orders:by_client_order_id` lookup then DELETEs it — leak-free (no growing
//! in-memory map) and restart-safe (a map would be lost on restart; a lookup isn't). `modify` has no
//! venue-native amend on Broker orders, so it stays the `ExecutionClient` default no-op
//! (cancel/replace is deferred).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::thread;

use vike_bridge_core::exec_actor::{
    CancelOutcome, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderRejected, OrderSubmitted};

use crate::auth::TokenSource;
use crate::config::AlpacaConfig;
use crate::event_mapper::{build_order_body, map_order_response};
use crate::rest::AlpacaRest;

/// Live Alpaca exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking); every
/// venue event returns through the core ingest. Dropping it stops both threads (command + SSE).
pub struct AlpacaExecutionClient(ExecActor);

impl AlpacaExecutionClient {
    pub fn spawn(config: AlpacaConfig, events: EventSender) -> Self {
        let token = Arc::new(TokenSource::new(
            config.client_id.clone(),
            config.client_secret.clone(),
            config.hosts.authx.to_string(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let actor = ExecActor::spawn("alpaca-exec", events.clone(), {
            let (config, events, token) = (config.clone(), events.clone(), token.clone());
            move |rx| run(config, token, events, rx)
        });
        // Background thread: the SSE trade-events stream (fills the order POST can't carry
        // inline). ExecActor flag-stops + joins it on teardown (deterministic).
        let stream_join = thread::Builder::new()
            .name("alpaca-events".into())
            .spawn({
                let stop = stop.clone();
                move || crate::stream::stream_trade_events(config, token, events, stop)
            })
            .expect("spawn alpaca-events thread");
        Self(actor.with_background(stop, stream_join))
    }
}

impl ExecutionClient for AlpacaExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

fn run(
    config: AlpacaConfig,
    token: Arc<TokenSource>,
    events: EventSender,
    rx: Receiver<ExecCommand>,
) {
    let rest = AlpacaRest::new(token);
    let base = config.hosts.broker;
    let acct = config.account_id.clone();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let path = format!("/v1/trading/accounts/{acct}/orders");
                match rest.post_json(base, &path, &build_order_body(&req)) {
                    Ok(resp) => {
                        for ev in map_order_response(&req.client_order_id, req.ts, &resp) {
                            let _ = events.blocking_send(ev);
                        }
                    }
                    Err(e) => {
                        // `e` is a transport/HTTP-status error (never carries the Bearer, which
                        // ureq/AlpacaRest never places into an error's Display) — safe to log.
                        tracing::warn!(
                            venue = "alpaca",
                            client_order_id = %req.client_order_id,
                            error = %e,
                            "order submit failed"
                        );
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            client_order_id: req.client_order_id.clone(),
                            reason: e.to_string().into(),
                            ts: req.ts,
                        }));
                    }
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                // Alpaca cancels by venue order id, not client id — resolve it with a fresh
                // `orders:by_client_order_id` lookup, then DELETE. Unknown/gone → non-terminal
                // reject (audit A2: never silently swallow a failed cancel).
                let lookup = format!("/v1/trading/accounts/{acct}/orders:by_client_order_id");
                let vid = rest
                    .get(base, &lookup, &format!("client_order_id={coid}"))
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string));
                let outcome = match vid {
                    None => CancelOutcome::Rejected(format!("unknown order {coid}")),
                    Some(vid) => {
                        let path = format!("/v1/trading/accounts/{acct}/orders/{vid}");
                        match rest.delete(base, &path) {
                            Ok(_) => CancelOutcome::Canceled,
                            Err(e) => CancelOutcome::Rejected(e.to_string()),
                        }
                    }
                };
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            // Broker orders have no in-place amend; cancel/replace is a deferred refinement.
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}
