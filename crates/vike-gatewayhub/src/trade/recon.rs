//! The RECONCILE worker: one thread per account that runs the lease's `Recon` requests against the
//! adapter's `ReconcileClient`, one at a time.
//!
//! # An empty answer is a claim
//!
//! The restore planner reads an empty order report as "nothing resting" and declares every in-scope
//! restored order gone, so the one thing this worker must never do is turn a fetch that did not happen
//! into an empty page. There are exactly three outcomes, and the client treats them differently:
//!
//! * **`ReconcileReply`** — the venue ANSWERED. A list kind may span pages; an empty answer is ONE page with
//!   no rows (so the client knows the answer was empty and not missing).
//! * **`ReconcileFailed{error}`** — the venue was asked and the fetch failed. An ERROR: it feeds the
//!   manager's staleness escalation.
//! * **`ReconcileNotReady{reason}`** — the pass was not attempted: no adapter, or an adapter with no
//!   reconcile handle (this module's caller answers that one, without a worker).
//!
//! # The symbols the client read
//!
//! Page 0 of a LIST answer (orders, fills, positions) names the admitted symbols the account's reconcile client
//! read for it ([`read_symbols`]: `ReconcileClient::admitted_symbols`, taken under the lock the fetch held), so the
//! node claims only what the daemon says was read instead of trusting its own mirror of the commits it sent
//! (`TradeDown::ReconcileReply::read`, feature `gateway_trade_reconcile_read`). A client that keeps no list names nothing.
//!
//! # A new lease
//!
//! A session outlives its lease, so one client answers every lease of its adapter's life and cannot see the
//! connection change. This worker can: every [`ReconcileWorkerJob::Fetch`] names the lease that asked (a link flap, a tradehub
//! restart and a takeover each get a new `TradeLeaseHold::id`) and the loop is one FIFO, so the first fetch of another lease
//! calls `ReconcileClient::lease_ended` before that fetch touches the venue, after everything the old lease queued.
//!
//! # Progress
//!
//! A fetch against a slow venue (ctrader's handshake, fxcm's 30 s calls) runs as long as it would in
//! process, and the client's per-frame bound is `max(floor, 3 x next_in_ms)`: the worker sends a
//! `ReconcileProgress` the moment a request is taken and again every `progress` until the answer is ready, so
//! a daemon that is alive is never called dead and one that is wedged is. The fetch itself runs on a
//! short-lived named thread so the worker (the progress clock) is never inside a venue call.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use vike_execution::recon::ReconcileClient;
use vike_gatewayhub_client::proto::{ReconcileKind, ReconcilePayload, TradeDown, pages_of};
use vike_model::AccountTier;

use super::lane::{TradeLane, TradeLaneTarget};
use super::lease::TradeLeaseHold;

/// One request for the worker.
pub enum ReconcileWorkerJob {
    /// A reconcile fetch.
    Fetch {
        /// The request id.
        rid: u64,
        /// What to fetch.
        kind: ReconcileKind,
        /// The lease that asked.
        to: Arc<TradeLeaseHold>,
    },
    /// Record the venue-confirmed account identity (`TradeLane::record_identity`).
    Identity {
        /// The request id.
        rid: u64,
        /// The lease that asked.
        to: Arc<TradeLeaseHold>,
    },
    /// Tell the reconcile client the account's engine admitted `symbol` (`ReconcileClient::admit_symbol`), so
    /// every pass queued after this one reads it too. Queued by the `SymbolFacts` hop, before its reply.
    Admit {
        /// The symbol, in the node's engine spelling.
        symbol: String,
    },
    /// Tell the reconcile client the account's engine GAVE `symbol` BACK (`ReconcileClient::forget_symbol`, decision
    /// 0205): every pass queued after this one stops reading it. Queued by the `SymbolFacts` hop with `forget`.
    Forget {
        /// The symbol, in the node's engine spelling.
        symbol: String,
    },
    /// End the loop after everything queued before this.
    Stop,
}

