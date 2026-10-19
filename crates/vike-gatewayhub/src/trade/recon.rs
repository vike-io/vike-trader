//! The RECONCILE worker: one thread per account that runs the lease's `Recon` requests against the
//! adapter's `ReconClient`, one at a time.
//!
//! # An empty answer is a claim
//!
//! The restore planner reads an empty order report as "nothing resting" and declares every in-scope
//! restored order gone, so the one thing this worker must never do is turn a fetch that did not happen
//! into an empty page. There are exactly three outcomes, and the client treats them differently:
//!
//! * **`ReconReply`** — the venue ANSWERED. A list kind may span pages; an empty answer is ONE page with
//!   no rows (so the client knows the answer was empty and not missing).
//! * **`ReconFailed{error}`** — the venue was asked and the fetch failed. An ERROR: it feeds the
//!   manager's staleness escalation.
//! * **`ReconNotReady{reason}`** — the pass was not attempted: no adapter, or an adapter with no
//!   reconcile handle (this module's caller answers that one, without a worker).
//!
//! # Progress
//!
//! A fetch against a slow venue (ctrader's handshake, fxcm's 30 s calls) runs as long as it would in
//! process, and the client's per-frame bound is `max(floor, 3 x next_in_ms)`: the worker sends a
//! `ReconProgress` the moment a request is taken and again every `progress` until the answer is ready, so
//! a daemon that is alive is never called dead and one that is wedged is. The fetch itself runs on a
//! short-lived named thread so the worker (the progress clock) is never inside a venue call.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto::{
    ReconKind, ReconPayload, TradeDown, WireFeeSchedule, pages_of,
};
use vike_model::AccountTier;

use super::lane::{TradeLane, TradeTarget};
use super::lease::Lease;

/// One request for the worker.
pub enum ReconJob {
    /// A reconcile fetch.
    Fetch {
        /// The request id.
        rid: u64,
        /// What to fetch.
        kind: ReconKind,
        /// The lease that asked.
        to: Arc<Lease>,
    },
    /// Record the venue-confirmed account identity (`TradeLane::record_identity`).
    Identity {
        /// The request id.
        rid: u64,
        /// The lease that asked.
        to: Arc<Lease>,
    },
    /// Tell the reconcile client the account's engine admitted `symbol` (`ReconClient::admit_symbol`), so
    /// every pass queued after this one reads it too. Queued by the `SymbolFacts` hop, before its reply.
    Admit {
        /// The symbol, in the node's engine spelling.
        symbol: String,
    },
    /// End the loop after everything queued before this.
    Stop,
}

/// What the worker needs to answer.
pub struct ReconCtx {
    /// The adapter's reconcile client, shared with the short-lived fetch threads.
    pub client: Arc<Mutex<Box<dyn ReconClient>>>,
    /// The lane (for `record_identity`).
    pub lane: Arc<dyn TradeLane>,
    /// The account.
    pub target: TradeTarget,
    /// The tier the adapter bound.
    pub bound_tier: AccountTier,
    /// The `ReconProgress` cadence.
    pub progress: Duration,
    /// The byte target of one page.
    pub page_bytes: usize,
}

/// A running worker: send it jobs, stop it, join it.
pub struct ReconWorker {
    /// The job queue.
    pub tx: Sender<ReconJob>,
    /// The thread.
    pub handle: JoinHandle<()>,
}

/// Start the worker (thread `gwh-trade-recon`).
pub fn spawn(ctx: ReconCtx) -> std::io::Result<ReconWorker> {
    let (tx, rx) = mpsc::channel();
    let handle =
        std::thread::Builder::new().name("gwh-trade-recon".into()).spawn(move || run(&rx, &ctx))?;
    Ok(ReconWorker { tx, handle })
}

fn run(rx: &Receiver<ReconJob>, ctx: &ReconCtx) {
    while let Ok(job) = rx.recv() {
        match job {
            ReconJob::Stop => break,
            ReconJob::Fetch { rid, kind, to } => fetch_job(ctx, rid, kind, &to),
            ReconJob::Identity { rid, to } => identity_job(ctx, rid, &to),
            ReconJob::Admit { symbol } => {
                // No fetch is in flight here (the loop runs one job at a time and `fetch_job` waits for
                // its thread), and `admit_symbol` never touches the network.
                let guard = ctx.client.lock().unwrap_or_else(PoisonError::into_inner);
                guard.admit_symbol(&symbol);
            }
        }
    }
}

