//! Pure reconciliation vocabulary: the divergences `diff` emits, the policy that decides
//! fold-vs-hold, and the `Recon` output `resolve` produces. No I/O, no clock, no threads — this is
//! what makes golden-fixture tests and the three-way journal cross-check possible.

use std::collections::BTreeMap;

use vike_model::events::Event;
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use crate::execution_engine::ReconcileSnapshot;
use crate::order::ManagedOrder;

/// The kinds of local-vs-venue disagreement `diff` can find. Ordered by resolution risk.
/// Serde-derived (fieldless variants → JSON strings) so a `ReconPolicy` can ride inside the
/// journaled `Command::ReconcileReports`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum DivergenceKind {
    /// Venue reports a fill whose trade_id we have never seen. (local origin)
    MissingFill,
    /// Venue order is terminal; local order is not. (local origin)
    MissingTerminal,
    /// A live local open order this pass's venue report never mentions — the order mirror of
    /// [`Self::OrphanLocalPosition`]. (no local origin — RECLASSIFIED from local origin, without
    /// changing a single event: what is local-origin here is the ORDER, while the DIVERGENCE is the
    /// venue's SILENCE about it, and an absence is not our own activity confirmed by the venue. It
    /// folds nothing under any policy — a synthesized cancel would terminalize an order that may
    /// still be resting at the venue — so the classification decides only whether held policies
    /// SURFACE it, which before this they did not: `hybrid` folded its empty event list and raised
    /// nothing at all. `vike_exec::recon::resolve`'s module doc is the authority.)
    OrphanLocalOrder,
    /// Venue reports an order with no local match. (no local origin)
    UnknownOrder,
    /// Venue net qty differs from local net qty beyond tolerance. (no local origin — but the ONE
    /// no-local-origin kind [`ReconPolicy::hybrid`] still AUTO-APPLIES. Deliberate: unlike every
    /// other no-local-origin kind its evidence is a POSITIVE venue position row rather than an
    /// absence, and it is the only path that re-converges qty after a fill older than the reconcile
    /// lookback. It was the one variant here carrying NO origin annotation, which is how it came to
    /// reach `Synthesize` through `ReconPolicy`'s `default` with no named row and no test. Pinned
    /// by `crates/vike-exec/tests/recon/recon_policy_pin.rs`. Together with `MissingFill` it is one of
    /// the only two kinds `hybrid` folds anything for; `MissingTerminal` auto-applies and resolves
    /// to no events.)
    PositionDrift,
    /// Venue position exists with zero local origin. (no local origin)
    PositionOnlyExternal,
    /// Local net position with NO venue position row at all — the venue's report omitted it
    /// entirely, so there is nothing to compare against (contrast [`Self::PositionDrift`], which
    /// needs a row). The position mirror of [`Self::OrphanLocalOrder`]. (no local origin — the
    /// EVIDENCE is an absence in the venue report, not local activity, and it can never be safely
    /// auto-applied; `resolve`'s module doc is the authority on why.)
    OrphanLocalPosition,
    /// Venue quote-currency cash differs from the realized-PnL-corrected local balance beyond
    /// tolerance — a genuinely unexplained cash move (withdrawal/deposit/liquidation-haircut/
    /// funding mis-track). (no local origin) See [`crate::recon::diff_balance`].
    BalanceDrift,
    /// (edge 2) local memory, venue, and journal disagree.
    JournalDivergence,
}

/// One detected disagreement, carrying the venue evidence needed to resolve it.
#[derive(Debug, Clone, PartialEq)]
pub enum Divergence {
    MissingFill(FillReport),
    MissingTerminal {
        order: OrderStatusReport,
    },
    OrphanLocalOrder {
        client_order_id: String,
    },
    UnknownOrder(OrderStatusReport),
    PositionDrift {
        report: PositionStatusReport,
        local_qty: f64,
    },
    PositionOnlyExternal(PositionStatusReport),
    /// A live LOCAL position for which this pass's venue report carries no row at all (the
    /// local-side position sweep — `diff`'s step 5, the mirror of the `OrphanLocalOrder` sweep).
    /// Carries the whole local key plus the qty, because there is no venue report to carry: an
    /// absent row is the evidence. `venue` is the `LocalView`'s venue, echoed so the alert detail
    /// is self-describing.
    OrphanLocalPosition {
        venue: String,
        symbol: String,
        /// `"BOTH"` / `"LONG"` / `"SHORT"` — the second half of the `LocalView::positions` key,
        /// kept verbatim so it matches the venue-side `position_side_str` spelling.
        position_side: String,
        /// Signed local net qty (never within `qty_tol` of flat — `diff` filters those out).
        local_qty: f64,
    },
    /// First-class cash reconcile (Feature 2). Carries both sides — `local` is the
    /// realized-PnL-CORRECTED expected cash (`balance + realized-since-sync`, NOT raw
    /// `Account::balance`), `venue_bal` the venue's authoritative quote cash — plus the `asset`
    /// (quote currency) `resolve` synthesizes the correcting `Event::AccountState` for.
    BalanceDrift {
        venue: String,
        asset: String,
        local: f64,
        venue_bal: f64,
        ts: i64,
    },
    /// A three-way persistence/restore-bug signal: the journal recorded something the live local
    /// state has since lost. `recover_order` is `Some` ONLY for the order-loss case (a venue order
    /// the journal has as still-live that local dropped) — it carries the venue report `resolve`
    /// turns into an operator-confirmable re-registration; `None` for the fill-loss case (a fill
    /// already at the venue has nothing economic to synthesize — investigate-only).
    JournalDivergence {
        detail: String,
        recover_order: Option<Box<OrderStatusReport>>,
    },
}

