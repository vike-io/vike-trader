//! The EMULATED route of an order kind the venue does not hold (decision 0275): one parent order worked
//! through ONE plain child limit at a time.
//!
//! A `pegged` order and a limit with a visible size are ORDER KINDS. Where the venue's row holds one, it is
//! sent as itself; where it does not, `vike_venues::emulation_of` names the engine's route
//! (`vike_venues::OrderEmulationKind`) and the engine works the parent with the law here: [`chase::OrderEmulationChase`]
//! for a peg, [`iceberg::OrderEmulationIceberg`] for a visible size. The law is no strategy and is mounted by no
//! name; it is the engine's own, and the live core and the simulator run the same code. Each law is an
//! `impl<B: OrderBroker> Strategy<B>` only because that is the hook set an engine already drives (the market
//! hooks, the fills, the order events, `save_state` / `load_state`). This module holds what the two laws
//! share: their config types, the child slot and the instrument's terms. The anchor a child is priced from
//! is `vike_model::OrderPegAnchor`, the one a native pegged order names too. It also holds what both ENGINES
//! share about a parent: [`OrderEmulationLaw`], built from the order on its route, [`DONE_EPS`], the slack a
//! parent is judged done by, and [`modify_refusal`], the words an amend of one is refused in.
//!
//! # The contract the law keeps
//!
//! * **Children go through the order seam under the law's tag only**: [`OrderBroker::place`] of a plain
//!   limit, [`OrderBroker::amend`] of its price, [`OrderBroker::cancel`]. A child is a plain limit, so the engine's
//!   own limit law fills it on either engine, and the law invents no price.
//! * **The law owns its children by a tag and re-adopts them after a restart.** The tag is a fixed word of
//!   the law ([`chase::TAG`], [`iceberg::TAG`], each `vike_venues::OrderEmulationKind`'s word), never a counter, so it
//!   means the same order after a restart. [`OrderEmulationChildSlot`] is the one fold: `save_state` persists the slot,
//!   and a tagged non-terminal order event on a FREE slot adopts the child the saved state predates (the
//!   same crash window `strategies/market_making/spread_maker.rs`'s `adopt_unplaced_side` closes for
//!   the maker). A live tag is re-priced or cancelled, never submitted again: a second submit under it orphans
//!   the first. So `load_state` marks the slot [`OrderEmulationChildSlot::restored`]: it places no new child until the
//!   engine names the tag or the law's adoption deadline passes ([`ADOPTION_DEADLINE_MS`] unless the
//!   engine sets another: the core sets none it ever reaches, so it never gives a child up and never
//!   cancels by the tag after a restart), and then it pulls what the tag still names first and places on
//!   the next hook. The engine's refusal of a child while a restored order still holds the tag sends the
//!   slot back to await it, and a late pass that adopts the child given up hands it back.
//! * **A saved state that EXISTS and does not read ENDS the parent; a missing one is a fresh parent.**
//!   Starting again would forget what the law last did (its stop, its child's ids), so the law stops in
//!   words (`restored_state`, logged at `error` with the parent's order client id) and, like every law
//!   that places nothing more, pulls on its next market hook whatever its restored slot still names.
//!   The engine then ends the parent once no child lives. A `null` document (none was saved) starts as new,
//!   so a law never saves none: a state that does not ENCODE is logged at `error` and saved as one that
//!   does not read (`saved_state`), which ends the parent at the restart in the same words.
//! * **A child is never re-sized.** A modify's size is the order's TOTAL, and the law may not know what
//!   filled while it was down, so a re-price moves the price alone.
//! * **It ends by cancelling its child, never by a market order.** What is left of the parent stays
//!   unfilled.
//! * **A book-following law refuses a bar run in words.** It places nothing on a bar and, while it has
//!   seen a bar and never a quote or book, answers `refusal()` with [`BAR_RUN_REFUSAL`]: following the
//!   book off a bar would mean inventing the book. The iceberg rests at its own fixed price and needs no
//!   book.
//! * **The venue is the engine's to judge, not the law's.** `vike_venues::emulation_of` refuses
//!   `vike_venues::EMULATION_REFUSED_VENUES` before a parent is armed, so the law takes no venue.
//! * **The instrument's grid and minimums come from the engine** (`InstrumentTerms`, read on the first
//!   market hook off the grid the pre-trade gate applies), on either engine, so every price is snapped to
//!   the instrument's own grid and no child under its minimum size or notional is ever sent.
//!
//! No queue law is added here: a passive child fills by the engine's limit law, or by its queue model
//! where a run arms one. A hidden-size queue needs L3 (decision 0045).

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use vike_model::{
    ContractValue, Fill, Grid, OrderBroker, OrderEvent, OrderEventKind, OrderPeg, OrderPegAnchor,
    OrderPlacement, OrderRequest, Strategy, StrategyState,
};
use vike_venues::OrderEmulationKind;
use vike_venues::venue_restore::RESTORED_TAG_DENY_REASON;

