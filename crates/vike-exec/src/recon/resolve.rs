//! Pure resolution: `Divergence`s × policy → `Recon` (events to fold + snapshot seed + alerts).
//! Missing-fill inference here (Task 4); zero-crossing position lifecycle in Task 5; pre-window
//! residual anchoring (partial-lookback lifecycle truncation — unexplained residual legs are older
//! than the lookback window, so they anchor before the earliest recovered fill and fold first) in
//! `position_events`. Synthetic orders use a reserved coid namespace (`external_coid`) so re-runs
//! map to the same order — the structural half of idempotency (the fold's trade_id dedup is the
//! other half).
//!
//! `resolve`'s `adopt` parameter (`Option<AdoptContext>`; `None` ⇔ `generate_missing_orders` off,
//! threaded in from `vike-core`'s `ReconConfig::generate_missing_orders` /
//! `VIKE_RECONCILE_GENERATE_MISSING` — this crate cannot name that config directly, down-only
//! layering) gates whether an `UnknownOrder` divergence (a venue order with no local match)
//! synthesizes adoption events. `None` reproduces the original behavior byte-for-byte —
//! `UnknownOrder` still falls into `events_for`'s catch-all (empty events), so under `hybrid` it
//! quarantines with nothing to one-click adopt. `Some` enables the NARROW adoption semantics of
//! [`adoption_case`]:
//!
//! - **Live (non-terminal) or unexecuted unknown orders synthesize NOTHING that folds.** A live
//!   order's executions arrive as `MissingFill` divergences with REAL venue trade-ids (they are
//!   inside the same lookback while the order is active) — the correct, already-deduped lane.
//!   Synthesizing a cumulative fill here as well would double-book the same activity under two
//!   different trade-ids in one pass.
//! - **A terminal unknown order whose executions are visible in THIS pass's fill reports**
//!   ([`AdoptContext::pass_fill_order_ids`]) likewise defers entirely to the fill lane.
//! - **Only a terminal unknown order with executed qty and NO fill report in-pass** (its fills
//!   fell outside the lookback, so the fill lane can never book them) synthesizes the adoption:
//!   an external-coid `OrderAccepted` (coid-less reports only, `MissingFill` parity) plus ONE
//!   `Fill` for the report's cumulative `filled_qty` at `avg_px`, deterministic
//!   `trade_id = EXT-ORD-{venue}-{venue_order_id}`. Terminal ⇒ `filled_qty` is final, so the
//!   cumulative print is safe.
//! - **Recurring passes are a true no-op.** Once the adoption fill has folded, its deterministic
//!   trade_id is in the engine's seen set ([`AdoptContext::seen_trade_ids`]) and the divergence
//!   resolves to nothing at all — no re-folded events, no anomaly-counter inflation
//!   (`dropped_unknown_coid` moves once per adoption, at first fold, exactly like `MissingFill`'s
//!   own synthesized accept), and no new alert rows (held UnknownOrder alerts carry a
//!   [`ReconAlert::dedup_key`] the runtime refreshes in place). Pinned by
//!   `already_adopted_unknown_order_is_a_true_no_op` here and the `recon_idempotency` /
//!   `recon_quarantine` integration tests.
//!
//! Accepted residual (documented, not silent): the adoption arm cannot see fills that folded with
//! their REAL trade-ids in an EARLIER pass and have since aged out of the lookback — a terminal
//! order re-entering the report window that way would re-book its qty once. This is NOT
//! self-healed by `PositionDrift`: every wired `ReconClient` today fetches OPEN orders only
//! (`fetch_order_status_reports` hits each venue's open-orders endpoint), so a terminal order
//! never reappears as a report at all — the residual is unreachable in practice, not caught by a
//! later check. A future `ReconClient` that fetches order HISTORY (closed/terminal orders too) on
//! a venue whose `fetch_position_status_reports` returns no rows (e.g. spot venues, which have no
//! position concept) would re-open this residual, since there would then be nothing to
//! re-converge qty against.
//!
//! ## `OrphanLocalPosition`: quarantined under `hybrid`, folds NOTHING under any policy
//!
//! `diff`'s step-5 local-side sweep raises `OrphanLocalPosition` when local holds a net position
//! this pass's venue report never mentions. `hybrid`'s rule is "auto-apply LOCAL-ORIGIN
//! divergences, quarantine no-local-origin ones", so the kind must be classified. It is
//! deliberately classified **no-local-origin** — quarantined — even though its name mirrors the
//! local-origin `OrphanLocalOrder`. Two independent reasons, either sufficient on its own:
//!
//! - **Nothing is safe to fold.** Flattening would mean synthesizing a closing `Fill`, and a
//!   `Fill` needs a PRICE. `LocalView::positions` carries the signed net qty and nothing else — no
//!   basis, no venue mark — and this divergence exists precisely because there is no venue report
//!   to read one from. Any price picked would be a guess that books fabricated realized PnL. So
//!   the kind resolves to ZERO events under EVERY policy; the only decision left is
//!   fold-vs-SURFACE.
//! - **The evidence is an absence, and absences lie.** Most wired `ReconClient`s scope the position
//!   fetch to the mounted symbol, so "no row" routinely means "not asked about", not "flat".
//!   Auto-applying would turn an incomplete fetch into a destructive flatten — the exact outcome
//!   the root `CLAUDE.md`'s *Reconciliation engine* rollout rule exists to prevent. (⚠ This
//!   bullet used to cite "`OrphanLocalOrder`'s auto-cancel" as the established precedent for that
//!   hazard. There is no such auto-cancel — that kind hits `events_for`'s catch-all and resolves
//!   to zero events under every policy, and nothing in this module constructs an `OrderCanceled`.
//!   The argument above
//!   never depended on it; see `crates/vike-exec/tests/recon/recon_policy_pin.rs`. The order-side kind
//!   now SURFACES under held policies — the section after this one — but it still folds nothing,
//!   so the precedent stays unavailable and the argument here is unchanged.)
//!
//! Per policy, therefore:
//!
//! - `synthesize` — folds its (empty) event list: a true no-op, nothing folds AND nothing alerts.
//!   The same shape [`AdoptionCase::SurfaceOnly`] takes under an auto-apply mode. A NAMED residual,
//!   not an oversight: an operator who wants this blind spot surfaced runs `hybrid` or
//!   `quarantine`.
//! - `hybrid` — quarantines. The per-kind preset in [`super::types::ReconPolicy::hybrid`] and the
//!   bare-`ReconMode::Hybrid` fallback in [`is_local_origin`] agree (both omit it from the
//!   local-origin set): one operator alert, nothing folds.
//! - `quarantine` — identical to `hybrid` for this kind: one operator alert, nothing folds.
//!
//! The alert carries `dedup_key = "position:{symbol}:{side}"`. Nothing about this divergence
//! self-heals — no events fold, so the venue keeps not reporting the row and every pass re-raises
//! it — which is exactly the unbounded-alert-row shape [`ReconAlert::dedup_key`] exists for
//! (fix-round-1 IMPORTANT-4): the runtime REFRESHES the one held row per (venue, symbol, side)
//! rather than appending a new one per pass.
//!
//! ## `OrphanLocalOrder`: ONE aggregated dedup-keyed alert under held policies; folds NOTHING
//!
//! `diff`'s step-3 local-side order sweep raises `OrphanLocalOrder` for every live LOCAL order whose
//! coid this pass's venue order report never mentions. Until this section existed the kind was
//! DETECTED and then resolved to nothing whatsoever: it fell into `events_for`'s catch-all (zero
//! events) while [`super::types::ReconPolicy::hybrid`] classified it auto-apply, so the empty list
//! was "folded" and no alert was raised either — under `hybrid`, `synthesize` AND (per-divergence,
//! un-keyed) the only visibility was `quarantine`'s. Detection with no outcome is worse than no
//! detection: it consumes a pass, counts as coverage everywhere the kind is enumerated, and tells
//! nobody. It now resolves the way its position twin above does, with one deliberate difference in
//! the alert's GRANULARITY.
//!
//! **Nothing folds under any policy, and nothing an operator confirm could fold either.** The only
//! action the divergence suggests is cancelling the local order, and `resolve` must never
//! synthesize one. Two independent reasons, either sufficient:
//!
//! - **A synthesized cancel would be a LIE about the venue.** Everything this module emits is a
//!   LOCAL fold — `crates/vike-core/src/runtime/mod.rs`'s `reconcile_reports` publishes the events
//!   into the engine and calls no venue. An `Event::OrderCanceled` here therefore terminalizes an
//!   order that may still be RESTING at the venue, and once it is out of the registry there is no
//!   managed order left to cancel: the position grows behind our back. (This is the same hazard
//!   `reconcile_reports` documents when it explains why it deliberately avoids
//!   `Command::ApplySnapshot`, whose reap arm would terminalize every live order on the venue.)
//! - **The evidence is an absence, and this absence lies more than most.** A coid missing from an
//!   order report is equally consistent with a genuine terminal we missed, an order still IN FLIGHT
//!   to the venue (submitted, not yet in its open-order snapshot), a venue whose report does not
//!   echo `client_order_id` at all, an orders-only-empty fetch, and a broker-prefix mismatch that
//!   orphans EVERY live order at once — `crates/bridges/binance/src/family/recon.rs`'s
//!   `parse_order_row` strips that prefix precisely because the un-stripped form does exactly that.
//!
//! So `proposed_events` stays EMPTY and a `ConfirmRecon` on this alert is a pure ACKNOWLEDGEMENT:
//! `crates/vike-core/src/runtime/mod.rs`'s `confirm_recon` folds `proposed_events` and re-registers
//! `recover_orders`, and both are empty by construction, so the confirm cannot cancel, re-register
//! or fold anything. `no_policy_ever_synthesizes_a_cancel_for_an_orphan_local_order`
//! (`crates/vike-exec/tests/recon/recon_policy_pin.rs`) pins that half and is unchanged by this section.
//!
//! Per policy, therefore:
//!
//! - `synthesize` — folds its (empty) event list: a true no-op, nothing folds AND nothing alerts.
//!   The same NAMED residual `OrphanLocalPosition` carries, for the same reason — an operator who
//!   chose "fold everything, no operator in front of it" opted out of alerts.
//! - `hybrid` — quarantines, hence alerts. Getting there RECLASSIFIED the kind as no-local-origin
//!   in both places that classify ([`super::types::ReconPolicy::hybrid`]'s preset and
//!   [`is_local_origin`], which must stay in agreement). That reclassification changes no event —
//!   the kind folded nothing before and folds nothing now — and rests on the same distinction its
//!   position twin's does: what is local-origin here is the ORDER, while the DIVERGENCE is the
//!   venue's SILENCE about it, and silence is not our own activity confirmed by the venue.
//! - `quarantine` — identical to `hybrid`: one operator alert, nothing folds. It also stops
//!   APPENDING one un-keyed alert per orphan per pass, which is what it did before.
//!
//! ### Why ONE aggregated alert and not one per coid
//!
//! The obvious mirror of `OrphanLocalPosition`'s `(symbol, side)` key is the coid, and it is the
//! wrong key. Two facts about the consumer decide it: `crates/vike-core/src/runtime/mod.rs`'s
//! `confirm_recon` is the ONLY thing that removes a held alert row (a divergence that heals on its
//! own leaves its row behind forever), and `crates/vike-core/src/runtime/publish.rs`'s
//! `recon_block` CLONES every held row into every published `CoreSnapshot`. So the KEY SPACE is the
//! thing to size. A position key space is (symbols × sides) — tiny — and that divergence genuinely
//! never heals, which makes a permanent row per key honest. A coid key space is one entry per order
//! ever placed, and this divergence heals ROUTINELY (every in-flight case above resolves by the
//! next pass), so a per-coid key would accrue a stale held row per order on a churning account and
//! grow the per-publish clone with it.
//!
//! The pass therefore emits ONE alert carrying the COUNT plus a bounded, SORTED sample of coids
//! (`ORPHAN_SAMPLE`), dedup-keyed on the constant [`ORPHAN_LOCAL_ORDER_KEY`] so the runtime holds
//! exactly one row per (venue, kind) and refreshes it in place. Sorting is load-bearing for that
//! refresh: a STEADY orphan set renders a byte-identical detail, which the runtime's change check
//! turns into a pure no-op (no ring note, no row churn), so the detail moves only when the orphan
//! set does. Ordering note: because it is aggregated, the alert is appended AFTER every
//! per-divergence alert of the pass rather than in divergence order.
//!
//! ⚠ Two residuals, named rather than hidden. **A healed orphan leaves a stale row** — the alert
//! API has no retraction, so the row keeps saying N orders were unreported after they are reported
//! again, until an operator acknowledges it. Bounded at one row per venue, with a side-effect-free
//! confirm, which is what makes it acceptable rather than merely tolerated. **The alert names a
//! COUNT and a sample, not every coid** — an operator who needs the full list reads the venue's
//! open orders against the local registry; this alert's job is to say THAT the two views disagree,
//! and by how much, which is exactly what silence never said.

