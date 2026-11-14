//! The emulated visible size: work a parent of `total` at ONE fixed price, showing at most `visible` at
//! once.
//!
//! The contract the module doc of `crates/vike-execution/src/emulated.rs` states, applied to slices:
//!
//! * **One tagged child at a time** through [`OrderEmulationChildSlot`], under the fixed tag [`TAG`]. Each child
//!   is a plain limit of `min(visible, remaining)` at the configured price, so the last slice is what
//!   is left and the engine's own limit law fills every slice. A remainder that would leave a last
//!   slice under the instrument's minimum rides on the slice before it instead.
//! * **A refill only on a market hook** (bar, quote, trade, book), never inside `on_fill` or
//!   `on_order_event`: live, a slice's last fill and its `Filled` event arrive in the same fold and
//!   carry the SAME tag, so a slice placed between the two would be wiped by the event and orphaned.
//! * **Progress is the larger of the fills heard and the position**, which the engine hands the law as
//!   the PARENT's own filled size, signed (the sum of its children's fills, never the account's
//!   position), so it counts from zero: `on_fill` undercounts fills made while the node was down
//!   (decision 0116), and the
//!   larger of the two never over-executes the parent. The next slice and "done" read it; a child is
//!   freed only by its own size filling or its terminal event, never by the position alone (a
//!   position moved by someone else would then orphan a resting child).
//! * **Re-adopts its child after a restart**: `save_state` carries the slot, the start position, the
//!   fills heard and any stop; a tagged non-terminal event on a free slot adopts the child the saved
//!   state predates ([`OrderEmulationChildSlot::on_order_event`]). `load_state` marks the slot
//!   [`OrderEmulationChildSlot::restored`], so no new slice goes out until the core names the tag or the adoption
//!   deadline passes ([`OrderEmulationChildSlot::confirmed`], asked on every market hook;
//!   [`OrderEmulationIceberg::adoption_deadline`], [`ADOPTION_DEADLINE_MS`] by default). The START refusal is
//!   never saved: a restart judges the terms again, and a saved state cannot clear it.
//! * **Runs on bars**: the price is fixed, so it needs no book and has no bar refusal.
//! * **Refuses in words at the start, and places nothing**, when the terms cannot run
//!   ([`OrderEmulationIcebergConfig::validate`]). On its first market hook it takes the instrument's grid and
//!   minimums from the engine (`InstrumentTerms`, off the grid the pre-trade gate applies, on either
//!   engine) and refuses, still before any slice, a visible slice under the instrument's minimum size
//!   or notional. The price is snapped PASSIVELY to the instrument's grid (a buy down, a sell up) by the
//!   ONE model rule (`vike_model::snap_limit`, decision 0269 row 27), so the wire gets a grid price and
//!   never a price more aggressive than typed.
//! * **Stops, never re-sends**, on a refused child (venue reject or risk-gate deny) or one pulled
//!   outside it (a cancel or expiry it did not send): re-sending a refused slice loops. Only the
//!   CURRENT child's events count, matched by order id ([`OrderEmulationChildSlot::on_order_event`]): a late event of
//!   an older slice neither frees nor stops the one resting now. [`OrderEmulationIceberg::cancel`] pulls the
//!   child and stops; what is left of the parent stays unfilled.

use serde::{Deserialize, Serialize};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{Fill, OrderBroker, OrderEvent, OrderEventKind, Strategy};

use super::{
    ADOPTION_DEADLINE_MS, InstrumentTerms, OrderEmulationChildSlot, OrderEmulationIcebergConfig,
};

/// The executor's one tag: a fixed word, so it names the same child after a restart. It is
/// `vike_venues::OrderEmulationKind::Iceberg`'s word.
pub const TAG: &str = "iceberg";

/// Relative float tolerance: partial fills that sum to a size in floats count as that size.
const EPS: f64 = 1e-9;

