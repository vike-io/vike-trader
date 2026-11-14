//! One engine exit that SPANS several positions rests as one venue order per position, and the engine sees ONE order.
//!
//! cTrader binds an order to ONE position (`ProtoOANewOrderReq::position_id`), and on a hedged account a net-sized
//! `reduce_only` stop over several positions has to be several orders, each bound to its own
//! (`crates/bridges/ctrader/src/execution/mod.rs`'s `submit`; before 0.1.97 it was refused by name, `EXIT_SPANS_POSITIONS`).
//! The engine placed ONE order under ONE order client id and expects ONE lifecycle for it: `OrderAccepted` once, then
//! partial fills, then exactly one terminal. This module is that fold, pure and keyed by order client id:
//!
//! * [`ExitLegs::register`] records a parent order client id and the order client ids and volumes of its legs (`<parent>L<n>`). The spelling
//!   proves nothing: a minted order client id may carry an `L` of its own (a v2 id's intent, `OL`, `CL`, `FL`), so the table is
//!   what recognises a leg, never the spelling.
//! * [`ExitLegs::fold`] rewrites every event of a leg to its parent and aggregates the lifecycle: ONE accept (the
//!   first), a bare `Fill` and an `OrderPartiallyFilled` per leg fill, `OrderFilled` when the last leg fills and every
//!   other leg either filled or ended with the engine told; an `OrderCanceled` when the last leg ends unfilled (or a
//!   leg vanished unseen, [`ExitLegs::retire_legs_where`]); an `OrderRejected` only when nothing was ever accepted. An
//!   event of any other order client id passes untouched, and a book with no group costs one empty-map test.
//! * **A leg that ends UNFILLED while its siblings still rest** (the venue rejected it, it was cancelled by the bridge's
//!   orphan rule, by hand or by the engine's own cancel, it expired) is not the end of the exit: the engine is told
//!   `OrderModified` with the size the venue still backs (what the legs filled plus what still rests), as FXCM's
//!   `ExitGroup` tells a leg the venue removed (`crates/bridges/fxcm/src/execution/exit/group.rs`). Before the exit's first
//!   accept the FSM takes no modify, so a leg refused that early is told with that accept.
//!
//! ⚠ **What it cannot do, and says.** An amend of a leg made at the venue is dropped and logged at `error` (an
//! `OrderModified` of the leg's own terms would resize the engine's whole order to one leg's size); its new volume
//! still counts in the next size told. The exit itself cannot be amended in place (`ExecutionClientCtrader::modify` refuses it):
//! cancel it and place the new terms.
//!
//! ⚠ **The table lives in this process.** After a bridge RESTART the legs that still rest are adopted as the plain
//! exits `<parent>L<n>` (`transport::connection::reconcile`'s `adopt_bound_exits`), they still protect their positions and the orphan
//! rule still cancels them, but the engine's own order client id for the exit maps to none of them: its cancel is refused
//! non-terminally (no venue order id), and a leg that fires reaches the core under an order client id it never placed. A RECONNECT
//! keeps the table: a leg whose row the reconnect dropped (nothing rests under it any more) is retired silently
//! ([`ExitLegs::retire_legs_where`]) so it cannot hold the exit open.

use std::collections::HashMap;

use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModified,
    OrderPartiallyFilled, OrderRejected,
};

/// The order client id of leg `n` (1-based) of the exit `parent`.
pub fn leg_order_client_id(parent: &str, n: usize) -> String {
    format!("{parent}L{n}")
}

/// How a leg ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Filled,
    Canceled,
    Expired,
    Rejected,
    /// It stopped resting at the venue with no event of its own (a reconnect found it gone,
    /// [`ExitLegs::retire_legs_where`]): neither a fill nor a cancel was seen.
    Gone,
}

#[derive(Debug)]
struct Leg {
    order_client_id: String,
    /// Centi-units: the volume the leg rests for ([`ExitLegs::set_volume`] keeps it to what the venue holds).
    volume: i64,
    /// What its fills so far came to, in the engine's units.
    filled: f64,
    /// How it ended (`None`: still resting).
    end: Option<End>,
}