// The emulated peg: one tagged child that follows the touch, over `OrderEmulationChaseConfig`, `OrderPegAnchor`,
// `OrderEmulationChildSlot` and `BAR_RUN_REFUSAL`.
pub mod chase;

// The emulated visible size: fixed-price slices of a parent, one tagged child at a time.
pub mod iceberg;

/// What a book-following law answers when a run feeds it bars and no book.
pub const BAR_RUN_REFUSAL: &str =
    "this execution algorithm follows the book, and a bar has no book: run it on a tick or L2 lane";

/// The words both engines refuse an amend of an emulated parent in (decision 0275: a parent is never
/// amended), `order` naming it as its engine does: the live core by its order client id, the simulator
/// by its tag.
pub fn modify_refusal(order: &str) -> String {
    format!(
        "order {order} NOT modified: it is an emulated order (its venue does not hold the kind), so it \
         cannot be changed: cancel it and send it again"
    )
}

/// The instrument terms the law works to, resolved once per run on its first market hook from the
/// grid the engine's own pre-trade gate applies (the broker's `instrument_grid`): live, the venue's
/// instrument properties (the run profile's `[risk]` on paper); in a backtest, the instrument data's.
/// A `0` is a term the engine does not know, and nothing is judged against it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct InstrumentTerms {
    /// The grid every price is snapped to.
    pub(crate) tick: f64,
    pub(crate) min_qty: f64,
    pub(crate) min_notional: f64,
    /// What one unit is worth (the broker's `contract_value`, the default linear unit when the engine
    /// does not say): the notional the minimum is judged on is the gate's, `order_notional`.
    pub(crate) value: ContractValue,
}

/// Relative slack for float noise when a size or a grid ratio is judged.
const TERMS_EPS: f64 = 1e-9;

impl InstrumentTerms {
    /// The terms on `broker`'s grid for the mounted symbol: the grid is always the instrument's.
    pub(crate) fn resolve<B: OrderBroker>(broker: &B) -> Self {
        let grid = broker.instrument_grid("");
        let term = |pick: fn(&Grid) -> Option<f64>| {
            grid.as_ref().and_then(pick).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(0.0)
        };
        Self {
            tick: term(|g| g.tick_size),
            min_qty: term(|g| g.min_qty),
            min_notional: term(|g| g.min_notional),
            value: broker.contract_value("").unwrap_or_default(),
        }
    }

    /// Why a child of `qty` at `price` is under the instrument's minimum size or notional, in words;
    /// `None` when it clears both. The notional is the contract's ([`ContractValue::order_notional`],
    /// the pre-trade gate's number): `qty · price · u` on a linear contract, the dollars `qty · u` on
    /// an inverse one, whose minimum a price-times-quantity figure overstated by the price.
    pub(crate) fn below_minimum(&self, qty: f64, price: f64) -> Option<String> {
        if qty < self.min_qty * (1.0 - TERMS_EPS) {
            return Some(format!(
                "a child of {qty} is under the instrument's minimum size {}",
                self.min_qty
            ));
        }
        let notional = self.value.order_notional(qty, price);
        (notional < self.min_notional * (1.0 - TERMS_EPS)).then(|| {
            format!(
                "a child of {qty} at {price} is a notional of {notional}, under the instrument's \
                 minimum {}",
                self.min_notional
            )
        })
    }
}