/// The durable half, persisted through `save_state`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct IcebergState {
    #[serde(default)]
    slot: OrderEmulationChildSlot,
    /// `position("")` when the executor first ran; the delta from it is filled parent.
    #[serde(default)]
    start_position: Option<f64>,
    /// Every fill size heard on `on_fill`.
    #[serde(default)]
    heard: f64,
    /// Why nothing more is placed once it stopped or finished. The START refusal is not here.
    #[serde(default)]
    stopped: Option<String>,
}

/// Work `config.total` at `config.price` in slices of at most `config.visible`, one tagged child at a time.
#[derive(Debug, Clone)]
pub struct OrderEmulationIceberg {
    /// The terms as typed; the price is snapped to the grid once the instrument's terms are known.
    pub(crate) config: OrderEmulationIcebergConfig,
    /// Engine milliseconds a restored slot waits for the engine to name its child
    /// ([`OrderEmulationChildSlot::confirmed`]).
    deadline_ms: i64,
    /// The terms refused, at construction or by the instrument's terms on the first market hook:
    /// judged again by every run and never saved.
    refused: Option<String>,
    /// The instrument's terms and the price snapped to their grid, read on the first market hook.
    armed: Option<(InstrumentTerms, f64)>,
    st: IcebergState,
}

impl OrderEmulationIceberg {
    /// Build the executor; terms it cannot run are held as its [`Self::refusal`], and it places nothing.
    pub fn new(config: OrderEmulationIcebergConfig) -> Self {
        let refused = config.validate().err();
        let deadline_ms = ADOPTION_DEADLINE_MS;
        // The position the engine hands the law is the parent's own filled size: it starts at zero.
        let st = IcebergState { start_position: Some(0.0), ..IcebergState::default() };
        Self { config, deadline_ms, refused, armed: None, st }
    }

    /// How long a restored slot waits for the engine to name its child before it gives it up and
    /// cancels by the tag ([`ADOPTION_DEADLINE_MS`] by default). The live core passes `i64::MAX`: it
    /// never gives a child up, so a restart never cancels one.
    pub fn adoption_deadline(mut self, ms: i64) -> Self {
        self.deadline_ms = ms;
        self
    }

    /// Why this executor places nothing more: refused at the start, stopped since, or done. `None`
    /// while it works.
    pub fn refusal(&self) -> Option<&str> {
        self.refused.as_deref().or(self.st.stopped.as_deref())
    }

    /// Pull the resting child and stop; what is left of the parent stays unfilled.
    pub fn cancel<B: OrderBroker>(&mut self, broker: &mut B) {
        self.st.slot.cancel(broker, TAG);
        self.stop("cancelled by its owner".to_string());
    }

    /// The instrument's terms on `broker` and the price snapped to their grid, or the start refusal
    /// they make in words: a price that snaps to nothing, a visible slice under the minimum. Every
    /// later slice is at least the visible one or rides on it.
    fn arm<B: OrderBroker>(&self, broker: &B) -> Result<(InstrumentTerms, f64), String> {
        let terms = InstrumentTerms::resolve(broker);
        let px = vike_model::snap_limit(self.config.side, self.config.price, Some(terms.tick));
        if px <= 0.0 {
            return Err(format!(
                "price {} snaps to {px} on the price grid {}",
                self.config.price, terms.tick
            ));
        }
        match terms.below_minimum(self.config.visible, px) {
            Some(why) => Err(format!("the visible slice cannot go out: {why}")),
            None => Ok((terms, px)),
        }
    }

    /// Record the first reason to stop; a later one never overwrites it.
    fn stop(&mut self, why: String) {
        self.st.stopped = self.st.stopped.take().or(Some(why));
    }

    /// The filled part of the parent at position `pos`, in `[0, total]`.
    fn filled(&self, pos: f64) -> f64 {
        let start = self.st.start_position.unwrap_or(pos);
        (f64::from(self.config.side) * (pos - start))
            .max(self.st.heard)
            .clamp(0.0, self.config.total)
    }