use std::collections::{HashMap, HashSet};

use vike_model::events::{Event, FillEvent, LiquiditySide, OrderAccepted, PositionSide};
use vike_model::{OrderStatusReport, PositionStatusReport};

use super::types::{
    Divergence, DivergenceKind, DivergenceOrigin, Recon, ReconAlert, ReconMode, ReconPolicy,
};

pub type PitFn<'a> = &'a dyn Fn(&str, &str, i64) -> Option<vike_model::SymbolProperties>;

/// Reserved, collision-proof coid for a venue-observed order we never sent. Deterministic in the
/// venue_order_id so a second reconcile pass maps to the same synthetic order (idempotency).
pub fn external_coid(venue: &str, venue_order_id: &str) -> String {
    format!("EXT-{venue}-{venue_order_id}")
}

/// The [`ReconAlert::dedup_key`] of the ONE aggregated `OrphanLocalOrder` alert a pass raises — a
/// CONSTANT, not a per-order key, so the runtime holds exactly one row per (venue, kind) and
/// refreshes it in place. The module doc argues why the coid is the wrong key here even though
/// `OrphanLocalPosition` keys per (symbol, side). `pub` because the pin test asserts it by symbol
/// rather than re-spelling the literal.
pub const ORPHAN_LOCAL_ORDER_KEY: &str = "orders:orphan-local";

