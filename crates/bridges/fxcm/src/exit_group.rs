//! **One stop or take-profit that covers SEVERAL trades** (decision 0214): N venue orders (`S` or `L`, one per trade, each for
//! its trade's WHOLE size, the venue's rule) that the node knows as ONE order under one client order id. No session, no FFI:
//! the venue is a trait ([`crate::exit::ExitVenue`]) a test scripts.
//!
//! The group is the exec thread's memory of the order. What it decides, each a row of the tests:
//!
//! * **The fills read as ONE order.** Each leg that FIRES is a closing fill under the leg's own order id, routed to the order's
//!   client order id like any fill of it. The core's FSM takes `OrderFilled` once, so every fill but the one that leaves no
//!   leg resting is relabelled `OrderPartiallyFilled` ([`ExitGroup::fold`]); a leg whose fill is delivered twice (a
//!   ForexConnect reconnect re-surfaces trades) counts once.
//! * **A leg the VENUE removes is not the end of the order** (a trade closed another way takes its exits with it, MEASURED):
//!   while other legs rest, the removal reaches the core as `OrderModified` with the quantity the venue still backs, and the
//!   order ends `OrderCanceled` only with its last leg. An order of one leg ends exactly as it always did.
//! * **A cancel removes every leg** and a partial answer is told as what holds ([`ExitGroup::cancel`]).
//! * **An amend may change the price and the SIZE** ([`ExitGroup::amend`]): the price moves every leg (and puts the ones that
//!   moved back when a later one refuses), the size adds or removes WHOLE trades' exits, exactly, never a part of one. What
//!   was applied is told with `OrderModified` and what was not with `OrderModifyRejected`, so the core never believes a
//!   cover the venue does not hold.
//! * **A partial close keeps the exits** (MEASURED: the same order ids, the amount cut to the remainder's, the NEW trade id):
//!   [`ExitGroup::follow_split`] moves the leg to the remainder's id and cuts its size.

use std::collections::HashSet;

use vike_model::events::{Event, OrderModified, OrderModifyRejected, OrderPartiallyFilled};

use crate::event_mapper::venue_rate_differs;
use crate::exit::{Ask, ExitKind, ExitLeg, ExitVenue, place_plan, plan_exits, read_book};

/// The legs of one exit order, and what its fills and the venue's removals have done to it.
#[derive(Debug)]
pub(crate) struct ExitGroup {
    kind: ExitKind,
    /// The ORDER's side: a SELL stop covers LONG trades.
    buy: bool,
    /// FXCM's slashed form (`EUR/USD`).
    instrument: String,
    legs: Vec<ExitLeg>,
    /// Base units of the legs that FIRED.
    filled: i64,
    /// The venue order ids of the legs that fired (a replayed fill is not counted again).
    fired: HashSet<String>,
}

/// What an amend asks, once the core's numbers are checked: the order's new TOTAL size and the new rate, either may be absent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Want {
    pub(crate) qty: Option<i64>,
    pub(crate) rate: Option<f64>,
}

impl Want {
    /// The core's `Modify` (`order_qty` is the order's own size, `new_qty` and `new_price` what it asks for) as a [`Want`],
    /// or the words for a request this venue cannot read. A size equal to the order's is no change. A size must be a whole
    /// number of base units; whether it is a SUM OF WHOLE TRADES is the amend's to judge, against the venue's trades.
    pub(crate) fn of(
        order_qty: f64,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Result<Self, String> {
        let qty = match new_qty {
            Some(q) if (q - order_qty).abs() <= f64::EPSILON * order_qty.abs().max(1.0) => None,
            Some(q) if q.is_finite() && (1.0..9.0e15).contains(&q) && q.fract() == 0.0 => {
                Some(q as i64)
            }
            Some(q) => {
                return Err(format!(
                    "fxcm: the new size {q} is not a positive whole number of base units; amend refused"
                ));
            }
            None => None,
        };
        let rate = match new_price {
            Some(px) if px.is_finite() && px > 0.0 => Some(px),
            Some(px) => {
                return Err(format!("fxcm: amend price {px} is not a positive, finite rate"));
            }
            None => None,
        };
        if qty.is_none() && rate.is_none() {
            return Err(
                "fxcm amends the price or the size of a resting exit, and this amend names neither"
                    .to_string(),
            );
        }
        Ok(Self { qty, rate })
    }
}

/// What an amend came to: what was APPLIED, and why the rest was not.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Amended {
    /// The order's new TOTAL size, when the size was asked for and now stands (or stands in part: see `refused`).
    pub(crate) qty: Option<i64>,
    /// The order's new rate, when the rate was asked for and now stands.
    pub(crate) rate: Option<f64>,
    /// Venue order ids that now rest for this order (their fills and removals route to it).
    pub(crate) added: Vec<String>,
    /// Venue order ids that are gone (their routes are dead).
    pub(crate) removed: Vec<String>,
    /// Why part of the amend was NOT done.
    pub(crate) refused: Option<String>,
}

