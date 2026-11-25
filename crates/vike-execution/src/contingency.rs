//! The ONE contingency resolver: OTO arm-on-fill, OCO cancel-sibling and OUO reduce-sibling,
//! resolved identically on EVERY lane that fills orders. A lane-dependent law once left a
//! take-profit resting on a flat book after its protective stop fired, so one wide bar could fire
//! both and open a spurious reversed position. One resolver, two linkage shapes:
//!
//! - **order-client-id-linked** ([`ContingencyBook`]): explicit `parent`/`linked` ids and a
//!   [`ContingencyType`] on the order request (what `vike_model::build_bracket` and
//!   `vike_model::build_order_list` emit). The paper book keys it by order client id, the backtest
//!   `OrderBrokerSimulator` by its order-list legs' handles.
//! - **kind-linked** ([`is_protective_exit_sibling`]): the id-less implicit bracket the backtest
//!   `OrderBrokerSimulator` arms via `OrderResting::stop` — after a protective-stop fill flattens the
//!   position, its OCO siblings are BY KIND the closing-side resting `Limit`/`LimitClose`/
//!   `TakeProfit` exits.
//!
//! Both callers keep their own book mechanics (resting order storage, event emission); this
//! module owns only the DECISION — which children arm, which siblings cancel — so the two lanes
//! structurally cannot disagree.

use indexmap::IndexMap;
use vike_model::{ContingencyType, OrderKind};

use crate::execution_engine::LinkedExits;

/// One order's linkage: `kind` = how its fill acts on `linked` (an `Ouo` leg's fill reduces them,
/// any other cancels them); `parent` = the OTO entry that arms it; `linked` = its siblings, plus its
/// own children on an entry; `active` = whether it may fill yet (entries start active, held exits
/// start inactive until the parent's fill arms them).
#[derive(Debug, Clone)]
struct Link {
    kind: ContingencyType,
    parent: Option<String>,
    linked: Vec<String>,
    active: bool,
}

/// The type a leg whose request states none has: what its shape gives (`Oto` for an entry, `Oco`
/// for an exit), the type `vike_model::build_bracket` states for each, and the law the book applied
/// before it knew types (only `Ouo` resolves differently).
fn shape_kind(parent: &Option<String>) -> ContingencyType {
    if parent.is_some() { ContingencyType::Oco } else { ContingencyType::Oto }
}

/// One recorded leg as [`ContingencyBook::snapshot`] captures it, in the arguments
/// [`ContingencyBook::insert_active`] restores it from.
#[derive(Debug, Clone, PartialEq)]
pub struct ContingencyLegSnapshot {
    pub order_client_id: String,
    /// `None` where the leg's shape gives its type (`OTO` for an entry, `OCO` for an exit: each of a
    /// bracket's three legs), so a `Snapshot` of a bracket keeps the bytes it had before the book knew
    /// types; `Some` for an OUO leg and for an OCO leg with no parent.
    pub kind: Option<ContingencyType>,
    pub parent: Option<String>,
    pub linked: Vec<String>,
    pub active: bool,
}

/// What one fill resolves to ([`ContingencyBook::on_fill`]), each list in `linked` order: the legs to
/// CANCEL (the filled leg is OCO, or OTO with siblings) and the legs to REDUCE by the filled quantity
/// (it is OUO). The caller holds the sizes and prices, so it applies the reduce: by the filled quantity
/// IN TOTAL over those legs, as [`spend_ouo_reduce`] spends it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ContingencyResolution {
    pub cancel: Vec<String>,
    /// `(order client id, by)`: a leg the fill reduces, `by` the quantity the filled leg just filled (the total
    /// [`spend_ouo_reduce`] spends over every listed leg, never each leg's own reduction).
    pub reduce: Vec<(String, f64)>,
}

