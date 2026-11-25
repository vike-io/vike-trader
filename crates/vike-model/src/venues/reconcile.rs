//! The reconcile VOCABULARY: the kinds of local-vs-venue disagreement a reconcile pass finds
//! ([`ReconcileDivergenceKind`]), whose activity a kind's evidence describes
//! ([`ReconcileDivergenceOrigin`]), and the four policy names an operator picks
//! ([`ReconcilePolicyName`], the `config.reconcile_policy` row's words). Each name is spelled from
//! this crate's root.
//!
//! **Why here.** The readers that must not disagree about these words cannot name each other:
//! vike-config judges the policy row and vike-execution builds the policy it names, both at layer
//! 20; vike-alerting types a rule's divergence kind, vike-client reads a held alert's kind, vike-docs
//! renders the roster, and the tradehub and the core publish the kind on the observe wire. The
//! cure for two sides that must not disagree is a vocabulary below both, which is this crate (the
//! precedent is `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`'s, for
//! `DeadManAction`). Beside the rows a pass diffs (`super::report`'s `OrderStatusReport`,
//! `FillReport` and `PositionStatusReport`).
//!
//! What stays in vike-execution: the divergences themselves (they carry the venue evidence), the
//! policy value and its presets (`vike_execution::recon::ReconcilePolicy`), the diff and `resolve`.
//! Pure data and exhaustive matches only: no I/O, no clock.
//!
//! ⚠ **Two frozen spellings of a kind, and the word is the serde tag.** A kind's word
//! ([`ReconcileDivergenceKind::word`]) is its variant name (`"PositionDrift"`), byte-identical to
//! its serde tag (the journaled `Command::ReconcileReports` policy map is keyed by it) and to the
//! text the observe wire's held alert carries. The kebab-case page slug vike-docs publishes
//! (`missing-fill`) is a different, docs-side spelling.

use serde::{Deserialize, Serialize};

vike_utility::word_enum! {
    /// The kinds of local-vs-venue disagreement a reconcile pass can find. Ordered by resolution
    /// risk; the derived `Ord` is the declaration order, which is a journal format (the policy's
    /// per-kind map is a `BTreeMap` keyed by it), so a new kind is APPENDED.
    ///
    /// Serde-derived (fieldless variants as JSON strings, the variant name) so a
    /// `vike_execution::recon::ReconcilePolicy` can ride inside the journaled
    /// `Command::ReconcileReports`.
    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
    )]
    pub enum ReconcileDivergenceKind {
        /// Venue reports a fill whose trade_id we have never seen. (local origin)
        MissingFill => "MissingFill",
        /// Venue order is terminal; local order is not. (local origin)
        MissingTerminal => "MissingTerminal",
        /// A live local open order this pass's venue report never mentions — the order mirror of
        /// [`Self::OrphanLocalPosition`]. (no local origin: the ORDER is ours, but the DIVERGENCE
        /// is the venue's SILENCE about it. It folds nothing under any policy, so the
        /// classification decides only whether held policies SURFACE it;
        /// `vike_execution::recon::resolve`'s module doc is the authority.)
        OrphanLocalOrder => "OrphanLocalOrder",
        /// Venue reports an order with no local match. (no local origin)
        UnknownOrder => "UnknownOrder",
        /// Venue net qty differs from local net qty beyond tolerance. (no local origin — but the
        /// ONE no-local-origin kind `vike_execution::recon::ReconcilePolicy::hybrid` AUTO-APPLIES,
        /// deliberately: unlike every other no-local-origin kind its evidence is a POSITIVE venue
        /// position row rather than an absence, and it is the only path that re-converges qty
        /// after a fill older than the reconcile lookback. Pinned by
        /// `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`. With `MissingFill` it is
        /// one of the only two kinds `hybrid` folds anything for; `MissingTerminal` auto-applies
        /// and resolves to no events.)
        PositionDrift => "PositionDrift",
        /// Venue position exists with zero local origin. (no local origin)
        PositionOnlyExternal => "PositionOnlyExternal",
        /// Local net position with NO venue position row at all — the venue's report omitted it
        /// entirely, so there is nothing to compare against (contrast [`Self::PositionDrift`],
        /// which needs a row). The position mirror of [`Self::OrphanLocalOrder`]. (no local
        /// origin — the EVIDENCE is an absence in the venue report, not local activity, and it can
        /// never be safely auto-applied; `resolve`'s module doc is the authority on why.)
        OrphanLocalPosition => "OrphanLocalPosition",
        /// Venue quote-currency cash differs from the realized-PnL-corrected local balance beyond
        /// tolerance — a genuinely unexplained cash move (withdrawal/deposit/liquidation-haircut/
        /// funding mis-track). (no local origin) See `vike_execution::recon::diff_balance`.
        BalanceDrift => "BalanceDrift",
        /// (edge 2) local memory, venue, and journal disagree.
        JournalDivergence => "JournalDivergence",
        /// A live local order the venue reports at other TERMS: its limit or trigger price differs
        /// on the instrument's tick, or its total size on the lot
        /// (`vike_execution::recon::diff_order_terms`, which says what is compared and when not).
        /// A lost amend answer, or an edit made at the venue. (no local origin: the ORDER is ours,
        /// but nothing shows the venue's terms are an amend WE sent, the way `PositionDrift`
        /// cannot show a residual is our own fill.) Its confirm adopts the venue's terms through
        /// `OrderModified`. Declared LAST, so no other kind's index or `Ord` moves.
        OrderTermsDrift => "OrderTermsDrift",
    }
}