/// A limit chaser's terms: work `qty` on `side` with one child priced off `anchor` and `offset`. An
/// emulated pegged order is `refresh_threshold: 0.0, max_chase: None, timeout_ms: None`: a peg carries no
/// field for the other three (decision 0275).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderEmulationChaseConfig {
    /// +1 buy / -1 sell.
    pub side: i32,
    /// The parent's whole size.
    pub qty: f64,
    pub anchor: OrderPegAnchor,
    /// Price units, signed as in [`OrderPegAnchor::price`].
    pub offset: f64,
    /// Re-price only when the anchored price is at least this far from the child's; `0` re-prices on every
    /// move, which is the emulated peg.
    pub refresh_threshold: f64,
    /// How far the child may move AGAINST the parent from its first price (a buy up, a sell down); past it
    /// the chaser cancels its child and stops. `None`: no bound.
    pub max_chase: Option<f64>,
    /// Engine milliseconds from the first placement after which the chaser cancels its child and stops.
    /// `None`: no limit.
    pub timeout_ms: Option<i64>,
}

impl OrderEmulationChaseConfig {
    /// The terms refused in words, or `Ok` when they can run.
    pub fn validate(&self) -> Result<(), String> {
        parent_refusal(self.side, self.qty)?;
        if !self.offset.is_finite() {
            return Err(format!("offset {} is not a number", self.offset));
        }
        if !(self.refresh_threshold.is_finite() && self.refresh_threshold >= 0.0) {
            return Err(format!("refresh_threshold {} must be 0 or more", self.refresh_threshold));
        }
        if let Some(m) = self.max_chase.filter(|m| !(m.is_finite() && *m > 0.0)) {
            return Err(format!("max_chase {m} must be more than 0"));
        }
        if let Some(t) = self.timeout_ms.filter(|t| *t <= 0) {
            return Err(format!("timeout_ms {t} must be more than 0"));
        }
        Ok(())
    }
}

/// An iceberg executor's terms: work `total` on `side` at `price`, showing at most `visible` at once and
/// resting the next slice after each slice fills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderEmulationIcebergConfig {
    /// +1 buy / -1 sell.
    pub side: i32,
    /// The parent's whole size.
    pub total: f64,
    /// The size of each slice; the last slice is what remains.
    pub visible: f64,
    /// The limit price every slice rests at.
    pub price: f64,
}

impl OrderEmulationIcebergConfig {
    /// The terms refused in words, or `Ok` when they can run.
    pub fn validate(&self) -> Result<(), String> {
        parent_refusal(self.side, self.total)?;
        if !(self.visible.is_finite() && self.visible > 0.0 && self.visible <= self.total) {
            return Err(format!(
                "visible {} must be more than 0 and at most total {}",
                self.visible, self.total
            ));
        }
        if !(self.price.is_finite() && self.price > 0.0) {
            return Err(format!("price {} must be more than 0", self.price));
        }
        Ok(())
    }
}

/// A law's saved state after a restart: `Ok(None)` when none was saved (the journal's `null`: a fresh
/// parent), `Ok(Some)` when it reads, and `Err` with the words the law stops in when a state exists and
/// does not read, logged once at `error`: the law then reduces its authority (decision 0013) instead of
/// working the parent again from a state it does not have.
fn restored_state<T: DeserializeOwned>(
    state: &StrategyState,
    tag: &str,
    order_client_id: &str,
) -> Result<Option<T>, String> {
    state.read::<Option<T>>().map_err(|why| {
        tracing::error!(
            order_client_id,
            law = tag,
            "emulated order {order_client_id} ({tag}): its saved state does not read after a restart \
             ({why}); it places nothing more, pulls its child and ends"
        );
        format!(
            "its saved state did not read after a restart ({why}), so it places nothing more and ends"
        )
    })
}