fn fetch_job(ctx: &ReconCtx, rid: u64, kind: ReconKind, to: &Arc<Lease>) {
    let next_in_ms = u64::try_from(ctx.progress.as_millis()).unwrap_or(u64::MAX);
    // Immediately: the client arms its first bound from the progress cadence.
    to.send(TradeDown::ReconProgress { rid, next_in_ms });
    let (tx, rx) = mpsc::channel();
    let client = Arc::clone(&ctx.client);
    let spawned = std::thread::Builder::new().name("gwh-trade-fetch".into()).spawn(move || {
        let guard = client.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = tx.send(fetch(&**guard, kind));
    });
    if let Err(e) = spawned {
        to.send(TradeDown::ReconFailed {
            rid,
            error: format!("gatewayhub: could not start the reconcile fetch: {e}"),
        });
        return;
    }
    let result = loop {
        match rx.recv_timeout(ctx.progress) {
            Ok(result) => break result,
            Err(RecvTimeoutError::Timeout) => {
                to.send(TradeDown::ReconProgress { rid, next_in_ms });
            }
            Err(RecvTimeoutError::Disconnected) => {
                break Err(
                    "gatewayhub: the reconcile fetch thread ended without an answer".to_string()
                );
            }
        }
    };
    match result {
        Err(error) => {
            to.send(TradeDown::ReconFailed { rid, error });
        }
        Ok(payload) => {
            let pages = pages_of_payload(payload, ctx.page_bytes);
            let last = pages.len().saturating_sub(1);
            for (n, payload) in pages.into_iter().enumerate() {
                let page = u32::try_from(n).unwrap_or(u32::MAX);
                if !to.send(TradeDown::ReconReply { rid, page, more: n < last, payload }) {
                    break;
                }
            }
        }
    }
}

fn identity_job(ctx: &ReconCtx, rid: u64, to: &Arc<Lease>) {
    let guard = ctx.client.lock().unwrap_or_else(PoisonError::into_inner);
    let outcome = ctx.lane.record_identity(&ctx.target, ctx.bound_tier, Some(&**guard));
    drop(guard);
    let (ok, note) = match outcome {
        Ok(()) => (true, String::new()),
        Err(why) => (false, why),
    };
    to.send(TradeDown::IdentityRecorded { rid, ok, note });
}

/// One fetch, as the wire's payload. `Err` is the venue's own error text.
pub fn fetch(client: &dyn ReconClient, kind: ReconKind) -> Result<ReconPayload, String> {
    Ok(match kind {
        ReconKind::Orders { since } => {
            ReconPayload::Orders(client.fetch_order_status_reports(since)?)
        }
        ReconKind::Fills { since } => ReconPayload::Fills(client.fetch_fill_reports(since)?),
        ReconKind::Positions => ReconPayload::Positions(client.fetch_position_status_reports()?),
        ReconKind::Balance => ReconPayload::Balance(client.fetch_balance()?),
        ReconKind::FeeRates => {
            ReconPayload::FeeRates(client.fetch_fee_rates()?.map(WireFeeSchedule::from))
        }
        ReconKind::AccountIdentity => {
            ReconPayload::AccountIdentity(client.fetch_account_identity()?)
        }
    })
}

/// The pages of one answer: a list kind is split under `page_bytes`; an EMPTY list and every scalar kind
/// is exactly one page. Never zero pages.
pub fn pages_of_payload(payload: ReconPayload, page_bytes: usize) -> Vec<ReconPayload> {
    fn list<T: Serialize>(
        rows: Vec<T>,
        page_bytes: usize,
        wrap: fn(Vec<T>) -> ReconPayload,
    ) -> Vec<ReconPayload> {
        if rows.is_empty() {
            return vec![wrap(rows)];
        }
        pages_of(rows, page_bytes).map(wrap).collect()
    }
    match payload {
        ReconPayload::Orders(rows) => list(rows, page_bytes, ReconPayload::Orders),
        ReconPayload::Fills(rows) => list(rows, page_bytes, ReconPayload::Fills),
        ReconPayload::Positions(rows) => list(rows, page_bytes, ReconPayload::Positions),
        scalar @ (ReconPayload::Balance(_)
        | ReconPayload::FeeRates(_)
        | ReconPayload::AccountIdentity(_)) => vec![scalar],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        balance: Result<Option<f64>, String>,
        orders: Vec<vike_model::OrderStatusReport>,
    }

    impl ReconClient for Scripted {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
            Ok(self.orders.clone())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
            Err("fills are down".to_string())
        }
        fn fetch_position_status_reports(
            &self,
        ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_balance(&self) -> Result<Option<f64>, String> {
            self.balance.clone()
        }
    }

    #[test]
    fn a_failed_fetch_is_an_error_never_an_empty_answer() {
        let client = Scripted { balance: Ok(Some(12.5)), orders: Vec::new() };
        assert_eq!(fetch(&client, ReconKind::Balance), Ok(ReconPayload::Balance(Some(12.5))));
        let err = fetch(&client, ReconKind::Fills { since: 0 }).expect_err("the venue failed");
        assert_eq!(err, "fills are down");
        // A scalar the venue does not surface is an ANSWER (`None`), distinct from a failure.
        assert_eq!(fetch(&client, ReconKind::FeeRates), Ok(ReconPayload::FeeRates(None)));
    }

    #[test]
    fn an_empty_list_is_one_empty_page_never_zero_pages() {
        let pages = pages_of_payload(ReconPayload::Orders(Vec::new()), 1024);
        assert_eq!(pages, vec![ReconPayload::Orders(Vec::new())]);
        let scalar = pages_of_payload(ReconPayload::Balance(None), 1024);
        assert_eq!(scalar, vec![ReconPayload::Balance(None)]);
    }
}
