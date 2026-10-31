//! One engine exit that SPANS several positions rests as one venue order per position, and the engine sees ONE order.
//!
//! cTrader binds an order to ONE position (`ProtoOANewOrderReq::position_id`), and on a hedged account a net-sized
//! `reduce_only` stop over several positions has to be several orders, each bound to its own
//! (`crates/bridges/ctrader/src/exec.rs`'s `submit`; before 0.1.97 it was refused by name, `EXIT_SPANS_POSITIONS`).
//! The engine placed ONE order under ONE coid and expects ONE lifecycle for it: `OrderAccepted` once, then
//! partial fills, then exactly one terminal. This module is that fold, pure and keyed by coid:
//!
//! * [`ExitLegs::register`] records a parent coid and the coids of its legs (`<parent>L<n>`, which no minted coid can
//!   equal: the engine's ids are `<8-hex-session><digits>`, with no `L`; the table is what recognises a leg, never the
//!   spelling).
//! * [`ExitLegs::fold`] rewrites every event of a leg to its parent and aggregates the lifecycle: ONE accept (the
//!   first), a bare `Fill` and an `OrderPartiallyFilled` per leg fill, `OrderFilled` only when EVERY leg filled; an
//!   `OrderCanceled` when the legs ended and some did not fill; an `OrderRejected` only when nothing was ever accepted.
//!   An event of any other coid passes untouched, and a book with no group costs one empty-map test.
//!
//! ⚠ **What it cannot do, and says.** A leg that ends UNFILLED while its siblings still rest (the venue rejected it, it
//! was cancelled by the bridge's orphan rule or by hand, it expired) leaves the exit covering LESS than the engine
//! counts: the engine is not told (there is no "partially cancelled" event), the fold drops the leg's terminal and logs
//! at `error`. An amend of a leg made at the venue is dropped for the same reason (an `OrderModified` would resize the
//! engine's whole order to one leg's size). The exit itself cannot be amended in place (`CtraderExec::modify` refuses it):
//! cancel it and place the new terms.
//!
//! ⚠ **The table lives in this process.** After a bridge RESTART the legs that still rest are adopted as the plain
//! exits `<parent>L<n>` (`conn::reconcile`'s `adopt_bound_exits`), they still protect their positions and the orphan
//! rule still cancels them, but the engine's own coid for the exit maps to none of them: its cancel is refused
//! non-terminally (no venue order id), and a leg that fires reaches the core under a coid it never placed. A RECONNECT
//! keeps the table: a leg whose row the reconnect dropped (nothing rests under it any more) is retired silently
//! ([`ExitLegs::retire_legs_where`]) so it cannot hold the exit open.

use std::collections::HashMap;

use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled,
    OrderRejected,
};

/// The coid of leg `n` (1-based) of the exit `parent`.
pub fn leg_coid(parent: &str, n: usize) -> String {
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
struct Group {
    /// The legs in the order they were placed: their coid and how each ended (`None`: still resting).
    legs: Vec<(String, Option<End>)>,
    /// The parent's single `OrderAccepted` has been passed on.
    accepted: bool,
    /// Some leg filled, wholly or in part.
    filled_any: bool,
}

/// The exits that span positions and the legs each rests as. Lives in the position book, which every event site
/// already holds; see the module doc.
#[derive(Debug, Default)]
pub struct ExitLegs {
    /// leg coid → parent coid.
    parent_of: HashMap<String, String>,
    groups: HashMap<String, Group>,
}

impl ExitLegs {
    /// Record `parent` as an exit that rests as `legs` (their coids, in placement order).
    pub fn register(&mut self, parent: &str, legs: &[String]) {
        for leg in legs {
            self.parent_of.insert(leg.clone(), parent.to_string());
        }
        self.groups.insert(
            parent.to_string(),
            Group {
                legs: legs.iter().map(|l| (l.clone(), None)).collect(),
                accepted: false,
                filled_any: false,
            },
        );
    }

    /// Forget `parent` and its legs (a submit that never reached the venue), returning the leg coids.
    pub fn forget(&mut self, parent: &str) -> Vec<String> {
        let Some(group) = self.groups.remove(parent) else { return Vec::new() };
        for (leg, _) in &group.legs {
            self.parent_of.remove(leg);
        }
        group.legs.into_iter().map(|(leg, _)| leg).collect()
    }

    /// The legs of `parent` that still rest, or `None` when `parent` is no exit group (or it has ended).
    pub fn live_legs(&self, parent: &str) -> Option<Vec<String>> {
        let group = self.groups.get(parent)?;
        let live: Vec<String> = group
            .legs
            .iter()
            .filter(|(_, end)| end.is_none())
            .map(|(leg, _)| leg.clone())
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
            for (leg, end) in &mut group.legs {
                if end.is_none() && gone(leg.as_str()) {
                    *end = Some(End::Gone);
                }
            }
        }
        let over: Vec<String> = self
            .groups
            .iter()
            .filter(|(_, group)| group.legs.iter().all(|(_, end)| end.is_some()))
            .map(|(parent, _)| parent.clone())
            .collect();
        for parent in over {
            self.forget(&parent);
        }
    }

    /// The parent of the leg `coid`, when it is one.
    pub fn parent_of(&self, coid: &str) -> Option<&str> {
        self.parent_of.get(coid).map(String::as_str)
    }

    /// Fold one canonical event: an event of a leg becomes the parent's (or nothing, when the parent's lifecycle
    /// does not move), every other event is returned as it came.
    pub fn fold(&mut self, event: Event) -> Vec<Event> {
        if self.parent_of.is_empty() {
            return vec![event];
        }
        let Some(leg) = coid_of(&event) else { return vec![event] };
        let Some(parent) = self.parent_of.get(leg).cloned() else { return vec![event] };
        let leg = leg.to_string();
        let Some(group) = self.groups.get_mut(&parent) else { return vec![event] };
        let out = fold_leg(group, &parent, &leg, event);
        if group.legs.iter().all(|(_, end)| end.is_some()) {
            self.forget(&parent);
        }
        out
    }
}

