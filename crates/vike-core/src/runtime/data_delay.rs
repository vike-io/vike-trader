//! The DELAY LABEL's two writes, its read and its stamp (the Decision section of
//! `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, item 2: an order may be
//! priced off delayed data).
//!
//! A feed reports a stream's label (real-time, delayed, frozen) only when it is first known and
//! when it changes; it arrives here as `Ingest::DataDelay`, an occasional control message, never on
//! the per-tick path. [`CoreThread::note_data_delay`] files it twice: in the `(venue, symbol)`
//! table the submit path stamps `OrderRequest::data_delay` from, and on every mount of that pair,
//! whose strategy dispatches COPY it into `LiveBroker::data_delay`.
//!
//! # ⚠ One labelling stream per symbol
//!
//! The table is keyed `(venue, symbol)` and the update's `stream` is dropped: a symbol carries ONE
//! label, the last any of its streams reported. That is exact while each symbol has one labelling
//! stream, which is today's whole tree (the IBKR socket feed labels its `"quotes"` lane and nothing
//! else). A second labelling stream on one symbol (say a bars lane served at another entitlement)
//! would make the label last-writer-wins between them. Keying by stream would not cure that
//! cheaply: an order is priced off a SYMBOL, so the submit stamp would have to pick a stream, and the
//! one lookup per submit would become a choice the core cannot make. The day a second labelling
//! stream appears, the rule for which one an order is priced off is decided first.
//!
//! # Where the label is recorded, and where it is not
//!
//! [`CoreThread::stamp_data_delay`] runs after an order PASSES pre-trade and records the label on the
//! order (the in-memory registry record, hence snapshots and the published `OrderView` the GUI
//! shows) and, when the label is not real-time, writes ONE note per order naming its coid, symbol
//! and label (the core's recent-events line). The journal does NOT carry it: the stamp follows the
//! minted-submit write and `Ingest::DataDelay` is not journaled, so a replay re-folds every order
//! unlabelled; recording it there would split the journal from what a replay reproduces
//! (`vike_exec::state_hash` leaves the label out for the same reason). So the boot restore and the
//! materialized order history carry no label.
//!
//! ⚠ The label is information, never a gate. Nothing here refuses, holds, re-routes or re-prices an
//! order, and nothing blocks arming a strategy. Nothing is logged per MESSAGE: the per-order note is
//! per order, and the one line per delayed stream is the feed side's, `vike_mount::CoreLaneSink`'s.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The label `(venue, symbol)` last reported, or `None` when it reported none. Allocation-free:
    /// two borrowed lookups, and one `is_empty` branch on a core whose feeds label nothing.
    pub(crate) fn delay_of(&self, venue: &str, symbol: &str) -> Option<vike_model::DataDelay> {
        if self.data_delays.is_empty() {
            return None;
        }
        self.data_delays.get(venue)?.get(symbol).copied()
    }

    /// File one label change: the submit path's table, then every mount on exactly that
    /// `(venue, symbol)` (the mount's OWN pair; a declared leg's label is not carried). The
    /// update's stream is not a key (module doc, "One labelling stream per symbol").
    pub(crate) fn note_data_delay(
        &mut self,
        venue: &str,
        symbol: &str,
        delay: vike_model::DataDelay,
    ) {
        self.data_delays.entry(venue.to_string()).or_default().insert(symbol.to_string(), delay);
        for (slot, (v, s)) in self.mount_delays.iter_mut().zip(self.mount_vs.iter()) {
            if v == venue && s == symbol {
                *slot = Some(delay);
            }
        }
    }

    /// Record the label of an order that PASSED pre-trade (module doc, "Where the label is
    /// recorded"): the core's table is the authority, so a caller's value is overwritten, and an
    /// order priced off data that is not real-time gets its one note. Never refuses or holds. On a
    /// core whose feeds label nothing this is one `is_empty` branch and allocates nothing.
    pub(crate) fn stamp_data_delay(&mut self, req: &mut OrderRequest) {
        req.data_delay = self.delay_of(&req.venue, &req.symbol);
        if let Some(delay) = req.data_delay
            && !delay.is_real_time()
        {
            self.note(format!(
                "PRE-TRADE {} {}: priced off {} data (recorded with the order; not a gate)",
                req.client_order_id,
                req.symbol,
                delay.word()
            ));
        }
    }
}
