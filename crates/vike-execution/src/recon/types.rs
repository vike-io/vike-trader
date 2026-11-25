//! Pure reconciliation vocabulary: the divergences `diff` emits, the policy that decides
//! fold-vs-hold, and the `ReconcilePassOutcome` output `resolve` produces. No I/O, no clock, no threads — this is
//! what makes golden-fixture tests and the three-way journal cross-check possible.

use std::collections::BTreeMap;

use vike_model::events::{Event, PositionSide};
use vike_model::orders::client_id::{
    OrderClientIdReading, OrderClientIdWords, describe_order_client_id, parse_order_client_id,
};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport, ReconcilePolicyName};

use crate::execution_engine::ReconcileSnapshot;
use vike_model::OrderRecord;

use vike_model::{ReconcileDivergenceKind, ReconcileDivergenceOrigin};

/// One detected disagreement, carrying the venue evidence needed to resolve it.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileDivergence {
    MissingFill(FillReport),
    MissingTerminal {
        order: OrderStatusReport,
    },
    OrphanLocalOrder {
        order_client_id: String,
    },
    UnknownOrder(OrderStatusReport),
    PositionDrift {
        report: PositionStatusReport,
        local_quantity: f64,
    },
    PositionOnlyExternal(PositionStatusReport),
    /// A live LOCAL position for which this pass's venue report carries no row at all (the
    /// local-side position sweep — `diff`'s step 5, the mirror of the `OrphanLocalOrder` sweep).
    /// Carries the whole local key plus the qty, because there is no venue report to carry: an
    /// absent row is the evidence. `venue` is the `ReconcileLocalView`'s venue, echoed so the alert detail
    /// is self-describing.
    OrphanLocalPosition {
        venue: String,
        symbol: String,
        /// The second half of the `ReconcileLocalView::positions` key.
        position_side: PositionSide,
        /// Signed local net qty (never within `quantity_tolerance` of flat — `diff` filters those out).
        local_quantity: f64,
    },
    /// First-class cash reconcile. Carries both sides — `local` is the realized-PnL-CORRECTED
    /// expected cash (`balance + realized-since-sync`, NOT raw `Account::balance`), `venue_balance` the
    /// venue's authoritative quote cash — plus the `asset` (quote currency) `resolve` synthesizes
    /// the correcting `Event::AccountState` for.
    BalanceDrift {
        venue: String,
        asset: String,
        local: f64,
        venue_balance: f64,
        timestamp: i64,
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
    /// A live local order whose venue row states other terms (`crate::recon::diff_order_terms`).
    /// Each term is `(local, venue)` where it drifted and `None` where it agrees or is not compared;
    /// at least one is `Some`. `venue` and `symbol` are the venue row's, `ts` its own stamp.
    OrderTermsDrift {
        venue: String,
        symbol: String,
        order_client_id: String,
        /// The limit price, `(local, venue)`.
        price: Option<(f64, f64)>,
        /// The trigger price, `(local, venue)`.
        trigger_price: Option<(f64, f64)>,
        /// The order's TOTAL size, `(local, venue)`.
        qty: Option<(f64, f64)>,
        timestamp: i64,
    },
}