impl Leg {
    /// The engine's units this leg backs: all of it while it rests, filled whole or vanished unseen (the reconcile
    /// owns that gap), else what it filled before it ended. Centi-units are `/ 100` as everywhere in this crate
    /// (`crate::feed::decode`'s `order_to_new_order`).
    fn backs(&self) -> f64 {
        match self.end {
            None | Some(End::Filled | End::Gone) => self.volume as f64 / 100.0,
            Some(End::Canceled | End::Expired | End::Rejected) => self.filled,
        }
    }

    fn ended_unfilled(&self) -> bool {
        matches!(self.end, Some(End::Canceled | End::Expired | End::Rejected))
    }
}

#[derive(Debug)]
struct Group {
    /// The legs in the order they were placed.
    legs: Vec<Leg>,
    /// The parent's single `OrderAccepted` has been passed on.
    accepted: bool,
    /// Some leg filled, wholly or in part.
    filled_any: bool,
}

/// The exits that span positions and the legs each rests as. Lives in the position book, which every event site
/// already holds; see the module doc.
#[derive(Debug, Default)]
pub struct ExitLegs {
    /// leg order client id → parent order client id.
    parent_of: HashMap<String, String>,
    groups: HashMap<String, Group>,
}

impl ExitLegs {
    /// Record `parent` as an exit that rests as `legs` (their order client ids and centi-volumes, in placement order).
    pub fn register(&mut self, parent: &str, legs: &[(String, i64)]) {
        for (leg, _) in legs {
            self.parent_of.insert(leg.clone(), parent.to_string());
        }
        self.groups.insert(
            parent.to_string(),
            Group {
                legs: legs
                    .iter()
                    .map(|(order_client_id, volume)| Leg {
                        order_client_id: order_client_id.clone(),
                        volume: *volume,
                        filled: 0.0,
                        end: None,
                    })
                    .collect(),
                accepted: false,
                filled_any: false,
            },
        );
    }

    /// The leg `order_client_id` rests for `volume` centi-units now (what was really sent, or a venue amend): a no-op for an order client id
    /// that is no leg.
    pub fn set_volume(&mut self, order_client_id: &str, volume: i64) {
        let Some(parent) = self.parent_of.get(order_client_id) else { return };
        if let Some(leg) = self
            .groups
            .get_mut(parent)
            .and_then(|g| g.legs.iter_mut().find(|l| l.order_client_id == order_client_id))
        {
            leg.volume = volume;
        }
    }

    /// Forget `parent` and its legs (a submit that never reached the venue), returning the leg order client ids.
    pub fn forget(&mut self, parent: &str) -> Vec<String> {
        let Some(group) = self.groups.remove(parent) else { return Vec::new() };
        for leg in &group.legs {
            self.parent_of.remove(&leg.order_client_id);
        }
        group.legs.into_iter().map(|leg| leg.order_client_id).collect()
    }

    /// The legs of `parent` that still rest, or `None` when `parent` is no exit group (or it has ended).
    pub fn live_legs(&self, parent: &str) -> Option<Vec<String>> {
        let group = self.groups.get(parent)?;
        let live: Vec<String> = group
            .legs
            .iter()
            .filter(|leg| leg.end.is_none())
            .map(|leg| leg.order_client_id.clone())
            .collect();
        (!live.is_empty()).then_some(live)
    }

    /// Retire the legs that still count as resting and for which `gone` says the venue no longer has them: after a
    /// reconnect dropped their rows (the order filled, expired or was swallowed in the gap) no event will ever
    /// arrive for them, and a leg left "resting" would refuse the engine's cancel of the exit for good. Nothing is
    /// emitted (the reconcile pass owns what happened in a gap, as for any other order); an exit with no leg left
    /// resting is forgotten.
    pub fn retire_legs_where(&mut self, gone: impl Fn(&str) -> bool) {
        for group in self.groups.values_mut() {
            for leg in &mut group.legs {
                if leg.end.is_none() && gone(leg.order_client_id.as_str()) {
                    leg.end = Some(End::Gone);
                }
            }
        }
        let over: Vec<String> = self
            .groups
            .iter()
            .filter(|(_, group)| group.legs.iter().all(|leg| leg.end.is_some()))
            .map(|(parent, _)| parent.clone())
            .collect();
        for parent in over {
            self.forget(&parent);
        }
    }

