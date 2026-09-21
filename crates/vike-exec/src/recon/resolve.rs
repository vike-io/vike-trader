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
//!   CLAUDE.md's `VIKE_RECONCILE` rollout rule exists to prevent. (⚠ This bullet used to cite
//!   "`OrphanLocalOrder`'s auto-cancel" as the established precedent for that hazard. There is no
//!   such auto-cancel — that kind hits `events_for`'s catch-all and resolves to zero events under
//!   every policy, and nothing in this module constructs an `OrderCanceled`. The argument above
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
/// `vike_ops::reconcile_config`'s `auto_applied_kinds` still computes from it and renders the
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
/// (`crates/vike-core/src/journal_lock.rs` holds the same-machine half and cannot reach past one
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recon::types::ReconPolicy;
    use vike_model::FillReport;

    fn ext_fill(trade_id: &'static str) -> FillReport {
        FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: trade_id.into(),
            venue_order_id: "v9".into(),
            client_order_id: None,
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        }
    }

    #[test]
    fn external_missing_fill_synthesizes_accept_then_fill() {
        let d = vec![Divergence::MissingFill(ext_fill("t1"))];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        assert_eq!(r.events.len(), 2);
        assert!(matches!(r.events[0], Event::OrderAccepted(_)));
        assert!(matches!(r.events[1], Event::Fill(_)));
    }

    #[test]
    fn journal_divergence_always_alerts_under_every_policy() {
        // A three-way persistence-bug signal never resolves to an event: under EVERY policy it must
        // surface as a single investigative alert with no proposed events (the always-surface rule).
        let policies = [
            ReconPolicy::default(), // Synthesize
            ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }, // Quarantine
            ReconPolicy::hybrid(),  // Hybrid
        ];
        for policy in &policies {
            let d = vec![Divergence::JournalDivergence {
                detail: "journal has fill t9, live account lacks it".into(),
                recover_order: None,
            }];
            let r = resolve(d, policy, None, None);
            assert!(r.events.is_empty(), "journal divergence synthesizes NO events ({policy:?})");
            assert_eq!(r.alerts.len(), 1, "exactly one alert ({policy:?})");
            assert_eq!(r.alerts[0].kind, DivergenceKind::JournalDivergence);
            assert!(
                r.alerts[0].proposed_events.is_empty(),
                "investigative alert carries no proposed events (nothing to auto-apply)"
            );
            assert!(
                r.alerts[0].recover_orders.is_empty(),
                "a fill-loss divergence carries no re-registration ({policy:?})"
            );
            assert!(r.alerts[0].detail.contains("t9"), "detail is preserved");
        }
    }

    #[test]
    fn order_loss_journal_divergence_carries_recovery_under_every_policy() {
        // The ORDER-loss case (recover_order = Some) always surfaces as an alert too, but now the
        // alert carries the venue order to RE-REGISTER on operator confirm — under EVERY policy.
        let report = vike_model::OrderStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some("c-lost".into()),
            side: 1,
            order_type: "LIMIT".into(),
            qty: 1.0,
            filled_qty: 0.0,
            avg_px: 0.0,
            status: "ACCEPTED".into(),
            ts: 3,
        };
        for policy in [ReconPolicy::default(), ReconPolicy::hybrid()] {
            let d = vec![Divergence::JournalDivergence {
                detail: "order c-lost live in journal, local lost it".into(),
                recover_order: Some(Box::new(report.clone())),
            }];
            let r = resolve(d, &policy, None, None);
            assert!(r.events.is_empty(), "never auto-folds ({policy:?})");
            assert_eq!(r.alerts.len(), 1);
            assert_eq!(r.alerts[0].recover_orders.len(), 1, "carries the order to re-register");
            assert_eq!(r.alerts[0].recover_orders[0].client_order_id.as_deref(), Some("c-lost"));
        }
    }

    #[test]
    fn quarantine_holds_events_as_alert() {
        let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
        let d = vec![Divergence::MissingFill(ext_fill("t1"))];
        let r = resolve(d, &policy, None, None);
        assert!(r.events.is_empty());
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].proposed_events.len(), 2);
    }

    #[test]
    fn external_coid_is_deterministic() {
        assert_eq!(external_coid("binance", "v9"), "EXT-binance-v9");
    }

    fn pos_drift(local_qty: f64, venue_qty: f64) -> Divergence {
        Divergence::PositionDrift {
            report: vike_model::PositionStatusReport {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                qty: venue_qty,
                avg_px: 100.0,
                ts: 7,
                margin_mode: Default::default(),
                isolated_margin: None,
                delta: None,
            },
            local_qty,
        }
    }

    #[test]
    fn same_sign_drift_is_one_fill() {
        // local +1, venue +3 → one synthetic buy of +2
        let r = resolve(vec![pos_drift(1.0, 3.0)], &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r.events.iter().filter(|e| matches!(e, Event::Fill(_))).collect();
        assert_eq!(fills.len(), 1);
        if let Event::Fill(f) = fills[0] {
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 2.0);
        }
    }

    #[test]
    fn zero_crossing_drift_is_two_fills() {
        // local +2, venue -1 → close +2 (sell 2), then open -1 (sell 1) = two legs
        let r = resolve(vec![pos_drift(2.0, -1.0)], &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].last_qty, 2.0); // close leg
        assert_eq!(fills[1].last_qty, 1.0); // open leg
        assert!(fills.iter().all(|f| f.side == -1));
    }

    // --- within-pass double-count netting (fills vs position-drift legs) ---

    fn missing_fill(side: i32, qty: f64, trade_id: &'static str) -> Divergence {
        Divergence::MissingFill(vike_model::FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: trade_id.into(),
            venue_order_id: "v-mf".into(),
            client_order_id: None,
            side,
            last_qty: qty,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        })
    }

    fn pos_drift_side(local_qty: f64, venue_qty: f64, side: PositionSide) -> Divergence {
        Divergence::PositionDrift {
            report: vike_model::PositionStatusReport {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: side,
                qty: venue_qty,
                avg_px: 100.0,
                ts: 7,
                margin_mode: Default::default(),
                isolated_margin: None,
                delta: None,
            },
            local_qty,
        }
    }

    #[test]
    fn missing_fill_fully_explains_position_drift_no_extra_leg() {
        // venue holds +1 BTC; local is flat; the +1 is entirely explained by one missed fill.
        // resolve must emit ONLY the fill's own events (accept + fill) — no EXT-POS-* leg.
        let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift(0.0, 1.0)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 1, "expected only the MissingFill's own Fill, got {fills:?}");
        assert_eq!(fills[0].trade_id.as_str(), "t-mf1");
        assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-POS-")));
        // OrderAccepted (for the external fill's order) + Fill = 2 total events.
        assert_eq!(r.events.len(), 2, "{:?}", r.events);
    }

    #[test]
    fn missing_fill_partially_explains_position_drift_residual_leg() {
        // venue holds +3 BTC; local is flat; a +1 missed fill explains part of it, leaving a
        // residual of +2 that must still be synthesized as a position leg.
        let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift(0.0, 3.0)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        // The MissingFill's own Fill, plus exactly one residual position leg.
        assert_eq!(fills.len(), 2, "{fills:?}");
        let pos_legs: Vec<_> =
            fills.iter().filter(|f| f.trade_id.starts_with("EXT-POS-")).collect();
        assert_eq!(pos_legs.len(), 1);
        assert_eq!(pos_legs[0].side, 1);
        assert_eq!(pos_legs[0].last_qty, 2.0);
    }

    // --- pre-window residual sequencing (partial-lookback lifecycle truncation) ---
    //
    // When a position report and recovered fills coexist, the UNEXPLAINED residual is activity the
    // lookback window could not see — which is necessarily OLDER than every recovered fill (that is
    // what beyond-lookback means). Its synthetic legs must therefore anchor BEFORE the earliest
    // recovered fill and fold FIRST, so the fold books the pre-window close at its true flat point
    // and the recovered fills open a fresh basis on top — instead of blending into a stale position
    // and misbooking realized PnL at snapshot time.

    #[test]
    fn truncated_lifecycle_close_leg_folds_before_recovered_fills() {
        // local +10; window recovered a buy +5 (ts 5); venue net +5. The unseen −10 close happened
        // before the window: one pre-anchored close leg (side −1, qty 10, ts 4) folds FIRST, then
        // the recovered fill opens the fresh +5. (Old behavior: one −10 leg AFTER the fill at
        // snapshot ts — position right, basis/PnL blended wrong.)
        let d = vec![missing_fill(1, 5.0, "t-mf1"), pos_drift(10.0, 5.0)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 2, "{fills:?}");
        assert!(
            fills[0].trade_id.starts_with("EXT-POS-"),
            "pre-window close leg must fold before the recovered fill: {fills:?}"
        );
        assert_eq!(fills[0].side, -1);
        assert_eq!(fills[0].last_qty, 10.0, "full close to flat, not a blended remainder");
        assert_eq!(fills[0].ts, 4, "anchored before the earliest recovered fill (ts 5)");
        assert_eq!(fills[1].trade_id.as_str(), "t-mf1");
    }

    #[test]
    fn pre_window_residual_crossing_splits_at_flat_before_fills() {
        // local +2; window recovered a buy +2 (ts 5); venue net −1. Pre-window truth: +2 crossed
        // flat to −3 (then the recovered +2 brings it to −1). TWO pre-anchored legs — close 2,
        // open 3, both ts 4, both sells — fold before the recovered fill.
        let d = vec![missing_fill(1, 2.0, "t-mf2"), pos_drift(2.0, -1.0)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(fills.len(), 3, "{fills:?}");
        assert!(fills[0].trade_id.starts_with("EXT-POS-"));
        assert_eq!((fills[0].side, fills[0].last_qty, fills[0].ts), (-1, 2.0, 4), "close to flat");
        assert!(fills[1].trade_id.starts_with("EXT-POS-"));
        assert_eq!((fills[1].side, fills[1].last_qty, fills[1].ts), (-1, 3.0, 4), "open short");
        assert_eq!(fills[2].trade_id.as_str(), "t-mf2", "recovered fill folds last: −3 + 2 = −1");
    }

    #[test]
    fn hedge_mode_position_report_is_not_netted_against_fills() {
        // A `Both`-side MissingFill does not cleanly map onto a hedge-mode Long/Short bucket, so
        // netting is intentionally skipped: the position leg is synthesized against the RAW
        // local_qty, exactly as before this fix.
        let d = vec![missing_fill(1, 1.0, "t-mf1"), pos_drift_side(0.0, 1.0, PositionSide::Long)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        let fills: Vec<_> = r
            .events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect();
        // The MissingFill's Fill, PLUS an un-netted position leg for the full venue_qty (1.0).
        assert_eq!(fills.len(), 2, "{fills:?}");
        let pos_legs: Vec<_> =
            fills.iter().filter(|f| f.trade_id.starts_with("EXT-POS-")).collect();
        assert_eq!(pos_legs.len(), 1);
        assert_eq!(pos_legs[0].last_qty, 1.0);
    }

    // --- generate_missing_orders / UnknownOrder adoption ---

    /// Owned backing for an [`AdoptContext`] (which borrows its two sets).
    #[derive(Default)]
    struct Adopt {
        fills: HashSet<String>,
        seen: HashSet<String>,
    }

    impl Adopt {
        fn ctx(&self) -> AdoptContext<'_> {
            AdoptContext { pass_fill_order_ids: &self.fills, seen_trade_ids: &self.seen }
        }
    }

    fn unknown_order_report(
        client_order_id: Option<&str>,
        filled_qty: f64,
        side: i32,
        status: &str,
    ) -> vike_model::OrderStatusReport {
        vike_model::OrderStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v-unk".into(),
            client_order_id: client_order_id.map(String::from),
            side,
            order_type: "LIMIT".into(),
            qty: 2.0,
            filled_qty,
            avg_px: if filled_qty != 0.0 { 100.0 } else { 0.0 },
            status: status.into(),
            ts: 11,
        }
    }

    fn unknown_order(client_order_id: Option<&str>, filled_qty: f64, side: i32) -> Divergence {
        // Terminal FILLED report — the adoption case (when no fill reports co-occur in-pass).
        Divergence::UnknownOrder(unknown_order_report(client_order_id, filled_qty, side, "FILLED"))
    }

    fn fill_events(r: &Recon) -> Vec<&FillEvent> {
        r.events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unknown_order_flag_off_synthesizes_nothing() {
        // Default (adopt: None): the original behavior — UnknownOrder still falls into the empty
        // catch-all, even for an already-filled terminal report.
        let d = vec![unknown_order(None, 1.0, 1)];
        let r = resolve(d, &ReconPolicy::default(), None, None);
        assert!(r.events.is_empty());
    }

    #[test]
    fn live_unknown_order_synthesizes_nothing_that_folds() {
        // A non-terminal (still-working) unknown order NEVER folds anything: its executions
        // arrive as MissingFill divergences with real venue trade-ids while it stays inside the
        // lookback. Under Synthesize mode that means a true no-op (no events, no alert).
        let a = Adopt::default();
        for filled in [0.0, 1.0] {
            let d =
                vec![Divergence::UnknownOrder(unknown_order_report(None, filled, 1, "ACCEPTED"))];
            let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
            assert!(r.events.is_empty(), "filled={filled}: {:?}", r.events);
            assert!(r.alerts.is_empty(), "synthesize mode never alerts");
        }
    }

    #[test]
    fn live_unknown_order_under_hybrid_holds_a_display_accept_only() {
        // Held modes still surface the live external order for operator visibility/adoption, but
        // the proposed events carry AT MOST the decorative accept — never a synthesized fill that
        // could double-book against the fill lane at confirm time.
        let a = Adopt::default();
        let d = vec![Divergence::UnknownOrder(unknown_order_report(None, 1.0, 1, "ACCEPTED"))];
        let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
        assert!(r.events.is_empty());
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].kind, DivergenceKind::UnknownOrder);
        assert_eq!(r.alerts[0].proposed_events.len(), 1, "{:?}", r.alerts[0].proposed_events);
        assert!(matches!(r.alerts[0].proposed_events[0], Event::OrderAccepted(_)));
        assert_eq!(r.alerts[0].dedup_key.as_deref(), Some("v-unk"), "keyed for runtime dedup");
    }

    #[test]
    fn terminal_unknown_order_beyond_the_fill_window_is_adopted() {
        // THE adoption case: terminal, executed, and no fill report in-pass (the executions fell
        // outside the lookback) — accept + one cumulative fill.
        let a = Adopt::default();
        let d = vec![unknown_order(None, 1.0, 1)];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
        assert_eq!(r.events.len(), 2, "{:?}", r.events);
        assert!(matches!(r.events[0], Event::OrderAccepted(_)));
        match &r.events[1] {
            Event::Fill(f) => {
                assert_eq!(f.side, 1);
                assert_eq!(f.last_qty, 1.0);
                assert_eq!(f.last_px, 100.0);
                assert!(f.trade_id.starts_with("EXT-ORD-"));
            }
            other => panic!("expected Fill, got {other:?}"),
        }
    }

    #[test]
    fn terminal_unknown_order_with_in_pass_fill_reports_defers_to_the_fill_lane() {
        // Fix-round-1 CRITICAL-1: a recently-executed external order surfaces BOTH a MissingFill
        // (real venue trade-id) and an UnknownOrder (coid absent from the registry) in the same
        // pass. The adoption arm must synthesize NOTHING — the fill lane books the qty exactly
        // once under its real trade-id; a same-pass PositionDrift nets against that fill alone.
        let mut a = Adopt::default();
        a.fills.insert("v-unk".into()); // this pass's fill reports cover order v-unk
        let mut mf = ext_fill("t-real-1");
        mf.venue_order_id = "v-unk".into();
        let d = vec![Divergence::MissingFill(mf), unknown_order(None, 1.0, 1), pos_drift(0.0, 1.0)];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
        let fills = fill_events(&r);
        assert_eq!(fills.len(), 1, "exactly one booking of the external qty: {fills:?}");
        assert_eq!(fills[0].trade_id.as_str(), "t-real-1", "the REAL venue trade-id");
        assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-ORD-")));
        assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-POS-")));
    }

    #[test]
    fn progressively_filling_unknown_order_converges_via_the_fill_lane() {
        // Fix-round-1 CRITICAL-2 (resolve half): pass 2 of a progressively-filling external order
        // — local already booked fill 1 (qty 1), the venue now reports filled_qty 2 with the
        // second fill as a MissingFill, and PositionDrift local 1 → venue 2. The live UnknownOrder
        // must feed NEITHER events NOR the fill window: only the new real fill folds, the drift is
        // fully explained by it (no phantom EXT-POS close leg), and local converges to 2.
        let mut a = Adopt::default();
        a.fills.insert("v-unk".into());
        a.seen.insert("t-real-1".into()); // fill 1 already folded in a prior pass
        let mut mf = ext_fill("t-real-2");
        mf.venue_order_id = "v-unk".into();
        let d = vec![
            Divergence::MissingFill(mf),
            Divergence::UnknownOrder(unknown_order_report(None, 2.0, 1, "PARTIALLY_FILLED")),
            pos_drift(1.0, 2.0),
        ];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
        let fills = fill_events(&r);
        assert_eq!(fills.len(), 1, "only the new real fill: {fills:?}");
        assert_eq!(fills[0].trade_id.as_str(), "t-real-2");
        assert_eq!(fills[0].last_qty, 1.0);
        assert!(!fills.iter().any(|f| f.trade_id.starts_with("EXT-")), "no synthetic legs");
    }

    #[test]
    fn already_adopted_unknown_order_is_a_true_no_op() {
        // The recurring-pass invariant: once the adoption fill's deterministic trade_id is in the
        // seen set, the divergence resolves to NOTHING — no events (nothing re-folds, so no
        // dropped_unknown_coid inflation either) and no alert row, under both auto-apply and held
        // policies.
        let mut a = Adopt::default();
        a.seen.insert("EXT-ORD-binance-v-unk".into());
        for policy in [ReconPolicy::default(), ReconPolicy::hybrid()] {
            let d = vec![unknown_order(None, 1.0, 1)];
            let r = resolve(d, &policy, None, Some(a.ctx()));
            assert!(r.events.is_empty(), "{policy:?}: {:?}", r.events);
            assert!(r.alerts.is_empty(), "{policy:?}: {:?}", r.alerts);
        }
    }

    #[test]
    fn unknown_order_with_client_id_skips_the_synthesized_accept() {
        // A report that DOES carry a client_order_id (just absent from the local registry) is not
        // externally-placed in the MissingFill sense — no accept is minted, mirroring the
        // MissingFill arm's own `client_order_id.is_none()` gate. Only the Fill rides the report's
        // own coid.
        let a = Adopt::default();
        let d = vec![unknown_order(Some("c-known"), 2.0, -1)];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
        assert_eq!(r.events.len(), 1, "{:?}", r.events);
        match &r.events[0] {
            Event::Fill(f) => {
                assert_eq!(f.client_order_id, "c-known");
                assert_eq!(f.side, -1);
                assert_eq!(f.last_qty, 2.0);
            }
            other => panic!("expected Fill, got {other:?}"),
        }
    }

    #[test]
    fn unknown_order_hybrid_policy_still_quarantines_but_now_carries_proposed_events() {
        // The point of the flag: under `hybrid` (no-local-origin -> Quarantine), an adoptable
        // UnknownOrder is still held for operator confirm, but its alert is no longer empty — it
        // carries the adoption events an operator's one-click confirm would fold.
        let a = Adopt::default();
        let d = vec![unknown_order(None, 1.0, 1)];
        let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
        assert!(r.events.is_empty(), "no-local-origin kind stays held under hybrid");
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].kind, DivergenceKind::UnknownOrder);
        assert_eq!(
            r.alerts[0].proposed_events.len(),
            2,
            "adoptable: accept + fill, not empty ({:?})",
            r.alerts[0].proposed_events
        );
        assert_eq!(r.alerts[0].dedup_key.as_deref(), Some("v-unk"));
    }

    #[test]
    fn adopted_unknown_order_fill_nets_against_same_pass_position_drift() {
        // Mirrors `missing_fill_fully_explains_position_drift_no_extra_leg`: an adoption fill that
        // WILL fold this pass feeds the SAME fill_window a MissingFill would, so a co-occurring
        // PositionDrift report on the identical (venue, symbol) nets against it instead of
        // double-booking the same external activity twice in one pass.
        //
        // ⚠ The quantities are 2.0, not 1.0, ON PURPOSE. The netting term is
        // `side as f64 * filled_qty`, and at qty 1.0 that multiplication is its own identity —
        // `1.0 * 1.0` and `1.0 / 1.0` are both 1.0 — so a sweep could replace the `*` with `/` and
        // this test stayed green (measured: `vike-exec` 473/473 with the mutant applied). At 2.0
        // the operator is observable: the true leg is 2.0, a division yields 0.5, and the drift no
        // longer nets to zero, so a phantom EXT-POS residual appears alongside the real fill.
        let a = Adopt::default();
        let d = vec![unknown_order(None, 2.0, 1), pos_drift(0.0, 2.0)];
        let r = resolve(d, &ReconPolicy::default(), None, Some(a.ctx()));
        let fills = fill_events(&r);
        assert_eq!(
            fills.len(),
            1,
            "expected only the UnknownOrder's own Fill, no EXT-POS residual: {fills:?}"
        );
        assert_eq!(fills[0].last_qty, 2.0, "the adopted leg is the order's own size: {fills:?}");
        assert!(fills[0].trade_id.starts_with("EXT-ORD-"));
    }

    #[test]
    fn held_unknown_order_adoption_does_not_feed_the_fill_window() {
        // Fix-round-1 IMPORTANT-3 (mode consistency): under hybrid the adoption is HELD — its
        // fill does NOT fold this pass — so a co-occurring PositionDrift (auto-applied under
        // hybrid) must synthesize its FULL healing leg, un-netted. Netting against held events
        // would silently suppress the drift's self-heal.
        let a = Adopt::default();
        let d = vec![unknown_order(None, 1.0, 1), pos_drift(0.0, 1.0)];
        let r = resolve(d, &ReconPolicy::hybrid(), None, Some(a.ctx()));
        let fills = fill_events(&r);
        assert_eq!(fills.len(), 1, "the drift's own healing leg folds: {fills:?}");
        assert!(fills[0].trade_id.starts_with("EXT-POS-"), "full un-netted drift leg");
        assert_eq!(fills[0].last_qty, 1.0);
        assert_eq!(r.alerts.len(), 1, "the adoption itself stays held");
        assert_eq!(r.alerts[0].proposed_events.len(), 2);
    }

    #[test]
    fn unknown_order_trade_id_is_deterministic_for_idempotent_refold() {
        // Same report resolved twice (e.g. a re-run pass over an unchanged venue snapshot,
        // BEFORE the first fold lands in seen_trade_ids) synthesizes the IDENTICAL trade_id both
        // times, so a racing second fold dedupes on `ExecutionEngine::on_event`'s
        // `seen_trade_ids` guard instead of double-booking — the structural half of idempotency
        // `external_coid`'s own doc describes for MissingFill.
        let a = Adopt::default();
        let r1 = resolve(
            vec![unknown_order(None, 1.0, 1)],
            &ReconPolicy::default(),
            None,
            Some(a.ctx()),
        );
        let r2 = resolve(
            vec![unknown_order(None, 1.0, 1)],
            &ReconPolicy::default(),
            None,
            Some(a.ctx()),
        );
        let tid = |r: &Recon| {
            r.events
                .iter()
                .find_map(|e| match e {
                    Event::Fill(f) => Some(f.trade_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(tid(&r1), tid(&r2));
    }

    // --- BalanceDrift resolution (Feature 2: first-class cash reconcile) ---

    fn balance_drift() -> Divergence {
        Divergence::BalanceDrift {
            venue: "binance".into(),
            asset: "USDT".into(),
            local: 10_000.0,
            venue_bal: 10_150.0,
            ts: 42,
        }
    }

    #[test]
    fn balance_drift_synthesize_folds_one_account_state_no_alert() {
        // synthesize ⇒ adopt venue truth immediately: one AccountState folds, no alert (this is
        // the equivalent-to-today trust-the-venue path, but now via the policy machinery).
        let r = resolve(vec![balance_drift()], &ReconPolicy::default(), None, None);
        assert!(r.alerts.is_empty());
        assert_eq!(r.events.len(), 1);
        match &r.events[0] {
            Event::AccountState(a) => {
                assert_eq!(a.venue.as_str(), "binance");
                assert_eq!(a.balances, vec![("USDT".to_string(), 10_150.0)]);
                assert_eq!(a.ts, 42);
            }
            other => panic!("expected AccountState, got {other:?}"),
        }
    }

    #[test]
    fn balance_drift_hybrid_and_quarantine_hold_the_account_state_for_confirm() {
        // The load-bearing safety choice: under BOTH hybrid (no-local-origin ⇒ Quarantine) and
        // explicit quarantine, a surprise cash move NEVER auto-folds — it is held with the
        // correcting AccountState carried in proposed_events, keyed for in-place refresh.
        for policy in [
            ReconPolicy::hybrid(),
            ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        ] {
            let r = resolve(vec![balance_drift()], &policy, None, None);
            assert!(r.events.is_empty(), "never auto-folds a surprise ({policy:?})");
            assert_eq!(r.alerts.len(), 1);
            let a = &r.alerts[0];
            assert_eq!(a.kind, DivergenceKind::BalanceDrift);
            assert_eq!(a.dedup_key.as_deref(), Some("balance:USDT"), "one row per (venue, asset)");
            assert_eq!(a.proposed_events.len(), 1, "the AccountState an operator confirm folds");
            assert!(matches!(a.proposed_events[0], Event::AccountState(_)));
            assert!(a.detail.contains("+150"), "unexplained delta surfaced: {}", a.detail);
        }
    }

    // --- OrphanLocalPosition resolution (diff's local-side position sweep) ---

    fn orphan_local_position(symbol: &str, side: &str, qty: f64) -> Divergence {
        Divergence::OrphanLocalPosition {
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: side.into(),
            local_qty: qty,
        }
    }

    #[test]
    fn orphan_local_position_never_folds_an_event_under_any_policy() {
        // The load-bearing safety property: there is no defensible price for a synthetic close
        // (LocalView carries qty only), so this kind resolves to ZERO events under EVERY policy —
        // including `synthesize`, which folds everything else it can.
        for policy in [
            ReconPolicy::default(), // Synthesize
            ReconPolicy::hybrid(),
            ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
            ReconPolicy { default: ReconMode::Hybrid, ..Default::default() },
        ] {
            let r =
                resolve(vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)], &policy, None, None);
            assert!(
                r.events.is_empty(),
                "never auto-flattens a position ({policy:?}): {:?}",
                r.events
            );
            assert!(
                r.alerts.iter().all(|a| a.proposed_events.is_empty()),
                "investigate-only: a confirm must not flatten at a guessed price either ({policy:?})"
            );
        }
    }

    #[test]
    fn orphan_local_position_is_a_silent_no_op_under_synthesize() {
        // NAMED RESIDUAL (module doc): `synthesize` folds this kind's empty event list, so nothing
        // folds AND nothing alerts — the same shape `AdoptionCase::SurfaceOnly` takes under an
        // auto-apply mode. An operator who wants the blind spot surfaced runs hybrid/quarantine.
        let r = resolve(
            vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)],
            &ReconPolicy::default(),
            None,
            None,
        );
        assert_eq!(r, Recon::default(), "synthesize: a true no-op");
    }

    #[test]
    fn orphan_local_position_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
        // hybrid classifies it no-local-origin (an absent venue row is evidence-by-absence, which
        // an incomplete/symbol-scoped fetch produces just as readily as a genuinely closed
        // position), so BOTH held policies surface exactly one operator alert — and it is
        // dedup-keyed, because nothing heals this divergence and an un-keyed alert would append a
        // new row every pass.
        for policy in [
            ReconPolicy::hybrid(),
            ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        ] {
            let r =
                resolve(vec![orphan_local_position("BTCUSDT", "BOTH", -2.5)], &policy, None, None);
            assert!(r.events.is_empty(), "{policy:?}");
            assert_eq!(r.alerts.len(), 1, "{policy:?}");
            let a = &r.alerts[0];
            assert_eq!(a.kind, DivergenceKind::OrphanLocalPosition);
            assert_eq!(
                a.dedup_key.as_deref(),
                Some("position:BTCUSDT:BOTH"),
                "one row per (venue, symbol, side)"
            );
            assert!(a.proposed_events.is_empty(), "nothing an operator confirm could fold");
            assert!(a.recover_orders.is_empty());
            assert!(a.detail.contains("BTCUSDT"), "operator-readable: {}", a.detail);
            assert!(a.detail.contains("-2.5"), "carries the local qty: {}", a.detail);
            assert!(a.detail.contains("binance"), "names the venue: {}", a.detail);
        }
    }

    #[test]
    fn orphan_local_position_bare_hybrid_default_matches_the_preset() {
        // The two classification sites must agree: `ReconPolicy::hybrid()`'s per-kind preset and
        // the bare `ReconMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind.
        let bare = ReconPolicy { default: ReconMode::Hybrid, ..Default::default() };
        let preset = ReconPolicy::hybrid();
        let d = || vec![orphan_local_position("BTCUSDT", "BOTH", 1.0)];
        assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
        assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
    }

    #[test]
    fn orphan_local_position_dedup_key_separates_symbol_and_side() {
        // Hedge mode / multi-symbol: each (symbol, side) gets its own held row, so refreshing one
        // never overwrites another.
        let policy = ReconPolicy::hybrid();
        let d = vec![
            orphan_local_position("BTCUSDT", "LONG", 1.0),
            orphan_local_position("BTCUSDT", "SHORT", -2.0),
            orphan_local_position("ETHUSDT", "BOTH", 3.0),
        ];
        let r = resolve(d, &policy, None, None);
        let keys: Vec<_> = r.alerts.iter().filter_map(|a| a.dedup_key.clone()).collect();
        assert_eq!(
            keys,
            vec!["position:BTCUSDT:LONG", "position:BTCUSDT:SHORT", "position:ETHUSDT:BOTH"]
        );
    }

    #[test]
    fn orphan_local_position_does_not_disturb_a_co_occurring_missing_fill() {
        // Non-interference: the sweep contributes nothing to the fill window and nothing to the
        // event stream, so a MissingFill in the same pass resolves byte-identically to a pass
        // without it.
        let policy = ReconPolicy::hybrid();
        let without = resolve(vec![Divergence::MissingFill(ext_fill("t1"))], &policy, None, None);
        let with = resolve(
            vec![
                Divergence::MissingFill(ext_fill("t1")),
                orphan_local_position("ETHUSDT", "BOTH", 4.0),
            ],
            &policy,
            None,
            None,
        );
        assert_eq!(with.events, without.events, "the fill's own events are untouched");
        assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
    }

    // --- OrphanLocalOrder resolution (diff's local-side ORDER sweep) ---
    //
    // The defect this section closes: the kind was detected and resolved to NOTHING under every
    // policy — `hybrid`/`synthesize` folded an empty event list and raised no alert, `quarantine`
    // appended one un-keyed, detail-less row per orphan per pass. See the module doc.

    fn orphan_order(coid: &str) -> Divergence {
        Divergence::OrphanLocalOrder { client_order_id: coid.into() }
    }

    fn quarantine_policy() -> ReconPolicy {
        ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }
    }

    #[test]
    fn orphan_local_order_never_folds_an_event_under_any_policy() {
        // The load-bearing safety property, unchanged by making the kind visible: a synthesized
        // cancel would terminalize an order that may still be RESTING at the venue (this module
        // folds locally and calls no venue), so the kind resolves to ZERO events under EVERY
        // policy — and its alert proposes zero events, so an operator confirm cannot fold one
        // either.
        for policy in [
            ReconPolicy::default(), // Synthesize
            ReconPolicy::hybrid(),
            quarantine_policy(),
            ReconPolicy { default: ReconMode::Hybrid, ..Default::default() },
        ] {
            let r = resolve(vec![orphan_order("c-1")], &policy, None, None);
            assert!(r.events.is_empty(), "never folds ({policy:?}): {:?}", r.events);
            assert!(
                !r.events.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
                "no policy may synthesize a cancel ({policy:?})"
            );
            assert!(
                r.alerts
                    .iter()
                    .all(|a| a.proposed_events.is_empty() && a.recover_orders.is_empty()),
                "confirm is a pure acknowledgement — nothing to fold, nothing to re-register \
                 ({policy:?})"
            );
        }
    }

    #[test]
    fn orphan_local_order_is_a_silent_no_op_under_synthesize() {
        // NAMED RESIDUAL (module doc), the same one `OrphanLocalPosition` carries: `synthesize`
        // folds this kind's empty event list, so nothing folds AND nothing alerts. An operator who
        // wants the blind spot surfaced runs hybrid/quarantine.
        let r = resolve(vec![orphan_order("c-1")], &ReconPolicy::default(), None, None);
        assert_eq!(r, Recon::default(), "synthesize: a true no-op");
    }

    #[test]
    fn orphan_local_order_holds_one_dedup_keyed_alert_under_hybrid_and_quarantine() {
        // THE fix. `hybrid` used to fold the empty list and raise nothing at all; `quarantine`
        // raised one un-keyed row per orphan per pass whose detail was the literal kind name.
        // Both now surface ONE dedup-keyed, operator-readable, event-free row.
        for policy in [ReconPolicy::hybrid(), quarantine_policy()] {
            let r = resolve(vec![orphan_order("c-1")], &policy, None, None);
            assert!(r.events.is_empty(), "{policy:?}");
            assert_eq!(r.alerts.len(), 1, "{policy:?}: {:?}", r.alerts);
            let a = &r.alerts[0];
            assert_eq!(a.kind, DivergenceKind::OrphanLocalOrder);
            assert_eq!(
                a.dedup_key.as_deref(),
                Some(ORPHAN_LOCAL_ORDER_KEY),
                "one held row per (venue, kind) — NOT one per coid, see the module doc"
            );
            assert!(a.proposed_events.is_empty(), "nothing an operator confirm could fold");
            assert!(a.recover_orders.is_empty());
            assert!(a.detail.contains("c-1"), "names the order: {}", a.detail);
            assert!(a.detail.contains('1'), "carries the count: {}", a.detail);
        }
    }

    #[test]
    fn orphan_local_order_bare_hybrid_default_matches_the_preset() {
        // The two classification sites must agree: `ReconPolicy::hybrid()`'s per-kind preset and
        // the bare `ReconMode::Hybrid` fallback (`is_local_origin`). Both must HOLD this kind —
        // the twin of `orphan_local_position_bare_hybrid_default_matches_the_preset`, and the
        // assertion that would have failed had only one of the two sites been moved.
        let bare = ReconPolicy { default: ReconMode::Hybrid, ..Default::default() };
        let preset = ReconPolicy::hybrid();
        let d = || vec![orphan_order("c-1")];
        assert_eq!(resolve(d(), &bare, None, None), resolve(d(), &preset, None, None));
        assert_eq!(resolve(d(), &bare, None, None).alerts.len(), 1);
    }

    #[test]
    fn many_orphan_local_orders_aggregate_into_exactly_one_alert() {
        // The whole reason the coid is NOT the dedup key: the held-alert store never self-clears
        // and its rows are cloned into every published snapshot, so a pass in which the venue
        // report echoes no client ids at all (every live order orphans at once) must still cost
        // ONE row. The count is exact; only the NAMED sample truncates.
        let d: Vec<Divergence> =
            (0..ORPHAN_SAMPLE + 5).map(|i| orphan_order(&format!("c-{i:02}"))).collect();
        let n = d.len();
        let r = resolve(d, &ReconPolicy::hybrid(), None, None);
        assert_eq!(r.alerts.len(), 1, "{:?}", r.alerts);
        let detail = &r.alerts[0].detail;
        assert!(detail.starts_with(&format!("{n} live LOCAL order(s)")), "exact count: {detail}");
        assert!(detail.contains("(+5 more)"), "sample truncated with a remainder: {detail}");
        assert!(detail.contains("c-00"), "sorted sample starts at the lowest coid: {detail}");
        assert!(!detail.contains("c-12"), "beyond the sample cap: {detail}");
    }

    #[test]
    fn a_steady_orphan_set_renders_a_byte_identical_detail() {
        // Sorting is load-bearing, not tidiness: the runtime REFRESHES a dedup-keyed row only when
        // the detail changed, so an unchanged orphan set arriving in a different registry order
        // must render the identical string (otherwise every pass writes a ring note and churns the
        // held row). A CHANGED set must move it.
        let p = ReconPolicy::hybrid();
        let fwd = resolve(vec![orphan_order("c-a"), orphan_order("c-b")], &p, None, None);
        let rev = resolve(vec![orphan_order("c-b"), orphan_order("c-a")], &p, None, None);
        assert_eq!(fwd.alerts[0].detail, rev.alerts[0].detail, "order-independent");
        let grown = resolve(
            vec![orphan_order("c-a"), orphan_order("c-b"), orphan_order("c-c")],
            &p,
            None,
            None,
        );
        assert_ne!(fwd.alerts[0].detail, grown.alerts[0].detail, "a changed set moves the detail");
    }

    #[test]
    fn orphan_local_order_does_not_disturb_a_co_occurring_missing_fill() {
        // Non-interference: the sweep contributes nothing to the fill window and nothing to the
        // event stream, so a MissingFill in the same pass resolves byte-identically to a pass
        // without it. Its own alert is appended LAST (the aggregate summarizes the pass).
        let policy = ReconPolicy::hybrid();
        let without = resolve(vec![Divergence::MissingFill(ext_fill("t1"))], &policy, None, None);
        let with = resolve(
            vec![orphan_order("c-1"), Divergence::MissingFill(ext_fill("t1"))],
            &policy,
            None,
            None,
        );
        assert_eq!(with.events, without.events, "the fill's own events are untouched");
        assert_eq!(with.alerts.len(), without.alerts.len() + 1, "exactly one added alert");
        assert_eq!(
            with.alerts.last().map(|a| a.kind),
            Some(DivergenceKind::OrphanLocalOrder),
            "aggregate lands last"
        );
    }
}
