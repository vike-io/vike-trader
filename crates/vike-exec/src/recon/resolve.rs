//! Pure resolution: `Divergence`s × policy → `Recon` (events to fold + snapshot seed + alerts).
//! Synthetic orders use a reserved coid namespace (`external_coid`) so re-runs map to the same
//! order — the structural half of idempotency (the fold's trade_id dedup is the other half).
//! Unexplained residual position legs are older than the lookback window, so they anchor before
//! the earliest recovered fill and fold first (`position_events`).
//!
//! **What folds.** Under `hybrid` exactly two kinds fold anything: `MissingFill` and
//! `PositionDrift` (`mode_applies` is the per-kind authority). `MissingTerminal` auto-applies but
//! `events_for` has no arm for it, so it folds an EMPTY list and raises no alert.
//! `JournalDivergence` becomes an investigative alert before the mode is consulted. Neither orphan
//! kind folds under any policy (sections below), and a divergence naming another instance is held
//! under every policy (`mode_applies_divergence`).
//!
//! ## `generate_missing_orders`: UnknownOrder adoption is NARROW
//!
//! `resolve`'s `adopt` parameter (`Option<AdoptContext>`; `None` ⇔ `generate_missing_orders` off)
//! is threaded in from `vike-core`'s `ReconConfig::generate_missing_orders` /
//! `VIKE_RECONCILE_GENERATE_MISSING` — this crate cannot name that config (down-only layering).
//! With `None`, `UnknownOrder` falls into `events_for`'s catch-all (empty events), so under
//! `hybrid` it quarantines with nothing to one-click adopt. `Some` enables [`adoption_case`]:
//!
//! - **Live (non-terminal) or unexecuted unknown orders synthesize NOTHING that folds.** A live
//!   order's executions arrive as `MissingFill` divergences with REAL venue trade-ids while it is
//!   inside the lookback; a cumulative fill here as well would double-book the same activity under
//!   two trade-ids in one pass.
//! - **A terminal unknown order whose executions are visible in THIS pass's fill reports**
//!   ([`AdoptContext::pass_fill_order_ids`]) likewise defers entirely to the fill lane.
//! - **Only a terminal unknown order with executed qty and NO fill report in-pass** (its fills fell
//!   outside the lookback, so the fill lane can never book them) synthesizes the adoption: an
//!   external-coid `OrderAccepted` (coid-less reports only, `MissingFill` parity) plus ONE `Fill`
//!   for the report's cumulative `filled_qty` at `avg_px`, deterministic
//!   `trade_id = EXT-ORD-{venue}-{venue_order_id}`. Terminal ⇒ `filled_qty` is final, so the
//!   cumulative print is safe.
//! - **Recurring passes are a true no-op.** Once the adoption fill has folded, its trade_id is in
//!   the engine's seen set ([`AdoptContext::seen_trade_ids`]) and the divergence resolves to
//!   nothing: no re-folded events, no anomaly-counter inflation (`dropped_unknown_coid` moves once
//!   per adoption, at first fold, like `MissingFill`'s own synthesized accept), and no new alert
//!   rows (held UnknownOrder alerts carry a [`ReconAlert::dedup_key`] the runtime refreshes in
//!   place). Pinned by `already_adopted_unknown_order_is_a_true_no_op` here and the
//!   `recon_idempotency` / `recon_quarantine` integration tests.
//!
//! ⚠ Accepted residual: the adoption arm cannot see fills that folded with their REAL trade-ids in
//! an EARLIER pass and have since aged out of the lookback — a terminal order re-entering the
//! report window that way would re-book its qty once. Whether one can re-enter is a property of the
//! venue's `fetch_order_status_reports`. Every wired `ReconClient` EXCEPT Alpaca's fetches OPEN
//! orders only, so a terminal order never reappears and the residual is unreachable there (not
//! caught later). `crates/bridges/alpaca/src/recon_client.rs`'s `fetch_order_status_reports`
//! requests `status=all` with a no-op `since`, so the residual is reachable on Alpaca whenever this
//! arm is on. What remains is `PositionDrift` (Alpaca's position fetch always returns a row, a
//! synthesized flat one when the symbol is absent), which only `hybrid` auto-applies; under the
//! default `quarantine` it is held for an operator. A client that fetches order HISTORY on a venue
//! whose `fetch_position_status_reports` returns no rows (spot) would have nothing to re-converge
//! qty against at all.
//!
//! ## `OrphanLocalPosition`: quarantined under `hybrid`, folds NOTHING under any policy
//!
//! `diff`'s step-5 local-side sweep raises it when local holds a net position this pass's venue
//! report never mentions. It is deliberately classified **no-local-origin** (quarantined under
//! `hybrid`), for two independent reasons, either sufficient on its own:
//!
//! - **Nothing is safe to fold.** Flattening means synthesizing a closing `Fill`, and a `Fill`
//!   needs a PRICE. `LocalView::positions` carries the signed net qty and nothing else — no basis,
//!   no venue mark — and there is no venue report to read one from. Any price picked would book
//!   fabricated realized PnL, so the kind resolves to ZERO events under EVERY policy; the only
//!   decision left is fold-vs-SURFACE.
//! - **The evidence is an absence, and absences lie.** Most wired `ReconClient`s scope the position
//!   fetch to the mounted symbol, so "no row" routinely means "not asked about", not "flat".
//!   Auto-applying would turn an incomplete fetch into a destructive flatten — the exact outcome
//!   the root `CLAUDE.md`'s *Reconciliation engine* rollout rule exists to prevent.
//!
//! Per policy: `synthesize` folds its (empty) event list — a true no-op, nothing folds AND nothing
//! alerts, the shape [`AdoptionCase::SurfaceOnly`] takes under an auto-apply mode. A NAMED
//! residual, not an oversight: an operator who wants this blind spot surfaced runs `hybrid` or
//! `quarantine`, which each raise one operator alert and fold nothing; the per-kind preset in
//! [`super::types::ReconPolicy::hybrid`] and the bare-`ReconMode::Hybrid` fallback in
//! [`is_local_origin`] agree (both omit it from the local-origin set).
//!
//! The alert carries `dedup_key = "position:{symbol}:{side}"`. Nothing about this divergence
//! self-heals (no events fold, so the venue keeps not reporting the row and every pass re-raises
//! it), so the runtime REFRESHES the one held row per (venue, symbol, side) in place whatever its
//! detail says — the shape [`ReconAlert::dedup_key`] exists for.
//!
//! ## `OrphanLocalOrder`: ONE aggregated dedup-keyed alert under held policies; folds NOTHING
//!
//! `diff`'s step-3 local-side order sweep raises it for every live LOCAL order whose coid this
//! pass's venue order report never mentions. It resolves the way its position twin above does,
//! with one deliberate difference in the alert's GRANULARITY.
//!
//! **Nothing folds under any policy, and nothing an operator confirm could fold either.** The only
//! action the divergence suggests is cancelling the local order, and `resolve` must never
//! synthesize one. Two independent reasons, either sufficient:
//!
//! - **A synthesized cancel would be a LIE about the venue.** Everything this module emits is a
//!   LOCAL fold — `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports` publishes the
//!   events into the engine and calls no venue. An `Event::OrderCanceled` here would terminalize an
//!   order that may still be RESTING at the venue, and once it is out of the registry there is no
//!   managed order left to cancel: the position grows behind our back. (`reconcile_reports` avoids
//!   `Command::ApplySnapshot` for the same hazard: its reap arm would terminalize every live order
//!   on the venue.)
//! - **The evidence is an absence, and this absence lies more than most.** A coid missing from an
//!   order report is equally consistent with a genuine terminal we missed, an order still IN FLIGHT
//!   to the venue (submitted, not yet in its open-order snapshot), a venue whose report does not
//!   echo `client_order_id` at all, an orders-only-empty fetch, and a broker-prefix mismatch that
//!   orphans EVERY live order at once — `crates/bridges/binance/src/family/recon.rs`'s
//!   `parse_order_row` strips that prefix precisely because the un-stripped form does exactly that.
//!
//! So `proposed_events` stays EMPTY and a `ConfirmRecon` on this alert is a pure ACKNOWLEDGEMENT:
//! `crates/vike-core/src/runtime/reconcile.rs`'s `confirm_recon` folds `proposed_events` and
//! re-registers `recover_orders`, both empty by construction, so the confirm cannot cancel,
//! re-register or fold anything. `no_policy_ever_synthesizes_a_cancel_for_an_orphan_local_order`
//! (`crates/vike-exec/tests/recon/recon_policy_pin.rs`) pins that half.
//!
//! Per policy: `synthesize` folds its (empty) event list — a true no-op, nothing folds AND nothing
//! alerts; the same NAMED residual `OrphanLocalPosition` carries (an operator who chose "fold
//! everything, no operator in front of it" opted out of alerts). `hybrid` and `quarantine` hold it,
//! hence alert. The kind is no-local-origin in both places that classify
//! ([`super::types::ReconPolicy::hybrid`]'s preset and [`is_local_origin`], which must stay in
//! agreement) for the same reason as its position twin: what is local-origin here is the ORDER,
//! while the DIVERGENCE is the venue's SILENCE about it, and silence is not our own activity
//! confirmed by the venue. The classification changes visibility only, never an event.
//!
//! ### Why ONE aggregated alert and not one per coid
//!
//! The KEY SPACE decides it. `crates/vike-core/src/runtime/reconcile.rs`'s `confirm_recon` is the
//! ONLY thing that removes a held alert row (a divergence that heals on its own leaves its row
//! behind forever), and `crates/vike-core/src/runtime/publish.rs`'s `recon_block` CLONES every held
//! row into every published `CoreSnapshot`. A position key space is (symbols × sides) — tiny — and
//! that divergence genuinely never heals, which makes a permanent row per key honest. A coid key
//! space is one entry per order ever placed, and this divergence heals ROUTINELY (every in-flight
//! case above resolves by the next pass), so a per-coid key would accrue a stale held row per order
//! on a churning account and grow the per-publish clone with it.
//!
//! The pass therefore emits ONE alert carrying the COUNT plus a bounded, SORTED sample of coids
//! (`ORPHAN_SAMPLE`), dedup-keyed on the constant [`ORPHAN_LOCAL_ORDER_KEY`] so the runtime holds
//! exactly one row per (venue, kind) and refreshes it in place. Sorting is load-bearing for that
//! refresh: a STEADY orphan set renders a byte-identical detail, which the runtime's change check
//! turns into a pure no-op (no ring note, no row churn). Being aggregated, the alert is appended
//! AFTER every per-divergence alert of the pass rather than in divergence order.
//!
//! ⚠ Two residuals, named rather than hidden. **A healed orphan leaves a stale row** — the alert
//! API has no retraction, so the row keeps its count until an operator acknowledges it; bounded at
//! one row per venue with a side-effect-free confirm, which is what makes it acceptable. **The
//! alert names a COUNT and a sample, not every coid** — its job is to say THAT the venue's open
//! orders and the local registry disagree, and by how much; the full list is a read of both.

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
/// CONSTANT, not a per-order key, so the runtime holds one row per (venue, kind) and refreshes it
/// in place (the module doc argues why the coid is the wrong key). `pub` so the pin test asserts
/// it by symbol rather than re-spelling the literal.
pub const ORPHAN_LOCAL_ORDER_KEY: &str = "orders:orphan-local";

