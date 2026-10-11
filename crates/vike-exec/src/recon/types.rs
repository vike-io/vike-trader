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
    /// [`Self::OrphanLocalPosition`]. (no local origin: the ORDER is ours, but the DIVERGENCE is
    /// the venue's SILENCE about it. It folds nothing under any policy, so the classification
    /// decides only whether held policies SURFACE it; `vike_exec::recon::resolve`'s module doc is
    /// the authority.)
    OrphanLocalOrder,
    /// Venue reports an order with no local match. (no local origin)
    UnknownOrder,
    /// Venue net qty differs from local net qty beyond tolerance. (no local origin — but the ONE
    /// no-local-origin kind [`ReconPolicy::hybrid`] AUTO-APPLIES, deliberately: unlike every other
    /// no-local-origin kind its evidence is a POSITIVE venue position row rather than an absence,
    /// and it is the only path that re-converges qty after a fill older than the reconcile
    /// lookback. Pinned by `crates/vike-exec/tests/recon/recon_policy_pin.rs`. With `MissingFill`
    /// it is one of the only two kinds `hybrid` folds anything for; `MissingTerminal` auto-applies
    /// and resolves to no events.)
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
    /// First-class cash reconcile. Carries both sides — `local` is the realized-PnL-CORRECTED
    /// expected cash (`balance + realized-since-sync`, NOT raw `Account::balance`), `venue_bal` the
    /// venue's authoritative quote cash — plus the `asset` (quote currency) `resolve` synthesizes
    /// the correcting `Event::AccountState` for.
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
    /// only this. It replaced a `format!("{:?}", d.kind())` fallback, which renders a kind as its
    /// own name: the live held-divergence log line read
    /// `kind=PositionOnlyExternal detail=PositionOnlyExternal`, answering *which* with *the same*.
    ///
    /// A TOTAL match, deliberately: a new [`Divergence`] variant is a compile error here rather
    /// than a variant that silently inherits an empty description.
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
    /// ⚠ **It exists because `vike_core`'s `HeldId::new` builds an un-keyed alert's identity out of
    /// the alert's `detail` STRING**, so the detail decides "same divergence, refresh the row"
    /// versus "new divergence, add one". A detail carrying `qty=` and `avg_px=` makes a drifting
    /// position a NEW alert every pass — the measured production bug
    /// `crates/vike-core/src/runtime/tests/recon_held.rs` exists to prevent (60 rows and 60 confirm
    /// ids per hour). Splitting the two beats impoverishing the detail: an operator needs the
    /// quantity, and the identity must not see it. Anything added below must be stable pass to
    /// pass for an UNCHANGED divergence — venue, instrument, side, an id. Never a number the venue
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
    /// carries. One refinement exists today: a [`Divergence::MissingFill`] whose
    /// `FillReport::client_order_id` is `None` reads [`DivergenceOrigin::External`] — the coid
    /// echo is the ONLY local linkage a fill report carries, so `None` means the fill names no
    /// local order (a foreign order's fill inside the lookback), the same predicate `resolve`'s
    /// `events_for` MissingFill arm keys its `external_coid` synthesis on. A coid-LINKED
    /// `MissingFill` keeps the kind's `ReconciliationMaterialized`: our own order's fill,
    /// materialized late.
    ///
    /// `MissingTerminal` needs no refinement BY CONSTRUCTION: `diff`'s order sweep raises it only
    /// from a report whose coid matched a live local order (the unmatched side is `UnknownOrder`,
    /// already `External` per kind).
    ///
    /// Exhaustive (no `_` arm) for the same reason as [`DivergenceKind::origin`]: a new variant
    /// fails to COMPILE until its instance-level evidence is classified. In production it is
    /// consumed only by [`crate::recon::mode_applies_divergence`]'s opt-in term
    /// ([`ReconPolicy::hold_external_instances`]); every other path classifies per kind.
    ///
    /// ⚠ This is the ORIGIN-BLIND answer: it never reads an instance tag, and the cross-instance
    /// hold does not go through it — [`crate::recon::mode_applies_divergence`] asks
    /// [`Self::is_foreign_instance`] FIRST and returns early. [`Self::origin_for`] is the
    /// instance-aware classification, and nothing in production calls it.
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

    /// The client order id this divergence's EVIDENCE carries, when it carries one. Exhaustive
    /// (no `_` arm): a new variant must state whether it names an order, because that answer is
    /// what [`Self::is_foreign_instance`] reads.
    ///
    /// `MissingFill`'s coid echo is `Option`, and `None` is already the "foreign order's fill"
    /// signal [`Self::origin`] refines on. The position- and cash-shaped kinds name no order.
    /// `JournalDivergence` carries a recoverable venue order, but `resolve` decides its mode before
    /// any of this is consulted, so it deliberately answers `None` rather than invite a fold
    /// decision on it.
    pub fn client_order_id(&self) -> Option<&str> {
        match self {
            Divergence::MissingFill(f) => f.client_order_id.as_deref(),
            Divergence::MissingTerminal { order } => order.client_order_id.as_deref(),
            Divergence::UnknownOrder(o) => o.client_order_id.as_deref(),
            Divergence::OrphanLocalOrder { client_order_id } => Some(client_order_id.as_str()),
            Divergence::PositionDrift { .. }
            | Divergence::PositionOnlyExternal(_)
            | Divergence::OrphanLocalPosition { .. }
            | Divergence::BalanceDrift { .. }
            | Divergence::JournalDivergence { .. } => None,
        }
    }

    /// Does this divergence's evidence carry ANOTHER instance's origin claim?
    ///
    /// The cross-MACHINE half of the duplicate-instance interlock
    /// (`crates/vike-journal/src/lock.rs` is the same-machine half, and its module doc lists what
    /// it cannot reach). Two deployments sharing one venue API key each hold their own journal lock
    /// and each believe they are alone; if both declare an [`vike_model::InstanceOrigin`], every id
    /// they mint says which one placed it, and this is where that claim is read.
    ///
    /// `false` unless the answer is positively yes — `crates/vike-model/src/instance_origin.rs`'s
    /// `coid_is_foreign` says why the asymmetry is the safety property. An UNTAGGED id is never
    /// foreign: an instance that turns the feature on while a sibling has not is no worse off than
    /// before.
    pub fn is_foreign_instance(&self, local: Option<&vike_model::InstanceOrigin>) -> bool {
        match self.client_order_id() {
            Some(coid) => vike_model::instance_origin::coid_is_foreign(coid, local),
            None => false,
        }
    }

    /// The operator-facing half of [`Self::is_foreign_instance`]: the sentence a held alert's
    /// detail carries when this divergence names another instance, and `None` when it does not.
    ///
    /// It names BOTH tags, because the useful question is "which of my deployments placed it" — an
    /// operator with three tagged instances needs that to know which box to look at. Neither tag is
    /// a secret: an origin already rides on the public wire inside every client order id.
    #[must_use]
    pub fn foreign_instance_note(
        &self,
        local: Option<&vike_model::InstanceOrigin>,
    ) -> Option<String> {
        let local = local?;
        let coid = self.client_order_id()?;
        let theirs = vike_model::instance_origin::origin_of_coid(coid)?;
        if theirs == local.as_str() {
            return None;
        }
        Some(format!(
            "placed by ANOTHER INSTANCE (origin={theirs}, this instance is {local}) — two \
             deployments are trading this venue account; nothing was folded into local books"
        ))
    }

    /// [`Self::origin`] refined by WHOSE instance the evidence names: an order or fill carrying
    /// another instance's origin claim reads [`DivergenceOrigin::External`] whatever its kind
    /// says, because "another process is trading this account" is exactly what External means.
    /// It can only ever move a divergence TOWARD External, never away from it. `local: None`
    /// (nothing configured — the default) is definitionally [`Self::origin`].
    ///
    /// ⚠ It classifies; it does not decide. No production code calls it (the pin test does):
    /// [`crate::recon::mode_applies_divergence`] reaches the same hold through
    /// [`Self::is_foreign_instance`] directly, under every policy.
    pub fn origin_for(&self, local: Option<&vike_model::InstanceOrigin>) -> DivergenceOrigin {
        if self.is_foreign_instance(local) {
            return DivergenceOrigin::External;
        }
        self.origin()
    }
}