impl Divergence {
    /// One line naming WHAT diverged — the symbol, the quantity, the id — for an operator who has
    /// only this.
    ///
    /// ⚠ **It exists because the fallback was `format!("{:?}", d.kind())`**, which renders a kind
    /// as its own name and nothing else. Measured on the live the CI box box 2026-08-24, an hour after
    /// the log line that surfaces these was shipped:
    ///
    /// ```text
    /// reconcile divergence HELD — awaiting operator confirm alert=8 venue=bybit
    ///   kind=PositionOnlyExternal detail=PositionOnlyExternal
    /// ```
    ///
    /// The `detail` repeated the `kind`. The whole point of raising the line was to answer *which*,
    /// and it answered *the same*.
    ///
    /// A TOTAL match, deliberately: a new [`Divergence`] variant is a compile error here rather
    /// than a variant that silently inherits an empty description. That is the property the
    /// fallback destroyed — it accepted everything, so nothing ever had to be described.
    ///
    /// No secret can reach it: every field below is a symbol, a side, a quantity, an asset or a
    /// client order id. There is no credential on `Divergence` to leak.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Divergence::MissingFill(f) => {
                format!(
                    "{} {} qty={} px={} coid={:?}",
                    f.venue, f.symbol, f.last_qty, f.last_px, f.client_order_id
                )
            }
            Divergence::MissingTerminal { order } => {
                format!(
                    "{} {} coid={:?} status={:?}",
                    order.venue, order.symbol, order.client_order_id, order.status
                )
            }
            Divergence::OrphanLocalOrder { client_order_id } => format!("coid={client_order_id}"),
            Divergence::UnknownOrder(o) => {
                format!(
                    "{} {} coid={:?} status={:?}",
                    o.venue, o.symbol, o.client_order_id, o.status
                )
            }
            Divergence::PositionDrift { report, local_qty } => format!(
                "{} {} {:?} venue_qty={} local_qty={local_qty}",
                report.venue, report.symbol, report.position_side, report.qty
            ),
            Divergence::PositionOnlyExternal(p) => format!(
                "{} {} {:?} qty={} avg_px={} — the venue holds a position this process has no book for",
                p.venue, p.symbol, p.position_side, p.qty, p.avg_px
            ),
            Divergence::OrphanLocalPosition { venue, symbol, position_side, local_qty } => {
                format!(
                    "{venue} {symbol} {position_side} local_qty={local_qty} — the venue reports none"
                )
            }
            Divergence::BalanceDrift { venue, asset, local, venue_bal, .. } => {
                format!("{venue} {asset} local={local} venue={venue_bal}")
            }
            Divergence::JournalDivergence { detail, .. } => detail.clone(),
        }
    }

    /// The CHURN-FREE half of [`Self::describe`] — what this divergence IS, with every quantity and
    /// price left out.
    ///
    /// ⚠ **Read this next to `vike_core`'s `HeldId::new`, which is the only reason it exists.** That
    /// function builds an un-keyed alert's identity out of the alert's `detail` STRING, so whatever
    /// the detail says becomes the thing that decides "same divergence, refresh the row" versus "new
    /// divergence, add one". A detail carrying `qty=` and `avg_px=` therefore makes a drifting
    /// position a NEW alert every pass — which is the measured production bug
    /// `crates/vike-core/src/runtime/recon_held_tests.rs` exists to prevent (60 rows and 60 confirm
    /// ids per hour; ~1,560 a day on the CI box).
    ///
    /// Splitting the two is the fix rather than impoverishing the detail: an operator needs the
    /// quantity, and the identity must not see it. Anything added below must be stable pass to pass
    /// for an UNCHANGED divergence — venue, instrument, side, an id. Never a number the venue
    /// recomputes.
    #[must_use]
    pub fn identity_detail(&self) -> String {
        match self {
            Divergence::MissingFill(f) => {
                format!(
                    "{} {} coid={:?} trade={}",
                    f.venue, f.symbol, f.client_order_id, f.trade_id
                )
            }
            Divergence::MissingTerminal { order } => {
                format!("{} {} coid={:?}", order.venue, order.symbol, order.client_order_id)
            }
            Divergence::OrphanLocalOrder { client_order_id } => format!("coid={client_order_id}"),
            Divergence::UnknownOrder(o) => {
                format!("{} {} coid={:?}", o.venue, o.symbol, o.client_order_id)
            }
            Divergence::PositionDrift { report, .. } => {
                format!("{} {} {:?}", report.venue, report.symbol, report.position_side)
            }
            Divergence::PositionOnlyExternal(p) => {
                format!("{} {} {:?}", p.venue, p.symbol, p.position_side)
            }
            Divergence::OrphanLocalPosition { venue, symbol, position_side, .. } => {
                format!("{venue} {symbol} {position_side}")
            }
            Divergence::BalanceDrift { venue, asset, .. } => format!("{venue} {asset}"),
            Divergence::JournalDivergence { detail, .. } => detail.clone(),
        }
    }
    pub fn kind(&self) -> DivergenceKind {
        match self {
            Divergence::MissingFill(_) => DivergenceKind::MissingFill,
            Divergence::MissingTerminal { .. } => DivergenceKind::MissingTerminal,
            Divergence::OrphanLocalOrder { .. } => DivergenceKind::OrphanLocalOrder,
            Divergence::UnknownOrder(_) => DivergenceKind::UnknownOrder,
            Divergence::PositionDrift { .. } => DivergenceKind::PositionDrift,
            Divergence::PositionOnlyExternal(_) => DivergenceKind::PositionOnlyExternal,
            Divergence::OrphanLocalPosition { .. } => DivergenceKind::OrphanLocalPosition,
            Divergence::BalanceDrift { .. } => DivergenceKind::BalanceDrift,
            Divergence::JournalDivergence { .. } => DivergenceKind::JournalDivergence,
        }
    }

    /// Per-INSTANCE origin: [`DivergenceKind::origin`] refined by the evidence THIS divergence
    /// actually carries. Exactly one refinement exists today: a [`Divergence::MissingFill`] whose
    /// `FillReport::client_order_id` is `None` reads [`DivergenceOrigin::External`] — the coid
    /// echo is the ONLY local linkage a fill report carries, so `None` means the venue fill names
    /// no local order (a foreign order's fill inside the lookback), which is the same predicate
    /// `resolve`'s `events_for` MissingFill arm keys its `external_coid` synthesis on. A
    /// coid-LINKED `MissingFill` keeps the kind's `ReconciliationMaterialized`: the venue echoed
    /// OUR client id, so this is our own order's fill, materialized late.
    ///
    /// `MissingTerminal` needs no such refinement BY CONSTRUCTION: `diff`'s order sweep raises it
    /// only from a report whose coid matched a live local order — the coid-less/unmatched side of
    /// that same `match` is `UnknownOrder`, already `External` per kind.
    ///
    /// Exhaustive over the variants (no `_` arm) for the same reason [`DivergenceKind::origin`]
    /// is: a new variant fails to COMPILE here until its instance-level evidence is classified.
    /// Consumed only by [`crate::recon::mode_applies_divergence`], and only when the policy opts
    /// in ([`ReconPolicy::hold_external_instances`]); every other path classifies per kind.
    pub fn origin(&self) -> DivergenceOrigin {
        match self {
            Divergence::MissingFill(f) => {
                if f.client_order_id.is_none() {
                    DivergenceOrigin::External
                } else {
                    self.kind().origin()
                }
            }
            Divergence::MissingTerminal { .. }
            | Divergence::OrphanLocalOrder { .. }
            | Divergence::UnknownOrder(_)
            | Divergence::PositionDrift { .. }
            | Divergence::PositionOnlyExternal(_)
            | Divergence::OrphanLocalPosition { .. }
            | Divergence::BalanceDrift { .. }
            | Divergence::JournalDivergence { .. } => self.kind().origin(),
        }
    }
}