/// What a law saves (its `save_state`): `state`, or, when that does not encode, [`UNSAVED_STATE`],
/// logged at `error` with the parent's order client id. That one EXISTS and reads as no law's state,
/// so a restart ends the parent and cancels its child (`restored_state`); saving nothing would leave
/// the journal's `null`, a fresh parent that works its whole size again beside the live child.
fn saved_state<T: Serialize>(state: &T, tag: &str, order_client_id: &str) -> StrategyState {
    StrategyState::of(state).unwrap_or_else(|why| {
        tracing::error!(
            order_client_id,
            law = tag,
            "emulated order {order_client_id} ({tag}): its state does not save ({why}); a restart \
             ends it and cancels its child instead of working it again"
        );
        StrategyState::from_json_value(&serde_json::Value::from(UNSAVED_STATE))
    })
}

/// The state [`saved_state`] saves in place of one that does not encode: a word, which no law's state reads as.
const UNSAVED_STATE: &str = "this emulated order's state did not save";

/// The side and size every parent needs.
fn parent_refusal(side: i32, qty: f64) -> Result<(), String> {
    if side != 1 && side != -1 {
        return Err(format!("side {side} must be 1 (buy) or -1 (sell)"));
    }
    if !(qty.is_finite() && qty > 0.0) {
        return Err(format!("size {qty} must be more than 0"));
    }
    Ok(())
}

/// Relative slack when a filled size is judged against its whole size: the ONE slack both engines end a
/// parent by (the live core's book and the simulator's), and the one every law here judges a child or a
/// slice by (chase's progress, iceberg's remainder, a slot's child). An ABSOLUTE quantity tolerance is
/// another rule (`crate::execution_engine::LOCAL_VIEW_QUANTITY_TOLERANCE`).
pub const DONE_EPS: f64 = 1e-9;

/// **The law that works one parent**, built from the order itself: a [`chase::OrderEmulationChase`] for a
/// [`OrderEmulationKind::Chase`] route (the request's peg, `Best` at `0` when it names none), an
/// [`iceberg::OrderEmulationIceberg`] for [`OrderEmulationKind::Iceberg`] (the request's visible size and price,
/// `0` when absent, which its own terms then refuse). The live core and the simulator both build it
/// here, so a knob added to a config reaches both engines or neither.
pub enum OrderEmulationLaw {
    Chase(Box<chase::OrderEmulationChase>),
    Iceberg(Box<iceberg::OrderEmulationIceberg>),
}