    /// The parent of the leg `order_client_id`, when it is one.
    pub fn parent_of(&self, order_client_id: &str) -> Option<&str> {
        self.parent_of.get(order_client_id).map(String::as_str)
    }

    /// Fold one canonical event: an event of a leg becomes the parent's (or nothing, when the parent's lifecycle
    /// does not move), every other event is returned as it came.
    pub fn fold(&mut self, event: Event) -> Vec<Event> {
        if self.parent_of.is_empty() {
            return vec![event];
        }
        let Some(leg) = order_client_id_of(&event) else { return vec![event] };
        let Some(parent) = self.parent_of.get(leg).cloned() else { return vec![event] };
        let Some(group) = self.groups.get_mut(&parent) else { return vec![event] };
        let Some(i) = group.legs.iter().position(|l| l.order_client_id == leg) else {
            return vec![event];
        };
        let out = fold_leg(group, &parent, i, event);
        if group.legs.iter().all(|leg| leg.end.is_some()) {
            self.forget(&parent);
        }
        out
    }
}

/// The order client id an event of the order lifecycle carries.
fn order_client_id_of(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(e) => Some(&e.order_client_id),
        Event::OrderAccepted(e) => Some(&e.order_client_id),
        Event::OrderRejected(e) => Some(&e.order_client_id),
        Event::OrderPartiallyFilled(e) => Some(&e.order_client_id),
        Event::OrderFilled(e) => Some(&e.order_client_id),
        Event::OrderCanceled(e) => Some(&e.order_client_id),
        Event::OrderExpired(e) => Some(&e.order_client_id),
        Event::OrderModified(e) => Some(&e.order_client_id),
        Event::OrderCancelRejected(e) => Some(&e.order_client_id),
        Event::OrderModifyRejected(e) => Some(&e.order_client_id),
        _ => None,
    }
}

fn as_parent(fill: FillEvent, parent: &str) -> FillEvent {
    FillEvent { order_client_id: parent.to_string(), ..fill }
}

/// The core's order `parent`, narrowed to the size the venue still backs (FXCM's `ExitGroup::narrowed`).
fn narrowed(group: &Group, parent: &str, ts: i64) -> Event {
    Event::OrderModified(OrderModified {
        order_client_id: parent.to_string(),
        venue_order_id: None,
        new_qty: Some(group.legs.iter().map(Leg::backs).sum()),
        new_price: None,
        new_trigger_price: None,
        timestamp: ts,
    })
}