impl Amended {
    /// The events the core is told, in order: `OrderModified` for what holds, `OrderModifyRejected` for what does not.
    pub(crate) fn events(&self, coid: &str, ts: i64) -> Vec<Event> {
        let mut out = Vec::new();
        if self.qty.is_some() || self.rate.is_some() {
            out.push(Event::OrderModified(OrderModified {
                client_order_id: coid.into(),
                venue_order_id: None,
                new_qty: self.qty.map(|q| q as f64),
                new_price: self.rate,
                ts,
            }));
        }
        if let Some(why) = &self.refused {
            out.push(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid.into(),
                reason: why.clone().into(),
                ts,
            }));
        }
        out
    }
}

/// What a cancel came to.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Cancelled {
    /// Venue order ids that are gone (their routes are dead).
    pub(crate) removed: Vec<String>,
    /// Why legs remain, when some do.
    pub(crate) refused: Option<String>,
}

/// The client order id and timestamp a terminal event carries.
fn terminal_of(ev: &Event) -> Option<(String, i64)> {
    match ev {
        Event::OrderCanceled(c) => Some((c.client_order_id.clone(), c.ts)),
        Event::OrderRejected(r) => Some((r.client_order_id.clone(), r.ts)),
        _ => None,
    }
}

impl ExitGroup {
    pub(crate) fn new(kind: ExitKind, buy: bool, instrument: &str, legs: Vec<ExitLeg>) -> Self {
        Self {
            kind,
            buy,
            instrument: instrument.to_string(),
            legs,
            filled: 0,
            fired: HashSet::new(),
        }
    }

    pub(crate) fn kind(&self) -> ExitKind {
        self.kind
    }

    /// The ORDER's side.
    pub(crate) fn buy(&self) -> bool {
        self.buy
    }

    pub(crate) fn instrument(&self) -> &str {
        &self.instrument
    }

    /// The legs still resting, in the order they were bound (oldest trade first).
    pub(crate) fn legs(&self) -> &[ExitLeg] {
        &self.legs
    }

    /// Base units the resting legs cover.
    pub(crate) fn covered(&self) -> i64 {
        self.legs.iter().map(|l| l.units).sum()
    }

    /// The order's size as the venue backs it: what fired and what still rests.
    pub(crate) fn total(&self) -> i64 {
        self.filled + self.covered()
    }

    /// The rate the legs rest at (the first leg's; they move together, and [`Self::amend`] reads each leg's own).
    pub(crate) fn rate(&self) -> f64 {
        self.legs.first().map_or(0.0, |l| l.rate)
    }