/// How many orphaned coids the aggregated alert's detail names before it truncates to `(+N more)`.
/// A cap, not a limit on detection: the COUNT is always exact. It exists because the detail string
/// is cloned into every published `CoreSnapshot` (`crates/vike-core/src/runtime/publish.rs`'s
/// `recon_block`), and a market maker's whole resting book can orphan at once when a venue's report
/// does not echo our client ids.
const ORPHAN_SAMPLE: usize = 8;

/// Per-pass context for `generate_missing_orders` UnknownOrder adoption (see the module doc).
/// `Some` ⇔ the flag is on; both call sites (`run_pass`, `vike-core`'s `reconcile_reports`)
/// already hold the two sets, so `resolve` stays pure — no I/O, no engine reach-through.
#[derive(Clone, Copy)]
pub struct AdoptContext<'a> {
    /// `venue_order_id` of EVERY fill report fetched this pass (whether or not the fill diffed to
    /// a divergence — an already-seen fill still proves the fill lane covers this order).
    pub pass_fill_order_ids: &'a HashSet<String>,
    /// The engine's folded trade-ids (the same set `diff`'s MissingFill check consults) — detects
    /// an adoption fill (`EXT-ORD-*`) that already folded in a prior pass.
    pub seen_trade_ids: &'a HashSet<String>,
}

/// The deterministic trade_id of an UnknownOrder adoption fill — keyed on `venue_order_id` alone
/// (never `filled_qty`/`ts`), so every pass over the same venue order maps to the same id: the
/// engine's `seen_trade_ids` guard is the fold-side dedup, [`AdoptContext::seen_trade_ids`] the
/// resolve-side skip.
fn adoption_trade_id(o: &OrderStatusReport) -> vike_model::events::TradeId {
    // `prefixed` rather than `TradeId::new(..).unwrap()`: the `EXT-ORD-` prefix is a static literal,
    // so the result is non-empty BY CONSTRUCTION even for a report whose venue/order id are both
    // blank — there is no error arm to invent a policy for. Byte-identical to the old `format!`.
    vike_model::events::TradeId::prefixed(
        "EXT-ORD-",
        format_args!("{}-{}", o.venue, o.venue_order_id),
    )
}

/// How one `UnknownOrder` divergence resolves under an active [`AdoptContext`] (module doc has
/// the full rationale per arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdoptionCase {
    /// Terminal, executed, invisible to this pass's fill lane, not yet adopted → synthesize
    /// accept + cumulative fill (the one case the adoption arm exists for).
    Adopt,
    /// Everything else that still deserves operator visibility (live order; terminal covered by
    /// the fill lane; nothing executed) → nothing folds, held modes surface a display alert.
    SurfaceOnly,
    /// The adoption fill already folded in a prior pass → the divergence is economically resolved;
    /// emit nothing at all (the recurring-pass no-op).
    AlreadyAdopted,
}

fn adoption_case(o: &OrderStatusReport, ctx: &AdoptContext) -> AdoptionCase {
    // Same status vocabulary as `diff`'s MissingTerminal check; an unparseable venue status is
    // conservatively NOT terminal (never synthesize a cumulative fill on a guess).
    let terminal =
        crate::order::OrderStatus::parse(&o.status).map(|s| s.is_terminal()).unwrap_or(false);
    if !terminal
        || o.filled_qty == 0.0
        || ctx.pass_fill_order_ids.contains(o.venue_order_id.as_str())
    {
        return AdoptionCase::SurfaceOnly;
    }
    if ctx.seen_trade_ids.contains(adoption_trade_id(o).as_str()) {
        return AdoptionCase::AlreadyAdopted;
    }
    AdoptionCase::Adopt
}