/// `event` of the leg `group.legs[i]` of `parent`, folded into `group`.
fn fold_leg(group: &mut Group, parent: &str, i: usize, event: Event) -> Vec<Event> {
    let leg = group.legs[i].order_client_id.clone();
    match event {
        Event::OrderAccepted(a) => {
            if group.accepted {
                return Vec::new();
            }
            group.accepted = true;
            let ts = a.timestamp;
            let mut out = vec![Event::OrderAccepted(OrderAccepted {
                order_client_id: parent.to_string(),
                ..a
            })];
            // A leg refused before this first accept was not told (the FSM takes no modify before it): told now.
            if group.legs.iter().any(Leg::ended_unfilled) {
                out.push(narrowed(group, parent, ts));
            }
            out
        }
        Event::Fill(fill) => vec![Event::Fill(as_parent(fill, parent))],
        Event::OrderPartiallyFilled(w) => {
            group.filled_any = true;
            group.legs[i].filled += w.fill.last_qty;
            let fill = as_parent(w.fill, parent);
            vec![Event::OrderPartiallyFilled(OrderPartiallyFilled {
                order_client_id: parent.to_string(),
                fill,
                timestamp: w.timestamp,
            })]
        }
        Event::OrderFilled(w) => {
            group.filled_any = true;
            group.legs[i].filled += w.fill.last_qty;
            group.legs[i].end = Some(End::Filled);
            let fill = as_parent(w.fill, parent);
            let over = group.legs.iter().all(|l| l.end.is_some());
            // Every other leg filled, or ended unfilled with the engine told the narrower size: the order the engine
            // now holds is filled. A leg that vanished unseen (`End::Gone`) was never told.
            let told = group
                .legs
                .iter()
                .all(|l| l.end == Some(End::Filled) || (group.accepted && l.ended_unfilled()));
            if over && told {
                vec![Event::OrderFilled(OrderFilled {
                    order_client_id: parent.to_string(),
                    fill,
                    timestamp: w.timestamp,
                })]
            } else if over {
                // The last leg filled but an earlier one vanished unseen: the exit is over, partly filled.
                let reason = "an exit leg ended without a fill; the exit filled in part";
                vec![
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        order_client_id: parent.to_string(),
                        fill,
                        timestamp: w.timestamp,
                    }),
                    Event::OrderCanceled(OrderCanceled {
                        order_client_id: parent.to_string(),
                        reason: reason.into(),
                        timestamp: w.timestamp,
                    }),
                ]
            } else {
                vec![Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    order_client_id: parent.to_string(),
                    fill,
                    timestamp: w.timestamp,
                })]
            }
        }
        Event::OrderCanceled(c) => {
            group.legs[i].end = Some(End::Canceled);
            leg_ended_unfilled(group, parent, &leg, c.reason.to_string(), c.timestamp, true)
        }
        Event::OrderExpired(x) => {
            group.legs[i].end = Some(End::Expired);
            leg_ended_unfilled(group, parent, &leg, "expired".to_string(), x.timestamp, true)
        }
        Event::OrderRejected(r) => {
            group.legs[i].end = Some(End::Rejected);
            leg_ended_unfilled(group, parent, &leg, r.reason.to_string(), r.timestamp, false)
        }
        // A refused cancel of a leg keeps the exit alive: the advisory is the parent's.
        Event::OrderCancelRejected(mut c) => {
            c.order_client_id = parent.to_string();
            vec![Event::OrderCancelRejected(c)]
        }
        Event::OrderModifyRejected(mut m) => {
            m.order_client_id = parent.to_string();
            vec![Event::OrderModifyRejected(m)]
        }
        // An amend of ONE leg, made at the venue: reported to the engine it would resize the whole order to this
        // leg's size. Dropped, said.
        Event::OrderModified(_) => {
            tracing::error!(
                target: "ctrader",
                parent,
                leg = leg.as_str(),
                "a leg of an exit that spans positions was AMENDED at the venue; the engine is not told \
                 (it would read one leg's size as the whole order's)"
            );
            Vec::new()
        }
        other => vec![other],
    }
}

/// A leg ended without filling (`cancelled` is false for a rejection, which is only legal before any accept): when it
/// was the last the exit is over and the parent gets its single terminal; otherwise the siblings still rest and the
/// engine is told the size the venue still backs ([`narrowed`]), once the exit has been accepted.
fn leg_ended_unfilled(
    group: &Group,
    parent: &str,
    leg: &str,
    reason: String,
    ts: i64,
    cancelled: bool,
) -> Vec<Event> {
    if group.legs.iter().any(|l| l.end.is_none()) {
        tracing::warn!(
            target: "ctrader",
            parent,
            leg,
            %reason,
            "a leg of an exit that spans positions ENDED UNFILLED while its siblings still rest: the engine \
             is told the size the venue still backs"
        );
        return if group.accepted { vec![narrowed(group, parent, ts)] } else { Vec::new() };
    }
    // The legs are all over. Rejected is legal only from Submitted: after any accept or fill the terminal is a cancel.
    if !cancelled && !group.accepted && !group.filled_any {
        return vec![Event::OrderRejected(OrderRejected {
            order_client_id: parent.to_string(),
            reason: reason.into(),
            timestamp: ts,
            cause: None,
        })];
    }
    vec![Event::OrderCanceled(OrderCanceled {
        order_client_id: parent.to_string(),
        reason: reason.into(),
        timestamp: ts,
    })]
}

#[path = "exit_legs_tests.rs"]
#[cfg(test)]
mod exit_legs_tests;