/// `(local, venue)` of each drifted term of an `OrderTermsDrift`, as an operator reads it.
fn terms_words(
    price: &Option<(f64, f64)>,
    trigger_price: &Option<(f64, f64)>,
    qty: &Option<(f64, f64)>,
) -> String {
    let word = |name: &str, t: &Option<(f64, f64)>| {
        t.map(|(local, venue)| format!("{name} local={local} venue={venue}"))
    };
    [word("price", price), word("trigger", trigger_price), word("qty", qty)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

impl ReconcileDivergence {
    /// One line naming WHAT diverged — the symbol, the quantity, the id — for an operator who has
    /// only this. It replaced a `format!("{:?}", d.kind())` fallback, which renders a kind as its
    /// own name: the live held-divergence log line read
    /// `kind=PositionOnlyExternal detail=PositionOnlyExternal`, answering *which* with *the same*.
    ///
    /// A TOTAL match, deliberately: a new [`ReconcileDivergence`] variant is a compile error here rather
    /// than a variant that silently inherits an empty description.
    ///
    /// No secret can reach it: every field below is a symbol, a side, a quantity, an asset or a
    /// order client id. There is no credential on `ReconcileDivergence` to leak.
    ///
    /// An `UnknownOrder` whose order client id is a v2 id ends with that id's words (source, strategy, account,
    /// intent, and the group and role of a leg: [`describe_order_client_id`]), after ` — `. A LABEL
    /// (decision 0270's D6): nothing links the order to its group by it, [`Self::identity_detail`]
    /// leaves it out, and every other id prints exactly as before.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            ReconcileDivergence::MissingFill(f) => {
                format!(
                    "{} {} qty={} px={} order_client_id={:?}",
                    f.venue, f.symbol, f.last_qty, f.last_price, f.order_client_id
                )
            }
            ReconcileDivergence::MissingTerminal { order } => {
                format!(
                    "{} {} order_client_id={:?} status={:?}",
                    order.venue,
                    order.symbol,
                    order.order_client_id,
                    order.status.as_str()
                )
            }
            ReconcileDivergence::OrphanLocalOrder { order_client_id } => {
                format!("order_client_id={order_client_id}")
            }
            ReconcileDivergence::UnknownOrder(o) => {
                let head = format!(
                    "{} {} order_client_id={:?} status={:?}",
                    o.venue,
                    o.symbol,
                    o.order_client_id,
                    o.status.as_str()
                );
                match self.v2_words() {
                    Some(words) => format!("{head} — {words}"),
                    None => head,
                }
            }
            ReconcileDivergence::PositionDrift { report, local_quantity } => format!(
                "{} {} {:?} venue_qty={} local_qty={local_quantity}",
                report.venue, report.symbol, report.position_side, report.qty
            ),
            ReconcileDivergence::PositionOnlyExternal(p) => format!(
                "{} {} {:?} qty={} average_price={} — the venue holds a position this process has no book for",
                p.venue, p.symbol, p.position_side, p.qty, p.average_price
            ),
            ReconcileDivergence::OrphanLocalPosition {
                venue,
                symbol,
                position_side,
                local_quantity,
            } => {
                format!(
                    "{venue} {symbol} {position_side} local_qty={local_quantity} — the venue reports none"
                )
            }
            ReconcileDivergence::BalanceDrift { venue, asset, local, venue_balance, .. } => {
                format!("{venue} {asset} local={local} venue={venue_balance}")
            }
            ReconcileDivergence::JournalDivergence { detail, .. } => detail.clone(),
            ReconcileDivergence::OrderTermsDrift {
                venue,
                symbol,
                order_client_id,
                price,
                trigger_price,
                qty,
                ..
            } => format!(
                "{venue} {symbol} order_client_id={order_client_id} {} — the venue's order rests at other \
                 terms than the book's (an amend whose answer was lost, or an edit at the venue)",
                terms_words(price, trigger_price, qty)
            ),
        }
    }

    /// The CHURN-FREE half of [`Self::describe`] — what this divergence IS, with every quantity and
    /// price left out.
    ///
    /// ⚠ **It exists because `vike_core`'s `ReconcileHeldId::new` builds an un-keyed alert's identity out of
    /// the alert's `detail` STRING**, so the detail decides "same divergence, refresh the row"
    /// versus "new divergence, add one". A detail carrying `qty=` and `average_price=` makes a drifting
    /// position a NEW alert every pass — the measured production bug
    /// `crates/vike-core/src/runtime/tests/recon_held.rs` exists to prevent (60 rows and 60 confirm
    /// ids per hour). Splitting the two beats impoverishing the detail: an operator needs the
    /// quantity, and the identity must not see it. Anything added below must be stable pass to
    /// pass for an UNCHANGED divergence — venue, instrument, side, an id. Never a number the venue
    /// recomputes.
    #[must_use]
    pub fn identity_detail(&self) -> String {
        match self {
            ReconcileDivergence::MissingFill(f) => {
                format!(
                    "{} {} order_client_id={:?} trade={}",
                    f.venue, f.symbol, f.order_client_id, f.trade_id
                )
            }
            ReconcileDivergence::MissingTerminal { order } => {
                format!(
                    "{} {} order_client_id={:?}",
                    order.venue, order.symbol, order.order_client_id
                )
            }
            ReconcileDivergence::OrphanLocalOrder { order_client_id } => {
                format!("order_client_id={order_client_id}")
            }
            ReconcileDivergence::UnknownOrder(o) => {
                format!("{} {} order_client_id={:?}", o.venue, o.symbol, o.order_client_id)
            }
            ReconcileDivergence::PositionDrift { report, .. } => {
                format!("{} {} {:?}", report.venue, report.symbol, report.position_side)
            }
            ReconcileDivergence::PositionOnlyExternal(p) => {
                format!("{} {} {:?}", p.venue, p.symbol, p.position_side)
            }
            ReconcileDivergence::OrphanLocalPosition { venue, symbol, position_side, .. } => {
                format!("{venue} {symbol} {position_side}")
            }
            ReconcileDivergence::BalanceDrift { venue, asset, .. } => format!("{venue} {asset}"),
            ReconcileDivergence::JournalDivergence { detail, .. } => detail.clone(),
            ReconcileDivergence::OrderTermsDrift { venue, symbol, order_client_id, .. } => {
                format!("{venue} {symbol} order_client_id={order_client_id}")
            }
        }
    }
    pub fn kind(&self) -> ReconcileDivergenceKind {
        match self {
            ReconcileDivergence::MissingFill(_) => ReconcileDivergenceKind::MissingFill,
            ReconcileDivergence::MissingTerminal { .. } => ReconcileDivergenceKind::MissingTerminal,
            ReconcileDivergence::OrphanLocalOrder { .. } => {
                ReconcileDivergenceKind::OrphanLocalOrder
            }
            ReconcileDivergence::UnknownOrder(_) => ReconcileDivergenceKind::UnknownOrder,
            ReconcileDivergence::PositionDrift { .. } => ReconcileDivergenceKind::PositionDrift,
            ReconcileDivergence::PositionOnlyExternal(_) => {
                ReconcileDivergenceKind::PositionOnlyExternal
            }
            ReconcileDivergence::OrphanLocalPosition { .. } => {
                ReconcileDivergenceKind::OrphanLocalPosition
            }
            ReconcileDivergence::BalanceDrift { .. } => ReconcileDivergenceKind::BalanceDrift,
            ReconcileDivergence::JournalDivergence { .. } => {
                ReconcileDivergenceKind::JournalDivergence
            }
            ReconcileDivergence::OrderTermsDrift { .. } => ReconcileDivergenceKind::OrderTermsDrift,
        }
    }

    /// Per-INSTANCE origin: [`ReconcileDivergenceKind::origin`] refined by the evidence THIS divergence
    /// carries. One refinement exists today: a [`ReconcileDivergence::MissingFill`] whose
    /// `FillReport::order_client_id` is `None` reads [`ReconcileDivergenceOrigin::External`] — the order client id
    /// echo is the ONLY local linkage a fill report carries, so `None` means the fill names no
    /// local order (a foreign order's fill inside the lookback), the same predicate `resolve`'s
    /// `events_for` MissingFill arm keys its `external_order_client_id` synthesis on. An order-client-id-LINKED
    /// `MissingFill` keeps the kind's `ReconciliationMaterialized`: our own order's fill,
    /// materialized late.
    ///
    /// `MissingTerminal` needs no refinement BY CONSTRUCTION: `diff`'s order sweep raises it only
    /// from a report whose order client id matched a live local order (the unmatched side is `UnknownOrder`,
    /// already `External` per kind).
    ///
    /// Exhaustive (no `_` arm) for the same reason as [`ReconcileDivergenceKind::origin`]: a new variant
    /// fails to COMPILE until its instance-level evidence is classified. In production it is
    /// consumed only by [`crate::recon::mode_applies_divergence`]'s opt-in term
    /// ([`ReconcilePolicy::hold_external_instances`]); every other path classifies per kind.
    ///
    /// ⚠ This is the ORIGIN-BLIND answer: it never reads an instance tag, and the cross-instance
    /// hold does not go through it — [`crate::recon::mode_applies_divergence`] asks
    /// [`Self::is_foreign_instance`] FIRST and returns early. [`Self::origin_for`] is the
    /// instance-aware classification, and nothing in production calls it.
    pub fn origin(&self) -> ReconcileDivergenceOrigin {
        match self {
            ReconcileDivergence::MissingFill(f) => {
                if f.order_client_id.is_none() {
                    ReconcileDivergenceOrigin::External
                } else {
                    self.kind().origin()
                }
            }
            ReconcileDivergence::MissingTerminal { .. }
            | ReconcileDivergence::OrphanLocalOrder { .. }
            | ReconcileDivergence::UnknownOrder(_)
            | ReconcileDivergence::PositionDrift { .. }
            | ReconcileDivergence::PositionOnlyExternal(_)
            | ReconcileDivergence::OrphanLocalPosition { .. }
            | ReconcileDivergence::BalanceDrift { .. }
            | ReconcileDivergence::JournalDivergence { .. }
            | ReconcileDivergence::OrderTermsDrift { .. } => self.kind().origin(),
        }
    }

    /// The order client id this divergence's EVIDENCE carries, when it carries one. Exhaustive
    /// (no `_` arm): a new variant must state whether it names an order, because that answer is
    /// what [`Self::is_foreign_instance`] reads.
    ///
    /// `MissingFill`'s order client id echo is `Option`, and `None` is already the "foreign order's fill"
    /// signal [`Self::origin`] refines on. The position- and cash-shaped kinds name no order.
    /// `JournalDivergence` carries a recoverable venue order, but `resolve` decides its mode before
    /// any of this is consulted, so it deliberately answers `None` rather than invite a fold
    /// decision on it.
    pub fn order_client_id(&self) -> Option<&str> {
        match self {
            ReconcileDivergence::MissingFill(f) => f.order_client_id.as_deref(),
            ReconcileDivergence::MissingTerminal { order } => order.order_client_id.as_deref(),
            ReconcileDivergence::UnknownOrder(o) => o.order_client_id.as_deref(),
            ReconcileDivergence::OrphanLocalOrder { order_client_id }
            | ReconcileDivergence::OrderTermsDrift { order_client_id, .. } => {
                Some(order_client_id.as_str())
            }
            ReconcileDivergence::PositionDrift { .. }
            | ReconcileDivergence::PositionOnlyExternal(_)
            | ReconcileDivergence::OrphanLocalPosition { .. }
            | ReconcileDivergence::BalanceDrift { .. }
            | ReconcileDivergence::JournalDivergence { .. } => None,
        }
    }

    /// Does this divergence's evidence carry ANOTHER instance's origin claim?
    ///
    /// The cross-MACHINE half of the duplicate-instance interlock
    /// (`crates/vike-journal/src/lock.rs` is the same-machine half, and its module doc lists what
    /// it cannot reach). Two deployments sharing one venue API key each hold their own journal lock
    /// and each believe they are alone; if both declare an [`vike_model::OrderClientIdOrigin`], every id
    /// they mint says which one placed it, and this is where that claim is read.
    ///
    /// `false` unless the answer is positively yes — `crates/vike-model/src/orders/client_id/origin.rs`'s
    /// `order_client_id_is_foreign` says why the asymmetry is the safety property. An UNTAGGED id is never
    /// foreign: an instance that turns the feature on while a sibling has not is no worse off than
    /// before.
    pub fn is_foreign_instance(&self, local: Option<&vike_model::OrderClientIdOrigin>) -> bool {
        match self.order_client_id() {
            Some(order_client_id) => {
                vike_model::orders::client_id::origin::order_client_id_is_foreign(
                    order_client_id,
                    local,
                )
            }
            None => false,
        }
    }

    /// The operator-facing half of [`Self::is_foreign_instance`]: the sentence a held alert's
    /// detail carries when this divergence names another instance, and `None` when it does not.
    ///
    /// It names BOTH tags, because the useful question is "which of my deployments placed it" — an
    /// operator with three tagged instances needs that to know which box to look at. Neither tag is
    /// a secret: an origin already rides on the public wire inside every order client id. Where
    /// [`Self::describe`] already names THEIR tag (a tagged v2 id's words end `instance <tag>`), the
    /// note leaves `origin=` out, so the detail the two make names it once.
    #[must_use]
    pub fn foreign_instance_note(
        &self,
        local: Option<&vike_model::OrderClientIdOrigin>,
    ) -> Option<String> {
        let local = local?;
        let order_client_id = self.order_client_id()?;
        let theirs =
            vike_model::orders::client_id::origin::origin_of_order_client_id(order_client_id)?;
        if theirs == local.as_str() {
            return None;
        }
        let origin = match self.v2_words() {
            Some(w) if w.origin.is_some() => String::new(),
            _ => format!("origin={theirs}, "),
        };
        Some(format!(
            "placed by ANOTHER INSTANCE ({origin}this instance is {local}) — two \
             deployments are trading this venue account; nothing was folded into local books"
        ))
    }

    /// The words [`Self::describe`] appends: an `UnknownOrder` whose order client id is a v2 id, `None` for
    /// every other divergence and id.
    fn v2_words(&self) -> Option<OrderClientIdWords> {
        let ReconcileDivergence::UnknownOrder(o) = self else { return None };
        match parse_order_client_id(o.order_client_id.as_deref()?)? {
            p @ OrderClientIdReading::V2(_) => Some(describe_order_client_id(&p)),
            OrderClientIdReading::V1 { .. } => None,
        }
    }

    /// [`Self::origin`] refined by WHOSE instance the evidence names: an order or fill carrying
    /// another instance's origin claim reads [`ReconcileDivergenceOrigin::External`] whatever its kind
    /// says, because "another process is trading this account" is exactly what External means.
    /// It can only ever move a divergence TOWARD External, never away from it. `local: None`
    /// (nothing configured — the default) is definitionally [`Self::origin`].
    ///
    /// ⚠ It classifies; it does not decide. No production code calls it (the pin test does):
    /// [`crate::recon::mode_applies_divergence`] reaches the same hold through
    /// [`Self::is_foreign_instance`] directly, under every policy.
    pub fn origin_for(
        &self,
        local: Option<&vike_model::OrderClientIdOrigin>,
    ) -> ReconcileDivergenceOrigin {
        if self.is_foreign_instance(local) {
            return ReconcileDivergenceOrigin::External;
        }
        self.origin()
    }
}