/// Does `policy` auto-apply this kind's events this pass (vs holding them in an alert)? The ONE
/// per-KIND definition both the phase-2 fold decision and the phase-1 `fill_window` accumulation
/// share, so the window can never count qty that did not actually fold (mode consistency). Phase 2
/// consults it through [`mode_applies_divergence`] — the per-DIVERGENCE refinement, which for
/// every policy that does not opt in (`ReconPolicy::hold_external_instances` false) is
/// definitionally this function.
///
/// **`pub` deliberately.** While this was private, "what does `hybrid` actually fold?" could only
/// be answered by reading `resolve` — and two independent reviews of the same code reached
/// opposite answers about `OrphanLocalOrder`, one of which had already propagated into six
/// documents and a rollout policy. Callers that need to STATE what a policy will do (operator
/// warnings, docs, tests) must consult this rather than re-deriving it from `mode_for`, which is
/// only half the rule: a bare `ReconMode::Hybrid` still has to go through [`is_local_origin`].
///
/// ⚠ Auto-apply is NOT the same as "does something". `MissingTerminal` auto-applies under `hybrid`
/// and resolves to an EMPTY event list — see `events_for`'s catch-all and
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`. (`OrphanLocalOrder` used to be the example here
/// and is no longer: it is now classified no-local-origin so that held policies can surface it —
/// it still folds nothing, but auto-apply is no longer its mode under `hybrid`.)
pub fn mode_applies(policy: &ReconPolicy, kind: DivergenceKind) -> bool {
    match policy.mode_for(kind) {
        ReconMode::Synthesize => true,
        ReconMode::Quarantine => false,
        // Hybrid is expressed via per_kind in ReconPolicy::hybrid(); a bare Hybrid default
        // means "apply local-origin, hold the rest".
        ReconMode::Hybrid => is_local_origin(kind),
    }
}

/// The per-DIVERGENCE refinement of [`mode_applies`]: the fold-vs-hold answer for one CONCRETE
/// divergence rather than its kind — the seam #1380's named residual asked for ("holding that
/// sub-case would need per-divergence mode_applies").
///
/// For every policy that does not opt in (`ReconPolicy::hold_external_instances` `false` — the
/// three flat policies, the `hybrid()` preset, and any hand-built bare `ReconMode::Hybrid`) this
/// is DEFINITIONALLY `mode_applies(policy, d.kind())`: the refinement term short-circuits to
/// no-op, so their byte-identity is by construction, not by test alone. With the opt-in
/// ([`ReconPolicy::external_quarantine`] is the only constructor that sets it), an instance whose
/// OWN evidence reads [`DivergenceOrigin::External`] ([`Divergence::origin`]) is HELD even where
/// its kind auto-applies — today exactly one sub-case: the coid-less `MissingFill` (a foreign
/// order's fill inside the lookback; a coid-LINKED `MissingFill` is our own order's fill and
/// keeps folding). The claim path needs no new machinery: the generic held arm computes `events`
/// BEFORE consulting the mode, so the held alert proposes exactly what `hybrid` would have
/// auto-folded and `confirm_recon` folds it verbatim — pinned by
/// `external_quarantine_holds_a_coidless_missing_fill_and_a_claim_folds_exactly_hybrids_events`
/// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`).
///
/// [`mode_applies`] stays the per-KIND authority callers use to STATE a policy's fold set;
/// `vike_tradehub::reconcile_config`'s `auto_applied_kinds` still computes from it and renders the
/// per-divergence qualifier by ASKING this function with a coid-less probe.
///
/// ⚠ Phase-1 `fill_window` accumulation deliberately does NOT consult this — a held coid-less
/// `MissingFill` still counts, exactly as every held `MissingFill` always has under plain
/// `quarantine`. That is what keeps confirm-everything equal to `hybrid`: every CONSTRUCTIBLE
/// policy that holds a fill also holds `PositionDrift` (`external_quarantine` computes that row
/// from the same origin tag), so the held drift's proposed legs net the held fill's qty and
/// held proposals stay mutually consistent. A hand-built policy setting
/// `hold_external_instances` while leaving `PositionDrift` auto-applied would re-open the
/// IMPORTANT-3 netting hazard — a latent trap of the same family as the bare-Hybrid
/// `PositionDrift` disagreement, unreachable from `VIKE_RECONCILE_POLICY`.
///
/// ⚠ **The instance refinement below is the ONE exception to that paragraph, and it has to be.**
/// It holds under `hybrid`, which still auto-applies `PositionDrift` — so the pairing the rule
/// above rests on ("every constructible policy that holds a fill also holds the drift") does not
/// hold for it, and counting a sibling's qty would net it out of a correction that WILL fold. A
/// foreign-instance divergence is therefore SKIPPED by the phase-1 accumulator outright, not
/// merely held; the loop states the same argument where it performs it.
///
/// ⚠ **One refinement is NOT behind the opt-in, and cannot be**: a divergence whose evidence
/// carries ANOTHER INSTANCE'S origin claim ([`Divergence::is_foreign_instance`]) is held under
/// EVERY policy. That is the cross-machine half of the duplicate-instance interlock
/// (`crates/vike-journal/src/lock.rs` holds the same-machine half and cannot reach past one
/// filesystem): two deployments sharing a venue API key each take their own journal lock and each
/// believe they are alone, and under `hybrid` a coid-LINKED `MissingFill` folds — so a sibling's
/// fill would be booked into THIS instance's position and realized PnL, silently, at the sibling's
/// price. Gating that behind `external-quarantine` would have left the default policy holding the
/// bag.
///
/// It stays byte-identical for everyone who has not configured an origin, BY CONSTRUCTION rather
/// than by test alone: [`ReconPolicy::local_instance_origin`] is `None` in every constructor, and
/// `coid_is_foreign` (`crates/vike-model/src/instance_origin.rs`) answers `false` for a `None`
/// local origin AND for any id carrying no claim — so no id that exists today can take this
/// branch. The direction is one-way too: it can only turn a fold into a HOLD, never the reverse,
/// so the worst a mis-read does is put a divergence in front of an operator.
pub fn mode_applies_divergence(policy: &ReconPolicy, d: &Divergence) -> bool {
    if d.is_foreign_instance(policy.local_instance_origin.as_ref()) {
        return false;
    }
    mode_applies(policy, d.kind())
        && !(policy.hold_external_instances && d.origin() == DivergenceOrigin::External)
}

/// Per-(venue, symbol) summary of THIS pass's MissingFill synthesis: the net signed qty those
/// fills contribute, plus the earliest fill ts (the observable head of the venue's lookback
/// window — everything the window could NOT see is older than it). Keyed by the raw venue/symbol
/// strings (not yet PIT-snapped — netting happens before PIT rounding, same as the pre-fix
/// `local_qty` input to `synth_position_legs`).
type FillWindowMap = HashMap<(String, String), (f64, i64)>;

