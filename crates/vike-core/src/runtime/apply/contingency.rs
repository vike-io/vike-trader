//! The core-emulated OTO/OCO/OUO drive: fill and terminal cascades, held and conditional scope
//! clears, and the double-fill note.
//!
//! An OUO leg's fill (a ladder's target or stop, partial fills included) RESIZES its siblings: the
//! quantity it just filled is spent over them by `vike_execution::spend_ouo_reduce` (the one law the paper
//! book and the simulator spend by too), and each sibling is changed through the amend of
//! `apply/amend.rs` (decision 0244: in place where the venue can, else a reduce-only stop is placed
//! NEW first) or cancelled when nothing of it is left. A sibling that fills after its cancel was sent
//! (the venue filled both legs before the cancel landed) is noted on the ring as a DOUBLE FILL,
//! judged at the drain-loop boundary, never on the fold.
//!
//! Two facts are kept between fills ([`ContingencyOuoLegs`]), so fast fills neither oversize a leg nor count
//! twice. A leg's SIZE is driven toward the size its fills so far give it, not by deltas: the spend
//! reads the leg's wanted size (the registry lags an amend in flight), a fill that lands while the
//! leg's cancel-then-new amend is in flight only moves the wanted size, and when that amend ends one
//! more resize sends what changed. Each OUO order's fills already DRIVEN are counted by its order client id, so a
//! terminal or an amend's re-key drives only what was not (an old order's fills after the re-key
//! reach no drive of their own).

use super::*;
use vike_model::OrderType;

/// A live leg the fill drive sent a cancel to, watched until it ends: did it fill after all?
/// Core-local, never snapped (a restart reads the venue).
#[derive(Debug)]
pub(crate) struct ContingencyRace {
    leg: String,
    /// the leg whose fill sent the cancel
    twin: String,
    engine_index: usize,
    /// what `leg` had filled when the cancel was sent
    filled: f64,
}

/// The OUO legs' state between fills (this module's doc). Core-local, never snapped or journaled: a
/// restart reads the registry, and `CoreThread::seed_ouo_driven` counts a restored leg's fills driven.
#[derive(Debug, Default)]
pub(crate) struct ContingencyOuoLegs {
    /// A live leg's WANTED unfilled size, keyed by its book order client id (re-keyed with it), once a sibling's
    /// fill has moved it; `asked` is the size its cancel-then-new amend in flight places.
    sizes: indexmap::IndexMap<String, OuoSize>,
    /// The quantity of each OUO order's fills already driven, keyed by the ORDER's order client id (an amend's
    /// old and new orders apart). Absent = nothing driven yet.
    driven: std::collections::HashMap<String, f64>,
}

#[derive(Debug)]
struct OuoSize {
    want: f64,
    asked: Option<f64>,
}

impl ContingencyOuoLegs {
    /// The book re-keyed the leg `old` as `new` (an amend's replacement is answered).
    pub(crate) fn rekey(&mut self, old: &str, new: &str) {
        if let Some(s) = self.sizes.shift_remove(old) {
            self.sizes.insert(new.to_string(), s);
        }
    }

    /// What of the order `order_client_id`'s fills (`filled` in all) was not driven yet; from here it all is.
    pub(crate) fn undriven(&mut self, order_client_id: &str, filled: f64) -> f64 {
        let before = self.driven.insert(order_client_id.to_string(), filled).unwrap_or(0.0);
        (filled - before).max(0.0)
    }

    /// The order `order_client_id` is done: its count goes.
    pub(crate) fn forget_order(&mut self, order_client_id: &str) {
        self.driven.remove(order_client_id);
    }