/// What to do with a resolved divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReconcileMode {
    /// Emit synthesized events; fold immediately.
    Synthesize,
    /// Emit no events; surface a `ReconAlert` for operator confirm.
    Quarantine,
    /// Per-kind: local-origin kinds Synthesize, no-local-origin kinds Quarantine.
    Hybrid,
}

/// Policy = a default mode + per-kind overrides. `Hybrid` is expressed as the per-kind map preset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReconcilePolicy {
    pub default: ReconcileMode,
    pub per_kind: BTreeMap<ReconcileDivergenceKind, ReconcileMode>,
    /// The per-DIVERGENCE refinement opt-in. `true` ⇒ [`crate::recon::mode_applies_divergence`]
    /// HOLDS any divergence whose INSTANCE origin ([`ReconcileDivergence::origin`]) reads
    /// [`ReconcileDivergenceOrigin::External`] even where its KIND auto-applies — today exactly one
    /// sub-case: an order-client-id-less `MissingFill`. Set ONLY by [`Self::external_quarantine`]; every other
    /// constructor leaves it `false`, under which `mode_applies_divergence` is definitionally
    /// `mode_applies(policy, d.kind())`.
    ///
    /// Serde: `default` + skipped-when-false, so an old journaled `Command::ReconcileReports`
    /// payload replays unchanged AND every flag-false policy still serializes byte-identically to
    /// its pre-refinement form (the journal is externally-tagged serde_json — self-describing).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold_external_instances: bool,

    /// THIS instance's own origin claim — the identity every fold decision is made RELATIVE TO.
    /// `None` (the default, and every constructor's value) leaves [`ReconcileDivergence::origin_for`] equal
    /// to [`ReconcileDivergence::origin`] and [`crate::recon::mode_applies_divergence`] unchanged.
    ///
    /// ⚠ **It is not a policy KNOB, and no operator sets it.**
    /// `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_compute` stamps it, per pass, from
    /// the tag its OWN `OrderClientIdGenerator` is minting under — not from configuration. Two
    /// reasons, and the second is the load-bearing one:
    ///
    /// - The comparison is then against the identity ACTUALLY on the wire. A core whose resumed
    ///   `order_client_id_session` predates the operator turning the origin on still mints untagged ids, and
    ///   reconcile must judge by what it minted, not by what it was told.
    /// - It is DETERMINISTIC UNDER REPLAY without journaling anything new. The generator's session
    ///   is restored from the journal's own `order_client_id_session`, so a replay stamps the same tag and
    ///   reaches the same fold decision; reading configuration would make a replay's folds depend
    ///   on the box replaying them — the determinism fence `crates/vike-journal/src/lock.rs` exists
    ///   to protect.
    ///
    /// `pub` and settable so a pure `resolve` caller (the offline `run_pass`, a test) can supply
    /// the identity explicitly. Serde: `default` + skipped-when-absent, so an old journaled
    /// `Command::ReconcileReports` replays unchanged and an unconfigured instance's policy still
    /// serializes byte-identically to its pre-origin form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_instance_origin: Option<vike_model::OrderClientIdOrigin>,

    /// `true` ⇒ the reconcile pass ITSELF adopts a held `BalanceDrift` whose venue figure is ABOVE
    /// the expected local figure beyond tolerance (a deposit: the one cash move an operator makes on
    /// purpose), provided the pass leaves nothing else held for the account that could carry the
    /// same money (`ReconcileDivergenceKind::blocks_balance_adoption`). A DECREASE is held as ever. Set ONLY
    /// by [`Self::of`]'s `quarantine`, the default; `hybrid`, `synthesize`,
    /// `external-quarantine` and every hand-built policy leave it `false`. The rule itself is
    /// `crates/vike-core/src/runtime/reconcile.rs`'s `adopt_a_pure_balance_increase` (it needs the
    /// account's held store, which this pure crate cannot see); `mode_applies` is unchanged, so it
    /// still answers `BalanceDrift` is HELD under `quarantine`: this is a per-divergence refinement,
    /// like [`Self::hold_external_instances`].
    ///
    /// Serde: `default` + skipped-when-false, so an old journaled `Command::ReconcileReports`
    /// payload replays exactly as it ran (a pre-rule `quarantine` pass held the deposit and the
    /// replay holds it too) and every flag-false policy serializes byte-identically to its old form.
    #[serde(default, skip_serializing_if = "is_false")]
    pub adopt_balance_increase: bool,

    /// [`ReconcileDivergenceKind::OrderTermsDrift`]'s mode, kept BESIDE `per_kind` rather than in it, and the
    /// switch of the comparison itself. `Some(mode)`: the pass compares each matched order's terms
    /// (`vike_core`'s `reconcile_compute` runs `crate::recon::diff_order_terms`) and a drift resolves
    /// under `mode`. `None`: the pass compares no terms, which is every pass journaled before the
    /// comparison existed (so a replay folds what the pass folded) and every hand-built policy;
    /// [`Self::mode_for`] then answers `default` for the kind. Every named preset sets it
    /// ([`Self::of`]).
    ///
    /// ⚠ **Why not a `per_kind` row.** The map's KEYS are `ReconcileDivergenceKind`s, which a build from
    /// before this kind cannot parse: a journaled `Command::ReconcileReports` under `hybrid` would be
    /// `ParseFailed` there and the boot restore would restore nothing from the directory
    /// (`crates/vike-journal/CLAUDE.md`'s format rules), a journal format step. A key beside the map is
    /// one that build ignores, and since it never raises the kind, ignoring it changes nothing it
    /// folds. So do not put `OrderTermsDrift` into `per_kind`: [`Self::mode_for`] does not read it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_terms: Option<ReconcileMode>,
}

