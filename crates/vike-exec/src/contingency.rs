//! The ONE contingency resolver (trigger-law wave 2, dedup A4): OTO arm-on-fill + OCO
//! cancel-sibling, resolved identically on EVERY lane that fills orders.
//!
//! Before this module the law lived lane-dependently: the paper book
//! (`vike_backtest::paper::apply_contingency`, the semantic template this module lifts) ran full
//! OCO on every fill, while the backtest `SimBroker` canceled a fired protective stop's exit
//! siblings only on the GRANULAR sub-bar lane — its coarse and tick lanes left the TP limit
//! resting on a flat book, so one wide bar could fire the stop AND the take-profit and open a
//! spurious reversed position. One resolver, two linkage shapes:
//!
//! - **coid-linked** ([`ContingencyBook`]): explicit `parent`/`linked` ids on the order request
//!   (the bracket lowering `vike_model::build_bracket` emits). The paper book keys it by coid.
//! - **kind-linked** ([`is_protective_exit_sibling`]): the id-less implicit bracket the backtest
//!   `SimBroker` arms via `WorkingOrder::stop` — after a protective-stop fill flattens the
//!   position, its OCO siblings are BY KIND the closing-side resting `Limit`/`LimitClose` exits.
//!
//! Both callers keep their own book mechanics (resting order storage, event emission); this
//! module owns only the DECISION — which children arm, which siblings cancel — so the two lanes
//! structurally cannot disagree again. Nautilus's `OrderEmulator`/`MatchingCore` contingency
//! handling is the target shape.

use indexmap::IndexMap;
use vike_model::OrderKind;

/// One order's bracket linkage: `parent` = the OTO entry that arms it; `linked` = the OCO
/// siblings to cancel when it fills; `active` = whether it may fill yet (entries start active,
/// held exits start inactive until the parent's fill arms them).
#[derive(Debug, Clone)]
struct Link {
    parent: Option<String>,
    linked: Vec<String>,
    active: bool,
}

/// The coid-linked contingency ledger: records each contingent order's linkage and resolves
/// OTO/OCO on every fill. Plain (link-free) orders are never inserted, so a book with no
/// brackets stays empty and every resolver call is a cheap no-op — the byte-identical path
/// for non-bracket runs.
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

    /// Is `coid` a recorded leg of some contingency group? (Membership only — see [`Self::is_held`]
    /// for the armed/held distinction.)
    pub fn contains(&self, coid: &str) -> bool {
        self.entries.contains_key(coid)
    }

    /// Record one contingent order's linkage. A held exit (has a `parent`) starts INACTIVE and
    /// arms on the parent's fill; an entry (no parent) is active immediately.
    pub fn insert(&mut self, coid: impl Into<String>, parent: Option<String>, linked: Vec<String>) {
        let active = parent.is_none();
        self.entries.insert(coid.into(), Link { parent, linked, active });
    }

    /// Restore one entry with an EXPLICIT `active` flag — the [`Self::insert`] twin a crash-restart
    /// re-seed uses. `insert` derives `active` from `parent.is_none()`, which is WRONG for a child
    /// that was already ARMED by its parent's fill before the crash (it has a parent yet is active):
    /// re-inserting it via `insert` would silently re-HOLD an armed exit. Never used on the live
    /// submit path — only when replaying a `Snap`'s captured book (see the runtime's `contingencies`
    /// re-seed).
    pub fn insert_active(
        &mut self,
        coid: impl Into<String>,
        parent: Option<String>,
        linked: Vec<String>,
        active: bool,
    ) {
        self.entries.insert(coid.into(), Link { parent, linked, active });
    }

    /// Snapshot every recorded entry as `(coid, parent, linked, active)` in insertion order — the
    /// durable capture a `Snap` rides so a restart can re-seed the book (via [`Self::insert_active`]).
    /// Insertion order is preserved so the re-seeded book reproduces the same arm/cancel iteration
    /// order the live book had.
    pub fn snapshot(&self) -> Vec<(String, Option<String>, Vec<String>, bool)> {
        self.entries
            .iter()
            .map(|(coid, l)| (coid.clone(), l.parent.clone(), l.linked.clone(), l.active))
            .collect()
    }

    /// Drop `coid`'s linkage (the order left the resting book: filled / canceled / expired).
    /// Unknown coid = no-op. `shift_remove` keeps the survivors' insertion order.
    pub fn remove(&mut self, coid: &str) {
        self.entries.shift_remove(coid);
    }

    /// Drop EVERY entry — the mass-cancel-all twin (the caller also clears its held-order map).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Is `coid` a HELD exit — linked, and not yet armed by its parent's fill? A held order
    /// must not fill. Orders with no entry (every plain order) are never held.
    pub fn is_held(&self, coid: &str) -> bool {
        self.entries.get(coid).is_some_and(|l| !l.active)
    }

    /// This leg's OTO parent id, if any — a protective EXIT has one (the entry that arms it); an
    /// entry has `None`. Used to tell a dead protective exit from a dead entry when surfacing an alert.
    pub fn parent_of(&self, coid: &str) -> Option<&str> {
        self.entries.get(coid).and_then(|l| l.parent.as_deref())
    }

    /// The recorded direct children of `parent` (insertion order) — the expire/cancel cascade's
    /// walk step (a child whose parent terminated without filling can never arm).
    pub fn children_of(&self, parent: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, l)| l.parent.as_deref() == Some(parent))
            .map(|(coid, _)| coid.clone())
            .collect()
    }

    /// The OCO siblings of `coid` — its `linked` ids that are NOT its own children — in `linked`
    /// order, resolved WITHOUT arming or mutating anything (the read-only twin of the sibling half
    /// of [`Self::on_fill`]). A dying protective exit resolves its surviving partner off this SAME
    /// linkage a fill would, so the terminal-cancel and fill-cancel paths cannot disagree on who
    /// the sibling is. Unknown coid (or one with no `linked` ids) ⇒ empty.
    pub fn siblings_of(&self, coid: &str) -> Vec<String> {
        let Some(linked) = self.entries.get(coid).map(|l| l.linked.clone()) else {
            return Vec::new();
        };
        linked
            .into_iter()
            .filter(|sib| self.entries.get(sib).and_then(|l| l.parent.as_deref()) != Some(coid))
            .collect()
    }

    /// Resolve one FILL: arm every held child of `filled_coid` (OTO), then return its OCO
    /// sibling coids — its `linked` ids that are NOT its own children — in `linked` order.
    /// The CALLER cancels whichever of those still rest in its book (and calls [`Self::remove`]
    /// for each actually-canceled sibling); a sibling already gone cancels nothing. A plain
    /// (unrecorded) fill arms nothing and returns empty.
    pub fn on_fill(&mut self, filled_coid: &str) -> Vec<String> {
        for l in self.entries.values_mut() {
            if l.parent.as_deref() == Some(filled_coid) && !l.active {
                l.active = true;
            }
        }
        let Some(linked) = self.entries.get(filled_coid).map(|l| l.linked.clone()) else {
            return Vec::new();
        };
        linked
            .into_iter()
            .filter(|sib| {
                // a linked id that is the filled order's own CHILD was just armed, not a sibling
                self.entries.get(sib).and_then(|l| l.parent.as_deref()) != Some(filled_coid)
            })
            .collect()
    }
}