impl OrderEmulationLaw {
    /// The law of `req` on `route`, giving a restored child up after `deadline_ms` of engine time
    /// (`OrderEmulationChase::adoption_deadline`): the live core passes `i64::MAX` (never, so a restart never
    /// cancels by the tag) unless its book knows no child lives; the simulator passes
    /// [`ADOPTION_DEADLINE_MS`], each law's default.
    #[must_use]
    pub fn of(req: &OrderRequest, route: OrderEmulationKind, deadline_ms: i64) -> Self {
        match route {
            OrderEmulationKind::Chase => {
                let peg = req.peg.unwrap_or(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 });
                let config = OrderEmulationChaseConfig {
                    side: req.side,
                    qty: req.qty,
                    anchor: peg.anchor,
                    offset: peg.offset,
                    refresh_threshold: 0.0,
                    max_chase: None,
                    timeout_ms: None,
                };
                OrderEmulationLaw::Chase(Box::new(
                    chase::OrderEmulationChase::new(config)
                        .adoption_deadline(deadline_ms)
                        .for_order(&req.order_client_id),
                ))
            }
            OrderEmulationKind::Iceberg => {
                let config = OrderEmulationIcebergConfig {
                    side: req.side,
                    total: req.qty,
                    visible: req.display_quantity.unwrap_or(0.0),
                    price: req.price.unwrap_or(0.0),
                };
                OrderEmulationLaw::Iceberg(Box::new(
                    iceberg::OrderEmulationIceberg::new(config)
                        .adoption_deadline(deadline_ms)
                        .for_order(&req.order_client_id),
                ))
            }
        }
    }

    /// The fixed tag its children rest under ([`chase::TAG`], [`iceberg::TAG`]).
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            OrderEmulationLaw::Chase(_) => chase::TAG,
            OrderEmulationLaw::Iceberg(_) => iceberg::TAG,
        }
    }

    /// The route it works.
    #[must_use]
    pub fn route(&self) -> OrderEmulationKind {
        match self {
            OrderEmulationLaw::Chase(_) => OrderEmulationKind::Chase,
            OrderEmulationLaw::Iceberg(_) => OrderEmulationKind::Iceberg,
        }
    }

    /// Why it places nothing (more), or `None` while it works.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self {
            OrderEmulationLaw::Chase(l) => l.refusal().map(str::to_string),
            OrderEmulationLaw::Iceberg(l) => l.refusal().map(str::to_string),
        }
    }

    /// The hooks an engine drives it through, on the engine's own broker `B`.
    pub fn hooks<B: OrderBroker>(&mut self) -> &mut dyn Strategy<B> {
        match self {
            OrderEmulationLaw::Chase(l) => l.as_mut(),
            OrderEmulationLaw::Iceberg(l) => l.as_mut(),
        }
    }

    /// Its `save_state` (the slot and its progress) as the JSON document the journal keeps. A law always
    /// saves one (`saved_state`: a state that does not encode is saved as `UNSAVED_STATE`), so the `Null`
    /// a fresh parent restores from is never written for a working law. The state is msgpack with field
    /// names (`vike_model::StrategyState`), whose document is the one `serde_json::to_value` of the same
    /// progress gives, so the journal's record does not move.
    #[must_use]
    pub fn saved<B: OrderBroker>(&self) -> serde_json::Value {
        match self {
            OrderEmulationLaw::Chase(l) => Strategy::<B>::save_state(l.as_ref()),
            OrderEmulationLaw::Iceberg(l) => Strategy::<B>::save_state(l.as_ref()),
        }
        .and_then(|state| state.to_json_value().ok())
        .unwrap_or(serde_json::Value::Null)
    }
}

/// How long a slot restored by `load_state` waits for the core to name its child, in engine
/// milliseconds from the first market hook after the restart: 60 seconds, the maker's own window for
/// a restored quote (`vike_market_making`'s `RESTORED_QUOTE_VERIFY_MS`). Until a tagged order event names the
/// tag or this passes, the slot places no new child ([`OrderEmulationChildSlot::restored`]). The DEFAULT of each law's
/// `adoption_deadline`; the live core passes `i64::MAX`, so a restart never gives a child up.
pub const ADOPTION_DEADLINE_MS: i64 = 60_000;

/// The ONE resting child an executor owns under its tag. Serde so an executor's `save_state` carries it.
///
/// **An event is the slot's only when it names the slot's own child BY ORDER ID**, not merely by tag:
/// every child goes out under the same fixed tag, so a late event of an older child must neither free
/// nor end the current one. A submit returns no id, so the slot learns its child's order client id
/// from the first order event that names the tag after the child went out, and keeps the previous
/// child's id to refuse that one's stragglers. The live core already stamps the tag only on the order
/// the tag names NOW (`vike_core`'s `tag_for_order_client_id`), so an older child's event reaches a mount
/// tagless there; the ids make the slot right on its own word too.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrderEmulationChildSlot {
    /// A child rests under the tag: placed here, restored by `load_state`, or adopted.
    #[serde(default)]
    pub live: bool,
    /// The child's limit price as last sent; `None` while free and after an adoption until re-priced.
    #[serde(default)]
    pub price: Option<f64>,
    /// The child's size as placed; `None` while free and for an adopted child, whose size is unknown.
    #[serde(default)]
    pub qty: Option<f64>,
    /// How much of the live child has filled, as heard on `on_fill`.
    #[serde(default)]
    pub filled: f64,
    /// The live child's order client id, learned from its first order event; `None` until then, and
    /// always on the simulator, which fires no order events.
    #[serde(default)]
    order_client_id: Option<String>,
    /// The id of the child before the live one: its late events are not this child's.
    #[serde(default)]
    previous_order_client_id: Option<String>,
    /// Restored by `load_state` and not yet named by the core ([`OrderEmulationChildSlot::restored`]). Never
    /// saved: it is this run's own doubt about what the saved state says.
    #[serde(skip)]
    awaiting: bool,
    /// Engine milliseconds of the first market hook that found the slot awaiting.
    #[serde(skip)]
    awaiting_since: Option<i64>,
    /// The deadline gave a RESTORED child up ([`OrderEmulationChildSlot::confirmed`]) with nobody having named it,
    /// so its fate is unknown: a late pass may still adopt it. Never saved.
    #[serde(skip)]
    released: bool,
}