/// serde `skip_serializing_if` helper for [`ReconcilePolicy::hold_external_instances`]: a `false`
/// flag is omitted from the wire so pre-refinement policies keep their exact journaled bytes.
fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for ReconcilePolicy {
    fn default() -> Self {
        ReconcilePolicy {
            default: ReconcileMode::Synthesize,
            per_kind: BTreeMap::new(),
            hold_external_instances: false,
            local_instance_origin: None,
            adopt_balance_increase: false,
            order_terms: None,
        }
    }
}

impl ReconcilePolicy {
    /// The Hybrid preset: auto-apply local-origin kinds, quarantine no-local-origin kinds.
    ///
    /// **Both** orphan kinds sit in the QUARANTINE list. Their shared evidence is an ABSENCE from
    /// the venue report, which an incomplete / symbol-scoped / order-client-id-less fetch produces just as
    /// readily as a genuinely closed position or terminal order, and neither has anything safe to
    /// auto-apply — `resolve`'s module doc is the authority on both. This preset and the bare
    /// `ReconcileMode::Hybrid` fallback (`resolve::is_local_origin`, which lists neither) must agree;
    /// `resolve`'s `orphan_local_position_bare_hybrid_default_matches_the_preset` and
    /// `orphan_local_order_bare_hybrid_default_matches_the_preset` pin that.
    ///
    /// ⚠ EVERY kind is a NAMED row here — none may reach its mode through `default`. A
    /// fall-through is indistinguishable from a kind nobody classified, which is how
    /// [`ReconcileDivergenceKind::PositionDrift`] once auto-applied on ten venues with no row and no
    /// assertion. Its row is `Synthesize`; the reasoning is on the variant's own doc.
    /// `the_hybrid_preset_classifies_every_kind_explicitly`
    /// (`crates/vike-execution/tests/recon/reconcile_policy_pin.rs`) gates the "no fall-through" rule.
    ///
    /// ⚠ `PositionDrift` is ALSO where this preset and the bare-`ReconcileMode::Hybrid` fallback
    /// genuinely disagree: `resolve::is_local_origin` does not list it, so a hand-built
    /// `ReconcilePolicy { default: Hybrid, .. }` QUARANTINES what this preset folds. Nothing in the
    /// workspace builds a bare `Hybrid` (`vike_tradehub::reconcile_config::parse_policy` builds this
    /// preset for `"hybrid"`), so it is a latent trap rather than a live divergence — pinned as
    /// such by `bare_hybrid_and_the_preset_disagree_about_position_drift`.
    pub fn hybrid() -> Self {
        use ReconcileDivergenceKind::*;
        let mut per_kind = BTreeMap::new();
        // `JournalDivergence`'s mode is decorative — `resolve` short-circuits it into an
        // investigative alert before `mode_applies` is consulted — but it gets a row like every
        // other kind so the map is exhaustive by construction.
        for k in [MissingFill, MissingTerminal, PositionDrift, JournalDivergence] {
            per_kind.insert(k, ReconcileMode::Synthesize);
        }
        for k in [
            UnknownOrder,
            PositionOnlyExternal,
            OrphanLocalOrder,
            OrphanLocalPosition,
            BalanceDrift,
        ] {
            per_kind.insert(k, ReconcileMode::Quarantine);
        }
        ReconcilePolicy {
            default: ReconcileMode::Synthesize,
            per_kind,
            hold_external_instances: false,
            local_instance_origin: None,
            adopt_balance_increase: false,
            // This kind's row, beside the map (the field's doc says why). Held: no-local-origin.
            order_terms: Some(ReconcileMode::Quarantine),
        }
    }