/// The ORIGIN dimension of a [`DivergenceKind`] — the EXTERNAL-activity tag (split-plane
/// Pattern A, `docs/superpowers/specs/2026-08-18-split-plane-client-backend-design.md` §3b): whose
/// activity the divergence's EVIDENCE describes, so a policy can hold foreign venue activity for
/// an operator CLAIM instead of folding it blind. A property of the KIND, because policy decides
/// per kind ([`ReconPolicy::mode_for`]). Pinned kind-by-kind (exhaustive match, no `_` arm) by
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivergenceOrigin {
    /// Our own activity, already settled by the venue: resolving the divergence merely
    /// MATERIALIZES locally what the venue confirmed about orders WE placed (a missed fill, a
    /// terminal we missed). Nothing foreign happened.
    ReconciliationMaterialized,
    /// Foreign venue activity (EXTERNAL): POSITIVE venue evidence — an order row, a
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
    /// Every variant, in declaration order. Held complete by a test below, not by construction.
    pub const ALL: &'static [DivergenceKind] = &[
        DivergenceKind::MissingFill,
        DivergenceKind::MissingTerminal,
        DivergenceKind::OrphanLocalOrder,
        DivergenceKind::UnknownOrder,
        DivergenceKind::PositionDrift,
        DivergenceKind::PositionOnlyExternal,
        DivergenceKind::OrphanLocalPosition,
        DivergenceKind::BalanceDrift,
        DivergenceKind::JournalDivergence,
    ];

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
    /// The per-DIVERGENCE refinement opt-in. `true` ⇒ [`crate::recon::mode_applies_divergence`]
    /// HOLDS any divergence whose INSTANCE origin ([`Divergence::origin`]) reads
    /// [`DivergenceOrigin::External`] even where its KIND auto-applies — today exactly one
    /// sub-case: a coid-less `MissingFill`. Set ONLY by [`Self::external_quarantine`]; every other
    /// constructor leaves it `false`, under which `mode_applies_divergence` is definitionally
    /// `mode_applies(policy, d.kind())`.
    ///
    /// Serde: `default` + skipped-when-false, so an old journaled `Command::ReconcileReports`
    /// payload replays unchanged AND every flag-false policy still serializes byte-identically to
    /// its pre-refinement form (the journal is externally-tagged serde_json — self-describing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold_external_instances: bool,

    /// THIS instance's own origin claim — the identity every fold decision is made RELATIVE TO.
    /// `None` (the default, and every constructor's value) leaves [`Divergence::origin_for`] equal
    /// to [`Divergence::origin`] and [`crate::recon::mode_applies_divergence`] unchanged.
    ///
    /// ⚠ **It is not a policy KNOB, and no operator sets it.**
    /// `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_compute` stamps it, per pass, from
    /// the tag its OWN `ClientOrderIdGenerator` is minting under — not from configuration. Two
    /// reasons, and the second is the load-bearing one:
    ///
    /// - The comparison is then against the identity ACTUALLY on the wire. A core whose resumed
    ///   `coid_session` predates the operator turning the origin on still mints untagged ids, and
    ///   reconcile must judge by what it minted, not by what it was told.
    /// - It is DETERMINISTIC UNDER REPLAY without journaling anything new. The generator's session
    ///   is restored from the journal's own `coid_session`, so a replay stamps the same tag and
    ///   reaches the same fold decision; reading configuration would make a replay's folds depend
    ///   on the box replaying them — the determinism fence `crates/vike-journal/src/lock.rs` exists
    ///   to protect.
    ///
    /// `pub` and settable so a pure `resolve` caller (the offline `run_pass`, a test) can supply
    /// the identity explicitly. Serde: `default` + skipped-when-absent, so an old journaled
    /// `Command::ReconcileReports` replays unchanged and an unconfigured instance's policy still
    /// serializes byte-identically to its pre-origin form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_instance_origin: Option<vike_model::InstanceOrigin>,
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
            local_instance_origin: None,
        }
    }
}