/// The kind-linked OCO law for an id-less book (the backtest `SimBroker`'s implicit protective
/// bracket): after a protective-STOP fill flattens the position, a resting order is that stop's
/// OCO sibling — and must cancel — iff it is a closing-side `Limit`/`LimitClose` exit.
/// `closing_side` is the side that closed the position (`-1` after a long was stopped,
/// `+1` after a short). Entries and adds (opposite side) never match; neither do stop/trailing
/// kinds (a second protective layer is not a take-profit).
pub fn is_protective_exit_sibling(kind: OrderKind, side: i32, closing_side: i32) -> bool {
    matches!(kind, OrderKind::Limit | OrderKind::LimitClose) && side == closing_side
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build_bracket's shape: entry (active, no links needed to hold it), stop + tp held under
    /// the entry, cross-linked OCO.
    fn bracket() -> ContingencyBook {
        let mut b = ContingencyBook::new();
        b.insert("entry", None, vec!["stop".into(), "tp".into()]);
        b.insert("stop", Some("entry".into()), vec!["tp".into()]);
        b.insert("tp", Some("entry".into()), vec!["stop".into()]);
        b
    }

    #[test]
    fn entry_fill_arms_children_and_cancels_no_sibling() {
        let mut b = bracket();
        assert!(b.is_held("stop") && b.is_held("tp"), "exits start held");
        let cancels = b.on_fill("entry");
        // stop and tp are the entry's own children — armed, never canceled
        assert!(cancels.is_empty(), "children are not OCO siblings: {cancels:?}");
        assert!(!b.is_held("stop") && !b.is_held("tp"), "exits armed by the parent fill");
    }

    #[test]
    fn exit_fill_cancels_the_oco_sibling() {
        let mut b = bracket();
        b.on_fill("entry");
        let cancels = b.on_fill("stop");
        assert_eq!(cancels, vec!["tp".to_string()], "the stop's fill cancels the take-profit");
        // the caller then removes both the filled leg and the canceled sibling
        b.remove("stop");
        b.remove("tp");
        assert!(!b.is_empty(), "entry link may remain until its order terminates");
    }

    #[test]
    fn siblings_of_reads_linkage_without_arming() {
        let mut b = bracket();
        b.on_fill("entry"); // arm sl + tp so both are active/resting exits
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
        assert!(b.on_fill("unrelated").is_empty());
        assert!(b.is_held("stop"), "unrelated fill arms nothing");
    }

    #[test]
    fn children_walk_supports_the_cascade() {
        let mut b = ContingencyBook::new();
        b.insert("a", None, vec![]);
        b.insert("b", Some("a".into()), vec![]);
        b.insert("c", Some("b".into()), vec![]);
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
        assert!(!is_protective_exit_sibling(OrderKind::Limit, 1, -1), "re-entry buy limit stays");
        assert!(!is_protective_exit_sibling(OrderKind::Stop, -1, -1), "a second stop stays");
        assert!(!is_protective_exit_sibling(OrderKind::Trailing, -1, -1));
        assert!(!is_protective_exit_sibling(OrderKind::Market, -1, -1));
        // short stopped out => closing side +1
        assert!(is_protective_exit_sibling(OrderKind::Limit, 1, 1));
        assert!(!is_protective_exit_sibling(OrderKind::Limit, -1, 1));
    }
}