    /// The `external-quarantine` preset (the `config.reconcile_policy` row's `external-quarantine`, parsed in
    /// `vike_tradehub::reconcile_config`'s `parse_policy`): [`Self::hybrid`] with every
    /// [`ReconcileDivergenceOrigin::External`] kind forced to [`ReconcileMode::Quarantine`] — the split-plane
    /// Pattern-A treatment of foreign venue activity (TAG and HOLD for an operator claim instead of
    /// blind auto-apply; spec §3b). COMPUTED from [`ReconcileDivergenceKind::origin`] over `hybrid()`'s rows
    /// rather than enumerated, so a future External kind is held here by construction. Because
    /// `hybrid()` names EVERY kind, so does this preset (same no-fall-through gate,
    /// `the_external_quarantine_preset_classifies_every_kind_explicitly`).
    ///
    /// At the KIND level: [`ReconcileDivergenceKind::PositionDrift`] — the ONE External kind `hybrid`
    /// auto-applies — stops folding and is HELD; the other External kinds were already quarantined
    /// under `hybrid`, and every non-External kind keeps its `hybrid` row verbatim
    /// (`external_quarantine_differs_from_hybrid_only_where_origin_is_external`,
    /// `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`, pins the delta).
    ///
    /// At the INSTANCE level it is the one constructor that sets [`Self::hold_external_instances`]
    /// (that field's doc): an order-client-id-less `MissingFill` (a foreign order's fill inside the lookback)
    /// is HELD while order-client-id-LINKED MissingFills (our own orders' fills) keep folding. The
    /// `MissingFill` mode ROW stays `Synthesize` — the refinement is per-divergence, not a mode
    /// edit, which keeps the journaled per-kind map stable — and the startup fold-set line renders
    /// the qualifier (`MissingFill (order-client-id-linked only)` —
    /// `vike_tradehub::reconcile_config::auto_applied_kinds`).
    ///
    /// The CLAIM path is the existing operator confirm: `resolve` computes events BEFORE consulting
    /// the mode, so a held External divergence's [`ReconAlert::proposed_events`] (for an order-client-id-less
    /// `MissingFill`, the synthesized `EXT-*` accept plus the fill) are exactly what `hybrid` would
    /// have auto-folded, and `Command::ConfirmReconcile` (`crates/vike-core/src/runtime/reconcile.rs`'s
    /// `confirm_reconcile`) folds them verbatim. A divergence that persists unconfirmed — a drift, or
    /// an order-client-id-less fill still inside the lookback — is re-raised un-keyed every pass (the generic
    /// held path sets no [`ReconAlert::dedup_key`]) and refreshes its held row by identity, exactly
    /// as under `quarantine`.
    pub fn external_quarantine() -> Self {
        let mut p = Self::hybrid();
        for (kind, mode) in p.per_kind.iter_mut() {
            if kind.origin() == ReconcileDivergenceOrigin::External {
                *mode = ReconcileMode::Quarantine;
            }
        }
        // The one row kept beside the map (`order_terms`), computed the same way.
        if ReconcileDivergenceKind::OrderTermsDrift.origin() == ReconcileDivergenceOrigin::External
        {
            p.order_terms = p.order_terms.map(|_| ReconcileMode::Quarantine);
        }
        // The per-DIVERGENCE half: the origin tag is per-kind, so an order-client-id-less MissingFill (whose
        // INSTANCE origin reads External while its kind's does not) still auto-applies above.
        // Opting in makes `mode_applies_divergence` hold exactly that sub-case; order-client-id-linked
        // MissingFills (our own orders' fills) keep folding.
        p.hold_external_instances = true;
        p
    }