impl ReconPolicy {
    /// The Hybrid preset: auto-apply local-origin kinds, quarantine no-local-origin kinds.
    ///
    /// **Both** orphan kinds sit in the QUARANTINE list. Their shared evidence is an ABSENCE from
    /// the venue report, which an incomplete / symbol-scoped / coid-less fetch produces just as
    /// readily as a genuinely closed position or terminal order, and neither has anything safe to
    /// auto-apply — `resolve`'s module doc is the authority on both. This preset and the bare
    /// `ReconMode::Hybrid` fallback (`resolve::is_local_origin`, which lists neither) must agree;
    /// `resolve`'s `orphan_local_position_bare_hybrid_default_matches_the_preset` and
    /// `orphan_local_order_bare_hybrid_default_matches_the_preset` pin that.
    ///
    /// ⚠ EVERY kind is a NAMED row here — none may reach its mode through `default`. A
    /// fall-through is indistinguishable from a kind nobody classified, which is how
    /// [`DivergenceKind::PositionDrift`] once auto-applied on ten venues with no row and no
    /// assertion. Its row is `Synthesize`; the reasoning is on the variant's own doc.
    /// `the_hybrid_preset_classifies_every_kind_explicitly`
    /// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`) gates the "no fall-through" rule.
    ///
    /// ⚠ `PositionDrift` is ALSO where this preset and the bare-`ReconMode::Hybrid` fallback
    /// genuinely disagree: `resolve::is_local_origin` does not list it, so a hand-built
    /// `ReconPolicy { default: Hybrid, .. }` QUARANTINES what this preset folds. Nothing in the
    /// workspace builds a bare `Hybrid` (`vike_tradehub::reconcile_config::parse_policy` builds this
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
        ReconPolicy {
            default: ReconMode::Synthesize,
            per_kind,
            hold_external_instances: false,
            local_instance_origin: None,
        }
    }

    /// The `external-quarantine` preset (`VIKE_RECONCILE_POLICY=external-quarantine`, parsed in
    /// `vike_tradehub::reconcile_config`'s `parse_policy`): [`Self::hybrid`] with every
    /// [`DivergenceOrigin::External`] kind forced to [`ReconMode::Quarantine`] — the split-plane
    /// Pattern-A treatment of foreign venue activity (TAG and HOLD for an operator claim instead of
    /// blind auto-apply; spec §3b). COMPUTED from [`DivergenceKind::origin`] over `hybrid()`'s rows
    /// rather than enumerated, so a future External kind is held here by construction. Because
    /// `hybrid()` names EVERY kind, so does this preset (same no-fall-through gate,
    /// `the_external_quarantine_preset_classifies_every_kind_explicitly`).
    ///
    /// At the KIND level: [`DivergenceKind::PositionDrift`] — the ONE External kind `hybrid`
    /// auto-applies — stops folding and is HELD; the other External kinds were already quarantined
    /// under `hybrid`, and every non-External kind keeps its `hybrid` row verbatim
    /// (`external_quarantine_differs_from_hybrid_only_where_origin_is_external`,
    /// `crates/vike-exec/tests/recon/recon_policy_pin.rs`, pins the delta).
    ///
    /// At the INSTANCE level it is the one constructor that sets [`Self::hold_external_instances`]
    /// (that field's doc): a coid-less `MissingFill` (a foreign order's fill inside the lookback)
    /// is HELD while coid-LINKED MissingFills (our own orders' fills) keep folding. The
    /// `MissingFill` mode ROW stays `Synthesize` — the refinement is per-divergence, not a mode
    /// edit, which keeps the journaled per-kind map stable — and the startup fold-set line renders
    /// the qualifier (`MissingFill (coid-linked only)` —
    /// `vike_tradehub::reconcile_config::auto_applied_kinds`).
    ///
    /// The CLAIM path is the existing operator confirm: `resolve` computes events BEFORE consulting
    /// the mode, so a held External divergence's [`ReconAlert::proposed_events`] (for a coid-less
    /// `MissingFill`, the synthesized `EXT-*` accept plus the fill) are exactly what `hybrid` would
    /// have auto-folded, and `Command::ConfirmRecon` (`crates/vike-core/src/runtime/reconcile.rs`'s
    /// `confirm_recon`) folds them verbatim. A divergence that persists unconfirmed — a drift, or
    /// a coid-less fill still inside the lookback — is re-raised un-keyed every pass (the generic
    /// held path sets no [`ReconAlert::dedup_key`]) and refreshes its held row by identity, exactly
    /// as under `quarantine`.
    pub fn external_quarantine() -> Self {
        let mut p = Self::hybrid();
        for (kind, mode) in p.per_kind.iter_mut() {
            if kind.origin() == DivergenceOrigin::External {
                *mode = ReconMode::Quarantine;
            }
        }
        // The per-DIVERGENCE half: the origin tag is per-kind, so a coid-less MissingFill (whose
        // INSTANCE origin reads External while its kind's does not) still auto-applies above.
        // Opting in makes `mode_applies_divergence` hold exactly that sub-case; coid-linked
        // MissingFills (our own orders' fills) keep folding.
        p.hold_external_instances = true;
        p
    }

    pub fn mode_for(&self, kind: DivergenceKind) -> ReconMode {
        self.per_kind.get(&kind).copied().unwrap_or(self.default)
    }

    /// The policy an operator NAMES, resolved to the value that name means — the one construction
    /// of the four presets, shared by everything that has to agree about them. It lives on the
    /// type rather than in a binary's settings parser because two consumers must not disagree
    /// about what `"quarantine"` builds: `vike_tradehub::reconcile_config::parse_policy` resolves
    /// `VIKE_RECONCILE_POLICY` for a running mount, and `vike_docs` renders the per-policy
    /// fold-vs-hold verdict of every [`DivergenceKind`] for the published capability data.
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
                local_instance_origin: None,
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
    /// Stable identity for a divergence that recurs by design across passes. Four shapes exist,
    /// all set by `resolve`: an `UnknownOrder`'s `venue_order_id` (`generate_missing_orders` on),
    /// `balance:{asset}`, `position:{symbol}:{side}` (`OrphanLocalPosition`) and the constant
    /// `orders:orphan-local` (`ORPHAN_LOCAL_ORDER_KEY`). `Some` makes the key the alert's whole
    /// identity, so the runtime's held-alert store REFRESHES the matching (venue, account, kind,
    /// key) row in place — keeping its confirm id — whatever the detail says. `None`: the runtime
    /// derives the identity from `identity_detail` (else the detail) plus the proposed fill legs
    /// (`vike_core`'s `HeldId::new`).
    pub dedup_key: Option<String>,

    /// The CHURN-FREE discriminator for an UN-KEYED alert — `None` means "use [`Self::detail`]".
    ///
    /// ⚠ `vike_core`'s `HeldId::new` builds an un-keyed alert's identity from it, so the identity
    /// must never carry a number the venue recomputes (`qty=`, `avg_px=`) or a drifting position
    /// becomes a NEW alert every pass. Fill it from [`Divergence::identity_detail`], whose doc
    /// carries the measured bug.
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
/// Serde-derived so it rides the journaled [`crate::ReconcileReports`] payload verbatim: the fold
/// thread reads a deployment's env-tuned tolerance only through that pass payload, as it reads
/// `policy`/`reconcile_balance`.
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
    /// `vike-tradehub`'s `reconcile_config`); an unset knob keeps the matching default here, so both
    /// unset is byte-identical to this constant default.
    fn default() -> Self {
        BalanceTol { abs_floor: 1.0, rel_frac: 1e-4 }
    }
}