/// The ORIGIN dimension of a [`DivergenceKind`] — the Nautilus-style EXTERNAL tag (split-plane
/// Pattern A, `docs/superpowers/specs/2026-08-18-split-plane-client-backend-design.md` §3b): whose
/// activity the divergence's EVIDENCE describes, so a policy can hold foreign venue activity for
/// an operator CLAIM instead of folding it blind. A property of the KIND, not of one divergence
/// instance — deliberately, because policy decides per kind ([`ReconPolicy::mode_for`]), so a
/// finer-grained tag could not be consumed by the policy machinery anyway. Pinned kind-by-kind
/// (exhaustive match, no `_` arm) by `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceOrigin {
    /// Our own activity, already settled by the venue: resolving the divergence merely
    /// MATERIALIZES locally, during the reconcile pass, what the venue confirmed about orders WE
    /// placed (a missed fill, a terminal we missed). The recon pass is the materializer — nothing
    /// foreign happened.
    ReconciliationMaterialized,
    /// Foreign venue activity (Nautilus's EXTERNAL): POSITIVE venue evidence — an order row, a
    /// position row, a net-qty residual, a cash move — that nothing local explains. ⚠ For
    /// [`DivergenceKind::PositionDrift`] this is deliberately the tag even though a drift can
    /// equally be OUR OWN fill aged out of the reconcile lookback: the evidence cannot
    /// distinguish the two, and "cannot be shown to be ours" is exactly what EXTERNAL means —
    /// which is why [`ReconPolicy::external_quarantine`] holds it for an operator claim rather
    /// than letting `hybrid`'s re-convergence argument fold it blind.
    External,
    /// The evidence is something MISSING, not observed activity: a venue-report row that never
    /// arrived (both orphan kinds) or live local state the journal says was lost
    /// ([`DivergenceKind::JournalDivergence`]). None of these fold any event under any policy
    /// (`vike_exec::recon::resolve`'s module doc is the authority), so the origin dimension is
    /// INERT for them — classified so the match stays exhaustive, never able to change a fold.
    Absence,
}