/// The coid an event of the order lifecycle carries.
fn coid_of(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(e) => Some(&e.client_order_id),
        Event::OrderAccepted(e) => Some(&e.client_order_id),
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderModified(e) => Some(&e.client_order_id),
        Event::OrderCancelRejected(e) => Some(&e.client_order_id),
        Event::OrderModifyRejected(e) => Some(&e.client_order_id),
        _ => None,
    }
}

fn as_parent(fill: FillEvent, parent: &str) -> FillEvent {
    FillEvent { client_order_id: parent.to_string(), ..fill }
}

/// `event` of leg `leg` of `parent`, folded into `group`.
fn fold_leg(group: &mut Group, parent: &str, leg: &str, event: Event) -> Vec<Event> {
    let ended = |group: &mut Group, end: End| {
        if let Some(slot) = group.legs.iter_mut().find(|(l, _)| l == leg) {
            slot.1 = Some(end);
        }
    };
    match event {
        Event::OrderAccepted(a) => {
            if group.accepted {
                return Vec::new();
            }
            group.accepted = true;
            vec![Event::OrderAccepted(OrderAccepted { client_order_id: parent.to_string(), ..a })]
        }
        Event::Fill(fill) => vec![Event::Fill(as_parent(fill, parent))],
        Event::OrderPartiallyFilled(w) => {
            group.filled_any = true;
            let fill = as_parent(w.fill, parent);
            vec![Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: parent.to_string(),
                fill,
                ts: w.ts,
            })]
        }
        Event::OrderFilled(w) => {
            group.filled_any = true;
            ended(group, End::Filled);
            let fill = as_parent(w.fill, parent);
            if group.legs.iter().all(|(_, end)| *end == Some(End::Filled)) {
                vec![Event::OrderFilled(OrderFilled {
                    client_order_id: parent.to_string(),
                    fill,
                    ts: w.ts,
                })]
            } else if group.legs.iter().all(|(_, end)| end.is_some()) {
                // The last leg filled but an earlier one ended unfilled: the exit is over, partly filled.
                let reason = "an exit leg ended without a fill; the exit filled in part";
                vec![
                    Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: parent.to_string(),
                        fill,
                        ts: w.ts,
                    }),
                    Event::OrderCanceled(OrderCanceled {
                        client_order_id: parent.to_string(),
                        reason: reason.into(),
                        ts: w.ts,
                    }),
                ]
            } else {
                vec![Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: parent.to_string(),
                    fill,
                    ts: w.ts,
                })]
            }
        }
        Event::OrderCanceled(c) => {
            ended(group, End::Canceled);
            leg_ended_unfilled(group, parent, leg, c.reason.to_string(), c.ts, true)
        }
        Event::OrderExpired(x) => {
            ended(group, End::Expired);
            leg_ended_unfilled(group, parent, leg, "expired".to_string(), x.ts, true)
        }
        Event::OrderRejected(r) => {
            ended(group, End::Rejected);
            leg_ended_unfilled(group, parent, leg, r.reason.to_string(), r.ts, false)
        }
        // A refused cancel of a leg keeps the exit alive: the advisory is the parent's.
        Event::OrderCancelRejected(mut c) => {
            c.client_order_id = parent.to_string();
            vec![Event::OrderCancelRejected(c)]
        }
        Event::OrderModifyRejected(mut m) => {
            m.client_order_id = parent.to_string();
            vec![Event::OrderModifyRejected(m)]
        }
        // An amend of ONE leg, made at the venue: reported to the engine it would resize the whole order to this
        // leg's size. Dropped, said.
        Event::OrderModified(_) => {
            tracing::error!(
                target: "ctrader",
                parent,
                leg,
                "a leg of an exit that spans positions was AMENDED at the venue; the engine is not told \
                 (it would read one leg's size as the whole order's)"
            );
            Vec::new()
        }
        other => vec![other],
    }
}

/// A leg ended without filling (`cancelled` is false for a rejection, which is only legal before any accept): when it
/// was the last the exit is over and the parent gets its single terminal; otherwise the siblings still rest, the exit
/// covers less than the engine counts, and nothing is passed on.
fn leg_ended_unfilled(
    group: &Group,
    parent: &str,
    leg: &str,
    reason: String,
    ts: i64,
    cancelled: bool,
) -> Vec<Event> {
    if group.legs.iter().any(|(_, end)| end.is_none()) {
        tracing::error!(
            target: "ctrader",
            parent,
            leg,
            %reason,
            "a leg of an exit that spans positions ENDED UNFILLED while its siblings still rest: the exit \
             now covers less than the engine counts, and the engine is not told"
        );
        return Vec::new();
    }
    // The legs are all over. Rejected is legal only from Submitted: after any accept or fill the terminal is a cancel.
    if !cancelled && !group.accepted && !group.filled_any {
        return vec![Event::OrderRejected(OrderRejected {
            client_order_id: parent.to_string(),
            reason: reason.into(),
            ts,
            cause: None,
        })];
    }
    vec![Event::OrderCanceled(OrderCanceled {
        client_order_id: parent.to_string(),
        reason: reason.into(),
        ts,
    })]
}

#[path = "exit_legs_tests.rs"]
#[cfg(test)]
mod exit_legs_tests;