/// What the worker needs to answer.
pub struct ReconcileWorkerContext {
    /// The adapter's reconcile client, shared with the short-lived fetch threads.
    pub client: Arc<Mutex<Box<dyn ReconcileClient>>>,
    /// The lane (for `record_identity`).
    pub lane: Arc<dyn TradeLane>,
    /// The account.
    pub target: TradeLaneTarget,
    /// The tier the adapter bound.
    pub bound_tier: AccountTier,
    /// The `ReconcileProgress` cadence.
    pub progress: Duration,
    /// The byte target of one page.
    pub page_bytes: usize,
}

/// A running worker: send it jobs, stop it, join it.
pub struct ReconcileWorker {
    /// The job queue.
    pub tx: Sender<ReconcileWorkerJob>,
    /// The thread.
    pub handle: JoinHandle<()>,
}

/// Start the worker (thread `gw-trade-recon`).
pub fn spawn(ctx: ReconcileWorkerContext) -> std::io::Result<ReconcileWorker> {
    let (tx, rx) = mpsc::channel();
    let handle =
        std::thread::Builder::new().name("gw-trade-recon".into()).spawn(move || run(&rx, &ctx))?;
    Ok(ReconcileWorker { tx, handle })
}

fn run(rx: &Receiver<ReconcileWorkerJob>, ctx: &ReconcileWorkerContext) {
    let mut asked_by: Option<u64> = None;
    while let Ok(job) = rx.recv() {
        match job {
            ReconcileWorkerJob::Stop => break,
            ReconcileWorkerJob::Fetch { rid, kind, to } => {
                // As in `Admit`: no fetch is in flight here, and this never touches the network.
                if asked_by.replace(to.id).is_some_and(|last| last != to.id) {
                    ctx.client.lock().unwrap_or_else(PoisonError::into_inner).lease_ended();
                }
                fetch_job(ctx, rid, kind, &to);
            }
            ReconcileWorkerJob::Identity { rid, to } => identity_job(ctx, rid, &to),
            ReconcileWorkerJob::Admit { symbol } => {
                // No fetch is in flight here (the loop runs one job at a time and `fetch_job` waits for
                // its thread), and `admit_symbol` never touches the network.
                let guard = ctx.client.lock().unwrap_or_else(PoisonError::into_inner);
                guard.admit_symbol(&symbol);
            }
            ReconcileWorkerJob::Forget { symbol } => {
                // The twin of `Admit`: in the same FIFO, so a fetch queued before it still reads the symbol.
                let guard = ctx.client.lock().unwrap_or_else(PoisonError::into_inner);
                guard.forget_symbol(&symbol);
            }
        }
    }
}

