//! [`OrderEmulationChase`]: the emulated pegged order. ONE parent order worked through ONE tagged child limit
//! that follows the touch.
//!
//! [`crate::emulated`]'s contract holds here word for word (the seam under its tag only, one child owned by a fixed
//! tag and re-adopted after a restart, never re-sized, ended by a cancel and never by a market order, a
//! bar run refused in words).
//!
//! On every quote or book update the child is priced off its [`vike_model::OrderPegAnchor`] with the signed
//! offset, snapped to the price grid, and re-priced IN PLACE once that price is at least
//! `refresh_threshold` from the child's (`0` re-prices on every move: the emulated peg). A crossed or
//! one-sided touch holds the child where it is. The first child goes out on the first touch; a later
//! one only after the last one is dead with part of the parent left, so two children are never live.
//!
//! * **Refuses in words at the start, and places nothing** ([`OrderEmulationChase::refusal`]), when the terms
//!   cannot run ([`OrderEmulationChaseConfig::validate`]). On its first touch it takes the instrument's grid and
//!   minimums from the engine (`InstrumentTerms`, off the grid the pre-trade gate applies, on either
//!   engine) and refuses, still before any child, a child (the parent's size) under the instrument's
//!   minimum size or notional at its first price.
//! * **The grid** is the instrument's, and the ONE model rule puts a price on it (`vike_model::snap_limit`,
//!   decision 0269 row 27): a buy rounds down and a sell up, so snapping never makes a child more
//!   aggressive. With none the price goes out as computed for the venue's wire formatting to round.
//!   A later child for what is left of the parent that would be under the minimum is never sent: the
//!   chaser stops in words instead.
//! * **Progress** is the larger of the fills heard on `on_fill` and the position (`position("")`),
//!   which the engine hands the law as the PARENT's own filled size, signed (the sum of its children's
//!   fills, never the account's position), so it starts at zero and fills made while the node was down
//!   still count (decision 0116). A child still resting once the parent is done is pulled.
//! * **It stops in words** ([`OrderEmulationChase::refusal`]): the parent done; the timeout run out (engine
//!   time from the first child); the child about to move past `max_chase` against its first price; or
//!   the child ended without the chaser asking (a reject, a deny, a cancel, an expiry): it never fights
//!   an operator's cancel and never loops on a refusal. Whatever is left of the parent stays unfilled.
//! * **A restart** restores the slot and the progress from `save_state` and marks the slot
//!   [`OrderEmulationChildSlot::restored`]: a restored live child is re-priced, never submitted again, and no NEW
//!   child goes out until the core names the tag or the adoption deadline passes
//!   ([`OrderEmulationChase::adoption_deadline`], [`ADOPTION_DEADLINE_MS`] by default).

use serde::{Deserialize, Serialize};
use vike_marketdata::{Bar, L2Book, TickQuote};
use vike_model::{Fill, OrderBroker, OrderEvent, OrderEventKind, Strategy};

use super::{
    ADOPTION_DEADLINE_MS, BAR_RUN_REFUSAL, InstrumentTerms, OrderEmulationChaseConfig,
    OrderEmulationChildSlot,
};

/// The ONE tag the child rests under: a fixed word, so it names the same order after a restart. It is
/// `vike_venues::OrderEmulationKind::Chase`'s word.
pub const TAG: &str = "chase";

/// Relative slack for float noise when sizes and distances are compared.
const EPS: f64 = 1e-9;

/// What survives a restart: everything but this run's own observations of the feed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Progress {
    slot: OrderEmulationChildSlot,
    /// Fills heard on `on_fill`, summed.
    heard: f64,
    /// `position("")` the progress counts from: zero, the parent's own filled size before its
    /// first child (`OrderEmulationChase::new`).
    start_position: Option<f64>,
    /// The first child's price, which max chase is measured from.
    first_price: Option<f64>,
    /// Engine milliseconds of the first child, which the timeout is measured from.
    started_ms: Option<i64>,
    /// Why it stopped, in words; `None` while it works.
    ended: Option<String>,
}