pub fn resolve(
    divergences: Vec<Divergence>,
    policy: &ReconPolicy,
    pit: Option<PitFn>,
    adopt: Option<AdoptContext>,
) -> Recon {
    // Phase 1: accumulate the net position movement (and window head) already covered by same-pass
    // MissingFill synthesis, BEFORE resolving anything. `events_for`'s MissingFill arm always
    // synthesizes its Fill with `position_side: Both` (see below), so this can only be netted
    // cleanly against a position report that is ALSO one-way (`Both`) — see `position_events`.
    // An adopted `UnknownOrder`'s cumulative fill feeds the SAME window — but ONLY when it will
    // actually fold THIS pass (`AdoptionCase::Adopt` AND `mode_applies`): a held (quarantined /
    // hybrid-quarantined) adoption folds nothing now, so counting its qty here would net a
    // co-occurring PositionDrift heal against events that never happened, silently suppressing
    // the drift's self-heal (mode consistency — the fix-round-1 IMPORTANT-3 finding).
    let unknown_applies = mode_applies(policy, DivergenceKind::UnknownOrder);
    let local_origin = policy.local_instance_origin.as_ref();
    let mut fill_window: FillWindowMap = HashMap::new();
    for d in &divergences {
        // ⚠ A divergence naming ANOTHER INSTANCE'S order is excluded from the window entirely —
        // not merely held. The window exists so that a fill this pass FOLDS is not counted twice
        // (once as the fill, once inside the venue's net position), and the "held fills still
        // count" rule below rests on the fact that every constructible policy holding a fill also
        // holds `PositionDrift`. A foreign-instance fill breaks that pairing: it is held under
        // EVERY policy (`mode_applies_divergence`), including `hybrid`, which still auto-applies
        // `PositionDrift`. Counting it would net a sibling's qty out of a drift correction that
        // WILL fold — silently suppressing the re-convergence — which is the IMPORTANT-3 netting
        // hazard that function's doc names. Excluding it keeps the two sides consistent: the
        // sibling's fill is not ours to book, and the drift is judged on the raw venue-vs-local
        // difference exactly as it was before an origin was configured.
        if d.is_foreign_instance(local_origin) {
            continue;
        }
        match d {
            Divergence::MissingFill(f) => {
                // Deliberately UNCONDITIONAL — a HELD MissingFill (plain `quarantine`, or the
                // coid-less sub-case `mode_applies_divergence` holds under `external-quarantine`)
                // still counts: every constructible policy that holds a fill also holds
                // `PositionDrift`, so the held drift's proposed legs must net the held fill's qty
                // for confirm-everything to reproduce `hybrid` exactly (see
                // `mode_applies_divergence`'s doc for the hand-built-policy trap this leaves).
                let e =
                    fill_window.entry((f.venue.clone(), f.symbol.clone())).or_insert((0.0, f.ts));
                e.0 += f.side as f64 * f.last_qty;
                e.1 = e.1.min(f.ts);
            }
            Divergence::UnknownOrder(o) => {
                if let Some(ctx) = &adopt
                    && unknown_applies
                    && adoption_case(o, ctx) == AdoptionCase::Adopt
                {
                    let e = fill_window
                        .entry((o.venue.clone(), o.symbol.clone()))
                        .or_insert((0.0, o.ts));
                    e.0 += o.side as f64 * o.filled_qty;
                    e.1 = e.1.min(o.ts);
                }
            }
            _ => {}
        }
    }

    // Phase 2: resolve every divergence, netting fill_window into position-drift/external legs so
    // the SAME missed fill isn't counted twice in one pass (once via MissingFill, once via the
    // venue's positionRisk net that already reflects it). Legs flagged `pre` by `position_events`
    // (pre-window residuals, chronologically older than every recovered fill) collect separately
    // and are spliced in FRONT of everything else at the end, so the fold books them first.
    let mut recon = Recon::default();
    let mut pre_events: Vec<Event> = Vec::new();
    // Every held `OrphanLocalOrder` of this pass, aggregated into ONE alert after the loop (see the
    // module doc: the coid is the wrong dedup key, because the held-alert store never self-clears
    // and this divergence heals routinely).
    let mut orphan_orders: Vec<String> = Vec::new();
    for d in divergences {
        // A JournalDivergence (three-way persistence/restore bug — the journal/materialized log has
        // something the live Account lost) is NEVER auto-applied: it ALWAYS surfaces as an
        // investigative `ReconAlert` regardless of policy ("no auto-apply of a persistence bug",
        // #349 review). The FILL-loss case carries nothing to fold (the fill already exists at the
        // venue). The ORDER-loss case (`recover_order` is `Some`) carries the venue order to
        // RE-REGISTER into local state — held in `recover_orders` for an operator `ConfirmRecon` to
        // apply via the insert-only registry seed (NOT `proposed_events`, which the event fold would
        // drop for an unknown coid). Either way it is HELD, never auto-folded.
        if let Divergence::JournalDivergence { detail, recover_order } = &d {
            recon.alerts.push(ReconAlert {
                kind: DivergenceKind::JournalDivergence,
                detail: detail.clone(),
                proposed_events: Vec::new(),
                recover_orders: recover_order.iter().map(|o| (**o).clone()).collect(),
                dedup_key: None,
                // UN-KEYED, and the detail is built elsewhere — its churn is not ours to judge here.
                // `None` keeps the identity exactly what it was before this field existed.
                identity_detail: None,
            });
            continue;
        }
        // BalanceDrift (Feature 2) synthesizes ONE correcting `Event::AccountState` — the identical
        // fold path a live venue AccountState takes (`apply_account_state`), so a Synthesize-mode
        // resolution is equivalent to today's silent authoritative seed, only now routed through the
        // policy machinery. Under a held policy it does NOT fold: the AccountState rides the alert's
        // `proposed_events` for `confirm_recon` to apply on operator approval, so a SURPRISE cash
        // move (withdrawal/liquidation/deposit) is never auto-absorbed. A `balance:{asset}`
        // dedup_key refreshes one alert row per (venue, asset) instead of appending every pass.
        if let Divergence::BalanceDrift { venue, asset, local: expected, venue_bal, ts } = &d {
            let account_state = Event::AccountState(vike_model::events::AccountState {
                venue: ustr::ustr(venue),
                balances: vec![(asset.clone(), *venue_bal)],
                ts: *ts,
                // The recon lane never routes through `vike_core`'s `route_event`:
                // `CoreThread::reconcile_reports` resolves the engine from
                // `ReconcileReports::route` and `confirm_recon` from the held alert's own stored
                // key, then publishes DIRECTLY to that index. So this synthesized correction needs
                // no key on the payload — stamping one here would be a second, weaker copy of a
                // routing decision that has already been made exactly.
                route_key: None,
            });
            if mode_applies(policy, DivergenceKind::BalanceDrift) {
                recon.events.push(account_state);
            } else {
                recon.alerts.push(ReconAlert {
                    kind: DivergenceKind::BalanceDrift,
                    detail: format!(
                        "balance drift {venue} {asset}: venue {venue_bal} vs expected {expected} \
                         ({:+} unexplained)",
                        venue_bal - expected
                    ),
                    proposed_events: vec![account_state],
                    recover_orders: Vec::new(),
                    dedup_key: Some(format!("balance:{asset}")),
                    identity_detail: None,
                });
            }
            continue;
        }
        // OrphanLocalPosition (diff's local-side position sweep) is resolved wholly here, never the
        // generic machinery: it can never fold ANYTHING (there is no price to close at — see this
        // module's doc), so `mode_applies` only chooses no-op-vs-surface, and its alert needs a real
        // detail plus a dedup_key (nothing heals it, so an un-keyed alert would append a row every
        // pass). Auto-apply modes fold its empty event list — a true no-op, the same shape
        // `AdoptionCase::SurfaceOnly` takes; held modes (quarantine, and hybrid, which classifies
        // this kind no-local-origin) surface one operator alert per (venue, symbol, side).
        if let Divergence::OrphanLocalPosition { venue, symbol, position_side, local_qty } = &d {
            if !mode_applies(policy, DivergenceKind::OrphanLocalPosition) {
                recon.alerts.push(ReconAlert {
                    kind: DivergenceKind::OrphanLocalPosition,
                    detail: format!(
                        "local {venue} {symbol} {position_side} position {local_qty} has NO venue \
                         position row this pass — the venue never reported it (not a drift: there \
                         is no venue qty to compare against). Nothing is auto-applied: verify at \
                         the venue whether the position is genuinely flat or simply unreported."
                    ),
                    // Deliberately empty: an operator confirm must not flatten a position at a
                    // guessed price either. This alert is investigate-only, like the fill-loss
                    // JournalDivergence above.
                    proposed_events: Vec::new(),
                    recover_orders: Vec::new(),
                    dedup_key: Some(format!("position:{symbol}:{position_side}")),
                    identity_detail: None,
                });
            }
            continue;
        }
        // OrphanLocalOrder (diff's local-side ORDER sweep) is COLLECTED here and resolved after the
        // loop, never one-by-one: it can never fold anything (a synthesized cancel would terminalize
        // an order that may still be resting at the venue — see this module's doc), so `mode_applies`
        // only chooses no-op-vs-surface, and the surfaced form must be ONE dedup-keyed row per venue
        // rather than one per coid. Auto-apply modes (`synthesize`) fold its empty event list — a
        // true no-op, the same shape `OrphanLocalPosition` takes there; held modes (`quarantine`,
        // and `hybrid`, which now classifies this kind no-local-origin) contribute to the aggregate.
        if let Divergence::OrphanLocalOrder { client_order_id } = &d {
            if !mode_applies(policy, DivergenceKind::OrphanLocalOrder) {
                orphan_orders.push(client_order_id.clone());
            }
            continue;
        }
        // UnknownOrder under an active AdoptContext is resolved wholly here (never the generic
        // fold/hold machinery below): its three cases need distinct fold-vs-display shapes and a
        // dedup-keyed alert. With `adopt` None it falls through to the generic path, where
        // `events_for`'s catch-all keeps the original empty-events behavior byte-for-byte.
        if let (Divergence::UnknownOrder(o), Some(ctx)) = (&d, &adopt) {
            match adoption_case(o, ctx) {
                AdoptionCase::AlreadyAdopted => {} // resolved in a prior pass: no events, no alert
                AdoptionCase::Adopt => {
                    let events = adoption_events(o, pit);
                    if unknown_applies {
                        recon.events.extend(events);
                    } else {
                        recon.alerts.push(unknown_order_alert(o, events));
                    }
                }
                AdoptionCase::SurfaceOnly => {
                    // Nothing safe to fold (the fill lane owns any executions). Synthesize mode
                    // never alerts; held modes surface a display/adoptable alert whose proposed
                    // events carry at most the decorative accept — never a synthesized fill that
                    // could double-book against the fill lane at confirm time.
                    if !unknown_applies {
                        recon.alerts.push(unknown_order_alert(o, adoption_accept(o).collect()));
                    }
                }
            }
            continue;
        }
        let (events, pre) = events_for(&d, pit, &fill_window);
        if mode_applies_divergence(policy, &d) {
            if pre {
                pre_events.extend(events);
            } else {
                recon.events.extend(events);
            }
        } else {
            // A held (quarantined) divergence keeps its events whole in the alert; the operator
            // applies them at confirm time, after everything auto-folded — same as before.
            //
            // This is also the CLAIM path for an EXTERNAL-origin divergence held by
            // `ReconPolicy::external_quarantine` (split-plane Pattern A, the Nautilus
            // `external_order_claims` analogue): `events` was computed BEFORE the mode was
            // consulted, so the alert proposes exactly what `hybrid` would have auto-folded, and
            // a `Command::ConfirmRecon` (`crates/vike-core/src/runtime/mod.rs`'s `confirm_recon`)
            // folds `proposed_events` verbatim — claiming an EXTERNAL divergence yields
            // byte-identical events to `hybrid`'s blind fold, with an operator in front of it.
            // The same contract covers the PER-DIVERGENCE hold (`mode_applies_divergence`): a
            // coid-less MissingFill held under `external-quarantine` proposes exactly the
            // synthesized EXT-* accept + fill `hybrid` would have folded. Pinned in
            // `crates/vike-exec/tests/recon/recon_policy_pin.rs` by
            // `external_quarantine_holds_position_drift_and_a_claim_folds_exactly_hybrids_events`
            // and its coid-less sibling
            // `external_quarantine_holds_a_coidless_missing_fill_and_a_claim_folds_exactly_hybrids_events`.
            recon.alerts.push(ReconAlert {
                kind: d.kind(),
                // A foreign-instance divergence says so IN THE DETAIL, because "UnknownOrder" and
                // "another deployment of yours is on this account" call for completely different
                // operator actions and the row is the only place an operator meets either. The
                // `identity_detail` beside it is deliberately NOT annotated: it is the alert's
                // dedup IDENTITY (`vike_core`'s `HeldId::new`), and a divergence must not become
                // a new row just because it acquired a note.
                detail: match d.foreign_instance_note(local_origin) {
                    Some(note) => format!("{} — {note}", d.describe()),
                    None => d.describe(),
                },
                identity_detail: Some(d.identity_detail()),
                proposed_events: events,
                recover_orders: Vec::new(),
                dedup_key: None,
            });
        }
    }
    // The aggregated orphan-order alert lands LAST among this pass's alerts (see the module doc's
    // ordering note) — it summarizes the pass rather than belonging to one divergence's position.
    if !orphan_orders.is_empty() {
        recon.alerts.push(orphan_local_order_alert(orphan_orders));
    }
    if !pre_events.is_empty() {
        pre_events.extend(std::mem::take(&mut recon.events));
        recon.events = pre_events;
    }
    recon
}