fn fetch_job(
    ctx: &ReconcileWorkerContext,
    rid: u64,
    kind: ReconcileKind,
    to: &Arc<TradeLeaseHold>,
) {
    let next_in_ms = u64::try_from(ctx.progress.as_millis()).unwrap_or(u64::MAX);
    // Immediately: the client arms its first bound from the progress cadence.
    to.send(TradeDown::ReconcileProgress { rid, next_in_ms });
    let (tx, rx) = mpsc::channel();
    let client = Arc::clone(&ctx.client);
    let (lane, target) = (Arc::clone(&ctx.lane), ctx.target.clone());
    let spawned = std::thread::Builder::new().name("gw-trade-fetch".into()).spawn(move || {
        let guard = client.lock().unwrap_or_else(PoisonError::into_inner);
        let fetched = fetch_for(&**guard, kind, lane.as_ref(), &target);
        // The client's own word on which admitted symbols that fetch read, taken under the lock the fetch
        // held: the worker is one FIFO and `Admit` is a job of it, so no symbol can be told between the
        // fetch and this line.
        let read = fetched.as_ref().ok().and_then(|_| read_symbols(kind, &**guard));
        let _ = tx.send(fetched.map(|payload| (payload, read)));
    });
    if let Err(e) = spawned {
        to.send(TradeDown::ReconcileFailed {
            rid,
            error: format!("gatewayhub: could not start the reconcile fetch: {e}"),
        });
        return;
    }
    let result = loop {
        match rx.recv_timeout(ctx.progress) {
            Ok(result) => break result,
            Err(RecvTimeoutError::Timeout) => {
                to.send(TradeDown::ReconcileProgress { rid, next_in_ms });
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
            to.send(TradeDown::ReconcileFailed { rid, error });
        }
        Ok((payload, read)) => {
            let pages = pages_of_payload(payload, ctx.page_bytes);
            let last = pages.len().saturating_sub(1);
            // Named once, on page 0: the node takes the first page's word for the whole answer.
            let mut read = read;
            for (n, payload) in pages.into_iter().enumerate() {
                let page = u32::try_from(n).unwrap_or(u32::MAX);
                let read = read.take();
                if !to.send(TradeDown::ReconcileReply { rid, page, more: n < last, payload, read })
                {
                    break;
                }
            }
        }
    }
}

/// **The symbols the client's last fetch read beyond the mounted one** (`ReconcileClient::admitted_symbols`),
/// for a LIST kind (orders, fills, positions) only; `None` for a scalar kind, and for a client that keeps no
/// list (its reads span the whole account, or it ignores what it is told), which the node then judges by its
/// own table. This is what `TradeDown::ReconcileReply::read` carries (feature `gateway_trade_reconcile_read`).
pub fn read_symbols(kind: ReconcileKind, client: &dyn ReconcileClient) -> Option<Vec<String>> {
    match kind {
        ReconcileKind::Orders { .. } | ReconcileKind::Fills { .. } | ReconcileKind::Positions => {
            client.admitted_symbols()
        }
        ReconcileKind::Balance | ReconcileKind::FeeRates | ReconcileKind::AccountIdentity => None,
    }
}

fn identity_job(ctx: &ReconcileWorkerContext, rid: u64, to: &Arc<TradeLeaseHold>) {
    let guard = ctx.client.lock().unwrap_or_else(PoisonError::into_inner);
    let outcome = ctx.lane.record_identity(&ctx.target, ctx.bound_tier, Some(&**guard));
    drop(guard);
    let (ok, note) = match outcome {
        Ok(()) => (true, String::new()),
        Err(why) => (false, why),
    };
    to.send(TradeDown::IdentityRecorded { rid, ok, note });
}

/// [`fetch`] for the account `target` of `lane`: the balance of an account whose currency the lane only ASSUMED
/// is answered `None` ("not reported") without asking the venue ([`TradeLane::balance_withheld`]), so the node
/// folds no balance and raises no drift on a figure that may not be in the ledger's currency. Every other fetch,
/// and every account whose currency was stated, is [`fetch`] itself.
///
/// # Errors
/// As [`fetch`].
pub fn fetch_for(
    client: &dyn ReconcileClient,
    kind: ReconcileKind,
    lane: &dyn TradeLane,
    target: &TradeLaneTarget,
) -> Result<ReconcilePayload, String> {
    if matches!(kind, ReconcileKind::Balance)
        && let Some(why) = lane.balance_withheld(target)
    {
        tracing::debug!(venue = %target.venue, "{why}");
        return Ok(ReconcilePayload::Balance(None));
    }
    fetch(client, kind)
}

/// One fetch, as the wire's payload. `Err` is the venue's own error text.
pub fn fetch(
    client: &dyn ReconcileClient,
    kind: ReconcileKind,
) -> Result<ReconcilePayload, String> {
    Ok(match kind {
        ReconcileKind::Orders { since } => {
            ReconcilePayload::Orders(client.fetch_order_status_reports(since)?)
        }
        ReconcileKind::Fills { since } => {
            ReconcilePayload::Fills(client.fetch_fill_reports(since)?)
        }
        ReconcileKind::Positions => {
            ReconcilePayload::Positions(client.fetch_position_status_reports()?)
        }
        // The wire carries the venue's words alone (`TradeDown::ReconcileFailed`).
        ReconcileKind::Balance => {
            ReconcilePayload::Balance(client.fetch_balance().map_err(|e| e.text)?)
        }
        ReconcileKind::FeeRates => ReconcilePayload::FeeRates(client.fetch_fee_rates()?),
        ReconcileKind::AccountIdentity => {
            ReconcilePayload::AccountIdentity(client.fetch_account_identity()?)
        }
    })
}

/// The pages of one answer: a list kind is split under `page_bytes`; an EMPTY list and every scalar kind
/// is exactly one page. Never zero pages.
pub fn pages_of_payload(payload: ReconcilePayload, page_bytes: usize) -> Vec<ReconcilePayload> {
    fn list<T: Serialize>(
        rows: Vec<T>,
        page_bytes: usize,
        wrap: fn(Vec<T>) -> ReconcilePayload,
    ) -> Vec<ReconcilePayload> {
        if rows.is_empty() {
            return vec![wrap(rows)];
        }
        pages_of(rows, page_bytes).map(wrap).collect()
    }
    match payload {
        ReconcilePayload::Orders(rows) => list(rows, page_bytes, ReconcilePayload::Orders),
        ReconcilePayload::Fills(rows) => list(rows, page_bytes, ReconcilePayload::Fills),
        ReconcilePayload::Positions(rows) => list(rows, page_bytes, ReconcilePayload::Positions),
        scalar @ (ReconcilePayload::Balance(_)
        | ReconcilePayload::FeeRates(_)
        | ReconcilePayload::AccountIdentity(_)) => vec![scalar],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        balance: Result<Option<f64>, String>,
        orders: Vec<vike_model::OrderStatusReport>,
    }

    impl ReconcileClient for Scripted {
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
        fn fetch_balance(&self) -> Result<Option<f64>, vike_execution::recon::ReconcileFetchError> {
            self.balance.clone().map_err(vike_execution::recon::ReconcileFetchError::unclassified)
        }
    }

    #[test]
    fn a_failed_fetch_is_an_error_never_an_empty_answer() {
        let client = Scripted { balance: Ok(Some(12.5)), orders: Vec::new() };
        assert_eq!(
            fetch(&client, ReconcileKind::Balance),
            Ok(ReconcilePayload::Balance(Some(12.5)))
        );
        let err = fetch(&client, ReconcileKind::Fills { since: 0 }).expect_err("the venue failed");
        assert_eq!(err, "fills are down");
        // A scalar the venue does not surface is an ANSWER (`None`), distinct from a failure.
        assert_eq!(fetch(&client, ReconcileKind::FeeRates), Ok(ReconcilePayload::FeeRates(None)));
    }

    /// A client with a list it can name, and one with none.
    struct Listing(Option<Vec<String>>);

    impl ReconcileClient for Listing {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(
            &self,
        ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
            Ok(Vec::new())
        }
        fn admitted_symbols(&self) -> Option<Vec<String>> {
            self.0.clone()
        }
    }

    /// Only a LIST kind names what the client read, an empty list is a claim of its own, and a client that
    /// keeps no list names nothing (the node then judges the venue by its own table).
    #[test]
    fn only_a_list_kind_names_what_the_client_read() {
        let named = Listing(Some(vec!["ETHUSDT.P".to_string()]));
        let empty = Listing(Some(Vec::new()));
        let none = Listing(None);
        for kind in [
            ReconcileKind::Orders { since: 0 },
            ReconcileKind::Fills { since: 0 },
            ReconcileKind::Positions,
        ] {
            assert_eq!(read_symbols(kind, &named), Some(vec!["ETHUSDT.P".to_string()]), "{kind:?}");
            assert_eq!(
                read_symbols(kind, &empty),
                Some(Vec::new()),
                "{kind:?}: a claim of nothing"
            );
            assert_eq!(read_symbols(kind, &none), None, "{kind:?}: no list");
        }
        for kind in
            [ReconcileKind::Balance, ReconcileKind::FeeRates, ReconcileKind::AccountIdentity]
        {
            assert_eq!(read_symbols(kind, &named), None, "{kind:?}: a scalar names nothing");
        }
    }

    #[test]
    fn an_empty_list_is_one_empty_page_never_zero_pages() {
        let pages = pages_of_payload(ReconcilePayload::Orders(Vec::new()), 1024);
        assert_eq!(pages, vec![ReconcilePayload::Orders(Vec::new())]);
        let scalar = pages_of_payload(ReconcilePayload::Balance(None), 1024);
        assert_eq!(scalar, vec![ReconcilePayload::Balance(None)]);
    }
}