/// The ORIGIN dimension of a [`ReconcileDivergenceKind`] — the EXTERNAL-activity tag (split-plane
/// Pattern A, `docs/superpowers/specs/2026-08-18-split-plane-client-backend-design.md` §3b): whose
/// activity the divergence's EVIDENCE describes, so a policy can hold foreign venue activity for an
/// operator CLAIM instead of folding it blind. A property of the KIND, because policy decides per
/// kind (`vike_execution::recon::ReconcilePolicy::mode_for`). Pinned kind-by-kind (exhaustive match,
/// no `_` arm) by `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileDivergenceOrigin {
    /// Our own activity, already settled by the venue: resolving the divergence merely
    /// MATERIALIZES locally what the venue confirmed about orders WE placed (a missed fill, a
    /// terminal we missed). Nothing foreign happened.
    ReconciliationMaterialized,
    /// Foreign venue activity (EXTERNAL): POSITIVE venue evidence — an order row, a position row, a
    /// net-qty residual, a cash move — that nothing local explains. ⚠ For
    /// [`ReconcileDivergenceKind::PositionDrift`] this is deliberately the tag even though a drift
    /// can equally be OUR OWN fill aged out of the reconcile lookback: the evidence cannot
    /// distinguish the two, and "cannot be shown to be ours" is exactly what EXTERNAL means — which
    /// is why `vike_execution::recon::ReconcilePolicy::external_quarantine` holds it for an operator
    /// claim rather than letting `hybrid`'s re-convergence argument fold it blind.
    External,
    /// The evidence is something MISSING, not observed activity: a venue-report row that never
    /// arrived (both orphan kinds) or live local state the journal says was lost
    /// ([`ReconcileDivergenceKind::JournalDivergence`]). None of these fold any event under any
    /// policy (`vike_execution::recon::resolve`'s module doc is the authority), so the origin
    /// dimension is INERT for them — classified so the match stays exhaustive, never able to change
    /// a fold.
    Absence,
}

impl ReconcileDivergenceKind {
    /// The kinds for which [`Self::resolves_to_events`] is `true`, in the order the operator-facing
    /// startup line lists them (`vike_tradehub::reconcile_config::auto_applied_kinds`).
    ///
    /// ⚠ NOT declaration order: `PositionDrift` is listed before `UnknownOrder`, as the line has
    /// always printed them (`synthesize` renders `[MissingFill, PositionDrift, UnknownOrder,
    /// PositionOnlyExternal, BalanceDrift, OrderTermsDrift]`). The match in
    /// [`Self::resolves_to_events`] is the authority on MEMBERSHIP; this list only fixes the ORDER,
    /// and this module's `event_resolving_is_exactly_the_kinds_that_resolve_to_events` holds the two
    /// equal as sets, so neither can drift from the other.
    pub const EVENT_RESOLVING: [ReconcileDivergenceKind; 6] = [
        ReconcileDivergenceKind::MissingFill,
        ReconcileDivergenceKind::PositionDrift,
        ReconcileDivergenceKind::UnknownOrder,
        ReconcileDivergenceKind::PositionOnlyExternal,
        ReconcileDivergenceKind::BalanceDrift,
        ReconcileDivergenceKind::OrderTermsDrift,
    ];