/// The ONE aggregated `OrphanLocalOrder` alert of a pass. Event-free by construction (see the
/// module doc: nothing here may synthesize a cancel, and an operator confirm must not be able to
/// either), keyed on the constant [`ORPHAN_LOCAL_ORDER_KEY`], and SORTED so a steady orphan set
/// renders a byte-identical detail — which the runtime's held-alert refresh turns into a pure
/// no-op instead of a per-pass ring note.
fn orphan_local_order_alert(mut coids: Vec<String>) -> ReconAlert {
    let n = coids.len();
    coids.sort();
    let shown = n.min(ORPHAN_SAMPLE);
    let mut sample = coids[..shown].join(", ");
    if n > shown {
        sample.push_str(&format!(" (+{} more)", n - shown));
    }
    ReconAlert {
        kind: DivergenceKind::OrphanLocalOrder,
        detail: format!(
            "{n} live LOCAL order(s) absent from this pass's venue order report: {sample}. Nothing \
             is auto-applied and there is nothing to confirm INTO an action: an absent row is \
             equally consistent with a terminal we missed, an order still in flight to the venue, \
             and an order report that does not echo our client id. Verify at the venue, then \
             cancel or re-sync by hand."
        ),
        // Deliberately empty: a synthesized cancel would terminalize an order that may still be
        // resting at the venue, so an operator confirm must not be able to fold one either. This
        // alert is investigate-only, like `OrphanLocalPosition`'s and the fill-loss
        // `JournalDivergence`'s.
        proposed_events: Vec::new(),
        recover_orders: Vec::new(),
        dedup_key: Some(ORPHAN_LOCAL_ORDER_KEY.to_string()),
        identity_detail: None,
    }
}