/// The OUO law's size step: what is left of a leg of `qty` reduced by `by`, `None` when nothing is,
/// and that leg is then cancelled. "Nothing" is the paper book's own `~0` size (`1e-12`).
pub fn reduced_quantity(qty: f64, by: f64) -> Option<f64> {
    let left = qty - by;
    (left > 1e-12).then_some(left)
}

/// The OUO law's spend, ONE copy for the core, the paper book and the simulator: one OUO leg's fill
/// of `by` reduces its resting siblings `legs` (`(order client id, price, qty)`) by `by` IN TOTAL, never each by
/// `by`, spent from the leg FARTHEST from `from_price` (where the filled leg filled) first. So a ladder's
/// stop filling `q` takes `q` off its farthest targets, and a target filling `q` takes `q` off its one
/// sibling, the stop. Returns each leg the spend reached, in spend order, with what is left of it
/// ([`reduced_quantity`]; `None` = nothing is, cancel it); a leg it did not reach is not listed. Equal
/// distances go to the smaller order client id first, so every lane spends alike. A `by` that is not above zero
/// (NaN included) reaches nothing.
pub fn spend_ouo_reduce(
    by: f64,
    from_price: f64,
    mut legs: Vec<(String, f64, f64)>,
) -> Vec<(String, Option<f64>)> {
    let away = |px: f64| (px - from_price).abs();
    legs.sort_by(|a, b| away(b.1).total_cmp(&away(a.1)).then_with(|| a.0.cmp(&b.0)));
    let mut left = by.max(0.0); // NaN.max(0.0) is 0.0
    let mut spent = Vec::new();
    for (order_client_id, _, qty) in legs {
        if left <= 1e-12 {
            break;
        }
        let take = qty.min(left);
        left -= take;
        spent.push((order_client_id, reduced_quantity(qty, take)));
    }
    spent
}

/// The order-client-id-linked contingency ledger: records each contingent order's linkage and resolves
/// OTO/OCO/OUO on every fill. Plain (link-free) orders are never inserted, so a book with no
/// brackets stays empty and every resolver call is a cheap no-op.
///
/// `IndexMap` (not `HashMap`): sibling-cancel order is the `linked` list's own order, but the
/// arm sweep and [`Self::children_of`] iterate the map — insertion order keeps both
/// deterministic (reproducible event streams).
#[derive(Debug, Default)]
pub struct ContingencyBook {
    entries: IndexMap<String, Link>,
}