    /// The one placing step, run on every market hook.
    fn work<B: OrderBroker>(&mut self, broker: &mut B) {
        let pos = broker.position("");
        // Zero from `new`; only a saved state that predates the field leaves it unset.
        self.st.start_position = Some(self.st.start_position.unwrap_or(pos));
        if self.refusal().is_some() {
            return;
        }
        let (terms, px) = match self.armed {
            Some(armed) => armed,
            None => match self.arm(broker) {
                Ok(armed) => *self.armed.insert(armed),
                Err(why) => {
                    self.refused = Some(why);
                    return;
                }
            },
        };
        let remaining = self.config.total - self.filled(pos);
        if remaining <= self.config.total * EPS {
            // Done by the position while a child still rests (its fills were never heard): pull it,
            // a no-op on a child that has already filled. The slot sends nothing when it holds none.
            self.st.slot.cancel(broker, TAG);
            self.stop(format!("done: the whole total of {} has filled", self.config.total));
            return;
        }
        // Restored: no new slice until the core names the tag or the deadline pulls what it names.
        if !self.st.slot.confirmed(broker, TAG, self.deadline_ms) || self.st.slot.live {
            return;
        }
        let mut qty = self.config.visible.min(remaining);
        // A remainder under the minimum would be a last slice the venue refuses: it rides on this one.
        let rest = remaining - qty;
        if rest > self.config.total * EPS && terms.below_minimum(rest, px).is_some() {
            qty = remaining;
        }
        // Fills from elsewhere can leave a remainder no slice can carry.
        if let Some(why) = terms.below_minimum(qty, px) {
            self.stop(format!("what is left cannot go out: {why}"));
            return;
        }
        self.st.slot.place(broker, TAG, self.config.side, qty, px);
    }
}

impl<B: OrderBroker> Strategy<B> for OrderEmulationIceberg {
    fn on_bar(&mut self, broker: &mut B, _bar: &Bar) {
        self.work(broker);
    }

    fn on_quote_tick(&mut self, broker: &mut B, _q: &TickQuote) {
        self.work(broker);
    }

    fn on_trade_tick(&mut self, broker: &mut B, _t: &TickTrade) {
        self.work(broker);
    }

    fn on_order_book(&mut self, broker: &mut B, _book: &L2Book) {
        self.work(broker);
    }

    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        self.st.heard += fill.size;
        self.st.slot.on_fill(fill);
    }

    fn on_order_event(&mut self, _broker: &mut B, event: &OrderEvent) {
        let was_live = self.st.slot.live;
        if !self.st.slot.on_order_event(TAG, event) {
            return;
        }
        // The slot took the event as its own child's (by order id), so it ends that child alone.
        let why = match &event.kind {
            OrderEventKind::Accepted | OrderEventKind::Filled => return,
            // A late terminal for a child the slot had already let go (our cancel, the deadline's).
            _ if !was_live => return,
            OrderEventKind::Rejected { reason } => format!("the venue refused a slice: {reason}"),
            OrderEventKind::Denied { reason } => format!("the risk gate refused a slice: {reason}"),
            OrderEventKind::Canceled { .. } | OrderEventKind::Expired => {
                "its child was pulled outside the executor".to_string()
            }
        };
        self.stop(why);
    }

    fn save_state(&self) -> Option<serde_json::Value> {
        serde_json::to_value(&self.st).ok()
    }

    /// A payload that does not parse keeps the fresh state. Either way this is a restart, so the slot
    /// awaits its child's adoption ([`OrderEmulationChildSlot::restored`]); the start refusal is not in the payload.
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(saved) = IcebergState::deserialize(state) {
            self.st = saved;
        }
        self.st.slot.restored();
    }
}

#[path = "iceberg_tests.rs"]
#[cfg(test)]
mod iceberg_tests;