    /// Read the events ONE routed envelope decoded to (`order_id` is the venue order it named, one of this group's legs) as
    /// the order's: `(the events to publish, the order has ended)`.
    pub(crate) fn fold(&mut self, order_id: &str, evs: Vec<Event>) -> (Vec<Event>, bool) {
        let mut out = Vec::with_capacity(evs.len());
        let mut ended = false;
        for ev in evs {
            // The venue removed ONE leg (the trade closed another way, or a leg was rejected after it rested). The echo of
            // a leg that already fired says nothing.
            if let Some((coid, ts)) = terminal_of(&ev) {
                if self.fired.contains(order_id) {
                    continue;
                }
                match self.drop_leg(order_id) {
                    Some(true) => {
                        ended = true;
                        out.push(ev);
                    }
                    Some(false) => out.push(self.narrowed(&coid, ts)),
                    None => {}
                }
                continue;
            }
            match ev {
                Event::OrderFilled(w) => {
                    let first = self.fired.insert(order_id.to_string());
                    if first {
                        self.filled += w.fill.last_qty.round() as i64;
                        self.legs.retain(|l| l.order_id != order_id);
                    }
                    if first && self.legs.is_empty() {
                        ended = true;
                        out.push(Event::OrderFilled(w));
                    } else {
                        out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                            client_order_id: w.client_order_id,
                            fill: w.fill,
                            ts: w.ts,
                        }));
                    }
                }
                other => out.push(other),
            }
        }
        (out, ended)
    }

    /// Forget the leg `order_id`: `Some(true)` = it was the last (the order is over), `Some(false)` = others rest,
    /// `None` = it was not one of this group's.
    fn drop_leg(&mut self, order_id: &str) -> Option<bool> {
        let before = self.legs.len();
        self.legs.retain(|l| l.order_id != order_id);
        (self.legs.len() != before).then_some(self.legs.is_empty())
    }

    /// The core's order, narrowed to the cover the venue still holds.
    pub(crate) fn narrowed(&self, coid: &str, ts: i64) -> Event {
        Event::OrderModified(OrderModified {
            client_order_id: coid.into(),
            venue_order_id: None,
            new_qty: Some(self.total() as f64),
            new_price: None,
            ts,
        })
    }

    /// A trade of this group was PARTIALLY closed: `closed_trade` is closed for `closed_units` and the rest came back as
    /// `remainder`. The venue keeps the exit on the remainder (same order id, the amount cut to the remainder's), so the leg
    /// follows. A trade no leg covers is none of this group's.
    pub(crate) fn follow_split(&mut self, closed_trade: &str, remainder: &str, closed_units: i64) {
        for leg in self.legs.iter_mut().filter(|l| l.trade_id == closed_trade) {
            leg.trade_id = remainder.to_string();
            leg.units = (leg.units - closed_units).max(0);
        }
    }

    /// Cancel the order: remove EVERY leg, trying all of them (a leg that will not go must not keep the others resting),
    /// inside the command's time bound. Legs that remain stay in the group, and the words say which.
    pub(crate) fn cancel(&mut self, venue: &mut impl ExitVenue) -> Cancelled {
        let mut out = Cancelled::default();
        let (mut kept, mut failed) = (Vec::new(), Vec::new());
        let mut late = false;
        for (i, leg) in std::mem::take(&mut self.legs).into_iter().enumerate() {
            late = late || (i > 0 && !venue.may_start());
            if late {
                kept.push(leg);
                continue;
            }
            match venue.delete(&leg) {
                Ok(()) => out.removed.push(leg.order_id),
                Err(e) => {
                    failed.push(format!("order {} on trade {} ({e})", leg.order_id, leg.trade_id));
                    kept.push(leg);
                }
            }
        }
        self.legs = kept;
        if !self.legs.is_empty() {
            let mut why = format!(
                "fxcm: {} of the {} exits of this order are still resting",
                self.legs.len(),
                self.legs.len() + out.removed.len()
            );
            if !failed.is_empty() {
                why.push_str(&format!(": {}", failed.join(", ")));
            }
            if late {
                why.push_str("; the time bound of one command ran out before the rest were tried");
            }
            out.refused = Some(why);
        }
        out
    }

    /// Apply `want`: the RATE first (every leg that is not at it, putting the ones that moved back when a later one
    /// refuses), then the SIZE (whole trades' exits added or removed). What was applied is in the answer even when the rest
    /// was refused, because the core must be told what holds.
    pub(crate) fn amend(
        &mut self,
        want: Want,
        ours: &impl Fn(&str) -> bool,
        venue: &mut impl ExitVenue,
    ) -> Amended {
        let mut out = Amended::default();
        if let Some(rate) = want.rate {
            match self.reprice(rate, venue) {
                Ok(()) => out.rate = Some(rate),
                Err(why) => {
                    out.refused = Some(why);
                    return out;
                }
            }
        }
        if let Some(total) = want.qty {
            if total == self.total() {
                out.qty = Some(total);
            } else {
                match self.resize(total, ours, venue, &mut out) {
                    Ok(()) => out.qty = Some(self.total()),
                    Err(why) => out.refused = Some(why),
                }
            }
        }
        out
    }

    /// Move every leg to `rate`. A leg that refuses (or the time bound) puts back the ones this call moved.
    fn reprice(&mut self, rate: f64, venue: &mut impl ExitVenue) -> Result<(), String> {
        let word = self.kind.word();
        let mut moved: Vec<(usize, f64)> = Vec::new();
        let mut failure = None;
        for i in 0..self.legs.len() {
            if !venue_rate_differs(rate, self.legs[i].rate, 0.0) {
                continue;
            }
            if !moved.is_empty() && !venue.may_start() {
                failure = Some(format!(
                    "the time bound of one command ran out before the {word} on trade {} was moved",
                    self.legs[i].trade_id
                ));
                break;
            }
            match venue.edit(&self.legs[i], rate) {
                Ok(()) => {
                    moved.push((i, self.legs[i].rate));
                    self.legs[i].rate = rate;
                }
                Err(e) => {
                    failure = Some(format!(
                        "the {word} on trade {} was not moved: {e}",
                        self.legs[i].trade_id
                    ));
                    break;
                }
            }
        }
        let Some(why) = failure else {
            return Ok(());
        };
        let mut stuck = Vec::new();
        for (i, old) in moved {
            match venue.edit(&self.legs[i], old) {
                Ok(()) => self.legs[i].rate = old,
                Err(e) => stuck.push(format!(
                    "order {} on trade {} stays at {rate} ({e})",
                    self.legs[i].order_id, self.legs[i].trade_id
                )),
            }
        }
        Err(if stuck.is_empty() {
            format!("fxcm: {why}; the exits that had moved were put back, so no rate changed")
        } else {
            format!(
                "fxcm: {why}; these could NOT be put back, so the exits rest at different rates until the next amend: {}",
                stuck.join(", ")
            )
        })
    }

    /// Make the legs cover `total - filled` units: remove whole legs from the END, or add legs on the oldest free trades.
    /// Exactly that, or a refusal that applies nothing; a removal that stops halfway is told as what holds (`out`).
    fn resize(
        &mut self,
        total: i64,
        ours: &impl Fn(&str) -> bool,
        venue: &mut impl ExitVenue,
        out: &mut Amended,
    ) -> Result<(), String> {
        let word = self.kind.word();
        let want = total - self.filled;
        if want <= 0 {
            return Err(format!(
                "fxcm: the new size {total} leaves nothing for the {word} to cover ({} already fired); cancel the order instead",
                self.filled
            ));
        }
        let have = self.covered();
        if want < have {
            return self.shrink(want, venue, out);
        }
        // Grow: the oldest free trades (or exits already resting on them at this rate) for the units still wanted.
        let need = want - have;
        let (trades, exits) = read_book(venue)?;
        let ask = Ask {
            instrument: &self.instrument,
            buy: self.buy,
            kind: self.kind,
            units: need,
            rate: self.rate(),
        };
        let plan = plan_exits(&trades, &exits, &ask, ours)?;
        let got: i64 = plan.iter().map(|s| s.units).sum();
        if got != need {
            return Err(format!(
                "fxcm: only {got} more units of open trades are free for the {word} (asked for {need} more): a {word} \
                 never covers more than the position holds"
            ));
        }
        match place_plan(&ask, &plan, venue) {
            Ok(added) => {
                out.added.extend(added.iter().map(|l| l.order_id.clone()));
                self.legs.extend(added);
                Ok(())
            }
            Err((unknown, text, _)) => Err(if unknown {
                format!(
                    "{text} (the {word} sent last MAY rest at the venue: reconcile will list it)"
                )
            } else {
                text
            }),
        }
    }

    /// Remove whole legs from the END until `want` units are covered; `want` must be the sum of the first legs.
    fn shrink(
        &mut self,
        want: i64,
        venue: &mut impl ExitVenue,
        out: &mut Amended,
    ) -> Result<(), String> {
        let word = self.kind.word();
        let (mut keep, mut sum) = (0, 0);
        for (i, l) in self.legs.iter().enumerate() {
            if sum >= want {
                break;
            }
            sum += l.units;
            keep = i + 1;
        }
        if sum != want {
            let mut running = self.filled;
            let totals: Vec<String> = self
                .legs
                .iter()
                .map(|l| {
                    running += l.units;
                    running.to_string()
                })
                .collect();
            let trades: Vec<String> =
                self.legs.iter().map(|l| format!("{} ({})", l.trade_id, l.units)).collect();
            return Err(format!(
                "fxcm: the {word} covers whole trades ({}), so a smaller size must be one of these totals: {}; the venue \
                 refuses an exit for a part of a trade (ORA-20134)",
                trades.join(", "),
                totals.join(", ")
            ));
        }
        while self.legs.len() > keep {
            let last = self.legs.len() - 1;
            let why = if !out.removed.is_empty() && !venue.may_start() {
                "the time bound of one command ran out".to_string()
            } else {
                match venue.delete(&self.legs[last]) {
                    Ok(()) => {
                        out.removed.push(self.legs.remove(last).order_id);
                        continue;
                    }
                    Err(e) => format!(
                        "order {} on trade {} was not removed: {e}",
                        self.legs[last].order_id, self.legs[last].trade_id
                    ),
                }
            };
            // What holds is told: the size the venue still backs.
            out.qty = (!out.removed.is_empty()).then(|| self.total());
            return Err(format!(
                "fxcm: the size was lowered only as far as {}: {why}",
                self.total()
            ));
        }
        Ok(())
    }
}

#[path = "exit_group_tests.rs"]
#[cfg(test)]
mod exit_group_tests;