impl DivergenceKind {
    /// The [`DivergenceOrigin`] of this kind. Exhaustive on purpose — a new kind fails to COMPILE
    /// here until somebody states whose activity its evidence describes, the same forcing shape as
    /// `expected_hybrid_mode` in `crates/vike-exec/tests/recon/recon_policy_pin.rs` (which pins
    /// every row of this match).
    ///
    /// Agreement, pinned rather than assumed: `resolve`'s private `is_local_origin` (the
    /// bare-[`ReconMode::Hybrid`] fallback classifier) must answer `true` for exactly the
    /// [`DivergenceOrigin::ReconciliationMaterialized`] kinds —
    /// `origin_agrees_with_the_bare_hybrid_classifier` in the same pin file asserts it through
    /// `vike_exec::recon::mode_applies`.
    pub fn origin(self) -> DivergenceOrigin {
        use DivergenceOrigin::*;
        match self {
            DivergenceKind::MissingFill | DivergenceKind::MissingTerminal => {
                ReconciliationMaterialized
            }
            DivergenceKind::UnknownOrder
            | DivergenceKind::PositionDrift
            | DivergenceKind::PositionOnlyExternal
            | DivergenceKind::BalanceDrift => External,
            DivergenceKind::OrphanLocalOrder
            | DivergenceKind::OrphanLocalPosition
            | DivergenceKind::JournalDivergence => Absence,
        }
    }
}

/// What to do with a resolved divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReconMode {
    /// Emit synthesized events; fold immediately.
    Synthesize,
    /// Emit no events; surface a `ReconAlert` for operator confirm.
    Quarantine,
    /// Per-kind: local-origin kinds Synthesize, no-local-origin kinds Quarantine.
    Hybrid,
}

/// Policy = a default mode + per-kind overrides. `Hybrid` is expressed as the per-kind map preset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconPolicy {
    pub default: ReconMode,
    pub per_kind: BTreeMap<DivergenceKind, ReconMode>,
    /// The per-DIVERGENCE refinement opt-in (the residual #1380 named: "the tag is per-kind").
    /// `true` ⇒ [`crate::recon::mode_applies_divergence`] HOLDS any divergence whose INSTANCE
    /// origin ([`Divergence::origin`]) reads [`DivergenceOrigin::External`] even where its KIND
    /// auto-applies — today exactly one sub-case: a coid-less `MissingFill`. Set ONLY by
    /// [`Self::external_quarantine`]; every other constructor leaves it `false`, which makes the
    /// three pre-existing policies (and `hybrid`) byte-identical BY CONSTRUCTION — with the flag
    /// unset, `mode_applies_divergence` is definitionally `mode_applies(policy, d.kind())`.
    ///
    /// Serde: `default` + skipped-when-false, so an old journaled `Command::ReconcileReports`
    /// payload replays unchanged AND every flag-false policy still serializes byte-identically to
    /// its pre-refinement form (the journal is externally-tagged serde_json — self-describing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold_external_instances: bool,
}

/// serde `skip_serializing_if` helper for [`ReconPolicy::hold_external_instances`]: a `false`
/// flag is omitted from the wire so pre-refinement policies keep their exact journaled bytes.
fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for ReconPolicy {
    fn default() -> Self {
        ReconPolicy {
            default: ReconMode::Synthesize,
            per_kind: BTreeMap::new(),
            hold_external_instances: false,
        }
    }
}