/// How many orphaned coids the aggregated alert's detail names before it truncates to `(+N more)`;
/// the COUNT is always exact. A cap because the detail is cloned into every published
/// `CoreSnapshot` (`crates/vike-core/src/runtime/publish.rs`'s `recon_block`), and a market maker's
/// whole resting book can orphan at once when a venue's report does not echo our client ids.
const ORPHAN_SAMPLE: usize = 8;

/// Per-pass context for `generate_missing_orders` UnknownOrder adoption (module doc). `Some` ⇔ the
/// flag is on; both call sites (`run_pass`, `vike-core`'s `reconcile_reports`) already hold the two
/// sets, so `resolve` stays pure — no I/O, no engine reach-through.
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
    // blank — there is no error arm to invent a policy for.
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
/// per-KIND definition the phase-2 fold decision and the phase-1 `fill_window` accumulation share,
/// so the window can never count qty that did not fold (mode consistency). Phase 2 consults it
/// through [`mode_applies_divergence`], which for every policy that does not opt in
/// (`ReconPolicy::hold_external_instances` false) is definitionally this function.
///
/// **`pub` deliberately**: callers that need to STATE what a policy will do (operator warnings,
/// docs, tests) must consult this rather than re-derive it from `mode_for`, which is only half the
/// rule — a bare `ReconMode::Hybrid` still has to go through `is_local_origin`.
///
/// ⚠ Auto-apply is NOT the same as "does something". `MissingTerminal` auto-applies under `hybrid`
/// and resolves to an EMPTY event list — see `events_for`'s catch-all and
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
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
/// divergence rather than its kind.
///
/// For every policy that does not opt in (`ReconPolicy::hold_external_instances` `false` — the
/// three flat policies, the `hybrid()` preset, and any hand-built bare `ReconMode::Hybrid`) this
/// is DEFINITIONALLY `mode_applies(policy, d.kind())`: the refinement term short-circuits, so
/// their byte-identity is by construction, not by test alone. With the opt-in
/// ([`ReconPolicy::external_quarantine`] is the only constructor that sets it), an instance whose
/// OWN evidence reads [`DivergenceOrigin::External`] ([`Divergence::origin`]) is HELD even where
/// its kind auto-applies — today exactly one sub-case: the coid-less `MissingFill` (a foreign
/// order's fill inside the lookback; a coid-LINKED `MissingFill` is our own order's fill and
/// keeps folding). The generic held arm computes `events` BEFORE consulting the mode, so the held
/// alert proposes exactly what `hybrid` would have auto-folded and `confirm_recon` folds it
/// verbatim — pinned by
/// `external_quarantine_holds_a_coidless_missing_fill_and_a_claim_folds_exactly_hybrids_events`
/// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`).
///
/// [`mode_applies`] stays the per-KIND authority callers use to STATE a policy's fold set;
/// `vike_tradehub::reconcile_config`'s `auto_applied_kinds` computes from it and renders the
/// per-divergence qualifier by ASKING this function with a coid-less probe.
///
/// ⚠ Phase-1 `fill_window` accumulation deliberately does NOT consult this — a held coid-less
/// `MissingFill` still counts, as every held `MissingFill` does under plain `quarantine`. That
/// keeps confirm-everything equal to `hybrid`: every CONSTRUCTIBLE policy that holds a fill also
/// holds `PositionDrift` (`external_quarantine` computes that row from the same origin tag), so
/// the held drift's proposed legs net the held fill's qty. A hand-built policy setting
/// `hold_external_instances` while leaving `PositionDrift` auto-applied would re-open that
/// netting hazard — a latent trap of the same family as the bare-Hybrid `PositionDrift`
/// disagreement, unreachable from `VIKE_RECONCILE_POLICY`.
///
/// ⚠ **One refinement is NOT behind the opt-in, and cannot be**: a divergence whose evidence
/// carries ANOTHER INSTANCE'S origin claim ([`Divergence::is_foreign_instance`]) is held under
/// EVERY policy. That is the cross-machine half of the duplicate-instance interlock
/// (`crates/vike-journal/src/lock.rs` holds the same-machine half and cannot reach past one
/// filesystem): two deployments sharing a venue API key each take their own journal lock and each
/// believe they are alone, and under `hybrid` a coid-LINKED `MissingFill` folds — so a sibling's
/// fill would be booked into THIS instance's position and realized PnL, silently, at the sibling's
/// price. Gating it behind `external-quarantine` would leave the default policy holding the bag.
///
/// It is also the ONE exception to the phase-1 paragraph above: it holds under `hybrid`, which
/// still auto-applies `PositionDrift`, so the pairing that rule rests on does not hold, and
/// counting a sibling's qty would net it out of a correction that WILL fold. A foreign-instance
/// divergence is therefore SKIPPED by the phase-1 accumulator outright, not merely held.
///
/// It is inert for everyone who has not configured an origin, BY CONSTRUCTION:
/// [`ReconPolicy::local_instance_origin`] is `None` in every constructor, and `coid_is_foreign`
/// (`crates/vike-model/src/instance_origin.rs`) answers `false` for a `None` local origin AND for
/// any id carrying no claim. It is one-way too: it can only turn a fold into a HOLD, never the
/// reverse, so the worst a mis-read does is put a divergence in front of an operator.
pub fn mode_applies_divergence(policy: &ReconPolicy, d: &Divergence) -> bool {
    if d.is_foreign_instance(policy.local_instance_origin.as_ref()) {
        return false;
    }
    mode_applies(policy, d.kind())
        && !(policy.hold_external_instances && d.origin() == DivergenceOrigin::External)
}

