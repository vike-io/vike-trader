//! OANDA exec half — `ExecutionClient` over the v20 orders API.
//!
//! Like FXCM, order I/O runs on ONE dedicated thread (blocking REST off the single-writer core);
//! `submit`/`cancel` enqueue, and venue events return through the ingest via [`EventSender`].
//! Orders carry `clientExtensions.id = client_order_id`, so cancel targets `@{client_order_id}`
//! (no id-tracking map needed). MARKET fills come back inline on the POST response; delayed
//! LIMIT/STOP fills + cancels arrive on the transactions stream (a 2nd reader thread — see
//! [`crate::stream`]). Audit A3: a fill/cancel that lands during a transactions-stream reconnect
//! is recovered by backfilling `/transactions/sinceid` on re-open — see [`crate::stream`]. The
//! shared `last_seen` watermark is advanced here (from each POST's `lastTransactionID`) and by the
//! stream (from each transaction `id`), so a resting-order fill during an early reconnect — before
//! any stream transaction — is still bounded to the right floor, not backfilled from account start.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::{
    CancelOutcome, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderFilled, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::{OrderRequest, TimeInForce};

use crate::config::OandaConfig;
use crate::data::to_oanda_instrument;
use crate::rest::OandaRest;

const VENUE: &str = "oanda";

/// Live OANDA exec client. `submit`/`cancel` enqueue onto the REST thread (non-blocking); every
/// venue event returns through the core ingest. Dropping it stops both threads.
pub struct OandaExecutionClient(ExecActor);

impl OandaExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's mount passes
    /// `MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/bridge_inputs_gate.rs` holds that
    /// every live mount calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    pub fn spawn(config: OandaConfig, events: EventSender) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // Shared audit-A3 watermark: the highest transaction id seen on EITHER the POST path
        // (below) or the transactions stream. The stream backfills `/transactions/sinceid` from
        // this on reconnect. 0 = nothing seen yet (skip backfill).
        let last_seen = Arc::new(AtomicU64::new(0));
        let actor = ExecActor::spawn("oanda-exec", events.clone(), {
            let (config, events, last_seen) = (config.clone(), events.clone(), last_seen.clone());
            move |rx| run(config, events, rx, last_seen)
        });
        // Background thread: the transactions stream (delayed LIMIT/STOP fills + cancels the order
        // POST can't carry). ExecActor flag-stops + joins it on teardown (deterministic).
        let stream_join = thread::Builder::new()
            .name("oanda-stream".into())
            .spawn({
                let stop = stop.clone();
                move || crate::stream::stream_transactions(config, events, stop, last_seen)
            })
            .expect("spawn oanda-stream thread");
        Self(actor.with_background(stop, stream_join))
    }
}

impl ExecutionClient for OandaExecutionClient {
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

/// Format a price for OANDA (string). NOTE: OANDA enforces per-instrument precision; 5 dp suits
/// the FX majors but JPY/metals differ — precise per-instrument rounding is a refinement.
fn fmt_price(p: f64) -> String {
    format!("{p:.5}")
}

/// Map a vike `TimeInForce` to OANDA's `timeInForce` string.
///
/// The working-order arm consumes this venue's row of the ONE cross-venue TIF authority
/// ([`vike_model::venues::venue_tif::venue_tif`]) — a future TIF flip is a one-line row edit there.
/// The MARKET arm stays local BY DESIGN: OANDA MARKET orders accept only FOK/IOC, so every
/// non-Ioc TIF coerces to FOK — a genuine venue constraint on the market path, documented as
/// a divergence in `vike_model::venues::venue_tif`'s module doc rather than modeled in the
/// resting-path table.
fn oanda_tif(tif: TimeInForce, is_market: bool) -> &'static str {
    if is_market {
        match tif {
            TimeInForce::Ioc => "IOC",
            _ => "FOK",
        }
    } else {
        vike_model::venues::venue_tif::venue_tif(VENUE, tif).wire().unwrap_or("GTC")
    }
}

/// Build the OANDA order-request body from a vike `OrderRequest`.
// `pub` (the `exec` module itself stays private; re-exported `#[doc(hidden)]` at the crate root)
// so `tests/*.rs` integration tests can exercise the real request-body construction directly
// instead of re-deriving it — last resort per the "prefer testing through the public caller"
// rule (there is no public caller: the only call site is the network-calling `run()` below).
// Not venue API.
pub fn build_order_body(req: &OrderRequest) -> serde_json::Value {
    let instrument = to_oanda_instrument(&req.symbol);
    let sign = if req.side >= 0 { 1 } else { -1 };
    let units = (req.qty.round() as i64) * sign; // OANDA units: signed base-currency amount
    let mut order = json!({
        "instrument": instrument,
        "units": units.to_string(),
        "clientExtensions": { "id": req.client_order_id },
    });
    let market = !(req.order_type == "limit" || req.order_type == "stop");
    order["timeInForce"] = json!(oanda_tif(req.time_in_force, market));
    match req.order_type.as_str() {
        "limit" => {
            order["type"] = json!("LIMIT");
            if let Some(p) = req.price {
                order["price"] = json!(fmt_price(p));
            }
        }
        "stop" => {
            order["type"] = json!("STOP");
            if let Some(p) = req.trigger_price {
                order["price"] = json!(fmt_price(p));
            }
        }
        _ => {
            order["type"] = json!("MARKET");
        }
    }
    // OANDA GTD needs an explicit gtdTime (epoch seconds, per Accept-Datetime-Format: UNIX).
    if !market
        && req.time_in_force == TimeInForce::Gtd
        && let Some(exp) = req.gtd_expiry
    {
        order["gtdTime"] = json!((exp / 1000).to_string());
    }
    json!({ "order": order })
}