/// One order a PREVIOUS session left behind, as the ownership file remembers it — the input of
/// `plan_restore`. It carries the coid and the scope the order was placed under, and
/// deliberately NO mount id: this crate does not know mounts (the caller keeps the coid → mount
/// map and joins on `coid`).
///
/// `venue`, `symbol` and `account` are `Option` only because an OLD ownership record may lack
/// them. A ref without BOTH `venue` and `symbol` is never declared gone (its absence from a
/// venue report proves nothing about a venue it does not name); it can only be ADOPTED, when its
/// coid shows up on the venue. `account: None` means "the venue's single/default account".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredOrderRef {
    pub coid: String,
    pub venue: Option<String>,
    pub symbol: Option<String>,
    pub account: Option<String>,
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
        // ...and its ORDER-side namesake agrees: quarantining it changes no event and buys the
        // operator alert; `resolve`'s module doc is the authority.
        assert_eq!(p.mode_for(DivergenceKind::OrphanLocalOrder), ReconMode::Quarantine);
        // Classified auto-apply on purpose; see `PositionDrift`'s own doc.
        assert_eq!(p.mode_for(DivergenceKind::PositionDrift), ReconMode::Synthesize);
    }

    #[test]
    fn default_policy_synthesizes_everything() {
        let p = ReconPolicy::default();
        assert_eq!(p.mode_for(DivergenceKind::PositionOnlyExternal), ReconMode::Synthesize);
    }

    /// [`DivergenceKind::ALL`]'s completeness guard. `declared_index` is an EXHAUSTIVE match with
    /// no `_` arm, so a new variant fails to COMPILE here: give it the next index, bump `ARMS`
    /// and append it to `ALL`. Every position must hold the variant declared at that index, which
    /// with the length check is `ALL[declared_index(k)] == k` for every variant: no gap, no
    /// duplicate, no reordering.
    #[test]
    fn all_is_every_divergence_kind_in_declaration_order() {
        fn declared_index(k: DivergenceKind) -> usize {
            match k {
                DivergenceKind::MissingFill => 0,
                DivergenceKind::MissingTerminal => 1,
                DivergenceKind::OrphanLocalOrder => 2,
                DivergenceKind::UnknownOrder => 3,
                DivergenceKind::PositionDrift => 4,
                DivergenceKind::PositionOnlyExternal => 5,
                DivergenceKind::OrphanLocalPosition => 6,
                DivergenceKind::BalanceDrift => 7,
                DivergenceKind::JournalDivergence => 8,
            }
        }
        // The number of arms in `declared_index`.
        const ARMS: usize = 9;
        assert_eq!(DivergenceKind::ALL.len(), ARMS, "ALL and `declared_index` disagree");
        for (i, &k) in DivergenceKind::ALL.iter().enumerate() {
            assert_eq!(declared_index(k), i, "ALL[{i}] is {k:?}, declared at another index");
        }
    }
}

#[path = "describe_tests.rs"]
#[cfg(test)]
mod describe_tests;