    /// The kind's row, else `default`. `OrderTermsDrift`'s row is [`Self::order_terms`], never `per_kind`.
    pub fn mode_for(&self, kind: ReconcileDivergenceKind) -> ReconcileMode {
        let row = match kind {
            ReconcileDivergenceKind::OrderTermsDrift => self.order_terms,
            _ => self.per_kind.get(&kind).copied(),
        };
        row.unwrap_or(self.default)
    }

    /// The policy an operator NAMES, resolved to the value that name means — the one construction
    /// of the four presets, shared by everything that has to agree about them. It lives on the
    /// type rather than in a binary's settings parser because two consumers must not disagree
    /// about what `quarantine` builds: `vike_tradehub::reconcile_config::parse_policy` resolves
    /// the `config.reconcile_policy` row for a running mount, and `vike_docs` renders the per-policy
    /// fold-vs-hold verdict of every [`ReconcileDivergenceKind`] for the published capability data.
    /// The names and their words are [`vike_model::ReconcilePolicyName`]'s; this match is exhaustive,
    /// so a fifth name does not compile until it builds a policy.
    ///
    /// ⚠ `synthesize` is [`ReconcilePolicy::default`] with the terms comparison on
    /// ([`Self::order_terms`]), and NOT a bare `Hybrid`-defaulted value; the two are not
    /// interchangeable, for the reason [`ReconcilePolicy::hybrid`]'s doc gives about `PositionDrift`.
    #[must_use]
    pub fn of(name: ReconcilePolicyName) -> Self {
        match name {
            ReconcilePolicyName::Hybrid => Self::hybrid(),
            ReconcilePolicyName::Synthesize => {
                Self { order_terms: Some(ReconcileMode::Synthesize), ..Self::default() }
            }
            ReconcilePolicyName::Quarantine => Self {
                default: ReconcileMode::Quarantine,
                per_kind: BTreeMap::new(),
                hold_external_instances: false,
                local_instance_origin: None,
                // The ONE preset that opts in: a deposit needs no human (decision record 0179,
                // Option B, owner ruling 2026-10-31); `hybrid` and `external-quarantine` stay as ever.
                adopt_balance_increase: true,
                order_terms: Some(ReconcileMode::Quarantine),
            },
            ReconcilePolicyName::ExternalQuarantine => Self::external_quarantine(),
        }
    }
}

/// An operator-facing reconciliation alert (Quarantine mode, or informational).
#[derive(Debug, Clone, PartialEq)]
pub struct ReconAlert {
    pub kind: ReconcileDivergenceKind,
    pub detail: String,
    /// The events `resolve` WOULD have folded, held pending confirm.
    pub proposed_events: Vec<Event>,
    /// Venue orders to RE-REGISTER into local state on operator confirm (order-loss
    /// `JournalDivergence` recovery). Empty for every other alert. Applied via an insert-only
    /// registry seed (NOT the event fold — lifecycle events for an unknown order client id are dropped, so a
    /// re-register cannot ride `proposed_events`); see `Engine::reregister_orders`.
    pub recover_orders: Vec<OrderStatusReport>,
    /// Stable identity for a divergence that recurs by design across passes. Four shapes exist,
    /// all set by `resolve`: an `UnknownOrder`'s `venue_order_id` (`generate_missing_orders` on),
    /// `balance:{asset}`, `position:{symbol}:{side}` (`OrphanLocalPosition`) and the constant
    /// `orders:orphan-local` (`ORPHAN_LOCAL_ORDER_KEY`). `Some` makes the key the alert's whole
    /// identity, so the runtime's held-alert store REFRESHES the matching (venue, account, kind,
    /// key) row in place — keeping its confirm id — whatever the detail says. `None`: the runtime
    /// derives the identity from `identity_detail` (else the detail) plus the proposed fill legs
    /// (`vike_core`'s `ReconcileHeldId::new`).
    pub dedup_key: Option<String>,

