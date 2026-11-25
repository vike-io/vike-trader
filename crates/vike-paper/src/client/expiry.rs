//! Time-in-force deadlines: the `OrderExpiry` state and the `expire_due` sweep (GTD/DAY only).

use vike_model::TimeInForce;
use vike_model::events::{Event, OrderExpired};

use super::ExecutionClientPaper;

/// Time-in-force expiry state for one resting order (kept OUT of `OrderResting` so the fill
/// primitives stay TIF-blind, exactly like [`Contingency`]).
///
/// Recorded ONLY for orders whose TIF is expiring — `Gtc` (the default everywhere) inserts
/// nothing, so the resting book, the fill pass and the r7 gate are byte-identical when the
/// feature is unused.
///
/// **Session concept:** this crate has no exchange-session calendar (no venue trading hours are
/// modeled anywhere in vike-backtest), so `Day` is scoped to the **UTC calendar day** — and the
/// day it is scoped to is the day of the FIRST BAR the order sees, never `OrderRequest.ts`.
/// Nothing in the write path stamps `request.ts` (`Engine::submit_order` takes `now` as a
/// separate argument and never writes it back, and `OrderRequest::ts` defaults to `0`), so
/// anchoring on it would make an ordinary `..Default::default()` request born expired — `utc_day(0)`
/// is 1970 while a live bar is ~20_650. Anchoring on the bar clock also removes any submitter-vs-bar
/// clock skew (the classic seconds-vs-ms mixup) from the deadline, and stays replay-deterministic
/// because the bar stream is.
///
/// **Not handled here — `Ioc`/`Fok` are NOT enforced and rest exactly like `Gtc`.** They are
/// immediate-execution semantics, not a resting deadline: enforcing them means changing the FILL
/// pass (fill-or-cancel on the arrival bar), which is a separate change to a parity-gated path.
/// This type enforces **GTD/DAY resting deadlines only** — do not read it as full server-side TIF
/// emulation.
#[derive(Debug, Clone, Copy)]
pub(super) struct OrderExpiry {
    pub(super) tif: TimeInForce,
    /// good-till-date deadline (epoch ms), only meaningful for [`TimeInForce::Gtd`]
    pub(super) gtd_expiry: Option<i64>,
    /// The `Day` session anchor: the ts of the first bar observed after submit, or `None` until
    /// that bar arrives. A `Day` order therefore never expires before it has seen a single bar. An
    /// order with a release is anchored at submit on the release instant instead, so its day is
    /// the day it is released on and it never expires before its release.
    pub(super) day_anchor: Option<i64>,
}

impl OrderExpiry {
    /// Is this order expired as of `now_ms` (always a BAR ts — see [`OrderExpiry`])?
    ///
    /// - `Gtd`: expired once `now_ms >= gtd_expiry`. A `Gtd` with no `gtd_expiry` has no deadline
    ///   and never expires.
    /// - `Day`: expired once `now_ms` lands on a later UTC day than the anchor bar. Un-anchored
    ///   (no bar seen yet) is never expired.
    /// - everything else (`Gtc`/`Ioc`/`Fok`): never expired by this check.
    fn is_expired(&self, now_ms: i64) -> bool {
        // The ONE expiry law (dedup A4): `vike_model::time_in_force_expired`. `day_anchor` is always `Some`
        // for a `Day` order by the time `expire_due` calls this (it anchors on the first bar
        // first); an un-anchored `Day` falls back to `now_ms` here, which the law reads as
        // same-day ⇒ not expired — byte-identical to the old `is_some_and(..)` None-arm.
        vike_model::time_in_force_expired(
            self.tif,
            self.gtd_expiry,
            self.day_anchor.unwrap_or(now_ms),
            now_ms,
        )
    }
}

impl ExecutionClientPaper {
    /// Time-in-force sweep, run as simulated time advances (once per `on_bar`, BEFORE the fill
    /// pass) — the paper twin of a venue's server-side **GTD/DAY** deadline enforcement, which a
    /// backtest otherwise lacks entirely (a `Gtd` order would rest forever). `Ioc`/`Fok` are NOT
    /// enforced here; see [`OrderExpiry`].
    ///
    /// Every resting order whose [`OrderExpiry`] says it is expired as of `now_ms` is removed from the
    /// book and terminalized with **`Event::OrderExpired`** — the model's purpose-built expiry
    /// vocabulary, the same variant binance/bybit/aster emit, so a paper GTD elapse drives
    /// `OrderEventKind::Expired` and the OMS `OrderStatus::Expired` exactly as the live venue does
    /// (`Accepted -> Expired` is a legal FSM edge; `submit` publishes `OrderAccepted`). It fires
    /// exactly once — the map entry is dropped with the order, and a filled/canceled order has
    /// already left both, so no order can expire twice or expire after it terminated.
    ///
    /// **Resolution caveat (real bars):** `now_ms` is `Bar.ts`, which is the bar's OPEN time, while
    /// a closed bar covers `[ts, ts+interval)`. The deadline is therefore evaluated at bar OPENS
    /// only: an order expires on **the first bar whose OPEN ts is at or past its deadline**, so a
    /// deadline lying strictly INSIDE a bar interval lets the order survive — and fill off that
    /// bar's full range — for up to one interval past the deadline. That coarseness is inherent to
    /// bar-resolution simulation; it is pinned by `a_deadline_inside_a_bar_overshoots_to_the_next`.
    ///
    /// Within a bar the ordering is deliberate: an order the sweep has already retired **cannot
    /// fill on that bar**. Expiry is a state change at the bar's timestamp and the bar's fills are
    /// attributed to that same timestamp, so filling an already-dead order would be a look-back.
    /// Cancels never touch the fill path or `position`.
    ///
    /// No-op (early return, no allocation) whenever no expiring-TIF order is resting — which is
    /// every `Gtc`-only run.
    pub(super) fn expire_due(&mut self, now_ms: i64) {
        if self.expiry.is_empty() {
            return;
        }
        // A `Day` order's session is the UTC day of the FIRST bar it sees — anchor it here rather
        // than on the (frequently unstamped) request ts. See [`OrderExpiry::day_anchor`].
        for e in self.expiry.values_mut() {
            if matches!(e.tif, TimeInForce::Day) && e.day_anchor.is_none() {
                e.day_anchor = Some(now_ms);
            }
        }
        let due: Vec<String> = self
            .expiry
            .iter()
            .filter(|(_, e)| e.is_expired(now_ms))
            .map(|(order_client_id, _)| order_client_id.clone())
            .collect();
        for order_client_id in due {
            self.expiry.shift_remove(&order_client_id);
            // only a still-RESTING order expires; anything already gone emits nothing. A resting
            // COMBO expires as one order too (its whole leg set leaves the book together).
            let before = self.pending.len() + self.combos.len();
            self.pending.retain(|(c, _)| c != &order_client_id);
            self.combos.retain(|(c, _)| c != &order_client_id);
            if self.pending.len() + self.combos.len() == before {
                continue;
            }
            self.contingency.remove(&order_client_id);
            self.events.push_back(Event::OrderExpired(OrderExpired {
                order_client_id: order_client_id.clone(),
                timestamp: now_ms,
            }));
            self.expire_children_of(&order_client_id, now_ms);
        }
        self.drop_marks_if_idle(); // the expired order may have been the last resting combo
    }
}