    /// Whether resolving this kind produces REAL events under a policy that folds it — the
    /// distinction between a kind that merely FOLDS (its policy verdict,
    /// `vike_execution::recon::mode_applies`) and one whose fold DOES something. Exhaustive on
    /// purpose (no `_` arm): a new kind fails to COMPILE here until somebody states whether
    /// `resolve` can turn it into events.
    ///
    /// The `false` kinds never fold an event under any policy: `MissingTerminal` lands in
    /// `resolve`'s `events_for` catch-all (an EMPTY list), `OrphanLocalOrder` and
    /// `OrphanLocalPosition` are only ever an event-free alert (or a silent no-op under
    /// `synthesize`), and `JournalDivergence` is short-circuited into an investigative alert before
    /// the policy is consulted. `UnknownOrder` is `true` because an adoption (`resolve`'s `adopt`
    /// argument, `generate_missing_orders`) folds a terminal executed order; with `adopt` off it
    /// resolves to nothing, which is the case this answer does not model.
    ///
    /// Pinned against `resolve` itself by
    /// `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`'s
    /// `resolves_to_events_agrees_with_what_resolve_folds`, which runs one divergence of every kind
    /// through it.
    #[must_use]
    pub const fn resolves_to_events(self) -> bool {
        match self {
            ReconcileDivergenceKind::MissingFill
            | ReconcileDivergenceKind::PositionDrift
            | ReconcileDivergenceKind::UnknownOrder
            | ReconcileDivergenceKind::PositionOnlyExternal
            | ReconcileDivergenceKind::BalanceDrift
            // One `OrderModified` to the venue's terms.
            | ReconcileDivergenceKind::OrderTermsDrift => true,
            ReconcileDivergenceKind::MissingTerminal
            | ReconcileDivergenceKind::OrphanLocalOrder
            | ReconcileDivergenceKind::OrphanLocalPosition
            | ReconcileDivergenceKind::JournalDivergence => false,
        }
    }

    /// Whether a HELD row of this kind could carry the same money as an unexplained cash increase,
    /// so that adopting the cash beside it (`vike_execution::recon::ReconcilePolicy`'s
    /// `adopt_balance_increase`) risks counting it twice. Exhaustive on purpose (no `_` arm): a new
    /// kind fails to COMPILE here until somebody states whether it can.
    ///
    /// The canonical double count: a position the venue closed EXTERNALLY at a profit. The venue's
    /// cash rose by the realized profit while the book still holds the position (`PositionDrift`,
    /// `PositionOnlyExternal`, `OrphanLocalPosition`), or an unbooked fill (`MissingFill`,
    /// `MissingTerminal`, an `UnknownOrder` that executed) still owes the book that same realized
    /// PnL: adopt the cash and confirm the row later, and the profit lands twice. A
    /// `JournalDivergence` is the same shape (local state lost something the journal has).
    ///
    /// `false`: `BalanceDrift` is the row being decided, `OrphanLocalOrder` is an ABSENCE of a
    /// resting order that carries no booked or unbooked money by itself (a fill it made would be a
    /// `MissingFill` or a position row of its own), and `OrderTermsDrift` is a resting order's
    /// price, which moves no money until it fills.
    #[must_use]
    pub const fn blocks_balance_adoption(self) -> bool {
        match self {
            ReconcileDivergenceKind::MissingFill
            | ReconcileDivergenceKind::MissingTerminal
            | ReconcileDivergenceKind::UnknownOrder
            | ReconcileDivergenceKind::PositionDrift
            | ReconcileDivergenceKind::PositionOnlyExternal
            | ReconcileDivergenceKind::OrphanLocalPosition
            | ReconcileDivergenceKind::JournalDivergence => true,
            ReconcileDivergenceKind::OrphanLocalOrder
            | ReconcileDivergenceKind::BalanceDrift
            | ReconcileDivergenceKind::OrderTermsDrift => false,
        }
    }