/// A limit chaser over [`OrderEmulationChaseConfig`]: one tagged child that follows the touch (module doc).
#[derive(Debug, Clone)]
pub struct OrderEmulationChase {
    config: OrderEmulationChaseConfig,
    /// Engine milliseconds a restored slot waits for the engine to name its child
    /// ([`OrderEmulationChildSlot::confirmed`]).
    deadline_ms: i64,
    /// The start refusal, from the terms or the instrument's terms on the first touch; never saved, so
    /// a restart judges them again.
    refused: Option<String>,
    /// The instrument's terms, read on the first touch.
    terms: Option<InstrumentTerms>,
    p: Progress,
    saw_bar: bool,
    saw_book: bool,
}

impl OrderEmulationChase {
    /// Build the chaser. Terms it cannot run are held as its [`Self::refusal`], and it places nothing.
    pub fn new(config: OrderEmulationChaseConfig) -> Self {
        let refused = config.validate().err();
        let p = Progress { start_position: Some(0.0), ..Progress::default() };
        let deadline_ms = ADOPTION_DEADLINE_MS;
        Self { config, deadline_ms, refused, terms: None, p, saw_bar: false, saw_book: false }
    }

    /// How long a restored slot waits for the engine to name its child before it gives it up and
    /// cancels by the tag ([`ADOPTION_DEADLINE_MS`] by default). The live core passes `i64::MAX`: it
    /// never gives a child up, so a restart never cancels one.
    pub fn adoption_deadline(mut self, ms: i64) -> Self {
        self.deadline_ms = ms;
        self
    }

    /// Why this chaser places nothing (more): refused at the start, stopped since or done, or, while
    /// this run has fed bars and never a quote or a book, [`BAR_RUN_REFUSAL`]. `None` while it works.
    pub fn refusal(&self) -> Option<&str> {
        self.refused
            .as_deref()
            .or(self.p.ended.as_deref())
            .or((self.saw_bar && !self.saw_book).then_some(BAR_RUN_REFUSAL))
    }

    /// How much of the parent has filled: see the module doc's "Progress".
    fn progress<B: OrderBroker>(&self, broker: &B) -> f64 {
        let moved = self
            .p
            .start_position
            .map_or(0.0, |s| f64::from(self.config.side) * (broker.position("") - s));
        self.p.heard.max(moved)
    }

    fn done<B: OrderBroker>(&self, broker: &B) -> bool {
        self.progress(broker) >= self.config.qty * (1.0 - EPS)
    }

    /// Stop for `why`: pull the child, then say what filled.
    fn finish<B: OrderBroker>(&mut self, broker: &mut B, why: String) {
        self.p.slot.cancel(broker, TAG);
        let (done, qty) = (self.progress(broker), self.config.qty);
        let rest = if self.done(broker) { "" } else { ", the rest stays unfilled" };
        self.p.ended = Some(format!("{why}: {done} of {qty} filled{rest}"));
    }

    /// End on the timeout; `true` once the chaser places nothing more, now or before.
    fn clock<B: OrderBroker>(&mut self, broker: &mut B) -> bool {
        if self.refused.is_some() || self.p.ended.is_some() {
            return true;
        }
        if let (Some(t0), Some(limit)) = (self.p.started_ms, self.config.timeout_ms)
            && broker.now() - t0 >= limit
        {
            self.finish(broker, format!("timed out {limit} ms after the first child"));
            return true;
        }
        false
    }