impl ReconPolicy {
    /// The Hybrid preset: auto-apply local-origin kinds, quarantine no-local-origin kinds.
    ///
    /// NOTE **both** orphan kinds sit in the QUARANTINE list. Their shared evidence is an ABSENCE
    /// from the venue report, which an incomplete / symbol-scoped / coid-less fetch produces just as
    /// readily as a genuinely closed position or a genuinely terminal order, and neither has
    /// anything safe to auto-apply — see `resolve`'s module doc, the authority on both. This preset
    /// and the bare `ReconMode::Hybrid` fallback (`resolve::is_local_origin`, which lists neither)
    /// must agree; `resolve`'s `orphan_local_position_bare_hybrid_default_matches_the_preset` and
    /// `orphan_local_order_bare_hybrid_default_matches_the_preset` pin that.
    ///
    /// ⚠ `OrphanLocalOrder` MOVED here from the Synthesize list, and the move folds no event
    /// either way — the kind resolves to an empty event list under every policy. What it changes is
    /// that `hybrid` stops "folding" that empty list in silence and instead surfaces one
    /// aggregated, dedup-keyed operator alert. Detection that produced no outcome at all was the
    /// defect; see `resolve`'s `OrphanLocalOrder` section for why an alert (and not an auto-cancel)
    /// is the outcome.
    ///
    /// ⚠ EVERY kind is a NAMED row here — none may reach its mode through `default`. A
    /// fall-through is indistinguishable from a kind nobody classified, and that is exactly what
    /// happened to [`DivergenceKind::PositionDrift`]: it auto-applied on ten venues via `default`
    /// with no row and no assertion, while the only test of this preset
    /// (`hybrid_quarantines_only_no_local_origin_kinds`) named six kinds and omitted it. Its row
    /// below is `Synthesize` — behaviour-identical to the fall-through it replaces, so this is a
    /// classification, not a behaviour change — and the reasoning is on the variant's own doc.
    /// `the_hybrid_preset_classifies_every_kind_explicitly`
    /// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`) gates the "no fall-through" rule.
    ///
    /// ⚠ `PositionDrift` is ALSO where this preset and the bare-`ReconMode::Hybrid` fallback
    /// genuinely disagree: `resolve::is_local_origin` does not list it, so a hand-built
    /// `ReconPolicy { default: Hybrid, .. }` QUARANTINES what this preset folds. Nothing in the
    /// workspace builds a bare `Hybrid` (`vike_ops::reconcile_config::parse_policy` builds this
    /// preset for `"hybrid"`), so it is a latent trap rather than a live divergence — pinned as
    /// such by `bare_hybrid_and_the_preset_disagree_about_position_drift`.
    pub fn hybrid() -> Self {
        use DivergenceKind::*;
        let mut per_kind = BTreeMap::new();
        // `JournalDivergence`'s mode is decorative — `resolve` short-circuits it into an
        // investigative alert before `mode_applies` is consulted — but it gets a row like every
        // other kind so the map is exhaustive by construction.
        for k in [MissingFill, MissingTerminal, PositionDrift, JournalDivergence] {
            per_kind.insert(k, ReconMode::Synthesize);
        }
        for k in [
            UnknownOrder,
            PositionOnlyExternal,
            OrphanLocalOrder,
            OrphanLocalPosition,
            BalanceDrift,
        ] {
            per_kind.insert(k, ReconMode::Quarantine);
        }
        ReconPolicy { default: ReconMode::Synthesize, per_kind, hold_external_instances: false }
    }

    /// The `external-quarantine` preset (`VIKE_RECONCILE_POLICY=external-quarantine`, parsed in
    /// `vike_ops::reconcile_config`'s `parse_policy`): [`Self::hybrid`] with every
    /// [`DivergenceOrigin::External`] kind forced to [`ReconMode::Quarantine`] — the split-plane
    /// Pattern-A treatment of foreign venue activity (TAG and HOLD for an operator claim,
    /// superseding blind auto-apply; spec §3b). COMPUTED from [`DivergenceKind::origin`] over
    /// `hybrid()`'s rows rather than enumerated, so a future External kind is held here by
    /// construction and this constructor can never drift from the origin classification. Because
    /// `hybrid()` names EVERY kind, so does this preset (same no-fall-through gate,
    /// `the_external_quarantine_preset_classifies_every_kind_explicitly`).
    ///
    /// What actually changes at the KIND level: [`DivergenceKind::PositionDrift`] — the ONE
    /// External kind `hybrid` auto-applies — stops folding and is HELD; the other External kinds
    /// were already quarantined under `hybrid`, and every non-External kind keeps its `hybrid`
    /// row verbatim (`external_quarantine_differs_from_hybrid_only_where_origin_is_external`,
    /// `crates/vike-exec/tests/recon/recon_policy_pin.rs`, pins the delta).
    ///
    /// And one sub-case changes at the INSTANCE level (the residual #1380 named, closed): this is
    /// the one constructor that sets [`Self::hold_external_instances`], so
    /// [`crate::recon::mode_applies_divergence`] additionally HOLDS a coid-less
    /// `MissingFill` — a foreign order's fill inside the lookback, whose instance origin
    /// ([`Divergence::origin`]) reads External while its kind stays
    /// `ReconciliationMaterialized` — and keeps folding coid-LINKED MissingFills (our own orders'
    /// fills), so `hybrid`'s local-origin folds remain untouched. The `MissingFill` mode ROW is
    /// deliberately unchanged (`Synthesize`): the refinement is per-divergence, not a mode edit,
    /// which is what keeps the journaled per-kind map stable. The startup fold-set line renders
    /// the qualifier (`MissingFill (coid-linked only)` —
    /// `vike_ops::reconcile_config::auto_applied_kinds`).
    ///
    /// The CLAIM path is the existing operator-confirm surface, not new machinery: a held
    /// External divergence's [`ReconAlert::proposed_events`] carry exactly the events `hybrid`
    /// would have auto-folded (`resolve` computes events BEFORE consulting the mode), and
    /// `Command::ConfirmRecon` (`crates/vike-core/src/runtime/mod.rs`'s `confirm_recon`) folds
    /// them verbatim — claiming folds precisely what `hybrid` would have, with an operator in
    /// front of it; the held coid-less `MissingFill` rides the same contract (its alert proposes
    /// the synthesized `EXT-*` accept plus the fill). Inherited residual, named: a divergence
    /// that persists unconfirmed — a drift, or a coid-less fill still inside the lookback —
    /// re-raises one un-keyed alert per pass (the generic held path has no
    /// [`ReconAlert::dedup_key`]), exactly as it always has under `quarantine`.
    pub fn external_quarantine() -> Self {
        let mut p = Self::hybrid();
        for (kind, mode) in p.per_kind.iter_mut() {
            if kind.origin() == DivergenceOrigin::External {
                *mode = ReconMode::Quarantine;
            }
        }
        // The per-DIVERGENCE half of the same treatment (the residual #1380 named): the origin
        // tag is per-kind, so a coid-less MissingFill — a foreign order's fill inside the
        // lookback, whose INSTANCE origin reads External while its kind's does not — kept
        // auto-applying above. Opting in makes `mode_applies_divergence` hold exactly that
        // sub-case; coid-linked MissingFills (our own orders' fills) keep folding.
        p.hold_external_instances = true;
        p
    }