    /// The [`ReconcileDivergenceOrigin`] of this kind. Exhaustive on purpose — a new kind fails to
    /// COMPILE here until somebody states whose activity its evidence describes, the same forcing
    /// shape as `expected_hybrid_mode` in `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`
    /// (which pins every row of this match).
    ///
    /// Agreement, pinned rather than assumed: `resolve`'s private `is_local_origin` (the
    /// bare-`Hybrid` fallback classifier) must answer `true` for exactly the
    /// [`ReconcileDivergenceOrigin::ReconciliationMaterialized`] kinds —
    /// `origin_agrees_with_the_bare_hybrid_classifier` in the same pin file asserts it through
    /// `vike_execution::recon::mode_applies`.
    #[must_use]
    pub fn origin(self) -> ReconcileDivergenceOrigin {
        use ReconcileDivergenceOrigin::{Absence, External, ReconciliationMaterialized};
        match self {
            ReconcileDivergenceKind::MissingFill | ReconcileDivergenceKind::MissingTerminal => {
                ReconciliationMaterialized
            }
            ReconcileDivergenceKind::UnknownOrder
            | ReconcileDivergenceKind::PositionDrift
            | ReconcileDivergenceKind::PositionOnlyExternal
            | ReconcileDivergenceKind::BalanceDrift
            | ReconcileDivergenceKind::OrderTermsDrift => External,
            ReconcileDivergenceKind::OrphanLocalOrder
            | ReconcileDivergenceKind::OrphanLocalPosition
            | ReconcileDivergenceKind::JournalDivergence => Absence,
        }
    }
}

/// The reconcile policy an operator NAMES: the `config.reconcile_policy` row's four words, ONE
/// spelling each, in the order an operator meets them (the published `rosters.json` renders this
/// order). What each name BUILDS is `vike_execution::recon::ReconcilePolicy`'s presets.
///
/// ⚠ A word outside the four is REFUSED, at the write and at the load: a typo must never reach a
/// reader that would fall back to another policy. The serde form is the word (`kebab-case`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReconcilePolicyName {
    /// `hybrid`: auto-apply the local-origin kinds, hold the rest
    /// (`vike_execution::recon::ReconcilePolicy::hybrid`).
    Hybrid,
    /// `synthesize`: fold every kind, the terms comparison on.
    Synthesize,
    /// `quarantine`: hold every kind for an operator, except a pure balance INCREASE the pass
    /// adopts itself (a deposit, decision 0179).
    Quarantine,
    /// `external-quarantine`: `hybrid` with every External-origin kind held for an operator claim
    /// (`vike_execution::recon::ReconcilePolicy::external_quarantine`).
    ExternalQuarantine,
}

vike_utility::word_enum! {
    impl ReconcilePolicyName {
        Hybrid => "hybrid",
        Synthesize => "synthesize",
        Quarantine => "quarantine",
        ExternalQuarantine => "external-quarantine",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`ReconcileDivergenceKind::EVENT_RESOLVING`] (the ORDER) and
    /// [`ReconcileDivergenceKind::resolves_to_events`] (the MEMBERSHIP) name the same kinds: no kind
    /// the match calls `true` is missing from the list, none the match calls `false` is in it, and
    /// none is listed twice.
    #[test]
    fn event_resolving_is_exactly_the_kinds_that_resolve_to_events() {
        let from_match: Vec<ReconcileDivergenceKind> =
            ReconcileDivergenceKind::ALL.into_iter().filter(|k| k.resolves_to_events()).collect();
        let mut listed = ReconcileDivergenceKind::EVENT_RESOLVING.to_vec();
        listed.sort();
        assert_eq!(listed, from_match, "EVENT_RESOLVING and resolves_to_events disagree");
        listed.dedup();
        assert_eq!(
            listed.len(),
            ReconcileDivergenceKind::EVENT_RESOLVING.len(),
            "a kind is listed twice"
        );
    }

    /// The origin of every kind, written out: two materialized, five external, three absences.
    #[test]
    fn every_kind_has_the_origin_its_evidence_describes() {
        use ReconcileDivergenceKind::*;
        use ReconcileDivergenceOrigin::{Absence, External, ReconciliationMaterialized};
        let origins: Vec<ReconcileDivergenceOrigin> =
            ReconcileDivergenceKind::ALL.into_iter().map(ReconcileDivergenceKind::origin).collect();
        assert_eq!(
            origins,
            [
                ReconciliationMaterialized, // MissingFill
                ReconciliationMaterialized, // MissingTerminal
                Absence,                    // OrphanLocalOrder
                External,                   // UnknownOrder
                External,                   // PositionDrift
                External,                   // PositionOnlyExternal
                Absence,                    // OrphanLocalPosition
                External,                   // BalanceDrift
                Absence,                    // JournalDivergence
                External,                   // OrderTermsDrift
            ]
        );
        assert!(!BalanceDrift.blocks_balance_adoption(), "the row being decided");
        assert!(PositionDrift.blocks_balance_adoption());
    }
}