    /// The leg `leg` left the book: everything kept of it goes.
    fn forget_leg(&mut self, leg: &str) {
        self.sizes.shift_remove(leg);
        self.driven.remove(leg);
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Drive the book for one folded event's [`ContingencyDrive`]: a fill (full or an OUO leg's
    /// partial) or an end without a fill. An OUO leg's fill drives what of its order's fills was not
    /// driven yet (the event's own quantity, unless an earlier fill reached no drive).
    pub(crate) fn drive_contingency(
        &mut self,
        order_client_id: &str,
        drive: ContingencyDrive,
        now: i64,
    ) {
        match drive {
            ContingencyDrive::Fill { qty, price: px, full } => {
                let qty = if self.contingency.kind_of(order_client_id)
                    == Some(vike_model::ContingencyType::Ouo)
                {
                    let engine_index =
                        self.order_client_id_venue.get(order_client_id).copied().unwrap_or(0);
                    let filled = self
                        .engine(engine_index)
                        .registry
                        .get(order_client_id)
                        .map_or(qty, |mo| mo.filled_qty);
                    self.ouo.undriven(order_client_id, filled)
                } else {
                    qty
                };
                self.drive_contingency_on_fill(order_client_id, qty, px, full, now);
            }
            ContingencyDrive::Ended => self.drive_contingency_on_terminal(order_client_id),
        }
    }

    /// Count every restored OUO leg's fills as driven (they were, before the restart), so the first
    /// fill after it drives only itself. Once, at assembly.
    pub(crate) fn seed_ouo_driven(&mut self) {
        for leg in self.contingency.snapshot() {
            if leg.kind != Some(vike_model::ContingencyType::Ouo) {
                continue;
            }
            let engine_index =
                self.order_client_id_venue.get(&leg.order_client_id).copied().unwrap_or(0);
            if let Some(filled) =
                self.engine(engine_index).registry.get(&leg.order_client_id).map(|mo| mo.filled_qty)
            {
                self.ouo.driven.insert(leg.order_client_id, filled);
            }
        }
    }

    /// Live-runtime OTO/OCO/OUO fill drive: after `filled_order_client_id` filled `qty` at `px` (that fill's
    /// own quantity, never the order's cumulative one) and the engine has folded it. A FULL fill
    /// ARMS this leg's held OTO children (submit each to the venue — the parent's fill is their
    /// trigger), CANCELS its OCO siblings and RESIZES its OUO ones; a PARTIAL fill resizes an OUO
    /// leg's siblings and nothing else (the group rests, as it always did for any other leg). The
    /// DECISION is the shared [`vike_execution::ContingencyBook`] resolver — the SAME `on_fill` the
    /// paper/backtest oracle drives, so the live path cannot diverge from it; this method owns only the
    /// book MECHANICS (release the held request, cancel or resize the resting sibling). A no-op for a
    /// plain fill (not in the book). COLD path — per FILL, never the market-data fold.
    pub(crate) fn drive_contingency_on_fill(
        &mut self,
        filled_order_client_id: &str,
        qty: f64,
        px: f64,
        full: bool,
        now: i64,
    ) {
        if self.contingency.is_empty() {
            return;
        }
        if !full {
            if self.contingency.kind_of(filled_order_client_id)
                == Some(vike_model::ContingencyType::Ouo)
            {
                // the leg's own fill takes from its own wanted size
                if let Some(s) = self.ouo.sizes.get_mut(filled_order_client_id) {
                    s.want = (s.want - qty).max(0.0);
                }
                let siblings = self.contingency.siblings_of(filled_order_client_id);
                self.resize_ouo_siblings(filled_order_client_id, siblings, qty, px, now);
            }
            return;
        }
        // `on_fill` arms every held child of the filled leg (flips it active) AND resolves the leg's
        // siblings (its `linked` ids that are NOT its own children), in `linked` order: an OCO leg's
        // to cancel, an OUO leg's to resize.
        let res = self.contingency.on_fill(filled_order_client_id, qty);
        let siblings = res.cancel;
        // RELEASE the just-armed OTO children: a child of the filled leg that is now active has its
        // held request drained + submitted to the venue. (A child still held — a deeper OTO tier not
        // armed by THIS fill — is left resting.)
        for child in self.contingency.children_of(filled_order_client_id) {
            if !self.contingency.is_held(&child)
                && let Some(req) = self.held_orders.shift_remove(&child)
            {
                // the spot rule counts the child with its group's other legs at the group's claim
                let engine_index = self.held_release_engine(&child, &req.venue);
                let linked = self.contingency.linked_exits(&child);
                self.engine_mut(engine_index).linked_exits = linked;
                self.submit_resolved(&req, now);
                self.engine_mut(engine_index).linked_exits = None;
                self.note(format!("OTO child {child} released by {filled_order_client_id} fill"));
            }
        }
        // CANCEL the OCO siblings: one still HELD (never went live) is simply dropped from the held
        // map; one resting at the venue is canceled (`cancel_order` is `is_live`-guarded and the
        // venue emits the authoritative `OrderCanceled`). Each leaves the book.
        //
        // Unclassified (`OrderCancelIntent::Unspecified`) on purpose: the sibling is removed from the
        // contingency book on the SAME pass, so nothing re-issues this cancel — a venue that held
        // it back to protect its own budget would strand a live protective leg whose OCO twin has
        // already filled. Book maintenance is not "routine churn" in the shed-and-re-offer sense.
        //
        // ⚠ A cancel the engine did NOT issue for a sibling the book still lists is WARNED, once per
        // sibling: after a restart the book can be restored while the registry is not (an order
        // nothing adopted), `cancel_order` is then a no-op, and the leg may still rest at the venue.
        // The link leaves the book all the same: nothing consumes a kept link, it would only leak,
        // make `contingency_terminal` clone per order event and later print "died unfilled"; the
        // WARN is what tells the operator to look at the venue. A sibling the book no longer lists
        // is a leg that already resolved (the venue cancelled it first, or it died unfilled):
        // nothing to say, as before.
        for sib in siblings {
            self.cancel_fill_sibling(&sib, filled_order_client_id, "OCO");
        }
        let reduce: Vec<String> = res.reduce.into_iter().map(|(c, _)| c).collect();
        if !reduce.is_empty() {
            self.resize_ouo_siblings(filled_order_client_id, reduce, qty, px, now);
        }
        // The filled leg leaves the book (it terminalized). Its just-armed children STAY (active/live)
        // so a later child fill can still resolve its own OCO sibling.
        // (An order the book does not hold, such as an amend's old order after the re-key, keeps its
        // driven count: the amend's end reads it.)
        if self.contingency.contains(filled_order_client_id) {
            self.ouo.forget_leg(filled_order_client_id);
        }
        self.contingency.remove(filled_order_client_id);
        // NB: no `pump_client` here on purpose. This runs from BOTH fold sites — the `Ingest::Event`
        // arm AND `pump_client`'s own drain loop (client-synthesized fills, e.g. the paper exchange).
        // Pumping here would re-enter `pump_client` from inside its own loop; instead each call site
        // pumps after driving, and the `pump_client` loop naturally drains the events this queued.
    }

    /// Cancel the sibling `sib` that `filled`'s fill resolves away (`law`: `OCO`, or `OUO` for one
    /// reduced to nothing), and leave the book of it. One still HELD (never went live) is dropped from
    /// the held map; one resting at the venue is cancelled and watched ([`ContingencyRace`]); an OCO
    /// sibling that has ALREADY filled some is a double fill, said at once.
    fn cancel_fill_sibling(&mut self, sib: &str, filled: &str, law: &str) {
        if self.held_orders.shift_remove(sib).is_some() {
            self.note(format!("{law} sibling {sib} dropped (was held) on {filled} fill"));
        } else {
            let engine_index = self.order_client_id_venue.get(sib).copied().unwrap_or(0);
            let had = self.engine(engine_index).registry.get(sib).map_or(0.0, |mo| mo.filled_qty);
            if law == "OCO" && had > 0.0 {
                self.note_double_fill(sib, filled, had);
            }
            if !self.engine_mut(engine_index).cancel_order(sib) && self.contingency.contains(sib) {
                // A row that exists but is terminal is "not live"; no row at all is "not in".
                let why = if self.engine(engine_index).registry.contains_key(sib) {
                    "not live in the registry"
                } else {
                    "not in the registry"
                };
                tracing::warn!(
                    target: "vike_core::core",
                    filled,
                    "{law} sibling {sib} NOT cancelled: {why}"
                );
                self.note(format!(
                    "{law} sibling {sib} NOT cancelled: {why} (its twin {filled} filled; \
                     check the sibling at the venue)"
                ));
            } else {
                self.note(format!("{law} sibling {sib} canceled on {filled} fill"));
                self.contingency_races.push(ContingencyRace {
                    leg: sib.to_string(),
                    twin: filled.to_string(),
                    engine_index,
                    filled: had,
                });
            }
        }
        self.contingency.remove(sib);
        self.ouo.forget_leg(sib);
    }

    /// Spend `filled`'s fill of `qty` at `px` over its OUO `siblings` by `vike_execution::spend_ouo_reduce`:
    /// a ladder's stop SHARES it, `qty` IN TOTAL with the target farthest from `px` first; any other
    /// OUO leg takes `qty` off EACH sibling (`ContingencyBook::ouo_shares_its_fill`, ruling 3 of the
    /// wave). Then carry out what each is left at: held, its request is resized in place (it is not at
    /// the venue, and its release is judged whole); live, it is amended (decision 0244: native, or a
    /// reduce-only stop placed new first); nothing left, it is cancelled. A sibling in neither the held
    /// map nor a registry has gone and takes nothing. Asked before the book drops anything of the fill.
    #[cold]
    #[inline(never)]
    fn resize_ouo_siblings(
        &mut self,
        filled: &str,
        siblings: Vec<String>,
        qty: f64,
        px: f64,
        now: i64,
    ) {
        let shares = self.contingency.ouo_shares_its_fill(filled);
        let legs: Vec<(String, f64, f64)> = siblings
            .into_iter()
            .filter_map(|s| {
                let (req, left) = match self.held_orders.get(&s) {
                    Some(req) => (req, req.qty),
                    None => {
                        let engine_index = self.order_client_id_venue.get(&s).copied().unwrap_or(0);
                        let mo = self
                            .engine(engine_index)
                            .registry
                            .get(&s)
                            .filter(|mo| mo.status.is_live())?;
                        // the size an earlier fill gave it, which an amend in flight has not moved
                        // the registry to yet
                        let wanted = self.ouo.sizes.get(&s).map(|w| w.want);
                        (&mo.request, wanted.unwrap_or(mo.request.qty - mo.filled_qty))
                    }
                };
                let at = req.trigger_price.or(req.price).unwrap_or(px);
                Some((s, at, left))
            })
            .collect();
        let spent: Vec<(String, Option<f64>)> = if shares {
            vike_execution::spend_ouo_reduce(qty, px, legs)
        } else {
            legs.into_iter()
                .flat_map(|l| vike_execution::spend_ouo_reduce(qty, px, vec![l]))
                .collect()
        };
        for (sib, left) in spent {
            match left {
                None => self.cancel_fill_sibling(&sib, filled, "OUO"),
                Some(left) => self.resize_ouo_leg(&sib, left, filled, now),
            }
        }
    }

    /// Resize the OUO leg `sib` to `left` unfilled, `filled`'s fill having spent the rest: the leg's
    /// wanted size becomes `left`, and it is sent unless a cancel-then-new amend of the leg is in
    /// flight, whose end sends it ([`Self::resize_after_amend`]).
    fn resize_ouo_leg(&mut self, sib: &str, left: f64, filled: &str, now: i64) {
        if let Some(held) = self.held_orders.get_mut(sib) {
            held.qty = left;
            self.note(format!("OUO sibling {sib} resized to {left} (held) on {filled} fill"));
            return;
        }
        let size =
            self.ouo.sizes.entry(sib.to_string()).or_insert(OuoSize { want: left, asked: None });
        size.want = left;
        if self.amend_in_flight(sib) {
            self.note(format!(
                "OUO sibling {sib} to be resized to {left} on {filled} fill, once its amend in flight \
                 ends"
            ));
            return;
        }
        self.send_ouo_size(sib, left, &format!("on {filled} fill"), now);
    }

    /// Send the OUO leg `sib`'s size `left` through the amend (decision 0244), and remember the size a
    /// cancel-then-new amend it starts is placing. `why` ends the note.
    fn send_ouo_size(&mut self, sib: &str, left: f64, why: &str, now: i64) {
        let engine_index = self.order_client_id_venue.get(sib).copied().unwrap_or(0);
        let engine = self.engine(engine_index);
        let Some(mo) = engine.registry.get(sib) else { return };
        // The amend's quantity is what the venue reads it as (`vike_venues::venue_amend`): a
        // replacement and an in-place REMAINING amend rest `left`, an in-place TOTAL one counts the lots
        // the order has already executed too.
        let new_qty = if engine.amend_mode_of(&mo.request)
            == Some(vike_venues::venue_amend_mode::AmendMode::CancelThenNew)
        {
            left
        } else {
            let semantics = engine
                .client
                .amend_semantics()
                .unwrap_or_else(|| vike_venues::amend_semantics(&engine.venue));
            semantics.already_in_position(mo.filled_qty) + left
        };
        self.note(format!("OUO sibling {sib} resized to {left} {why}"));
        self.modify_resting(sib.to_string(), Some(new_qty), None, None, now);
        if self.amend_in_flight(sib)
            && let Some(s) = self.ouo.sizes.get_mut(sib)
        {
            s.asked = Some(left);
        }
    }

    /// A cancel-then-new amend of the order `old` ended (`new` its replacement, empty when none was
    /// placed): when the OUO leg it was (the book keys it by `new` once that was answered) is wanted at
    /// another size than the amend placed, because a fill moved it while the amend was in flight, the
    /// one more resize that size needs is sent now. Asked from `finish_amend`, after the book drive.
    pub(crate) fn resize_after_amend(&mut self, old: &str, new: &str, now: i64) {
        let leg = if !new.is_empty() && self.contingency.contains(new) { new } else { old };
        let Some(s) = self.ouo.sizes.get_mut(leg) else { return };
        let (want, asked) = (s.want, s.asked.take());
        let engine_index = self.order_client_id_venue.get(leg).copied().unwrap_or(0);
        let Some(rests) =
            self.engine(engine_index).registry.get(leg).map(|mo| mo.request.qty - mo.filled_qty)
        else {
            return;
        };
        // The amend that placed this very size has had its answer, refused or not: no loop on a refusal.
        let answered = asked.is_some_and(|a| (a - want).abs() <= 1e-12);
        if (want - rests).abs() > 1e-12 && !answered && self.contingency.contains(leg) {
            let leg = leg.to_string();
            self.send_ouo_size(&leg, want, "once its amend ended", now);
        }
    }

    /// Say a double fill: `leg` filled `qty` though `twin`'s fill had resolved it away (both legs of
    /// the group traded). Alert only: the position is the venue's to check.
    #[cold]
    #[inline(never)]
    fn note_double_fill(&mut self, leg: &str, twin: &str, qty: f64) {
        tracing::warn!(target: "vike_core::core", leg, twin, qty, "contingency DOUBLE FILL");
        self.note(format!(
            "DOUBLE FILL: {leg} filled {qty} though its twin {twin} filled first (the venue filled \
             both before the cancel landed): check the position at the venue"
        ));
    }

    /// The drain-loop boundary's look at the legs a fill sent a cancel to ([`ContingencyRace`]): one
    /// that ended having filled more than it had is a double fill, said once; one that ended any other
    /// way, or left the registry, is forgotten; one still live is kept. The caller has gated on
    /// `!self.contingency_races.is_empty()`.
    pub(crate) fn note_contingency_races(&mut self) {
        let races = std::mem::take(&mut self.contingency_races);
        for r in races {
            let Some(mo) = self.engine(r.engine_index).registry.get(&r.leg) else { continue };
            if !mo.status.is_terminal() {
                self.contingency_races.push(r);
                continue;
            }
            if mo.filled_qty > r.filled + 1e-12 {
                let extra = mo.filled_qty - r.filled;
                self.note_double_fill(&r.leg, &r.twin, extra);
            }
        }
    }

    /// Live-runtime OTO/OCO terminal drive: a contingency leg that terminalized WITHOUT filling
    /// (canceled / rejected / DENIED / expired) can never arm its still-held OTO children —
    /// cascade-drop them (they were never sent to the venue, so there is nothing to cancel there) and
    /// drop the terminated leg's OWN book entry (stop the stale-entry leak). The paper oracle's
    /// `expire_children_of` on the live path, plus own-entry cleanup. Already-armed (live) children
    /// are LEFT — they are real resting orders now, cleaned by their own terminal event.
    ///
    /// A dying PROTECTIVE EXIT (a released leg with a parent — e.g. a stop-loss the venue rejected)
    /// is SURFACED (warn + recent-events note) so the operator knows a leg of the protection died;
    /// when it was a STOP and no other stop covers the position it protected, a second line,
    /// `POSITION WITHOUT STOP` ([`Self::note_bracket_stop_died`]), says the position is naked now.
    /// Its surviving OCO sibling's fate is the [`crate::CoreConfig::oco_cancel_sibling_on_dead_exit`]
    /// knob:
    ///
    /// - **OFF (default)** — the sibling is KEPT, not auto-canceled: a position that just lost one
    ///   protective leg retains whatever protection it still has (a naked position is the worse
    ///   default). Byte-identical to the feature as first shipped; only a FILL cancels a sibling.
    /// - **ON** — the surviving OCO sibling is ALSO canceled and cleaned from the book, through the
    ///   SAME mechanics the fill-driven OCO-cancel uses ([`Self::drive_contingency_on_fill`]): a
    ///   still-held sibling is dropped, a resting one is canceled at the venue (its authoritative
    ///   `OrderCanceled` re-enters here, but the book no longer holds it, so that is a no-op). A
    ///   deployment that prefers a fully flat book over partial protection opts into this.
    pub(crate) fn drive_contingency_on_terminal(&mut self, order_client_id: &str) {
        if self.contingency.is_empty() || !self.contingency.contains(order_client_id) {
            return;
        }
        let ouo =
            self.contingency.kind_of(order_client_id) == Some(vike_model::ContingencyType::Ouo);
        if ouo {
            // An OUO leg's fills that reached no drive are spent before it goes: only what of them was
            // not driven yet, so none is spent twice.
            let engine_index =
                self.order_client_id_venue.get(order_client_id).copied().unwrap_or(0);
            let engine = self.engine(engine_index);
            if let Some((filled, px, now)) = engine
                .registry
                .get(order_client_id)
                .map(|mo| (mo.filled_qty, mo.average_fill_price, engine.now_ms))
            {
                let undriven = self.ouo.undriven(order_client_id, filled);
                if undriven > 1e-12 {
                    self.drive_contingency_on_fill(order_client_id, undriven, px, false, now);
                }
            }
        }
        // Snapshot whether this is a dead protective exit BEFORE the removals below borrow the book.
        let dead_exit_parent = self.contingency.parent_of(order_client_id).map(str::to_string);
        // The dead exit's surviving OCO siblings, captured NOW, before its own book entry is removed
        // below — that removal drops the `linked` list `siblings_of` reads. They are what the keep line
        // below names (is one of them a STOP?), and, with the opt-in knob
        // (`CoreConfig::oco_cancel_sibling_on_dead_exit`, default OFF), what is cancelled: OFF leaves
        // `doomed_siblings` empty and the cancel block inert, so the keep-protection default is unchanged.
        let linked_siblings: Vec<String> = if dead_exit_parent.is_some() {
            self.contingency.siblings_of(order_client_id)
        } else {
            Vec::new()
        };
        // The ones the book still lists survive: a sibling it dropped already resolved.
        let survivors: Vec<String> =
            linked_siblings.iter().filter(|sib| self.contingency.contains(sib)).cloned().collect();
        // An OUO leg's death cancels none of its siblings, whatever the knob says: OUO resizes, and a
        // ladder's stop cancelled because a target died would leave the position without one.
        let doomed_siblings: Vec<String> = if self.config.oco_cancel_sibling_on_dead_exit && !ouo {
            linked_siblings
        } else {
            Vec::new()
        };
        let mut queue = vec![order_client_id.to_string()];
        while let Some(parent) = queue.pop() {
            for child in self.contingency.children_of(&parent) {
                if self.contingency.is_held(&child) {
                    self.held_orders.shift_remove(&child);
                    self.contingency.remove(&child);
                    self.note(format!("OTO child {child} dropped: parent {parent} terminated"));
                    queue.push(child);
                }
            }
        }
        self.contingency.remove(order_client_id);
        self.ouo.forget_leg(order_client_id);
        let Some(parent) = dead_exit_parent else {
            return;
        };
        // A dead STOP leaves the position it protected with none: say so, once (alert only; the
        // lines below keep the take-profit and, with the knob, cancel it). Per dead exit, never
        // per event: `reconcile_stops.rs` carries the rule and the reasons.
        self.note_bracket_stop_died(order_client_id, &parent);
        if doomed_siblings.is_empty() {
            self.say_dead_exit_kept(order_client_id, &parent, &survivors);
            return;
        }
        // KNOB ON: cancel every surviving OCO sibling too — the same mechanics as the fill-driven
        // cancel (`drive_contingency_on_fill`): a still-held sibling is dropped, a resting one is
        // canceled at the venue; each leaves the book.
        for sib in doomed_siblings {
            if self.held_orders.shift_remove(&sib).is_some() {
                self.note(format!(
                    "OCO sibling {sib} dropped (was held): protective exit {order_client_id} died unfilled"
                ));
            } else {
                let engine_index = self.order_client_id_venue.get(&sib).copied().unwrap_or(0);
                self.engine_mut(engine_index).cancel_order(&sib);
                self.note(format!(
                    "OCO sibling {sib} canceled: protective exit {order_client_id} died unfilled"
                ));
            }
            self.contingency.remove(&sib);
        }
        tracing::warn!(
            target: "vike_core::core",
            order_client_id,
            parent = %parent,
            "protective exit leg terminated UNFILLED — sibling-cancel knob ON: the surviving OCO \
             sibling was canceled (book left flat)"
        );
        self.note(format!(
            "protective exit {order_client_id} died unfilled (parent {parent}); surviving OCO sibling CANCELED"
        ));
    }

    /// **The words of a dead protective exit whose siblings were KEPT** (knob OFF, or nothing left to cancel): surface
    /// the death and say what protection the position still has. Only a STOP among the survivors is protection: a
    /// take-profit closes a winner and leaves the loss open, and with no survivor at all the bracket has nothing left,
    /// so the line says which of the three it is. Cold and out of line on purpose: [`Self::drive_contingency_on_terminal`]
    /// is called from the fold's `dispatch`, so the words must not grow it.
    #[cold]
    #[inline(never)]
    fn say_dead_exit_kept(&mut self, order_client_id: &str, parent: &str, survivors: &[String]) {
        if survivors.iter().any(|sib| self.leg_is_stop(sib)) {
            tracing::warn!(
                target: "vike_core::core",
                order_client_id,
                parent = %parent,
                "protective exit leg terminated UNFILLED — the position keeps its remaining OCO \
                 protection (the surviving sibling is not auto-canceled)"
            );
            self.note(format!(
                "protective exit {order_client_id} died unfilled (parent {parent}); surviving OCO sibling KEPT"
            ));
        } else if survivors.is_empty() {
            tracing::warn!(
                target: "vike_core::core",
                order_client_id,
                parent = %parent,
                "protective exit leg terminated UNFILLED — no OCO sibling is left, so this bracket \
                 leaves the position with NO stop"
            );
            self.note(format!(
                "protective exit {order_client_id} died unfilled (parent {parent}); no OCO sibling left: NO stop \
                 from this bracket"
            ));
        } else {
            let sibs = survivors.join(", ");
            tracing::warn!(
                target: "vike_core::core",
                order_client_id,
                parent = %parent,
                survivors = %sibs,
                "protective exit leg terminated UNFILLED — the surviving OCO sibling is not a stop \
                 (a take-profit closes a winner and leaves the loss open), so the position has NO \
                 stop from this bracket; the sibling is not auto-canceled"
            );
            self.note(format!(
                "protective exit {order_client_id} died unfilled (parent {parent}); surviving OCO sibling {sibs} \
                 KEPT, but it is not a stop: NO stop from this bracket"
            ));
        }
    }

    /// Whether the contingency leg `order_client_id` is a STOP (`Stop` or `StopLimit`), the one kind of exit that protects a
    /// position: a take-profit closes a winner and leaves the loss open. A leg the core still holds is read from the
    /// held map, a live one from its engine's registry; a leg found in neither is not claimed to be a stop.
    fn leg_is_stop(&self, order_client_id: &str) -> bool {
        let kind = match self.held_orders.get(order_client_id) {
            Some(req) => Some(&req.order_type),
            None => {
                let engine_index =
                    self.order_client_id_venue.get(order_client_id).copied().unwrap_or(0);
                self.engine(engine_index)
                    .registry
                    .get(order_client_id)
                    .map(|mo| &mo.request.order_type)
            }
        };
        matches!(kind, Some(OrderType::Stop | OrderType::StopLimit))
    }

    /// Cancel a HELD (not-yet-at-venue) contingency leg: drop it from `held_orders` and the book.
    /// Returns `true` iff `order_client_id` was held (so a cancel caller skips the venue path). A held leg has no
    /// registered order to terminalize, so this NOTES the removal rather than emitting an
    /// `OrderCanceled` the FSM would drop as an unknown order client id. Its OTO parent's `linked` list still
    /// names it, but `on_fill`/`children_of` resolve by the BOOK, from which it is now gone, so the
    /// parent's later fill neither releases nor references it.
    pub(crate) fn cancel_held(&mut self, order_client_id: &str) -> bool {
        if self.held_orders.shift_remove(order_client_id).is_some() {
            self.contingency.remove(order_client_id);
            self.note(format!(
                "held bracket exit {order_client_id} canceled before its parent filled"
            ));
            true
        } else {
            false
        }
    }

    /// Drop every HELD contingency leg matching a `(venue, symbol)` scope (`None` = any) from
    /// `held_orders` + the book — the `MassCancel` scoped twin of [`Self::cancel_held`]. Live legs are
    /// NOT touched here (the engine's `mass_cancel` cancels those at the venue; their `OrderCanceled`
    /// cleans the book).
    ///
    /// ⚠ **The scope is a VENUE, never an account, and that is now the right shape rather than an
    /// oversight.** A held exit is matched on its `OrderRequest::venue` — the canonical exchange id
    /// both accounts of a venue carry — so this drops both accounts' held exits. It was ASYMMETRIC
    /// while the arm above it cancelled one engine: the default account's live orders went, and
    /// BOTH accounts' protective held exits went with them, so the second account kept its resting
    /// orders and lost the exits that covered them. Now that the arm fans over
    /// [`Self::exit_scope_engines`], the operator path cancels exactly the books this empties.
    ///
    /// **`engines` NARROWS it to the held exits of those engines** — `Some` exactly when the
    /// payload NAMED an account (`OrderIntent::MassCancel`'s `account`), carrying the engine set
    /// the cancel itself reached. Each held exit is attributed to the engine its release would
    /// reach ([`Self::held_release_engine`], the one answer `submit_resolved` acts on), so
    /// `mass-cancel binance ALT` drops ALT's held exits and leaves the default account's, whose
    /// entries are still live and still need them. `None` is the venue-wide clear above,
    /// unchanged. The replay twin (`crate::replay`'s `fold_intent_scope`) has no engine to narrow
    /// by and needs none: it is single-engine by construction, and on one engine the narrowed set
    /// IS the venue scope.
    ///
    /// **Residual, declared:** where the caller named one engine by its ROUTE rather than by the
    /// payload (a labelled mount's own account-less venue-scoped mass-cancel), this still clears
    /// the other account's held exits. The mount's intent carries no account for it to narrow by,
    /// and narrowing on the route instead would change a strategy-lane behaviour this change is
    /// not about.
    pub(crate) fn clear_held_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let doomed: Vec<String> = self
            .held_orders
            .iter()
            .filter(|(c, r)| {
                venue.is_none_or(|v| r.venue == v)
                    && symbol.is_none_or(|s| r.symbol == s)
                    && engines
                        .is_none_or(|set| set.contains(&self.held_release_engine(c, &r.venue)))
            })
            .map(|(c, _)| c.clone())
            .collect();
        for c in doomed {
            self.held_orders.shift_remove(&c);
            self.contingency.remove(&c);
        }
    }