impl ContingencyBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Is `order_client_id` a recorded leg of some contingency group? (Membership only — see [`Self::is_held`]
    /// for the armed/held distinction.)
    pub fn contains(&self, order_client_id: &str) -> bool {
        self.entries.contains_key(order_client_id)
    }

    /// Record one contingent order's linkage, `kind` its request's `contingency_type` (`None` takes
    /// the type its shape gives: `OTO` for an entry, `OCO` for an exit). A held exit (has a `parent`)
    /// starts INACTIVE and arms on the parent's fill; an entry (no parent) is active immediately.
    pub fn insert(
        &mut self,
        order_client_id: impl Into<String>,
        kind: Option<ContingencyType>,
        parent: Option<String>,
        linked: Vec<String>,
    ) {
        let active = parent.is_none();
        self.insert_active(order_client_id, kind, parent, linked, active);
    }

    /// Restore one entry with an EXPLICIT `active` flag — the [`Self::insert`] twin a crash-restart
    /// re-seed uses (replaying a `Snapshot`'s captured book, never the live submit path). `insert`
    /// derives `active` from `parent.is_none()`, which would silently re-HOLD a child its parent's
    /// fill had already ARMED before the crash. `kind` is as [`Self::snapshot`] gave it.
    pub fn insert_active(
        &mut self,
        order_client_id: impl Into<String>,
        kind: Option<ContingencyType>,
        parent: Option<String>,
        linked: Vec<String>,
        active: bool,
    ) {
        let kind = kind.unwrap_or_else(|| shape_kind(&parent));
        self.entries.insert(order_client_id.into(), Link { kind, parent, linked, active });
    }

    /// Snapshot every recorded entry in insertion order — the durable capture a `Snapshot` rides so a
    /// restart can re-seed the book (via [`Self::insert_active`]) with the same arm/cancel iteration
    /// order the live book had.
    pub fn snapshot(&self) -> Vec<ContingencyLegSnapshot> {
        self.entries
            .iter()
            .map(|(order_client_id, l)| ContingencyLegSnapshot {
                order_client_id: order_client_id.clone(),
                kind: Some(l.kind).filter(|k| *k != shape_kind(&l.parent)),
                parent: l.parent.clone(),
                linked: l.linked.clone(),
                active: l.active,
            })
            .collect()
    }

    /// Drop `order_client_id`'s linkage (the order left the resting book: filled / canceled / expired).
    /// Unknown order client id = no-op. `shift_remove` keeps the survivors' insertion order.
    pub fn remove(&mut self, order_client_id: &str) {
        self.entries.shift_remove(order_client_id);
    }

    /// Drop EVERY entry — the mass-cancel-all twin (the caller also clears its held-order map).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Re-key the leg `old` as `new`, everywhere it appears: its own entry (it keeps its place in the
    /// insertion order, so the arm and cancel iteration is unchanged) and every `parent` / `linked`
    /// reference to it. The cancel-then-new amend (`vike-core`'s `runtime/apply/amend.rs`) uses it so
    /// that the replacement of a bracket's stop IS that leg to its siblings. Unknown `old` = no-op.
    pub fn rename(&mut self, old: &str, new: &str) {
        if old == new || !self.entries.contains_key(old) {
            return;
        }
        let swap = |id: &mut String| {
            if id == old {
                *id = new.to_string();
            }
        };
        let entries = std::mem::take(&mut self.entries);
        for (order_client_id, mut link) in entries {
            if let Some(parent) = link.parent.as_mut() {
                swap(parent);
            }
            link.linked.iter_mut().for_each(swap);
            let key = if order_client_id == old { new.to_string() } else { order_client_id };
            self.entries.insert(key, link);
        }
    }

    /// Is `order_client_id` a HELD exit — linked, and not yet armed by its parent's fill? A held order
    /// must not fill. Orders with no entry (every plain order) are never held.
    pub fn is_held(&self, order_client_id: &str) -> bool {
        self.entries.get(order_client_id).is_some_and(|l| !l.active)
    }

    /// This leg's OTO parent id, if any — a protective EXIT has one (the entry that arms it); an
    /// entry has `None`. Used to tell a dead protective exit from a dead entry when surfacing an alert.
    pub fn parent_of(&self, order_client_id: &str) -> Option<&str> {
        self.entries.get(order_client_id).and_then(|l| l.parent.as_deref())
    }

    /// This leg's type (its shape's when its request stated none), `None` for an order client id not in the book.
    /// A partial fill resolves only an `Ouo` leg's siblings (`vike-core`'s fill drive asks this first).
    pub fn kind_of(&self, order_client_id: &str) -> Option<ContingencyType> {
        self.entries.get(order_client_id).map(|l| l.kind)
    }

    /// Does a fill of the OUO leg `order_client_id` SHARE its quantity over its siblings ([`spend_ouo_reduce`]
    /// over all of them: a ladder's stop against its targets, each of which links `order_client_id` alone, so each
    /// holds a PART of what `order_client_id` covers), or reduce EACH sibling by it (any other OUO group, IB's
    /// OUO: every leg covers the same size)? Only the siblings still in the book count (a filled target
    /// has left it), and a leg with one sibling answers alike either way.
    pub fn ouo_shares_its_fill(&self, order_client_id: &str) -> bool {
        let siblings: Vec<String> = self
            .siblings_of(order_client_id)
            .into_iter()
            .filter(|s| self.entries.contains_key(s))
            .collect();
        siblings.len() > 1
            && siblings.iter().all(
                |s| matches!(self.siblings_of(s).as_slice(), [only] if only == order_client_id),
            )
    }

    /// The recorded direct children of `parent` (insertion order) — the expire/cancel cascade's
    /// walk step (a child whose parent terminated without filling can never arm).
    pub fn children_of(&self, parent: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, l)| l.parent.as_deref() == Some(parent))
            .map(|(order_client_id, _)| order_client_id.clone())
            .collect()
    }

    /// The OCO siblings of `order_client_id` — its `linked` ids that are NOT its own children — in `linked`
    /// order, resolved WITHOUT arming or mutating anything (the read-only twin of the sibling half
    /// of [`Self::on_fill`]). A dying protective exit resolves its surviving partner off this SAME
    /// linkage a fill would, so the terminal-cancel and fill-cancel paths cannot disagree on who
    /// the sibling is. Unknown order client id (or one with no `linked` ids) ⇒ empty.
    pub fn siblings_of(&self, order_client_id: &str) -> Vec<String> {
        let Some(linked) = self.entries.get(order_client_id).map(|l| l.linked.clone()) else {
            return Vec::new();
        };
        linked
            .into_iter()
            .filter(|sib| {
                self.entries.get(sib).and_then(|l| l.parent.as_deref()) != Some(order_client_id)
            })
            .collect()
    }

    /// The linked group of the leg `order_client_id` as the spot rule counts it ([`LinkedExits`],
    /// [`crate::risk::cash_oversell_refusal`]): the exits under its parent (with none, it and the legs
    /// it links), in sets of legs that do not link one another (a ladder's targets each hold a part,
    /// so they sum) whose sets are alternatives (the stop, or the targets). `None` for a leg with no
    /// group. ONE copy for the live core's contingency drive and the backtest `OrderBrokerSimulator`.
    pub fn linked_exits(&self, order_client_id: &str) -> Option<LinkedExits> {
        let members = match self.parent_of(order_client_id) {
            Some(parent) => self.children_of(parent),
            None if self.contains(order_client_id) => std::iter::once(order_client_id.to_string())
                .chain(self.siblings_of(order_client_id))
                .collect(),
            None => return None,
        };
        if members.len() < 2 {
            return None;
        }
        let links = |a: &str, b: &str| {
            self.siblings_of(a).iter().any(|s| s == b) || self.siblings_of(b).iter().any(|s| s == a)
        };
        let mut sets: Vec<Vec<String>> = Vec::new();
        for m in members {
            match sets.iter_mut().find(|set| set.iter().all(|x| !links(x, &m))) {
                Some(set) => set.push(m),
                None => sets.push(vec![m]),
            }
        }
        let own = sets.iter().position(|set| set.iter().any(|c| c == order_client_id))?;
        Some(LinkedExits { sets, own })
    }

    /// Resolve one FILL of `filled_qty`: arm every held child of `filled_order_client_id` (OTO), then resolve its
    /// siblings ([`Self::siblings_of`]: its `linked` ids that are NOT its own children, in `linked`
    /// order) by its type: an `Ouo` leg's fill REDUCES each by `filled_qty`, any other leg's CANCELS
    /// them ([`ContingencyResolution`]). The CALLER acts on whichever of those still rest in its book (and calls
    /// [`Self::remove`] for each actually-canceled sibling); a sibling already gone takes nothing. A
    /// plain (unrecorded) fill arms nothing and resolves to nothing.
    pub fn on_fill(
        &mut self,
        filled_order_client_id: &str,
        filled_qty: f64,
    ) -> ContingencyResolution {
        for l in self.entries.values_mut() {
            if l.parent.as_deref() == Some(filled_order_client_id) && !l.active {
                l.active = true;
            }
        }
        let Some(kind) = self.entries.get(filled_order_client_id).map(|l| l.kind) else {
            return ContingencyResolution::default();
        };
        let siblings = self.siblings_of(filled_order_client_id);
        if kind == ContingencyType::Ouo {
            ContingencyResolution {
                cancel: Vec::new(),
                reduce: siblings.into_iter().map(|s| (s, filled_qty)).collect(),
            }
        } else {
            ContingencyResolution { cancel: siblings, reduce: Vec::new() }
        }
    }
}

