//! The symbol each order was SUBMITTED under, so its fills carry it.
//!
//! The order path spells `EURUSD` as `EUR_USD` (`vike_catalog::to_oanda_instrument`) and every OANDA
//! transaction names the instrument that way. `vike_exec::ExecutionEngine` compares symbols by exact
//! string and DROPS a bare `Fill` whose symbol it neither mounts nor registered the order under, with no
//! log and no counter: the order's lifecycle wrap advanced and the position never moved. So a fill of an
//! order this process submitted is stamped with the symbol of the ORDER, whichever spelling the
//! submitter used (`EURUSD` and `EUR_USD` both book, each under its own name). The same shape as
//! `crates/bridges/vike-ibkr/src/exec/event_mapper.rs`'s `register_symbol`.
//!
//! One table, shared by the exec thread (records at the submit), the transactions-stream thread and the
//! confirm worker (both read it). A fill of a coid nobody recorded (another client or the web platform
//! on the same account, an order placed before a restart that the host did not teach through
//! `ExecutionClient::adopt_restored`) keeps the venue's own spelling: there is nothing to relabel it by.
//!
//! It also carries the one other thing those two threads share about an order: **whether its submit has
//! been answered**. The exec thread publishes `Submitted`, then `Accepted` (and a market order's fill)
//! from the POST's reply; the stream publishes the same fill from the transaction line, and OANDA writes
//! that line before the reply is read, so the stream's fill routinely reaches the engine while the order
//! is still `Submitted`, where the FSM refuses it. [`SubmittedSymbols::hold`] marks the order from the
//! exec thread, [`SubmittedSymbols::park`] keeps the stream's events for it back, and
//! [`SubmittedSymbols::release`] hands them on once the exec thread has published its own.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_model::events::Event;

/// The most orders [`SubmittedSymbols`] holds. Entries leave with their order ([`SubmittedSymbols::settle`]),
/// so this is a leak guard: past it a new order is not recorded and its fills carry the venue's spelling
/// (loudly). Nothing is evicted to make room, because a live order's entry is the only thing that books
/// its fill.
const MAX_RECORDED: usize = 4096;

/// The most stream events [`SubmittedSymbols::park`] keeps back for one unanswered submit. Past it an
/// event goes out at once, out of order, rather than waiting: a lost fill is worse than an early one.
const MAX_PARKED: usize = 256;

/// coid -> the symbol the order was submitted under. Cloning shares the one table.
#[derive(Clone)]
pub struct SubmittedSymbols {
    table: Arc<Mutex<HashMap<String, String>>>,
    /// coid -> the stream events kept back while its submit is unanswered (see the module doc). Its own
    /// lock, never held across a send: the core thread takes `table` (through `record`) and must never
    /// wait behind a thread that is blocked on a full channel.
    unanswered: Arc<Mutex<HashMap<String, Vec<Event>>>>,
    cap: usize,
}

impl Default for SubmittedSymbols {
    fn default() -> Self {
        Self::with_capacity(MAX_RECORDED)
    }
}