    /// Clear every ARMED conditional matching a `(venue, symbol)` scope (`None` = any) — the
    /// `MassCancel` twin of [`Self::clear_held_scope`], and the ONE spelling of "a book is being
    /// emptied", so an arm's account entry ([`crate::emulator::OrderEmulationBook::cond_engine`]) cannot be left behind by a
    /// site that remembered to clear the book and forgot the map.
    ///
    /// Two passes rather than one because the ids are read from `conditional_books` and spent on
    /// `cond_engine`; the books are few (one per armed `(venue, symbol)`) and this is command
    /// cadence, never the per-message fold.
    ///
    /// ⚠ **VENUE-KEYED, like the books themselves.** `conditional_books` is keyed `(venue, symbol)`
    /// — an EXCHANGE fact, which is why an arm needs `cond_engine` to remember which account armed
    /// it at all — so emptying one book empties both accounts' arms. Same story as
    /// [`Self::clear_held_scope`]: asymmetric while the `MassCancel` arm cancelled a single engine,
    /// matched to the cancel now that the arm fans over [`Self::exit_scope_engines`], and carrying
    /// the same declared residual for a caller that named ONE engine by its route.
    ///
    /// `engines` narrows it exactly as it narrows [`Self::clear_held_scope`]: `Some` when the
    /// payload NAMED an account, and then only the arms that would FIRE onto one of those engines
    /// ([`Self::armed_engine`]) leave the shared book — each through `ConditionalArms::disarm`, so
    /// the other account's stop keeps its place in fire order. An arm whose fire reaches NO engine
    /// (a restored arm on a venue with several accounts, which the Submit arm refuses as ambiguous)
    /// is in no named account's scope, so a narrowed clear leaves it. `None` empties the whole
    /// book, as it always did.
    pub(crate) fn clear_conditional_scope(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        let in_scope = |(v, s): &(String, String)| {
            venue.is_none_or(|want| v == want) && symbol.is_none_or(|want| s == want)
        };
        // A core-held stop that fired and is awaiting its close's verdict is part of the book being
        // emptied: do not re-arm it behind the operator's mass-cancel or market-exit.
        self.forget_fired_stops(venue, symbol, engines);
        let Some(engines) = engines else {
            // The venue-wide clear, exactly as it always ran: every arm in scope leaves, and the
            // books are emptied whole.
            let doomed: Vec<String> = self
                .emulated
                .order_emulation_arms
                .iter()
                .filter(|(k, _)| in_scope(k))
                .flat_map(|(_, b)| b.iter().map(|(id, _)| id.to_string()))
                .collect();
            for id in doomed {
                self.emulated.cond_engine.remove(&id);
            }
            for (k, book) in self.emulated.order_emulation_arms.iter_mut() {
                if in_scope(k) {
                    book.clear();
                }
            }
            self.refresh_tick_conditionals();
            return;
        };
        // The NARROWED clear: only the arms that fire onto one of `engines`, each disarmed out of
        // the shared book by id so the survivors keep their fire order. An arm whose fire reaches
        // no engine (`armed_engine` answers `None`) is not in the named account's scope.
        let mut doomed: Vec<((String, String), String)> = Vec::new();
        for (k, book) in self.emulated.order_emulation_arms.iter().filter(|(k, _)| in_scope(k)) {
            for (id, _) in book.iter() {
                if self.armed_engine(id, &k.0).is_some_and(|e| engines.contains(&e)) {
                    doomed.push((k.clone(), id.to_string()));
                }
            }
        }
        for (k, id) in &doomed {
            self.emulated.cond_engine.remove(id);
            if let Some(book) = self.emulated.order_emulation_arms.get_mut(k) {
                let removed = book.disarm(id);
                debug_assert!(removed, "the walk above read this arm out of this book");
            }
        }
        self.refresh_tick_conditionals();
    }
}
