//! The replay core's inert collaborators: the no-op client, the inert strategy and the stamp clock.

use std::collections::VecDeque;
use std::sync::Mutex;

use vike_exec::ExecutionClient;
use vike_model::{Clock, OrderRequest, Strategy};

use crate::LiveBroker;

/// A PURE no-op [`ExecutionClient`] for replay: it suppresses ALL venue side-effects and
/// synthesizes NO events. The original venue events are already in the journal as `Ingest::Event`
/// records and are re-pumped during replay; a client that ALSO emitted events would double the
/// state and (correctly) break the fence. This is the deliberate OPPOSITE of `TestExecutionClient`
/// (which DOES synthesize). `submit`/`cancel` are explicit no-ops; every other method (including
/// `poll_events`, the load-bearing one) uses the trait default, and the default `poll_events`
/// returns `None` — so no event is ever synthesized.
#[derive(Debug, Default)]
pub struct ReplayClient;

impl ExecutionClient for ReplayClient {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
    // modify / submit_batch / cancel_batch / detach / on_bar: trait defaults are already no-ops.
    // poll_events: default returns None -> NO synthesized events (the load-bearing property).
}

/// A [`Strategy`] with every hook at its trait default (no-op) — mounted by [`replay_from`] SOLELY
/// to mirror the base snapshot's `EngineSnapshot::collect_applied_fills` flag (see the mount site
/// below for why). Its hooks are never actually invoked: the tail pump only ever sends
/// `Ingest::Event`/`Ingest::Command` (see the loop below), never the Quote/Trade/Book/BarClose
/// messages that would dispatch to a mount — so mounting it has NO other observable effect on the
/// replayed state.
#[derive(Debug)]
pub(crate) struct InertStrategy;

impl Strategy<LiveBroker> for InertStrategy {}

/// A [`Clock`] that replays a recorded sequence of `now_ms` stamps — the tail commands' journaled
/// dispatch timestamps, in order. The core reads the clock exactly once per dispatched message
/// (`runtime.rs`: `engine.now_ms = clock.now_ms()`), so popping the front per call hands message
/// `k` its exact recorded stamp. Once only the last stamp remains it is PEEKED, not popped, so any
/// further reads (the exit snapshot's single clock read, which is not part of `state_hash`) repeat
/// the last value harmlessly. Interior-mutable (`Mutex`) because [`Clock::now_ms`] takes `&self`.
pub struct QueueClock {
    stamps: Mutex<VecDeque<i64>>,
}

impl QueueClock {
    /// Seed with the tail commands' recorded `now_ms`, in journal order.
    pub fn new(stamps: Vec<i64>) -> Self {
        QueueClock { stamps: Mutex::new(stamps.into()) }
    }
}

impl Clock for QueueClock {
    fn now_ms(&self) -> i64 {
        let mut q = self.stamps.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            // keep the last stamp resident so post-tail reads repeat it (empty ⇒ degenerate 0)
            q.front().copied().unwrap_or(0)
        }
    }
}