/// Per-(venue, symbol) summary of THIS pass's MissingFill synthesis: the net signed qty those
/// fills contribute, plus the earliest fill ts (the observable head of the venue's lookback
/// window). Keyed by the raw venue/symbol strings: netting happens before PIT rounding.
type FillWindowMap = HashMap<(String, String), (f64, i64)>;

pub fn resolve(
    divergences: Vec<Divergence>,
    policy: &ReconPolicy,
    pit: Option<PitFn>,
    adopt: Option<AdoptContext>,
) -> Recon {
    // Phase 1: accumulate the net position movement (and window head) already covered by same-pass
    // MissingFill synthesis, BEFORE resolving anything. `events_for`'s MissingFill arm always
    // synthesizes `position_side: Both`, so this nets cleanly only against a position report that
    // is ALSO one-way (`position_events`). An adopted `UnknownOrder`'s cumulative fill feeds the
    // SAME window — but ONLY when it will fold THIS pass (`AdoptionCase::Adopt` AND
    // `mode_applies`): a held adoption folds nothing now, so counting its qty would net a
    // co-occurring PositionDrift heal against events that never happened (mode consistency).
    let unknown_applies = mode_applies(policy, DivergenceKind::UnknownOrder);
    let local_origin = policy.local_instance_origin.as_ref();
    let mut fill_window: FillWindowMap = HashMap::new();
    for d in &divergences {
        // ⚠ A divergence naming ANOTHER INSTANCE'S order is excluded from the window entirely, not
        // merely held (`mode_applies_divergence`'s doc): the sibling's fill is not ours to book,
        // and the drift is judged on the raw venue-vs-local difference.
        if d.is_foreign_instance(local_origin) {
            continue;
        }
        match d {
            Divergence::MissingFill(f) => {
                // Deliberately UNCONDITIONAL: a HELD MissingFill still counts, so the held drift's
                // proposed legs net it and confirm-everything reproduces `hybrid` exactly (see
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
    // collect separately and are spliced in FRONT at the end, so the fold books them first.
    let mut recon = Recon::default();
    let mut pre_events: Vec<Event> = Vec::new();
    // Held `OrphanLocalOrder`s of this pass, aggregated into ONE alert after the loop (module doc).
    let mut orphan_orders: Vec<String> = Vec::new();
    for d in divergences {
        // A JournalDivergence (the journal has something live local state lost — a persistence /
        // restore bug) is NEVER auto-applied: it ALWAYS surfaces as an investigative `ReconAlert`,
        // whatever the policy. The FILL-loss case carries nothing to fold (the fill already exists
        // at the venue). The ORDER-loss case (`recover_order` is `Some`) carries the venue order to
        // RE-REGISTER, held in `recover_orders` for an operator `ConfirmRecon` to apply via the
        // insert-only registry seed (NOT `proposed_events`, which the event fold would drop for an
        // unknown coid).
        if let Divergence::JournalDivergence { detail, recover_order } = &d {
            recon.alerts.push(ReconAlert {
                kind: DivergenceKind::JournalDivergence,
                detail: detail.clone(),
                proposed_events: Vec::new(),
                recover_orders: recover_order.iter().map(|o| (**o).clone()).collect(),
                dedup_key: None,
                // UN-KEYED, and the detail is built elsewhere — its churn is not ours to judge
                // here, so `None` (the identity is the detail).
                identity_detail: None,
            });
            continue;
        }
        // BalanceDrift synthesizes ONE correcting `Event::AccountState` — the fold path a live
        // venue AccountState takes (`apply_account_state`). Under a held policy it does NOT fold:
        // it rides the alert's `proposed_events` for `confirm_recon` to apply on operator approval,
        // so a SURPRISE cash move (withdrawal/liquidation/deposit) is never auto-absorbed. A
        // `balance:{asset}` dedup_key refreshes one row per (venue, asset) as the amounts move.
        if let Divergence::BalanceDrift { venue, asset, local: expected, venue_bal, ts } = &d {
            let account_state = Event::AccountState(vike_model::events::AccountState {
                venue: ustr::ustr(venue),
                balances: vec![(asset.clone(), *venue_bal)],
                ts: *ts,
                // No key: the recon lane never routes through `vike_core`'s `route_event`.
                // `CoreThread::reconcile_reports` resolves the engine from
                // `ReconcileReports::route` and `confirm_recon` from the held alert's own stored
                // key, then publishes DIRECTLY to that index; a key here would be a second, weaker
                // copy of a routing decision already made.
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
        // OrphanLocalPosition is resolved wholly here: it can never fold ANYTHING (no price to
        // close at — module doc), so `mode_applies` only chooses no-op-vs-surface, and its alert
        // needs a real detail plus a dedup_key (nothing heals it). Held modes (quarantine, and
        // hybrid, which classifies it no-local-origin) surface one alert per (venue, symbol, side).
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
                    // Deliberately empty: an operator confirm must not flatten at a guessed price
                    // either. Investigate-only, like the fill-loss JournalDivergence above.
                    proposed_events: Vec::new(),
                    recover_orders: Vec::new(),
                    dedup_key: Some(format!("position:{symbol}:{position_side}")),
                    identity_detail: None,
                });
            }
            continue;
        }
        // OrphanLocalOrder is COLLECTED here and resolved after the loop: it can never fold
        // anything (a synthesized cancel would terminalize an order that may still be resting at
        // the venue — module doc), so `mode_applies` only chooses no-op-vs-surface, and held modes
        // contribute to ONE dedup-keyed row per venue rather than one per coid.
        if let Divergence::OrphanLocalOrder { client_order_id } = &d {
            if !mode_applies(policy, DivergenceKind::OrphanLocalOrder) {
                orphan_orders.push(client_order_id.clone());
            }
            continue;
        }
        // UnknownOrder under an active AdoptContext is resolved wholly here: its three cases need
        // distinct fold-vs-display shapes and a dedup-keyed alert. With `adopt` None it falls
        // through to the generic path, where `events_for`'s catch-all yields empty events.
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
                    // Nothing safe to fold (the fill lane owns any executions). Synthesize never
                    // alerts; held modes surface an alert whose proposed events carry at most the
                    // decorative accept — never a fill that could double-book at confirm time.
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
            // A held divergence keeps its events whole in the alert; the operator applies them at
            // confirm time, after everything auto-folded.
            //
            // This is also the CLAIM path for an EXTERNAL-origin divergence held by
            // `ReconPolicy::external_quarantine`, and for the per-divergence hold
            // (`mode_applies_divergence`): `events` was computed BEFORE the mode was consulted, so
            // a `Command::ConfirmRecon` (`crates/vike-core/src/runtime/reconcile.rs`'s
            // `confirm_recon`) folds `proposed_events` verbatim — byte-identical to `hybrid`'s
            // blind fold, with an operator in front of it. Pinned in
            // `crates/vike-exec/tests/recon/recon_policy_pin.rs` by
            // `external_quarantine_holds_position_drift_and_a_claim_folds_exactly_hybrids_events`
            // and its coid-less sibling
            // `external_quarantine_holds_a_coidless_missing_fill_and_a_claim_folds_exactly_hybrids_events`.
            recon.alerts.push(ReconAlert {
                kind: d.kind(),
                // A foreign-instance divergence says so IN THE DETAIL ("UnknownOrder" and "another
                // deployment of yours is on this account" call for different operator actions).
                // `identity_detail` is deliberately NOT annotated: it is the dedup IDENTITY
                // (`vike_core`'s `HeldId::new`), and a note must not make a divergence a new row.
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
    // The aggregated orphan-order alert lands LAST (module doc): it summarizes the pass.
    if !orphan_orders.is_empty() {
        recon.alerts.push(orphan_local_order_alert(orphan_orders));
    }
    if !pre_events.is_empty() {
        pre_events.extend(std::mem::take(&mut recon.events));
        recon.events = pre_events;
    }
    recon
}

/// The ONE aggregated `OrphanLocalOrder` alert of a pass: event-free by construction, keyed on the
/// constant [`ORPHAN_LOCAL_ORDER_KEY`], and SORTED so a steady orphan set renders a byte-identical
/// detail, which the runtime's held-alert refresh turns into a pure no-op (module doc).
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
        // Deliberately empty: an operator confirm must not be able to fold a cancel either.
        // Investigate-only, like `OrphanLocalPosition`'s and the fill-loss `JournalDivergence`'s.
        proposed_events: Vec::new(),
        recover_orders: Vec::new(),
        dedup_key: Some(ORPHAN_LOCAL_ORDER_KEY.to_string()),
        identity_detail: None,
    }
}

/// The bare-`ReconMode::Hybrid` fallback classification (`ReconPolicy::hybrid()`'s per-kind preset
/// is the other half; the two MUST agree).
///
/// ⚠ NEITHER orphan kind is in this set: the ORDER is local-origin, the DIVERGENCE is the venue's
/// silence about it, and an absence is not our activity confirmed by the venue (module doc).
/// Neither folds any event under any policy, so this decides visibility only.
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
        // UnknownOrder lands here only with `adopt` None (with `Some`, `resolve` handles it
        // first). `MissingTerminal` lands here too and resolves to nothing under every policy — a
        // known gap whose cure is a terminal fold, not an alert (its evidence is a POSITIVE venue
        // report).
        _ => (Vec::new(), false),
    }
}

/// The decorative adoption `OrderAccepted` — minted ONLY for a coid-less (externally-placed)
/// report, mirroring the `MissingFill` arm's `client_order_id.is_none()` gate (an order WITH a
/// client id is presumed to have gone through its own accept). Decorative because
/// `ExecutionEngine::on_event` drops any lifecycle event whose coid is not in the registry (its
/// `dropped_unknown_coid` counter moves once, at first fold): not a registry adoption, just
/// parity so the operator sees an accept in `recent_events`/the journal.
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
/// `Fill` for the report's CUMULATIVE `filled_qty` at `avg_px` (`snap_pit`-rounded like
/// `synth_position_legs`). Safe because `adoption_case` guaranteed the order is terminal (the qty
/// is final), invisible to this pass's fill lane, and not yet adopted. The [`adoption_trade_id`]
/// is deterministic, so even a racing refold dedupes on the engine's `seen_trade_ids` guard.
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
/// and a `dedup_key` so the runtime REFRESHES the held row across passes — an UnknownOrder recurs
/// by design (the registry never adopts it) while its status and filled qty move, and an un-keyed
/// identity would read each move as a new divergence.
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
/// MissingFill synthesis in this pass): the unexplained residual is activity the lookback could
/// NOT see — necessarily OLDER than every recovered fill. Its legs move
/// `local_qty → report.qty − fill_delta` (the position the recovered fills then build on), anchor
/// one tick BEFORE the earliest recovered fill, and return `pre = true` so `resolve` folds them
/// FIRST: a pre-window close realizes PnL at its true flat point and the recovered fills open a
/// fresh basis, instead of blending into a stale position at snapshot time. This also nets the
/// within-pass double-count (`local + delta → venue` and `local → venue − delta` are the same
/// residual qty).
///
/// **Without** (no same-pass recovered fills, or a hedge-mode `Long`/`Short` report): legs move
/// `local_qty → report.qty` at the report's own ts, folded in place. Synthesized MissingFill legs
/// always carry `position_side: Both` (one order-history fill has no hedge-mode bucket), so they
/// net cleanly only against a one-way report; a hedge-mode report's two server-side buckets cannot
/// be mapped onto without venue context, so netting is skipped — an accepted limitation (hedge mode
/// is rarer): the residual leg can still double-count against an unrelated same-symbol `Both` fill
/// history in a mixed-mode setup.
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
            // ⚠ KNOWN and deliberately UNCHANGED: `ts` makes this id unstable across a replay of
            // the same drift, which a synthesized dedup key must not be (contrast
            // `adoption_trade_id`, a pure function of venue+order id). The SAME `ts` is in this
            // leg's paired `client_order_id` on the next line, so correcting it changes a live
            // journal key and its coid together — a behaviour change owing its own PR and demo
            // validation.
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

/// Snap px/qty to the PIT instrument grid in effect at `ts`, when a PIT source is provided, with
/// the pinned rounding primitive `vike_model::round_to` (half-to-even onto
/// `tick_size`/`step_size`) that backtest fills snap to — `SymbolProperties` is plain bounds data
/// with no rounding of its own (`crates/vike-model/src/instrument/mod.rs`). A `0.0` tick/step is
/// a no-op, matching `round_to`'s own guard.
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
            vike_model::round_to(px, Some(props.tick_size)),
            vike_model::round_to(qty, Some(props.step_size)),
        ),
        None => (px, qty),
    }
}

#[path = "resolve_tests.rs"]
#[cfg(test)]
mod resolve_tests;