    /// The CHURN-FREE discriminator for an UN-KEYED alert — `None` means "use [`Self::detail`]".
    ///
    /// ⚠ `vike_core`'s `ReconcileHeldId::new` builds an un-keyed alert's identity from it, so the identity
    /// must never carry a number the venue recomputes (`qty=`, `average_price=`) or a drifting position
    /// becomes a NEW alert every pass. Fill it from [`ReconcileDivergence::identity_detail`], whose doc
    /// carries the measured bug.
    pub identity_detail: Option<String>,

    /// Set ONLY on an `OrphanLocalPosition` alert (the LOCAL position the venue never reported) and
    /// on a `PositionDrift` (held under every policy) whose venue row is flat and unpriced, so an
    /// operator `Command::ConfirmReconcile` can drop it (`vike_core`'s `confirm_reconcile`). `None` on
    /// every other kind. It is data the confirm needs and the fold cannot derive from the event
    /// list: `proposed_events` of this kind stays EMPTY (module doc of `resolve`).
    pub orphan_position: Option<ReconcileOrphanPosition>,

    /// `true` ONLY on a `PositionDrift` / `PositionOnlyExternal` held because its venue row has no
    /// usable `average_price` and is NOT flat (`resolve`'s `unpriced_position_hold`): no price is safe to
    /// book at, so `proposed_events` is empty, `orphan_position` is `None` and an operator
    /// `Command::ConfirmReconcile` REFUSES (`vike_core`'s `confirm_reconcile`) instead of acknowledging.
    pub confirm_refused: bool,

    /// Set ONLY on an un-keyed `UnknownOrder` alert whose venue report carries an order client id: that id and
    /// the symbol the order rests on (`crate::RestoredOrder::of_report`). `vike_core` names an unheld cancel's
    /// row and sends the operator's `recon cancel` by it, never by reading `detail` back (decision 0301).
    /// `None` on every other alert (a venue that reports no order client id: IG, FXCM, Polymarket).
    pub reported_order: Option<crate::RestoredOrder>,
}

/// The local position an `OrphanLocalPosition` alert (or a held `PositionDrift` on a flat,
/// unpriced venue row: `resolve`'s `unpriced_position_hold`) names, as `diff` saw it: the key of the
/// `ReconcileLocalView::positions` row (`symbol`, `position_side`) and its signed net qty at the pass.
/// `confirm_reconcile` flattens it only while the engine STILL holds exactly `local_quantity` there, so a
/// strategy fill between the pass and the confirm is never swept away unseen.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileOrphanPosition {
    pub symbol: String,
    pub position_side: PositionSide,
    pub local_quantity: f64,
    /// What the confirm leaves: `0.0` drops the whole local position (an orphan, a flat venue row); a spot
    /// wallet holding less than the book (the spot rule) SHRINKS it to the wallet's figure, the rest closed
    /// at the position's own average price like a drop.
    pub venue_qty: f64,
}

/// The pure output of one reconcile pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcilePassOutcome {
    /// Synthesized events to fold (Synthesize / auto-Hybrid kinds).
    pub events: Vec<Event>,
    /// Write-only: `resolve` leaves it empty and vike-core's `apply_reconcile_balance` records the
    /// pass's venue balance in `balance`. Nothing applies it; the balance seed writes the
    /// `Account` directly.
    pub snapshot: ReconcileSnapshot,
    /// Held divergences (Quarantine / quarantined-Hybrid kinds).
    pub alerts: Vec<ReconAlert>,
}

/// The cash/balance slice of a [`ReconcileLocalView`], grouped so every `ReconcileLocalView` construction site adds
/// ONE field and so [`crate::recon::diff_balance`]'s realized-PnL confound correction has all its
/// inputs in one place. Populated from `Account` by `Engine::local_view`. Only the
/// first-class cash reconcile reads it; `diff` ignores it entirely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconcileLocalCash {
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

impl Default for ReconcileLocalCash {
    fn default() -> Self {
        ReconcileLocalCash {
            balance: 0.0,
            realized_pnl: 0.0,
            realized_at_sync: None,
            mode: crate::account::BalanceMode::Delta,
        }
    }
}

/// Money tolerance for the first-class cash reconcile diff ([`crate::recon::diff_balance`]). The
/// diff fires only when the realized-corrected drift exceeds `max(abs_floor, rel_frac·|venue_balance|)`.
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
pub struct ReconcileBalanceTolerance {
    pub abs_floor: f64,
    pub rel_frac: f64,
}

impl Default for ReconcileBalanceTolerance {
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
        ReconcileBalanceTolerance { abs_floor: 1.0, rel_frac: 1e-4 }
    }
}