    pub fn mode_for(&self, kind: DivergenceKind) -> ReconMode {
        self.per_kind.get(&kind).copied().unwrap_or(self.default)
    }

    /// The policy an operator NAMES, resolved to the value that name means — the one construction
    /// of the four presets, shared by everything that has to agree about them.
    ///
    /// It lives on the type rather than in a binary's settings parser because two consumers must
    /// not disagree about what `"quarantine"` builds: `vike_ops::reconcile_config::parse_policy`
    /// resolves `VIKE_RECONCILE_POLICY` for a running mount, and `vike_ops::docs_data` renders the
    /// per-policy fold-vs-hold verdict of every [`DivergenceKind`] for the published capability
    /// data. A second hand-built `ReconPolicy { default: Quarantine, .. }` in either place is a
    /// second answer that can rot; there is now one, below both.
    ///
    /// Names are the spelling `VIKE_RECONCILE_POLICY` accepts, listed in [`POLICY_NAMES`], and the
    /// match is EXACT — lower-casing is the caller's, because a settings parser and a renderer
    /// disagree about whether to be lenient. `None` for anything else: the caller decides what an
    /// unrecognized value means (the mount falls back to `hybrid` and says so).
    ///
    /// ⚠ `"synthesize"` is [`ReconPolicy::default`] and NOT a bare `Hybrid`-defaulted value; the
    /// two are not interchangeable, for the reason [`ReconPolicy::hybrid`]'s doc gives about
    /// `PositionDrift`.
    #[must_use]
    pub fn from_policy_name(name: &str) -> Option<Self> {
        match name {
            "hybrid" => Some(Self::hybrid()),
            "synthesize" => Some(Self::default()),
            "quarantine" => Some(Self {
                default: ReconMode::Quarantine,
                per_kind: BTreeMap::new(),
                hold_external_instances: false,
            }),
            "external-quarantine" => Some(Self::external_quarantine()),
            _ => None,
        }
    }
}