/// The kind-linked OCO law for an id-less book (the backtest `OrderBrokerSimulator`'s implicit protective
/// bracket): after a protective-STOP fill flattens the position, a resting order is that stop's
/// OCO sibling — and must cancel — iff it is a closing-side `Limit`/`LimitClose`/`TakeProfit`/
/// `TrailingTakeProfit` exit (an untouched if-touched exit, a trailing one too, is a take-profit
/// like a limit; a touched one already rests as the `Limit`). `closing_side` is the side that
/// closed the position (`-1` after a long was stopped, `+1` after a short). Entries and adds
/// (opposite side) never match; neither do stop/trailing/stop-limit kinds (a second protective
/// layer is not a take-profit).
///
/// Only the implicit bracket: an order list the backtest places (`OrderBrokerSimulator::submit_group`,
/// `submit_ladder`, `submit_oco`) is order-client-id-linked, its legs keyed by their handles in a
/// [`ContingencyBook`] and resolved by [`ContingencyBook::on_fill`] like the paper book's.
pub fn is_protective_exit_sibling(kind: OrderKind, side: i32, closing_side: i32) -> bool {
    matches!(
        kind,
        OrderKind::Limit
            | OrderKind::LimitClose
            | OrderKind::TakeProfit
            | OrderKind::TrailingTakeProfit
    ) && side == closing_side
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build_bracket's shape: entry (active, no links needed to hold it), stop + tp held under
    /// the entry, cross-linked OCO. Typeless, as a leg recorded before the book knew types.
    fn bracket() -> ContingencyBook {
        let mut b = ContingencyBook::new();
        b.insert("entry", None, None, vec!["stop".into(), "tp".into()]);
        b.insert("stop", None, Some("entry".into()), vec!["tp".into()]);
        b.insert("tp", None, Some("entry".into()), vec!["stop".into()]);
        b
    }

    /// build_ladder's shape: the entry arms a whole-size stop and two targets; the stop and each
    /// target are linked OUO.
    fn ladder() -> ContingencyBook {
        let ouo = Some(ContingencyType::Ouo);
        let mut b = ContingencyBook::new();
        let exits = ["stop", "t1", "t2"].map(String::from).to_vec();
        b.insert("entry", Some(ContingencyType::Oto), None, exits);
        b.insert("stop", ouo, Some("entry".into()), vec!["t1".into(), "t2".into()]);
        b.insert("t1", ouo, Some("entry".into()), vec!["stop".into()]);
        b.insert("t2", ouo, Some("entry".into()), vec!["stop".into()]);
        b
    }

    /// The spot rule's groups: a ladder's exits are two alternatives, the stop or the targets
    /// together; a bracket's exits are two alternatives of one leg each; an entry with children and an
    /// unknown leg have no group; and the verdict claims the largest set once.
    #[test]
    fn linked_exits_sets_a_group_into_alternatives_and_the_spot_rule_claims_the_largest() {
        let s = |v: &[&str]| v.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        let l = ladder().linked_exits("t2").expect("a ladder's target has a group");
        assert_eq!((l.sets, l.own), (vec![s(&["stop"]), s(&["t1", "t2"])], 1));
        let l = bracket().linked_exits("stop").expect("a bracket's stop has a group");
        assert_eq!((l.sets, l.own), (vec![s(&["stop"]), s(&["tp"])], 0));
        assert_eq!(ladder().linked_exits("entry"), None, "an entry alone is no group");
        assert_eq!(ladder().linked_exits("nope"), None);
        let refusal = crate::risk::cash_oversell_refusal;
        // the long of 3: the stop of 3 rests, t1 of 1 rests, t2 of 2 is placed: max(3, 1 + 2) = 3
        assert_eq!(refusal(3.0, 0.0, Some((&[3.0, 1.0][..], 1)), 2.0), None, "one claim of 3");
        assert_eq!(
            refusal(3.0, 0.0, None, 6.0).as_deref(),
            Some(
                "a spot account cannot sell more than it holds: it has 3, this sells 6; nothing was sent"
            )
        );
        assert!(refusal(-1.0, 0.0, None, 0.5).is_some(), "a short holds nothing to sell");
        assert_eq!(refusal(3.0, 1.0, None, 2.0), None, "within what the sell out leaves");
    }

    #[test]
    fn entry_fill_arms_children_and_cancels_no_sibling() {
        let mut b = bracket();
        assert!(b.is_held("stop") && b.is_held("tp"), "exits start held");
        let res = b.on_fill("entry", 2.0);
        // stop and tp are the entry's own children — armed, never canceled
        assert_eq!(res, ContingencyResolution::default(), "children are not OCO siblings");
        assert!(!b.is_held("stop") && !b.is_held("tp"), "exits armed by the parent fill");
    }

    /// OUO: a target's fill reduces the stop by what it filled and cancels nothing; the stop's fill
    /// reduces every target; the entry's fill arms all three and resolves no sibling.
    #[test]
    fn an_ouo_fill_reduces_its_siblings_by_the_filled_quantity() {
        let mut b = ladder();
        assert_eq!(b.on_fill("entry", 3.0), ContingencyResolution::default());
        assert!(["stop", "t1", "t2"].iter().all(|c| !b.is_held(c)), "every exit armed");
        let res = b.on_fill("t1", 1.0);
        assert_eq!(
            res,
            ContingencyResolution { cancel: vec![], reduce: vec![("stop".into(), 1.0)] }
        );
        let res = b.on_fill("stop", 2.0);
        assert_eq!(res.reduce, vec![("t1".to_string(), 2.0), ("t2".to_string(), 2.0)]);
        assert!(res.cancel.is_empty());
        assert_eq!(reduced_quantity(3.0, 1.0), Some(2.0));
        assert_eq!(reduced_quantity(1.0, 2.0), None, "reduced past nothing: cancelled");
        assert_eq!(reduced_quantity(0.3, 0.1 + 0.2), None, "a float residue is nothing");
    }

    fn legs(rows: &[(&str, f64, f64)]) -> Vec<(String, f64, f64)> {
        rows.iter().map(|&(c, px, q)| (c.to_string(), px, q)).collect()
    }

    /// Ruling 2: a long ladder's stop filling 1.5 at 90 takes 1.5 off its targets IN TOTAL, the
    /// farthest (120) first, and a short ladder's farthest target is its lowest; never every target
    /// by 1.5. A target's fill takes its quantity off its one sibling, the stop.
    #[test]
    fn an_ouo_spend_takes_the_total_from_the_farthest_leg_first() {
        let long = legs(&[("t1", 110.0, 1.0), ("t3", 130.0, 1.0), ("t2", 120.0, 1.0)]);
        assert_eq!(
            spend_ouo_reduce(1.5, 90.0, long),
            vec![("t3".to_string(), None), ("t2".to_string(), Some(0.5))],
            "t1, the nearest, is not reached"
        );
        let short = legs(&[("t1", 90.0, 1.0), ("t2", 80.0, 1.0)]);
        assert_eq!(spend_ouo_reduce(0.25, 110.0, short), vec![("t2".to_string(), Some(0.75))]);
        let stop = legs(&[("stop", 90.0, 3.0)]);
        assert_eq!(spend_ouo_reduce(1.0, 110.0, stop), vec![("stop".to_string(), Some(2.0))]);
    }

    /// A `by` past every leg cancels every leg, farthest first; a `by` of nothing (zero, negative,
    /// NaN) changes nothing; equal distances spend the smaller order client id first, whatever the input order.
    #[test]
    fn an_ouo_spend_past_every_leg_cancels_all_and_nothing_spends_nothing() {
        let rows = legs(&[("t1", 110.0, 1.0), ("t2", 120.0, 2.0)]);
        assert_eq!(
            spend_ouo_reduce(5.0, 90.0, rows.clone()),
            vec![("t2".to_string(), None), ("t1".to_string(), None)]
        );
        assert_eq!(
            spend_ouo_reduce(f64::INFINITY, 90.0, rows.clone()).len(),
            2,
            "an unbounded fill reaches every leg"
        );
        for by in [0.0, -1.0, f64::NAN, 1e-13] {
            assert!(spend_ouo_reduce(by, 90.0, rows.clone()).is_empty(), "{by} spends nothing");
        }
        let tie = legs(&[("b", 110.0, 1.0), ("a", 70.0, 1.0)]);
        assert_eq!(spend_ouo_reduce(1.0, 90.0, tie), vec![("a".to_string(), None)]);
        let tie = legs(&[("a", 70.0, 1.0), ("b", 110.0, 1.0)]);
        assert_eq!(spend_ouo_reduce(1.0, 90.0, tie), vec![("a".to_string(), None)]);
        assert!(spend_ouo_reduce(1.0, 90.0, Vec::new()).is_empty(), "no leg, no spend");
    }

    /// A ladder's stop SHARES its fill over its targets (each links the stop alone); a target's fill
    /// has one sibling; an OUO group whose legs all link one another reduces each by the fill.
    #[test]
    fn only_a_ladders_stop_shares_its_fill_over_its_siblings() {
        let mut b = ladder();
        assert!(b.ouo_shares_its_fill("stop"));
        assert!(!b.ouo_shares_its_fill("t1"), "one sibling: alike either way");
        b.remove("t1");
        assert!(!b.ouo_shares_its_fill("stop"), "t1 filled and left: one target is left");
        let ouo = Some(ContingencyType::Ouo);
        let mut g = ContingencyBook::new();
        g.insert("a", ouo, None, vec!["b".into(), "c".into()]);
        g.insert("b", ouo, None, vec!["a".into(), "c".into()]);
        g.insert("c", ouo, None, vec!["a".into(), "b".into()]);
        assert!(!g.ouo_shares_its_fill("a"), "every leg covers the same size");
        assert_eq!(g.kind_of("a"), ouo);
        assert_eq!(b.kind_of("entry"), Some(ContingencyType::Oto));
        assert_eq!(b.kind_of("nope"), None);
    }

    /// The snapshot writes a type only where the shape does not give it, and a restore reads every
    /// leg back to the same type: a bracket's Snapshot keeps its bytes, an OUO ladder its law.
    #[test]
    fn the_snapshot_states_a_type_only_where_the_shape_does_not_give_it() {
        assert!(bracket().snapshot().iter().all(|l| l.kind.is_none()), "a bracket states none");
        let mut oco = ContingencyBook::new();
        oco.insert("a", Some(ContingencyType::Oco), None, vec!["b".into()]);
        assert_eq!(oco.snapshot()[0].kind, Some(ContingencyType::Oco), "an entry's OCO is stated");
        let snap = ladder().snapshot();
        assert_eq!(snap[0].kind, None, "the ladder's entry is OTO by its shape");
        assert!(snap[1..].iter().all(|l| l.kind == Some(ContingencyType::Ouo)));
        let mut restored = ContingencyBook::new();
        for l in snap.clone() {
            restored.insert_active(l.order_client_id, l.kind, l.parent, l.linked, l.active);
        }
        assert_eq!(restored.snapshot(), snap);
        restored.on_fill("entry", 3.0);
        assert_eq!(restored.on_fill("t2", 1.0).reduce, vec![("stop".to_string(), 1.0)]);
    }

    #[test]
    fn exit_fill_cancels_the_oco_sibling() {
        let mut b = bracket();
        b.on_fill("entry", 2.0);
        let cancels = b.on_fill("stop", 2.0).cancel;
        assert_eq!(cancels, vec!["tp".to_string()], "the stop's fill cancels the take-profit");
        // the caller then removes both the filled leg and the canceled sibling
        b.remove("stop");
        b.remove("tp");
        assert!(!b.is_empty(), "entry link may remain until its order terminates");
    }

    #[test]
    fn siblings_of_reads_linkage_without_arming() {
        let mut b = bracket();
        b.on_fill("entry", 2.0); // arm sl + tp so both are active/resting exits
        // a dying stop's OCO sibling is the take-profit (and vice-versa) — read-only, arms nothing
        assert_eq!(b.siblings_of("stop"), vec!["tp".to_string()]);
        assert_eq!(b.siblings_of("tp"), vec!["stop".to_string()]);
        assert!(!b.is_held("stop") && !b.is_held("tp"), "siblings_of did not re-hold anything");
        // the entry's linked ids are its own CHILDREN, never siblings
        assert!(b.siblings_of("entry").is_empty(), "children are not OCO siblings");
        // unknown id => empty
        assert!(b.siblings_of("nope").is_empty());
    }

    #[test]
    fn plain_fill_is_a_noop() {
        let mut b = bracket();
        assert_eq!(b.on_fill("unrelated", 1.0), ContingencyResolution::default());
        assert!(b.is_held("stop"), "unrelated fill arms nothing");
    }

    #[test]
    fn children_walk_supports_the_cascade() {
        let mut b = ContingencyBook::new();
        b.insert("a", None, None, vec![]);
        b.insert("b", None, Some("a".into()), vec![]);
        b.insert("c", None, Some("b".into()), vec![]);
        assert_eq!(b.children_of("a"), vec!["b".to_string()]);
        assert_eq!(b.children_of("b"), vec!["c".to_string()]);
        b.remove("b");
        assert!(b.children_of("a").is_empty());
    }

    #[test]
    fn kind_linked_law_matches_closing_side_limits_only() {
        // long stopped out => closing side -1: sell limits cancel, everything else stays
        assert!(is_protective_exit_sibling(OrderKind::Limit, -1, -1));
        assert!(is_protective_exit_sibling(OrderKind::LimitClose, -1, -1));
        assert!(is_protective_exit_sibling(OrderKind::TakeProfit, -1, -1), "an if-touched exit");
        let trailing_take_profit = OrderKind::TrailingTakeProfit;
        assert!(
            is_protective_exit_sibling(trailing_take_profit, -1, -1),
            "a trailing if-touched exit"
        );
        assert!(
            !is_protective_exit_sibling(trailing_take_profit, 1, -1),
            "a trailing if-touched entry"
        );
        assert!(!is_protective_exit_sibling(OrderKind::Limit, 1, -1), "re-entry buy limit stays");
        assert!(!is_protective_exit_sibling(OrderKind::TakeProfit, 1, -1), "an if-touched entry");
        assert!(!is_protective_exit_sibling(OrderKind::Stop, -1, -1), "a second stop stays");
        assert!(!is_protective_exit_sibling(OrderKind::StopLimit, -1, -1), "so does a stop-limit");
        assert!(!is_protective_exit_sibling(OrderKind::Trailing, -1, -1));
        assert!(!is_protective_exit_sibling(OrderKind::Market, -1, -1));
        // short stopped out => closing side +1
        assert!(is_protective_exit_sibling(OrderKind::Limit, 1, 1));
        assert!(!is_protective_exit_sibling(OrderKind::Limit, -1, 1));
    }
}