impl OrderEmulationChildSlot {
    /// Mark the slot as restored from saved state, live or free. The saved state may be stale in
    /// both directions: a child it records may have died while the daemon was down, and a child placed
    /// after the last save is one it does not record. So until a tagged order event names the tag
    /// ([`OrderEmulationChildSlot::on_order_event`]: adopt) or the deadline [`OrderEmulationChildSlot::confirmed`] is given passes
    /// (pull whatever the tag still names, then free), [`OrderEmulationChildSlot::place`] sends nothing. A restored
    /// live child may still be re-priced in the meantime.
    pub fn restored(&mut self) {
        self.awaiting = true;
        self.awaiting_since = None;
    }

    /// Whether a NEW child may be placed now; an executor calls it on every market hook. While the slot
    /// awaits adoption the first call starts the clock and `deadline_ms` of engine time later
    /// ([`ADOPTION_DEADLINE_MS`] is the default) the slot gives up: it cancels by the tag, a no-op for a
    /// tag the core no longer knows, so no new child can stand beside an old one, and frees itself.
    /// It still answers `false` on that hook: the live core drains a hook's submits BEFORE its cancels
    /// (`vike_core`'s `drain_broker`), so a child placed in the same hook would be the order the cancel
    /// by the tag pulls. The next hook places. A late event of the child given up still adopts it
    /// ([`OrderEmulationChildSlot::on_order_event`]).
    pub fn confirmed<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        tag: &str,
        deadline_ms: i64,
    ) -> bool {
        if !self.awaiting {
            return true;
        }
        let now = broker.now();
        if now - *self.awaiting_since.get_or_insert(now) < deadline_ms {
            return false;
        }
        broker.cancel(tag);
        self.free();
        self.released = true;
        false
    }

    /// Let the child go, keeping its id as the previous child's.
    fn free(&mut self) {
        let previous_order_client_id =
            self.order_client_id.take().or(self.previous_order_client_id.take());
        *self = Self { previous_order_client_id, ..Self::default() };
    }

    /// Rest a NEW child under `tag`. Sends nothing and returns `false` while a child is live or the
    /// slot awaits adoption: a second submit under a live tag orphans the first.
    pub fn place<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        tag: &str,
        side: i32,
        qty: f64,
        price: f64,
    ) -> bool {
        if self.live || self.awaiting {
            return false;
        }
        broker.place("", tag, side, qty, OrderPlacement::Limit { price }, false);
        let previous_order_client_id =
            self.order_client_id.take().or(self.previous_order_client_id.take());
        *self = Self {
            live: true,
            price: Some(price),
            qty: Some(qty),
            previous_order_client_id,
            ..Self::default()
        };
        true
    }

    /// Move the live child to `price` in place, its size untouched. Nothing is sent while free or when
    /// the price is the one last sent.
    pub fn reprice<B: OrderBroker>(&mut self, broker: &mut B, tag: &str, price: f64) {
        if !self.live || self.price == Some(price) {
            return;
        }
        broker.amend(tag, None, Some(price), None);
        self.price = Some(price);
    }

    /// Pull the live child, or whatever the tag names while the slot awaits adoption; the slot is free
    /// at once.
    pub fn cancel<B: OrderBroker>(&mut self, broker: &mut B, tag: &str) {
        if self.live || self.awaiting {
            broker.cancel(tag);
        }
        self.free();
    }

    /// Count a fill against the live child. While no order event has named the child (the simulator
    /// fires none) the slot frees once the placed size has filled, which is how a child's death is
    /// heard there; once one has, only the child's own terminal event frees it, because a fill names no
    /// order and a late fill of an older child would otherwise free this one early.
    pub fn on_fill(&mut self, fill: &Fill) {
        if !self.live {
            return;
        }
        self.filled += fill.size;
        // A relative tolerance, so partial fills that sum to the size in floats free the slot.
        if self.order_client_id.is_none()
            && self.qty.is_some_and(|q| self.filled >= q * (1.0 - DONE_EPS))
        {
            self.free();
        }
    }

    /// Fold one order event; `true` when it is the slot's: it names `tag` and, unless the slot awaits
    /// adoption, it is not another child's by id (the previous child's, or a different one than the
    /// child the slot has learned). A terminal event frees the slot; a non-terminal one on a FREE slot
    /// adopts the child, price and size unknown until the executor re-prices it. While the slot awaits
    /// adoption the core's word on what the tag names is the authority, whatever id the saved state
    /// carried, and either way the slot awaits no more.
    ///
    /// Two restart cases answer otherwise. The core's refusal of a new child because a restored order
    /// still holds the tag ([`RESTORED_TAG_DENY_REASON`]: the child the saved state predates is not
    /// judged yet) is no refusal of the executor's: the slot awaits adoption again and the event is not
    /// the slot's (`false`), so no executor stops on it. And a restored child the deadline gave up is
    /// taken back by its own non-terminal event while the slot is free: a late first pass adopted it,
    /// and placing beside it would orphan it.
    ///
    /// The refusal of a new child because the tag is BUSY (P1, [`OrderEvent::is_tag_busy_refusal`]:
    /// an older order still works under it) is no refusal of the executor's either: the order behind
    /// the tag is the slot's child, so the slot ADOPTS it (live, price, size and id unknown; the next
    /// tagged event teaches the id) and the executor re-prices it, never placing a child that would be
    /// refused on every hook. Not the slot's event (`false`): the refusal names no child.
    pub fn on_order_event(&mut self, tag: &str, event: &OrderEvent) -> bool {
        if event.tag.as_deref() != Some(tag) {
            return false;
        }
        if event.is_tag_busy_refusal() {
            *self = Self { live: true, ..Self::default() };
            return false;
        }
        if self.live
            && matches!(&event.kind, OrderEventKind::Denied { reason } if reason == RESTORED_TAG_DENY_REASON)
        {
            self.free();
            self.restored();
            return false;
        }
        let id = event.order_client_id.as_str();
        // Another child than the one the slot records by id, when it records one.
        let replaced = self.order_client_id.as_deref().is_some_and(|own| own != id);
        // The previous child's straggler, unless it is a released restored child coming back. The
        // slot's own child is never one: an adopted-back child is both its `order_client_id` and its `previous_order_client_id`.
        let taken_back = self.released && !self.live && !event.kind.is_terminal();
        let own = self.order_client_id.as_deref() == Some(id);
        let stale = !own && self.previous_order_client_id.as_deref() == Some(id) && !taken_back;
        if (replaced || stale) && !self.awaiting {
            return false;
        }
        self.order_client_id = Some(id.to_string());
        self.awaiting = false;
        self.awaiting_since = None;
        self.released = false;
        if event.kind.is_terminal() {
            self.free();
        } else if !self.live || replaced {
            // Adopt: a child the slot holds no price or size for.
            self.live = true;
            (self.price, self.qty, self.filled) = (None, None, 0.0);
        }
        true
    }
}

// These unit tests drive `vike_model::strategy::OrderBrokerMock`, so they say what the law ASKS for; what an engine does
// with it is the engines' own suites.
#[path = "emulated_tests.rs"]
#[cfg(test)]
mod emulated_tests;