/// The order an event the stream decode produces belongs to.
fn stream_coid(ev: &Event) -> Option<&str> {
    match ev {
        Event::Fill(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        _ => None,
    }
}

impl SubmittedSymbols {
    fn with_capacity(cap: usize) -> Self {
        Self { table: Arc::default(), unanswered: Arc::default(), cap }
    }

    fn table(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn unanswered(&self) -> MutexGuard<'_, HashMap<String, Vec<Event>>> {
        self.unanswered.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The exec thread is about to POST `coid`'s order: from here the stream's events for it wait
    /// ([`Self::park`]) until [`Self::release`]. Called BEFORE the POST, so no line of the order can be
    /// handled first.
    pub(crate) fn hold(&self, coid: &str) {
        self.unanswered().entry(coid.to_string()).or_default();
    }

    /// `Some(ev)`: publish it now. `None`: `ev` belongs to an order whose submit is unanswered and was
    /// kept back, to be handed to [`Self::release`]'s sender after the exec thread's own events.
    pub(crate) fn park(&self, ev: Event) -> Option<Event> {
        let Some(coid) = stream_coid(&ev) else { return Some(ev) };
        let mut unanswered = self.unanswered();
        match unanswered.get_mut(coid) {
            Some(parked) if parked.len() < MAX_PARKED => {
                parked.push(ev);
                None
            }
            _ => Some(ev),
        }
    }

    /// The submit of `coid` is answered (its events are published, or it produced none): hand every
    /// event [`Self::park`] kept to `send`, in the order it arrived, and stop holding. An event that is
    /// parked WHILE `send` runs is picked up by the next round, and the order stops being held only
    /// once a round finds nothing, so the stream can never publish an event of this order ahead of one
    /// that was kept back.
    pub(crate) fn release(&self, coid: &str, mut send: impl FnMut(Vec<Event>)) {
        loop {
            let batch = {
                let mut unanswered = self.unanswered();
                match unanswered.get_mut(coid).map(std::mem::take) {
                    Some(batch) if !batch.is_empty() => batch,
                    _ => {
                        unanswered.remove(coid);
                        return;
                    }
                }
            };
            send(batch);
        }
    }

    /// Remember that `coid` was submitted (or, for a restored order, taught) under `symbol`.
    pub(crate) fn record(&self, coid: &str, symbol: &str) {
        if coid.is_empty() || symbol.is_empty() {
            return;
        }
        let mut table = self.table();
        if table.len() >= self.cap && !table.contains_key(coid) {
            tracing::warn!(
                coid,
                cap = self.cap,
                "oanda: submitted-symbol table full - this order's fills will carry the venue's \
                 spelling of the instrument, which an engine mounted on another spelling drops"
            );
            return;
        }
        table.insert(coid.to_string(), symbol.to_string());
    }

    /// The symbol a fill of `coid` carries: the one the order was submitted under, else the venue's own
    /// spelling `venue_symbol` (an order nobody recorded).
    pub(crate) fn label(&self, coid: &str, venue_symbol: &str) -> String {
        self.table().get(coid).map_or_else(|| venue_symbol.to_string(), Clone::clone)
    }

    /// Drop what is kept for every order `events` ends (filled, cancelled, rejected). Called where the
    /// events are published, AFTER they were built: a duplicate of a fill (the stream carries a market
    /// order's fill the order POST already returned) then falls back to the venue's spelling, which the
    /// engine drops, and its `trade_id` dedup would have dropped it anyway.
    pub(crate) fn settle(&self, events: &[Event]) {
        let mut table = self.table();
        for ev in events {
            let coid = match ev {
                Event::OrderFilled(e) => &e.client_order_id,
                Event::OrderCanceled(e) => &e.client_order_id,
                Event::OrderRejected(e) => &e.client_order_id,
                _ => continue,
            };
            table.remove(coid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{OrderAccepted, OrderCanceled};

    fn canceled(coid: &str) -> Event {
        canceled_for(coid, "")
    }

    /// A stream-shaped event of `coid`, told apart from its siblings by `reason`.
    fn canceled_for(coid: &str, reason: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts: 0,
        })
    }

    fn reasons(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::OrderCanceled(c) => c.reason.to_string(),
                other => panic!("not a stream event: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_recorded_coid_keeps_its_symbol_and_an_unknown_one_the_venue_spelling() {
        let s = SubmittedSymbols::default();
        s.record("c1", "EURUSD");
        s.record("c2", "EUR_USD");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD");
        assert_eq!(s.label("c2", "EUR_USD"), "EUR_USD");
        assert_eq!(s.label("foreign", "EUR_USD"), "EUR_USD");
        assert_eq!(s.label("", "EUR_USD"), "EUR_USD");
    }

    #[test]
    fn a_clone_shares_the_table() {
        let s = SubmittedSymbols::default();
        s.clone().record("c1", "EURUSD");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD");
    }

    /// Only an order that ended leaves; a full table is never cleared to make room.
    #[test]
    fn the_table_is_bounded_and_only_a_finished_order_leaves_it() {
        let s = SubmittedSymbols::with_capacity(2);
        s.record("c1", "EURUSD");
        s.record("c2", "GBPUSD");
        s.record("c3", "USDJPY");
        assert_eq!(s.label("c3", "USD_JPY"), "USD_JPY", "past the cap: not recorded");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD", "and nothing was evicted for it");
        s.record("c1", "EURUSD"); // re-recording a known coid is not growth
        s.settle(&[canceled("c1"), canceled("never-recorded")]);
        assert_eq!(s.label("c1", "EUR_USD"), "EUR_USD", "the cancelled order left");
        assert_eq!(s.label("c2", "GBP_USD"), "GBPUSD", "a live order stayed");
        s.record("c3", "USDJPY");
        assert_eq!(s.label("c3", "USD_JPY"), "USDJPY", "the freed slot took the new order");
    }

    /// The stream's events of an order whose submit is unanswered wait; anyone else's, and anything
    /// that is not a stream event, go out at once; and once released the order is not held any more.
    #[test]
    fn a_held_orders_stream_events_wait_and_come_out_in_order_on_release() {
        let s = SubmittedSymbols::default();
        assert!(s.park(canceled_for("c1", "before")).is_some(), "nothing is held yet");
        s.hold("c1");
        assert!(s.park(canceled_for("c1", "first")).is_none());
        assert!(s.park(canceled_for("c1", "second")).is_none());
        assert!(s.park(canceled_for("c2", "other order")).is_some());
        let accepted = Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".to_string(),
            venue_order_id: None,
            ts: 0,
        });
        assert!(s.park(accepted).is_some(), "only the stream's own kinds are held");

        let mut batches = Vec::new();
        s.release("c1", |batch| batches.push(reasons(&batch)));
        assert_eq!(batches, vec![vec!["first", "second"]]);
        assert!(s.park(canceled_for("c1", "after")).is_some(), "released: no longer held");
        s.release("never-held", |_| panic!("nothing to hand on"));
    }

    /// An event parked WHILE a batch is being sent is sent after it, not ahead of it: the order stops
    /// being held only once a round finds nothing left.
    #[test]
    fn an_event_parked_during_the_release_is_not_overtaken() {
        let s = SubmittedSymbols::default();
        s.hold("c1");
        assert!(s.park(canceled_for("c1", "first")).is_none());
        let mut batches = Vec::new();
        s.release("c1", |batch| {
            if batches.is_empty() {
                assert!(s.park(canceled_for("c1", "late")).is_none(), "still held mid-release");
            }
            batches.push(reasons(&batch));
        });
        assert_eq!(batches, vec![vec!["first"], vec!["late"]]);
        assert!(s.park(canceled_for("c1", "after")).is_some());
    }

    /// A submit that never answers cannot make the parked queue grow without bound, and the overflow
    /// goes out rather than being lost.
    #[test]
    fn the_parked_queue_is_bounded_and_the_overflow_goes_out() {
        let s = SubmittedSymbols::default();
        s.hold("c1");
        for _ in 0..MAX_PARKED {
            assert!(s.park(canceled_for("c1", "x")).is_none());
        }
        assert!(s.park(canceled_for("c1", "overflow")).is_some());
    }
}
