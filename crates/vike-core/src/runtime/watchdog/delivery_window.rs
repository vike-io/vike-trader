//! **THE DELIVERY-WINDOW SWEEP** (the owner's ruling of 2026-11-22; decisions 0232 and 0233's amendments): when a
//! dated contract's delivery window starts, OUR OWN resting OPENING orders on it are cancelled.
//!
//! The venue delivers a dated contract at its expiry and the engine books no delivery settlement, so inside the
//! window no order may OPEN a position on it: each bridge refuses an opening order there at send and a reduce-only
//! one is taken until the expiry (`vike_venues::venue_delivery_guard`'s module doc, okx's rule for every venue). An
//! order that was RESTING before the window started would still fill inside it, so this sweep pulls it.
//!
//! - **The one verdict.** Per engine, the venue's row (`delivery_guard_for`, skipped at once when it has no window);
//!   per live order, the contract's expiry read off its ENGINE NAME (`DeliveryGuardRow::expiry_of`: decision 0290's
//!   `_YYMMDD`, or a deribit option's own `DDMMMYY`, at the venue's time of day), then `verdict(expiry, now, false)`.
//!   `OpeningInsideWindow` and `Delivered` cancel; nobody computes an expiry another way.
//! - **What it never touches.** A REDUCE-ONLY order (it is taken until the expiry, so no position is trapped at the
//!   delivery); a FOREIGN order (one this node did not place is no registry entry: a reconcile pass reports it as an
//!   `UnknownOrder` divergence, and only our previous session's orders are adopted); a perpetual, a spot pair, an FX
//!   pair or a combo (their names state no day); a stopped account's engine (its client is detaching); and what the
//!   core HOLDS rather than rests at a venue (an emulated parent, a core-held arm, a held release): those reach a
//!   bridge only when they fire, and its send-time verdict refuses an opening one there as an `OrderRejected`, which
//!   ends an emulated parent (`crates/vike-execution/src/emulated/chase.rs` and `iceberg.rs`).
//! - **An adopted order whose reduce-only flag nobody remembers is reported, never cancelled.** A venue report carries
//!   no such flag, so an adopted order's comes from its ownership line or the journal (`runtime/restore.rs`), and a
//!   `false` one is not written: an ownership line from before the flag (2026-10-24) reads exactly like an opening
//!   order. The lifetime field is younger (2026-11-06) and is written even at its default, so an adopted order whose
//!   lifetime no memory stated (`OrderRecord::lifetime_unknown`) is the one whose `false` may be a MISSING flag: it is
//!   left resting, with one WARN and one ring line, the owner's safe default (an operator decides; a reduce-only stop
//!   cancelled at its delivery would leave the position unprotected).
//! - **Fire-once, journal-neutral.** A cancel goes engine-directly (`Engine::cancel_order` + `pump_client`, the GTD
//!   sweep's shape and its `Unspecified` intent: fire-once, so never `Routine`, which a venue may shed) and the id is
//!   kept in `CoreThread::delivery_canceled` until it is no longer live, so a venue slow to answer is not asked twice.
//!   `cancel_order` publishes nothing locally; the venue's `OrderCanceled` is journaled as any event is, and the risk
//!   clock already writes the waker records, so the sweep adds no journal record.
//! - **Cold.** It rides the risk clock ([`CoreThread::sweep_risk_clock`]'s last stage), never the fold: per engine a
//!   string match, per live order a name read; nothing allocates until an order is due, and each cancel is ONE WARN
//!   and one ring line. A core with no risk clock (`CoreConfig::price_only` unset: every replay core and the paper
//!   daemon) is not swept.