/// One order a PREVIOUS session left behind, as the ownership file remembers it — the input of
/// `plan_restore`. It carries the order client id and the scope the order was placed under, and
/// deliberately NO mount id: this crate does not know mounts (the caller keeps the order client id → mount
/// map and joins on `order_client_id`).
///
/// `venue`, `symbol` and `account` are `Option` only because an OLD ownership record may lack
/// them. A ref without BOTH `venue` and `symbol` is never declared gone (its absence from a
/// venue report proves nothing about a venue it does not name); it can only be ADOPTED, when its
/// order client id shows up on the venue. `account: None` means "the venue's single/default account".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoredOrderRef {
    pub order_client_id: String,
    pub venue: Option<String>,
    pub symbol: Option<String>,
    pub account: Option<String>,
    /// The order's kind as the ownership file recorded it at the mint; `None` for a line written
    /// before the field. The planner reads it to tell a TRIGGER kind (stop, stop-limit,
    /// take-profit), which a client's open-order read may not list, from a plain one: an absent
    /// trigger order is declared gone only where the pass's client claims to list them
    /// (`ReconcileClient::lists_trigger_orders`).
    pub order_type: Option<vike_model::OrderType>,
    /// The venue order id the venue acknowledged for the order, when the ownership file learned it
    /// (`None` for an order whose acknowledgement was never folded, and for a line written before the
    /// field). A fill row that carries no order client id joins the ref by it (same symbol): binance, aster,
    /// cTrader, IG and Alpaca fill rows name their order only by this id, and so does the child
    /// order of a triggered stop on a venue that keeps stops apart.
    pub venue_order_id: Option<String>,
    /// How many QUIET passes the caller has counted for this ref: passes in which it was in scope and
    /// absent from the report, whose fill window opened after the first such pass
    /// (`crate::recon::RESTORE_QUIET_PASSES` of them make an absent ref gone). The planner is pure and
    /// keeps no memory; the caller counts and hands the count in. `0` for a ref never judged absent.
    pub quiet_passes: u32,
}

/// A point-in-time READ of local engine state handed to `diff`. Borrowed, never mutated.
pub struct ReconcileLocalView<'a> {
    pub venue: &'a str,
    /// order client id -> managed order (the engine registry).
    pub orders: &'a indexmap::IndexMap<String, OrderRecord>,
    /// venue trade_ids already folded (fill dedup source of truth).
    pub seen_trade_ids: &'a std::collections::HashSet<String>,
    /// (symbol, position_side) -> signed net qty.
    pub positions: &'a indexmap::IndexMap<(String, PositionSide), f64>,
    /// tolerance for PositionDrift (absolute qty).
    pub quantity_tolerance: f64,
    /// cash/balance slice for the first-class cash reconcile ([`crate::recon::diff_balance`]).
    /// `diff` ignores it — an existing `diff` fixture/test may leave it `ReconcileLocalCash::default()`.
    pub cash: ReconcileLocalCash,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_quarantines_only_no_local_origin_kinds() {
        let p = ReconcilePolicy::hybrid();
        assert_eq!(p.mode_for(ReconcileDivergenceKind::MissingFill), ReconcileMode::Synthesize);
        assert_eq!(p.mode_for(ReconcileDivergenceKind::UnknownOrder), ReconcileMode::Quarantine);
        assert_eq!(
            p.mode_for(ReconcileDivergenceKind::PositionOnlyExternal),
            ReconcileMode::Quarantine
        );
        // A surprise cash move is no-local-origin: hybrid quarantines it (never auto-fold).
        assert_eq!(p.mode_for(ReconcileDivergenceKind::BalanceDrift), ReconcileMode::Quarantine);
        // A local position the venue never reported is evidence-by-absence: hybrid quarantines it
        // too, so an incomplete position fetch can never become an auto-destructive flatten.
        assert_eq!(
            p.mode_for(ReconcileDivergenceKind::OrphanLocalPosition),
            ReconcileMode::Quarantine
        );
        // ...and its ORDER-side namesake agrees: quarantining it changes no event and buys the
        // operator alert; `resolve`'s module doc is the authority.
        assert_eq!(
            p.mode_for(ReconcileDivergenceKind::OrphanLocalOrder),
            ReconcileMode::Quarantine
        );
        // Classified auto-apply on purpose; see `PositionDrift`'s own doc.
        assert_eq!(p.mode_for(ReconcileDivergenceKind::PositionDrift), ReconcileMode::Synthesize);
        // Held, through the row kept beside the map (`ReconcilePolicy::order_terms`).
        assert_eq!(p.mode_for(ReconcileDivergenceKind::OrderTermsDrift), ReconcileMode::Quarantine);
        assert!(!p.per_kind.contains_key(&ReconcileDivergenceKind::OrderTermsDrift));
    }

    /// The row kept beside the map: every named preset compares terms and says its mode; a hand-built
    /// policy and a payload journaled before the key compare none. A policy serializes the key, never a
    /// `per_kind` entry an older build could not parse.
    #[test]
    fn order_terms_ride_beside_the_map_and_an_old_payload_compares_none() {
        for (name, mode) in [
            (ReconcilePolicyName::Hybrid, ReconcileMode::Quarantine),
            (ReconcilePolicyName::Synthesize, ReconcileMode::Synthesize),
            (ReconcilePolicyName::Quarantine, ReconcileMode::Quarantine),
            (ReconcilePolicyName::ExternalQuarantine, ReconcileMode::Quarantine),
        ] {
            let p = ReconcilePolicy::of(name);
            let name = name.word();
            assert_eq!(p.order_terms, Some(mode), "{name}");
            assert_eq!(p.mode_for(ReconcileDivergenceKind::OrderTermsDrift), mode, "{name}");
            let json = serde_json::to_string(&p).expect("serializes");
            assert!(!json.contains("\"OrderTermsDrift\""), "{name}: {json}");
            let back: ReconcilePolicy = serde_json::from_str(&json).expect("round-trips");
            assert_eq!(back.order_terms, Some(mode), "{name}");
        }
        assert_eq!(ReconcilePolicy::default().order_terms, None);
        let old: ReconcilePolicy =
            serde_json::from_str(r#"{"default":"Quarantine","per_kind":{}}"#).expect("parses");
        assert_eq!(old.order_terms, None, "a pass journaled before the key compares no terms");
    }

    #[test]
    fn default_policy_synthesizes_everything() {
        let p = ReconcilePolicy::default();
        assert_eq!(
            p.mode_for(ReconcileDivergenceKind::PositionOnlyExternal),
            ReconcileMode::Synthesize
        );
    }
}

#[path = "describe_tests.rs"]
#[cfg(test)]
mod describe_tests;