/// Every policy name [`ReconPolicy::from_policy_name`] resolves, in the order an operator meets
/// them: the default first, then the two absolutes, then the split-plane refinement.
///
/// Kept beside that function so a fifth policy cannot be added to one and forgotten in the other;
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs` holds them exhaustive against each other.
pub const POLICY_NAMES: &[&str] = &["hybrid", "synthesize", "quarantine", "external-quarantine"];

/// An operator-facing reconciliation alert (Quarantine mode, or informational).
#[derive(Debug, Clone, PartialEq)]
pub struct ReconAlert {
    pub kind: DivergenceKind,
    pub detail: String,
    /// The events `resolve` WOULD have folded, held pending confirm.
    pub proposed_events: Vec<Event>,
    /// Venue orders to RE-REGISTER into local state on operator confirm (order-loss
    /// `JournalDivergence` recovery). Empty for every other alert. Applied via an insert-only
    /// registry seed (NOT the event fold — lifecycle events for an unknown coid are dropped, so a
    /// re-register cannot ride `proposed_events`); see `ExecutionEngine::reregister_orders`.
    pub recover_orders: Vec<OrderStatusReport>,
    /// Stable identity for a divergence that recurs by design across passes (today: an
    /// `UnknownOrder`'s `venue_order_id` under `generate_missing_orders`). `Some` tells the
    /// runtime's held-alert store to REFRESH the matching (venue, kind, key) row in place —
    /// keeping its confirm id — instead of appending a new alert every pass. `None` (every other
    /// alert) keeps the original append behavior byte-for-byte.
    pub dedup_key: Option<String>,

    /// The CHURN-FREE discriminator for an UN-KEYED alert — `None` means "use [`Self::detail`]",
    /// which is what every caller did before this field existed.
    ///
    /// ⚠ It exists because `vike_core`'s `HeldId::new` builds an un-keyed alert's identity out of
    /// the alert's `detail` STRING. So the detail is not only operator-facing text: it decides
    /// "same divergence, refresh the row" versus "new divergence, add one". The moment a detail
    /// starts carrying `qty=` or `avg_px=`, a drifting position becomes a NEW alert every pass —
    /// the measured production bug `crates/vike-core/src/runtime/recon_held_tests.rs` exists to
    /// prevent (60 rows and 60 confirm ids an hour; ~1,560 a day on the CI box).
    ///
    /// Splitting the two beats impoverishing the detail: an operator needs the quantity, and the
    /// identity must never see it. Fill it from [`Divergence::identity_detail`].
    pub identity_detail: Option<String>,
}

/// The pure output of one reconcile pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recon {
    /// Synthesized events to fold (Synthesize / auto-Hybrid kinds).
    pub events: Vec<Event>,
    /// The net-position seed to apply via `Command::ApplySnapshot`.
    pub snapshot: ReconcileSnapshot,
    /// Held divergences (Quarantine / quarantined-Hybrid kinds).
    pub alerts: Vec<ReconAlert>,
}

/// The cash/balance slice of a [`LocalView`], grouped so every `LocalView` construction site adds
/// ONE field and so [`crate::recon::diff_balance`]'s realized-PnL confound correction has all its
/// inputs in one place. Populated from `Account` by `ExecutionEngine::local_view`. Only the
/// first-class cash reconcile reads it; `diff` ignores it entirely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalCash {
    /// `Account::balance` — the local cash scalar. NOT a clean mirror of venue cash: realized PnL
    /// never enters it in Authoritative mode, so it legitimately drifts by Σ realized-since-sync.
    pub balance: f64,
    /// `Account::realized_pnl` — the running realized-PnL accumulator (fill-folded).
    pub realized_pnl: f64,
    /// `Account::realized_pnl_at_balance_sync` — realized_pnl captured at the last authoritative
    /// balance sync. `None` = never synced (first observation ⇒ adopt, never flag).
    pub realized_at_sync: Option<f64>,
    /// `Account::balance_mode` — `Delta` means `balance` is the arbitrary `seed_cash`, not venue
    /// truth, so a "drift" there is meaningless (first observation ⇒ adopt, never flag).
    pub mode: crate::account::BalanceMode,
}

impl Default for LocalCash {
    fn default() -> Self {
        LocalCash {
            balance: 0.0,
            realized_pnl: 0.0,
            realized_at_sync: None,
            mode: crate::account::BalanceMode::Delta,
        }
    }
}

/// Money tolerance for the first-class cash reconcile diff ([`crate::recon::diff_balance`]). The
/// diff fires only when the realized-corrected drift exceeds `max(abs_floor, rel_frac·|venue_bal|)`.
///
/// THIS IS MONEY — the two bands trade false alarms against missed drift:
/// - `rel_frac` (a fraction of wallet size) absorbs float/rounding noise and any commission/funding
///   TRACKING mismatch that scales with turnover, and grows with account size.
/// - `abs_floor` (quote-currency units) guards tiny accounts where the relative band underflows the
///   benign noise floor.
///
/// The default is deliberately conservative — see [`Default`]. Because the default policy
/// quarantines a `BalanceDrift` (never auto-folds a surprise), the cost of "too tight" is only a
/// noisy operator alert, not silent equity corruption — so we err slightly toward catching drift.
///
/// Serde-derived (like [`ReconPolicy`]) so it can ride the journaled [`crate::ReconcileReports`]
/// payload verbatim — the fold thread reads a deployment's env-tuned tolerance only through that
/// pass payload, exactly as it reads `policy`/`reconcile_balance`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BalanceTol {
    pub abs_floor: f64,
    pub rel_frac: f64,
}

impl Default for BalanceTol {
    /// `abs_floor = 1.0` quote unit, `rel_frac = 1e-4` (1 basis point of wallet). Rationale:
    /// after the realized-PnL correction the residual is (commission/funding tracking mismatch +
    /// float noise + any genuine surprise). 1 bp sits comfortably above per-pass rounding/tracking
    /// noise on any real account, and the 1-unit floor keeps a near-empty wallet from flagging on
    /// sub-unit noise, while a material cash move (a withdrawal/deposit/liquidation haircut, which
    /// is what we exist to catch) clears both bands. Each band is env-tunable per deployment via
    /// `VIKE_RECONCILE_BALANCE_TOL_ABS` / `VIKE_RECONCILE_BALANCE_TOL_REL` (parsed in
    /// `vike-app`'s `reconcile_config`); an unset knob keeps the matching default here, so both
    /// unset is byte-identical to this constant default.
    fn default() -> Self {
        BalanceTol { abs_floor: 1.0, rel_frac: 1e-4 }
    }
}

/// A point-in-time READ of local engine state handed to `diff`. Borrowed, never mutated.
pub struct LocalView<'a> {
    pub venue: &'a str,
    /// coid -> managed order (the engine registry).
    pub orders: &'a indexmap::IndexMap<String, ManagedOrder>,
    /// venue trade_ids already folded (fill dedup source of truth).
    pub seen_trade_ids: &'a std::collections::HashSet<String>,
    /// (symbol, position_side) -> signed net qty.
    pub positions: &'a indexmap::IndexMap<(String, String), f64>,
    /// tolerance for PositionDrift (absolute qty).
    pub qty_tol: f64,
    /// cash/balance slice for the first-class cash reconcile ([`crate::recon::diff_balance`]).
    /// `diff` ignores it — an existing `diff` fixture/test may leave it `LocalCash::default()`.
    pub cash: LocalCash,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_quarantines_only_no_local_origin_kinds() {
        let p = ReconPolicy::hybrid();
        assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
        assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
        assert_eq!(p.mode_for(DivergenceKind::PositionOnlyExternal), ReconMode::Quarantine);
        // A surprise cash move is no-local-origin: hybrid quarantines it (never auto-fold).
        assert_eq!(p.mode_for(DivergenceKind::BalanceDrift), ReconMode::Quarantine);
        // A local position the venue never reported is evidence-by-absence: hybrid quarantines it
        // too, so an incomplete position fetch can never become an auto-destructive flatten.
        assert_eq!(p.mode_for(DivergenceKind::OrphanLocalPosition), ReconMode::Quarantine);
        // ...and its ORDER-side namesake now agrees. It used to be classified local-origin, which
        // (since it folds nothing) meant `hybrid` "applied" an empty list and raised no alert — the
        // kind was detected and produced no outcome whatsoever. Quarantining it changes no event and
        // buys the operator alert; `resolve`'s module doc is the authority.
        assert_eq!(p.mode_for(DivergenceKind::OrphanLocalOrder), ReconMode::Quarantine);
        // The kind this test used to omit, which is also the only one that folds anything without
        // an operator. See `PositionDrift`'s own doc for why it is classified auto-apply.
        assert_eq!(p.mode_for(DivergenceKind::PositionDrift), ReconMode::Synthesize);
    }

    #[test]
    fn default_policy_synthesizes_everything() {
        let p = ReconPolicy::default();
        assert_eq!(p.mode_for(DivergenceKind::PositionOnlyExternal), ReconMode::Synthesize);
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    /// One of every [`Divergence`] variant, all naming `BTCUSDT` where they have a symbol at all.
    ///
    /// ⚠ A hand-written list, and its completeness is asserted below rather than assumed — the
    /// thing that FORCES a new variant to be described is `describe`'s total match, which is a
    /// compile error. This list only makes the assertions iterate, and
    /// `the_fixture_covers_every_kind` is what stops it silently covering eight of nine.
    fn every_variant() -> Vec<Divergence> {
        let fill = vike_model::FillReport {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            trade_id: "t1".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some("c1".into()),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: vike_model::events::LiquiditySide::Taker,
            ts: 1,
        };
        let order = vike_model::OrderStatusReport {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some("c1".into()),
            order_type: "LIMIT".into(),
            status: "FILLED".into(),
            side: 1,
            qty: 1.0,
            filled_qty: 1.0,
            avg_px: 100.0,
            ts: 1,
        };
        let pos = vike_model::PositionStatusReport {
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Long,
            qty: 1.0,
            avg_px: 100.0,
            ts: 1,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        };
        vec![
            Divergence::MissingFill(fill),
            Divergence::MissingTerminal { order: order.clone() },
            Divergence::OrphanLocalOrder { client_order_id: "c9".into() },
            Divergence::UnknownOrder(order),
            Divergence::PositionDrift { report: pos.clone(), local_qty: 0.0 },
            Divergence::PositionOnlyExternal(pos),
            Divergence::OrphanLocalPosition {
                venue: "bybit".into(),
                symbol: "BTCUSDT".into(),
                position_side: "long".into(),
                local_qty: 1.0,
            },
            Divergence::BalanceDrift {
                venue: "bybit".into(),
                asset: "USDT".into(),
                local: 1.0,
                venue_bal: 2.0,
                ts: 1,
            },
            Divergence::JournalDivergence {
                detail: "a journal detail built elsewhere".into(),
                recover_order: None,
            },
        ]
    }

    /// The fixture covers EVERY kind — otherwise the two assertions below quietly test eight of nine.
    #[test]
    fn the_fixture_covers_every_kind() {
        let mut seen: Vec<String> =
            every_variant().iter().map(|d| format!("{:?}", d.kind())).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), every_variant().len(), "the fixture repeats a kind: {seen:?}");
    }

    /// **A description must not be the kind's own name.**
    ///
    /// The fallback this replaced was `format!("{:?}", d.kind())`, and on the live the CI box box it
    /// produced `kind=PositionOnlyExternal detail=PositionOnlyExternal` — a line raised
    /// specifically to answer *which* that answered *the same*. This asserts the two differ for
    /// every variant, which is the one property the fallback could never have.
    #[test]
    fn no_variant_describes_itself_as_its_own_kind() {
        for d in every_variant() {
            let desc = d.describe();
            assert_ne!(
                desc,
                format!("{:?}", d.kind()),
                "{:?} describes itself as its own name — that is the fallback this replaced",
                d.kind()
            );
            assert!(!desc.is_empty(), "{:?} describes itself as nothing", d.kind());
        }
    }

    /// ...and it must name the SYMBOL, which is the first thing an operator needs.
    ///
    /// ⚠ Exempt by construction, not by oversight: `OrphanLocalOrder` carries a client order id and
    /// no symbol, `BalanceDrift` is per ASSET rather than per instrument, and `JournalDivergence`
    /// forwards a detail built elsewhere. Naming them here is what keeps the exemption a decision.
    #[test]
    fn every_instrument_shaped_divergence_names_its_symbol() {
        for d in every_variant() {
            let exempt = matches!(
                d.kind(),
                DivergenceKind::OrphanLocalOrder
                    | DivergenceKind::BalanceDrift
                    | DivergenceKind::JournalDivergence
            );
            if exempt {
                continue;
            }
            assert!(
                d.describe().contains("BTCUSDT"),
                "{:?} does not name the symbol: {}",
                d.kind(),
                d.describe()
            );
        }
    }
}