use vike_venues::venue_delivery_guard::{DeliveryGuardVerdict, delivery_guard_for};

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// One pass of the delivery-window sweep at `now` (the module doc).
    #[cold]
    #[inline(never)]
    pub(crate) fn sweep_delivery_window(&mut self, now: i64) {
        // (engine index, order client id, symbol, ms left to the expiry, whether its reduce-only flag is unknown):
        // collected first, so the walk borrows the registries alone; empty, and never allocated, while nothing is due.
        let mut due: Vec<(usize, String, String, i64, bool)> = Vec::new();
        for index in 0..=self.extra_engines.len() {
            if self.is_retired(index) {
                continue;
            }
            let engine = self.engine(index);
            let row = delivery_guard_for(&engine.venue);
            if row.window_ms.is_none() {
                continue;
            }
            for (order_client_id, order) in engine.registry.live() {
                if is_terminal_status(order.status)
                    || order.request.reduce_only
                    || self.delivery_canceled.contains(order_client_id)
                    || self.delivery_unknown_reported.contains(order_client_id)
                {
                    continue;
                }
                let Some(expiry) = row.expiry_of(&order.request.symbol) else { continue };
                let left_ms = match row.verdict(expiry, now, false) {
                    DeliveryGuardVerdict::Taken => continue,
                    DeliveryGuardVerdict::OpeningInsideWindow { left_ms } => left_ms,
                    DeliveryGuardVerdict::Delivered => 0,
                };
                let flag_unknown = order.lifetime_unknown;
                due.push((
                    index,
                    order_client_id.clone(),
                    order.request.symbol.clone(),
                    left_ms,
                    flag_unknown,
                ));
            }
        }
        if !due.is_empty() {
            let mut cancelled = false;
            for (index, order_client_id, symbol, left_ms, flag_unknown) in due {
                let route_key = self.engine(index).route_key.clone();
                if flag_unknown {
                    tracing::warn!(
                        target: "vike_core::watchdog",
                        %route_key,
                        %order_client_id,
                        %symbol,
                        left_ms,
                        "DELIVERY WINDOW: an adopted order whose reduce-only flag nobody remembers is NOT cancelled"
                    );
                    self.note(format!(
                        "DELIVERY WINDOW on {route_key}: the adopted order {order_client_id} on {symbol} rests {} h \
                         before its delivery and nothing remembers whether it only reduces, so it is NOT cancelled: \
                         cancel it by hand if it opens a position",
                        left_ms / vike_model::MS_PER_HOUR
                    ));
                    self.delivery_unknown_reported.insert(order_client_id);
                    continue;
                }
                tracing::warn!(
                    target: "vike_core::watchdog",
                    %route_key,
                    %order_client_id,
                    %symbol,
                    left_ms,
                    "DELIVERY WINDOW: cancelling our resting opening order"
                );
                self.note(format!(
                    "DELIVERY WINDOW on {route_key}: cancelling our resting opening order {order_client_id} on \
                     {symbol}, {} h before its delivery (a reduce-only order stays)",
                    left_ms / vike_model::MS_PER_HOUR
                ));
                self.delivery_canceled.insert(order_client_id.clone());
                self.engine_mut(index).cancel_order(&order_client_id);
                cancelled = true;
            }
            if cancelled {
                self.pump_client();
            }
            self.dirty = true;
        }
        // Self-cleaning: an id leaves once no registry holds it live (the cancel landed, or it ended otherwise).
        if !self.delivery_canceled.is_empty() || !self.delivery_unknown_reported.is_empty() {
            let mut canceled = std::mem::take(&mut self.delivery_canceled);
            let mut reported = std::mem::take(&mut self.delivery_unknown_reported);
            canceled.retain(|order_client_id| self.holds_live(order_client_id));
            reported.retain(|order_client_id| self.holds_live(order_client_id));
            self.delivery_canceled = canceled;
            self.delivery_unknown_reported = reported;
        }
    }

    /// Whether some registry holds `order_client_id` as a live (non-terminal) order.
    fn holds_live(&self, order_client_id: &str) -> bool {
        (0..=self.extra_engines.len()).any(|index| {
            self.engine(index)
                .registry
                .get(order_client_id)
                .is_some_and(|order| !is_terminal_status(order.status))
        })
    }
}