/// The bare-`ReconMode::Hybrid` fallback classification (`ReconPolicy::hybrid()`'s per-kind preset
/// is the other half; the two MUST agree).
///
/// ⚠ NEITHER orphan kind is in this set. `OrphanLocalPosition` never was; `OrphanLocalOrder` was
/// removed so that held policies surface it instead of folding an empty list in silence — see this
/// module's own doc for the argument (the ORDER is local-origin, the DIVERGENCE is the venue's
/// silence about it, and an absence is not our activity confirmed by the venue). Neither folds any
/// event under any policy, so the move changed visibility only.
fn is_local_origin(kind: DivergenceKind) -> bool {
    matches!(kind, DivergenceKind::MissingFill | DivergenceKind::MissingTerminal)
}

/// The events a single divergence resolves to (before policy decides fold-vs-hold), plus a `pre`
/// flag: `true` marks pre-window residual legs that must fold BEFORE everything else in the pass
/// (see `position_events`).
fn events_for(
    d: &Divergence,
    pit: Option<PitFn>,
    fill_window: &FillWindowMap,
) -> (Vec<Event>, bool) {
    match d {
        Divergence::MissingFill(f) => {
            let coid = f
                .client_order_id
                .clone()
                .unwrap_or_else(|| external_coid(&f.venue, &f.venue_order_id));
            let mut out = Vec::new();
            // If the fill's order is external (no client id), synthesize its acceptance first so the
            // FSM has an order to advance.
            if f.client_order_id.is_none() {
                out.push(Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid.clone(),
                    venue_order_id: Some(f.venue_order_id.clone()),
                    ts: f.ts,
                }));
            }
            let (px, qty) = snap_pit(pit, &f.venue, &f.symbol, f.ts, f.last_px, f.last_qty);
            out.push(Event::Fill(FillEvent {
                trade_id: f.trade_id.clone(),
                client_order_id: coid,
                venue: ustr::ustr(&f.venue),
                symbol: ustr::ustr(&f.symbol),
                side: f.side,
                last_qty: qty,
                last_px: px,
                commission: f.commission,
                commission_asset: ustr::ustr(&f.commission_asset),
                liquidity_side: f.liquidity_side,
                ts: f.ts,
                mark_price: None,
                position_side: PositionSide::Both,
            }));
            (out, false)
        }
        Divergence::PositionDrift { report, local_qty } => {
            position_events(report, *local_qty, pit, fill_window)
        }
        Divergence::PositionOnlyExternal(report) => position_events(report, 0.0, pit, fill_window),
        // UnknownOrder deliberately lands in this catch-all: with `resolve`'s `adopt` None (the
        // flag off) that IS the original empty-events behavior; with `adopt` Some, `resolve`
        // handles the divergence before ever reaching here. `MissingTerminal` lands here too and
        // genuinely resolves to nothing under every policy — the last kind that still does, and a
        // sibling defect this module's `OrphanLocalOrder` section deliberately did NOT fold into
        // one change (its evidence is a POSITIVE venue report, so its cure is a terminal fold, not
        // an alert).
        _ => (Vec::new(), false),
    }
}

/// The decorative adoption `OrderAccepted` — minted ONLY for a coid-less (externally-placed)
/// report, mirroring the `MissingFill` arm's own `client_order_id.is_none()` gate (an order WITH
/// a client id that is absent from the registry is presumed to have already gone through its own
/// accept at some point). Decorative because `ExecutionEngine::on_event`'s lifecycle dispatch
/// drops any event whose coid is not already in the registry (its `dropped_unknown_coid`
/// counter moves once, at first fold, same as `MissingFill`'s own synthesized accept) — this is
/// not a registry-adoption mechanism, just parity with the existing synthesis shape so the
/// operator sees an accept in `recent_events`/the journal.
fn adoption_accept(o: &OrderStatusReport) -> impl Iterator<Item = Event> {
    o.client_order_id
        .is_none()
        .then(|| {
            Event::OrderAccepted(OrderAccepted {
                client_order_id: external_coid(&o.venue, &o.venue_order_id),
                venue_order_id: Some(o.venue_order_id.clone()),
                ts: o.ts,
            })
        })
        .into_iter()
}

/// The [`AdoptionCase::Adopt`] synthesis: the decorative accept (coid-less reports only) plus ONE
/// `Fill` for the report's CUMULATIVE `filled_qty` at `avg_px` (`snap_pit`-rounded the same way
/// `synth_position_legs` is). Safe precisely because `adoption_case` guaranteed the order is
/// terminal (the cumulative qty is final — it cannot grow into a later double-count), invisible
/// to this pass's fill lane, and not yet adopted. The `trade_id` is [`adoption_trade_id`] —
/// deterministic, so even a racing refold dedupes on the engine's `seen_trade_ids` guard.
fn adoption_events(o: &OrderStatusReport, pit: Option<PitFn>) -> Vec<Event> {
    let coid =
        o.client_order_id.clone().unwrap_or_else(|| external_coid(&o.venue, &o.venue_order_id));
    let mut out: Vec<Event> = adoption_accept(o).collect();
    let (px, qty) = snap_pit(pit, &o.venue, &o.symbol, o.ts, o.avg_px, o.filled_qty);
    out.push(Event::Fill(FillEvent {
        trade_id: adoption_trade_id(o),
        client_order_id: coid,
        venue: ustr::ustr(&o.venue),
        symbol: ustr::ustr(&o.symbol),
        side: o.side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: ustr::ustr(""),
        liquidity_side: LiquiditySide::Unknown,
        ts: o.ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }));
    out
}