    fn on_touch<B: OrderBroker>(&mut self, broker: &mut B, bid: f64, ask: f64) {
        self.saw_book = true;
        if self.clock(broker) {
            return;
        }
        if self.done(broker) {
            return self.finish(broker, "the parent is done".to_string());
        }
        let terms = *self.terms.get_or_insert_with(|| InstrumentTerms::resolve(broker));
        let may_place = self.p.slot.confirmed(broker, TAG, self.deadline_ms);
        let side = self.config.side;
        let Some(px) = self
            .config
            .anchor
            .price(side, bid, ask, self.config.offset)
            .map(|px| vike_model::snap_limit(side, px, Some(terms.tick)))
            .filter(|px| *px > 0.0)
        else {
            return; // a crossed, one-sided or unpriceable touch holds the child where it is
        };
        let (live, current_price) = (self.p.slot.live, self.p.slot.price);
        if !live && !may_place {
            return; // restored, and the core has not yet named a child the saved state may miss
        }
        let moves = match current_price {
            Some(current) if live => {
                current != px && (px - current).abs() >= self.config.refresh_threshold * (1.0 - EPS)
            }
            // Free (a child goes out), or adopted at a price nobody knows.
            _ => true,
        };
        if !moves {
            return;
        }
        if let (Some(first), Some(max)) = (self.p.first_price, self.config.max_chase) {
            let against = f64::from(side) * (px - first);
            if against > max * (1.0 + EPS) {
                return self.finish(
                    broker,
                    format!(
                        "past max_chase {max}: the child would move to {px}, {against} against its \
                         first price {first}"
                    ),
                );
            }
        }
        let rest = self.config.qty - self.progress(broker);
        // A new child under the instrument's minimum is never sent: before the first one that is a
        // start refusal, after it the chaser stops with the rest unfilled.
        if !live && let Some(why) = terms.below_minimum(rest, px) {
            if self.p.first_price.is_none() {
                self.refused = Some(format!("the child cannot go out: {why}"));
            } else {
                self.finish(broker, format!("what is left cannot go out: {why}"));
            }
            return;
        }
        self.p.start_position.get_or_insert(broker.position(""));
        self.p.first_price.get_or_insert(px);
        self.p.started_ms.get_or_insert(broker.now());
        if live {
            self.p.slot.reprice(broker, TAG, px);
        } else {
            self.p.slot.place(broker, TAG, side, rest, px);
        }
    }
}

impl<B: OrderBroker> Strategy<B> for OrderEmulationChase {
    fn on_bar(&mut self, broker: &mut B, _bar: &Bar) {
        self.saw_bar = true;
        self.clock(broker);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.on_touch(broker, q.bid, q.ask);
    }

    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        let bid = book.best_bid().map_or(f64::NAN, |l| l.price);
        let ask = book.best_ask().map_or(f64::NAN, |l| l.price);
        self.on_touch(broker, bid, ask);
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        self.p.slot.on_fill(fill);
        self.p.heard += fill.size;
        if self.refused.is_none() && self.p.ended.is_none() && self.done(broker) {
            self.finish(broker, "the parent is done".to_string());
        }
    }

    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {
        let was_live = self.p.slot.live;
        if !self.p.slot.on_order_event(TAG, event)
            || self.refused.is_some()
            || self.p.ended.is_some()
        {
            return;
        }
        match &event.kind {
            // A sign of life or an adoption; the next touch re-prices it.
            OrderEventKind::Accepted => {}
            // Done, or a remainder the next touch rests a new child for.
            OrderEventKind::Filled => {
                if self.done(broker) {
                    self.finish(broker, "the parent is done".to_string());
                }
            }
            other if was_live => {
                let why = format!("the child ended without the chaser asking ({other:?})");
                self.finish(broker, why);
            }
            // A late terminal for a child the slot had already let go.
            _ => {}
        }
    }

    fn save_state(&self) -> Option<serde_json::Value> {
        serde_json::to_value(&self.p).ok()
    }

    /// Fail-open on the payload: one that does not parse keeps the fresh progress. Either way this is
    /// a restart, so the slot awaits its child's adoption ([`OrderEmulationChildSlot::restored`]).
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(p) = serde_json::from_value(state.clone()) {
            self.p = p;
        }
        self.p.slot.restored();
    }
}

#[path = "chase_tests.rs"]
#[cfg(test)]
mod chase_tests;