/// Parse an OANDA `orderFillTransaction` into a vike [`FillEvent`].
///
/// `None` when the transaction carries no `id` — the same refusal, for the same reason, as
/// [`crate::stream::fill_from_transaction`] (which see: OANDA's transaction `id` is this venue's
/// only per-fill identity, so there is nothing to synthesize a replay-stable id from).
///
/// ⚠ This function is a near-duplicate of that one — a law spelled twice, differing only in taking
/// `coid` from the request instead of the transaction's `clientExtensions`. Both had the identical
/// `unwrap_or_default()` defect and both are fixed here; MERGING them is a separate change (the coid
/// provenance genuinely differs) and is deliberately not attempted on a type-change PR.
fn parse_fill(coid: &str, fill: &serde_json::Value) -> Option<FillEvent> {
    let s = |k: &str| fill.get(k).and_then(|v| v.as_str());
    let units: f64 = s("units").and_then(|u| u.parse().ok()).unwrap_or(0.0);
    let price: f64 = s("price").and_then(|p| p.parse().ok()).unwrap_or(0.0);
    let commission: f64 = s("commission").and_then(|c| c.parse().ok()).unwrap_or(0.0);
    let ts = s("time").and_then(|t| t.parse::<f64>().ok()).map_or(0, |secs| (secs * 1000.0) as i64);
    let trade_id = match TradeId::new(s("id").unwrap_or_default()) {
        Ok(t) => t,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                %coid,
                "orderFillTransaction carries no `id` — dropping the fill rather than folding an \
                 un-dedupable one"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: coid.to_string(),
        venue: VENUE.to_string().into(),
        symbol: s("instrument").unwrap_or_default().to_string().into(),
        side: if units >= 0.0 { 1 } else { -1 },
        last_qty: units.abs(),
        last_px: price,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    })
}

/// Map an OANDA order-POST response to the vike events it implies (Accepted [+ Filled], or Rejected).
// `pub` (re-exported `#[doc(hidden)]`) — same rationale as `build_order_body`: the only
// production caller is `run()`'s network path, so integration tests need direct access to prove
// the dual-publish / reject-short-circuit contract without a live REST double.
pub fn map_order_response(coid: &str, ts: i64, resp: &serde_json::Value) -> Vec<Event> {
    let mut out = Vec::new();
    if let Some(rej) = resp.get("orderRejectTransaction") {
        let reason = rej.get("rejectReason").and_then(|r| r.as_str()).unwrap_or("rejected");
        out.push(Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts,
        }));
        return out;
    }
    if let Some(create) = resp.get("orderCreateTransaction") {
        out.push(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: create.get("id").and_then(|i| i.as_str()).map(Into::into),
            ts,
        }));
    }
    if let Some(fill) = resp.get("orderFillTransaction") {
        // Dual-publish (crypto contract): bare Fill first so the Account folds position/PnL,
        // then the OrderFilled wrap so the FSM applies it — both the same FillEvent.
        // Bare Fill and wrap stand or fall together — an id-less fill publishes NEITHER (the
        // OrderAccepted above still stands: the order genuinely was accepted).
        if let Some(fill) = parse_fill(coid, fill) {
            out.push(Event::Fill(fill.clone()));
            out.push(Event::OrderFilled(OrderFilled {
                client_order_id: coid.to_string(),
                fill,
                ts,
            }));
        }
    }
    out
}

/// Advance the shared A3 watermark from an order-POST response's `lastTransactionID`.
// `pub` (re-exported `#[doc(hidden)]`) — the exec-side half of the A3 watermark contract;
// promoted so its monotonic-max / missing-field behavior is integration-testable directly.
pub fn note_last_transaction_id(last_seen: &Arc<AtomicU64>, resp: &serde_json::Value) {
    if let Some(id) =
        resp.get("lastTransactionID").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok())
    {
        last_seen.fetch_max(id, Ordering::Relaxed);
    }
}

fn run(
    config: OandaConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    last_seen: Arc<AtomicU64>,
) {
    let rest = OandaRest::new(config.api_token.clone());
    let (acct, base) = (config.account_id.clone(), config.rest_base.clone());

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let body = build_order_body(&req);
                let path = format!("/v3/accounts/{acct}/orders");
                match rest.post_json(&base, &path, &body) {
                    Ok(resp) => {
                        // A3: advance the shared watermark so a reconnect backfills from AFTER
                        // this order's transactions, not from account start.
                        note_last_transaction_id(&last_seen, &resp);
                        for ev in map_order_response(&req.client_order_id, req.ts, &resp) {
                            let _ = events.blocking_send(ev);
                        }
                    }
                    Err(e) => {
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            client_order_id: req.client_order_id.clone(),
                            reason: e.to_string().into(),
                            ts: req.ts,
                        }));
                    }
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                // A failed/ambiguous cancel must not vanish (audit A2): map every outcome to an
                // event. A genuine venue-side cancel still confirms via the transactions stream.
                let path = format!("/v3/accounts/{acct}/orders/@{coid}/cancel");
                let outcome = match rest.put_empty(&base, &path) {
                    Ok(resp) if resp.get("orderCancelTransaction").is_some() => {
                        CancelOutcome::Canceled
                    }
                    Ok(_) => {
                        CancelOutcome::Rejected(format!("no orderCancelTransaction for {coid}"))
                    }
                    Err(e) => CancelOutcome::Rejected(e.to_string()),
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
            // no native amend on this venue: a modify leaves the resting order at its terms
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