/// A held UnknownOrder alert: operator-readable detail (which order, venue status, executed qty)
/// and a `dedup_key` so the runtime REFRESHES the held row in place across recurring passes
/// instead of appending a new alert per pass (an UnknownOrder recurs by design — the registry
/// never adopts it — so unkeyed re-alerts would grow without bound; fix-round-1 IMPORTANT-4).
fn unknown_order_alert(o: &OrderStatusReport, proposed_events: Vec<Event>) -> ReconAlert {
    ReconAlert {
        kind: DivergenceKind::UnknownOrder,
        detail: format!(
            "unknown venue order {} ({} {}) status {} filled {}/{}",
            o.venue_order_id, o.venue, o.symbol, o.status, o.filled_qty, o.qty
        ),
        proposed_events,
        recover_orders: Vec::new(),
        dedup_key: Some(o.venue_order_id.to_string()),
        identity_detail: None,
    }
}

/// Position-report resolution, in two regimes.
///
/// **With same-pass recovered fills** (the report is one-way `Both` AND this (venue, symbol) has
/// MissingFill synthesis in this pass): the unexplained residual is activity the venue's lookback
/// window could NOT see — necessarily OLDER than every recovered fill (that is what
/// beyond-lookback means). Its legs therefore move `local_qty → report.qty − fill_delta` (the
/// position the recovered fills then build on top of), anchor one tick BEFORE the earliest
/// recovered fill, and return `pre = true` so `resolve` folds them FIRST: a pre-window close
/// realizes PnL at its true flat point and the recovered fills open a fresh basis — instead of
/// blending into a stale position at snapshot time (the partial-lookback lifecycle-truncation
/// fix). This nets the same within-pass double-count the old `effective_local_qty` netted
/// (`local + delta → venue` and `local → venue − delta` are the same residual qty), with correct
/// sequencing.
///
/// **Without** (no same-pass recovered fills, or a hedge-mode `Long`/`Short` report): unchanged
/// legacy behavior — legs move `local_qty → report.qty` at the report's own ts, folded in place.
/// Synthesized MissingFill legs always carry `position_side: Both` (one order-history fill has no
/// hedge-mode bucket of its own), so they only net cleanly against a report that is ALSO one-way;
/// a hedge-mode report keeps two independent server-side buckets that a `Both` fill does not map
/// onto without venue context (which bucket did it open/close?) — netting/anchoring is
/// deliberately skipped there, an accepted limitation (hedge mode is the rarer case): the residual
/// leg can still double-count against an unrelated same-symbol `Both` fill history in the rare
/// mixed-mode setup, same as pre-fix behavior.
fn position_events(
    report: &PositionStatusReport,
    local_qty: f64,
    pit: Option<PitFn>,
    fill_window: &FillWindowMap,
) -> (Vec<Event>, bool) {
    let window = if report.position_side == PositionSide::Both {
        fill_window.get(&(report.venue.clone(), report.symbol.clone())).copied()
    } else {
        None
    };
    match window {
        Some((delta, earliest_ts)) => (
            synth_position_legs(
                local_qty,
                report.qty - delta,
                &report.venue,
                &report.symbol,
                report.avg_px,
                earliest_ts - 1,
                pit,
            ),
            true,
        ),
        None => (
            synth_position_legs(
                local_qty,
                report.qty,
                &report.venue,
                &report.symbol,
                report.avg_px,
                report.ts,
                pit,
            ),
            false,
        ),
    }
}

/// Synthesize the minimal EXTERNAL fill legs that move local net → venue net. A sign change across
/// zero is TWO legs (close then open) so `Account.compute_fill` books realized PnL on the close leg;
/// one leg would misbook PnL. Same-sign or from-flat is one leg.
fn synth_position_legs(
    local_qty: f64,
    venue_qty: f64,
    venue: &str,
    symbol: &str,
    px: f64,
    ts: i64,
    pit: Option<PitFn>,
) -> Vec<Event> {
    let mut out = Vec::new();
    let crosses_zero =
        local_qty != 0.0 && venue_qty.signum() != local_qty.signum() && venue_qty != 0.0;
    let legs: Vec<f64> = if crosses_zero {
        vec![-local_qty, venue_qty] // close local to flat, then open venue
    } else {
        vec![venue_qty - local_qty]
    };
    for (i, delta) in legs.into_iter().enumerate() {
        if delta == 0.0 {
            continue;
        }
        let side = if delta > 0.0 { 1 } else { -1 };
        let (spx, sqty) = snap_pit(pit, venue, symbol, ts, px, delta.abs());
        out.push(Event::Fill(FillEvent {
            // ⚠ KNOWN, PRE-EXISTING and deliberately UNCHANGED here: `ts` makes this id unstable
            // across a replay of the same drift, which is the one thing a synthesized dedup key must
            // not be (contrast `adoption_trade_id`, which is a pure function of venue+order id). It
            // is left byte-identical because the SAME `ts` is baked into this leg's paired
            // `client_order_id` on the next line, so correcting the shape changes a live journal key
            // and its coid together — a behaviour change owing its own PR and demo validation, not a
            // silent rider on a type change. `TradeId::prefixed` renders the identical string.
            trade_id: vike_model::events::TradeId::prefixed(
                "EXT-POS-",
                format_args!("{venue}-{symbol}-{ts}-{i}"),
            ),
            client_order_id: external_coid(venue, &format!("POS-{symbol}-{ts}-{i}")),
            venue: ustr::ustr(venue),
            symbol: ustr::ustr(symbol),
            side,
            last_qty: sqty,
            last_px: spx,
            commission: 0.0,
            commission_asset: ustr::ustr(""),
            liquidity_side: LiquiditySide::Unknown,
            ts,
            mark_price: None,
            position_side: PositionSide::Both,
        }));
    }
    out
}

/// Snap px/qty to the PIT instrument grid in effect at `ts`, when a PIT source is provided.
/// `SymbolProperties` carries no rounding methods of its own (it is plain per-symbol bounds
/// data — `crates/vike-model/src/instrument.rs`); this reuses the existing pinned rounding
/// primitive `crate::risk::round_to` (half-to-even onto `tick_size`/`step_size`), the same one
/// backtest fills snap to. A `0.0` tick/step (venue did not constrain it) is a no-op, matching
/// `round_to`'s own guard.
fn snap_pit(
    pit: Option<PitFn>,
    venue: &str,
    symbol: &str,
    ts: i64,
    px: f64,
    qty: f64,
) -> (f64, f64) {
    match pit.and_then(|f| f(venue, symbol, ts)) {
        Some(props) => (
            crate::risk::round_to(px, Some(props.tick_size)),
            crate::risk::round_to(qty, Some(props.step_size)),
        ),
        None => (px, qty),
    }
}

#[path = "resolve_tests.rs"]
#[cfg(test)]
mod resolve_tests;
