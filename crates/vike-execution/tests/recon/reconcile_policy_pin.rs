//! What each `ReconcilePolicy` ACTUALLY does with each `ReconcileDivergenceKind`, pinned because a hand-written
//! fold list rots: the claim below was stated in nine in-tree documents, was false, and drove a
//! rollout policy.
//!
//! ## The false claim: under `hybrid` an `OrphanLocalOrder` "auto-cancels every pass"
//!
//! 1. `vike_execution::recon::diff` raises `ReconcileDivergence::OrphanLocalOrder { order_client_id }`: an order client id
//!    and nothing else, because the evidence IS an absence.
//! 2. `resolve`'s `events_for` has arms for `MissingFill` / `PositionDrift` /
//!    `PositionOnlyExternal` only; `OrphanLocalOrder` falls into the `_ => (Vec::new(), false)`
//!    catch-all: **zero events**.
//! 3. The kind is no-local-origin (`ReconcilePolicy::hybrid`'s preset and `resolve::is_local_origin`
//!    agree), so `hybrid` and `quarantine` surface ONE aggregated, dedup-keyed, EVENT-FREE alert
//!    per pass. Folding the empty list instead would be detection with no outcome: a kind that
//!    reads as coverage and tells nobody anything.
//! 4. `resolve` constructs no `Event::OrderCanceled` anywhere, under any policy, for any kind.
//!
//! So no policy folds an event for this kind, no alert proposes one, and an operator confirm is a
//! pure acknowledgement. `vike_execution::recon::resolve`'s module doc carries why an alert (never an
//! auto-cancel) is right, and why it is aggregated rather than keyed per order client id.
//!
//! The one place that terminalizes an order the venue stopped reporting is
//! `Engine::apply_snapshot`'s reap (`crates/vike-execution/tests/recon/reconcile_reap.rs`), and
//! it folds a **local** `OrderCanceled` through the FSM; it never calls the venue. The live
//! reconcile path cannot reach it: `vike_tradehub_core::ReconcileManager`'s only side effect is enqueuing
//! `Command::ReconcileReports`, and `CoreThread::reconcile_reports` deliberately never calls
//! `apply_snapshot` *because* its reap "would cancel EVERY live order on this venue as a side
//! effect". No runtime path calls `apply_snapshot` at all: the `Command::ApplySnapshot` arm and its
//! `CoreHandle::spawn_periodic_reconcile` driver were deleted as dead code (unit A2).
//!
//! ## The kind that DOES auto-apply under `hybrid`
//!
//! `PositionDrift` (`hybrid_auto_applies_position_drift`), deliberately: the venue's own position
//! row is positive evidence, and it is the only thing that re-converges qty after a fill older
//! than the lookback. `every_divergence_kind_has_a_pinned_hybrid_mode`'s exhaustive match makes a
//! newly-added kind a COMPILE error here rather than a silent auto-fold.

use std::collections::HashSet;

use vike_execution::recon::{
    ORPHAN_LOCAL_ORDER_KEY, ReconcileAdoptContext, ReconcileDivergence, ReconcileDivergenceKind,
    ReconcileDivergenceOrigin, ReconcileMode, ReconcilePolicy, mode_applies,
    mode_applies_divergence, resolve,
};
use vike_model::events::{Event, LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

fn orphan(order_client_id: &str) -> ReconcileDivergence {
    ReconcileDivergence::OrphanLocalOrder { order_client_id: order_client_id.to_string() }
}

fn drift(local_quantity: f64, venue_qty: f64) -> ReconcileDivergence {
    ReconcileDivergence::PositionDrift {
        report: PositionStatusReport {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            position_side: PositionSide::Both,
            qty: venue_qty,
            average_price: 0.55,
            timestamp: 1_000,
            margin_mode: MarginMode::Cross,
            isolated_margin: None,
            delta: None,
        },
        local_quantity,
    }
}

fn quarantine() -> ReconcilePolicy {
    ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() }
}

/// A bare `ReconcileMode::Hybrid` default — NOT what `VIKE_RECONCILE_POLICY=hybrid` builds (that is
/// `ReconcilePolicy::hybrid()`), but the fallback `resolve::is_local_origin` serves.
fn bare_hybrid() -> ReconcilePolicy {
    ReconcilePolicy { default: ReconcileMode::Hybrid, ..Default::default() }
}

/// A `MissingFill` in either linkage shape: `Some` order client id = our own order's fill, materialized late;
/// `None` = the fill names no local order (a foreign order's fill inside the lookback), the one
/// divergence whose EVIDENCE refines its kind's origin (`ReconcileDivergence::origin`).
fn missing_fill(order_client_id: Option<&str>) -> ReconcileDivergence {
    ReconcileDivergence::MissingFill(FillReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        trade_id: TradeId::new("T-77").expect("non-empty"),
        venue_order_id: "V-9".into(),
        order_client_id: order_client_id.map(str::to_string),
        side: 1,
        last_qty: 5.0,
        last_price: 0.55,
        commission: 0.01,
        commission_asset: "USDC".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1_000,
    })
}

// --- OrphanLocalOrder: the falsified claim, and the silence behind it ---

/// THE pin. Under `hybrid`, `OrphanLocalOrder` folds NOTHING and surfaces exactly one event-free
/// alert, dedup-keyed on `ORPHAN_LOCAL_ORDER_KEY` so a recurring pass refreshes that row rather
/// than appending.
#[test]
fn hybrid_surfaces_an_orphan_local_order_and_still_folds_nothing() {
    let r = resolve(vec![orphan("paper-1")], &ReconcilePolicy::hybrid(), None, None);
    assert!(r.events.is_empty(), "hybrid folded events for OrphanLocalOrder: {:?}", r.events);
    assert_eq!(r.alerts.len(), 1, "hybrid must SURFACE the divergence: {:?}", r.alerts);
    assert_eq!(r.alerts[0].kind, ReconcileDivergenceKind::OrphanLocalOrder);
    assert_eq!(r.alerts[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(r.alerts[0].detail.contains("paper-1"), "names the order: {}", r.alerts[0].detail);
}

/// The aggregation pin, which makes surfacing this kind affordable: the held-alert store never
/// self-clears (`crates/vike-core/src/runtime/reconcile.rs`'s `confirm_reconcile` is its only remover)
/// and `crates/vike-core/src/runtime/publish.rs`'s `reconcile_block` clones every held row into every
/// published snapshot, while an orphan heals routinely and its natural key (the order client id) is unbounded.
/// So a whole book orphaning at once (a venue report echoing no client ids) costs ONE row.
#[test]
fn a_whole_book_of_orphans_costs_exactly_one_alert_row() {
    let d: Vec<ReconcileDivergence> = (0..50).map(|i| orphan(&format!("mm-{i:03}"))).collect();
    for (name, policy) in [("hybrid", ReconcilePolicy::hybrid()), ("quarantine", quarantine())] {
        let r = resolve(d.clone(), &policy, None, None);
        assert_eq!(r.alerts.len(), 1, "{name} raised {} rows", r.alerts.len());
        assert!(r.alerts[0].detail.starts_with("50 live LOCAL order(s)"), "{name}: exact count");
    }
}

/// NO policy turns an `OrphanLocalOrder` into a cancel, or into any event: the assertion the
/// false claim contradicted.
#[test]
fn no_policy_ever_synthesizes_a_cancel_for_an_orphan_local_order() {
    for (name, policy) in [
        ("hybrid", ReconcilePolicy::hybrid()),
        ("synthesize", ReconcilePolicy::default()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
    ] {
        let r = resolve(vec![orphan("paper-1")], &policy, None, None);
        assert!(
            !r.events.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
            "{name} synthesized an OrderCanceled: {:?}",
            r.events
        );
        assert!(r.events.is_empty(), "{name} folded events for OrphanLocalOrder: {:?}", r.events);
        // ...and nothing an operator could confirm INTO a cancel either.
        for a in &r.alerts {
            assert!(
                a.proposed_events.is_empty(),
                "{name} proposed events for OrphanLocalOrder: {:?}",
                a.proposed_events
            );
        }
    }
}

/// `synthesize` is the ONLY policy under which the divergence is invisible (the NAMED residual
/// `OrphanLocalPosition` shares: an operator who chose "fold everything, no operator in front of
/// it" opted out of alerts). `hybrid`, `quarantine` and the bare-`Hybrid` fallback all surface it,
/// with EMPTY `proposed_events`: visibility and nothing else.
#[test]
fn every_held_policy_surfaces_an_orphan_local_order_and_synthesize_alone_stays_silent() {
    for (name, policy) in [
        ("hybrid", ReconcilePolicy::hybrid()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
    ] {
        let held = resolve(vec![orphan("paper-1")], &policy, None, None);
        assert_eq!(held.alerts.len(), 1, "{name} surfaced nothing");
        assert_eq!(held.alerts[0].kind, ReconcileDivergenceKind::OrphanLocalOrder);
        assert!(held.alerts[0].proposed_events.is_empty(), "{name} proposed an action");
    }
    let silent = resolve(vec![orphan("p")], &ReconcilePolicy::default(), None, None);
    assert!(silent.alerts.is_empty(), "synthesize: the named residual, a true no-op");
    assert!(silent.events.is_empty());
}

// --- PositionDrift: the kind that really does auto-apply ---

/// `PositionDrift` auto-applies under `hybrid`: it folds legs that move local qty onto the venue's
/// number and books realized PnL at the venue's price. Deliberate (this file's doc).
#[test]
fn hybrid_auto_applies_position_drift() {
    let r = resolve(vec![drift(0.0, 100.0)], &ReconcilePolicy::hybrid(), None, None);
    assert!(!r.events.is_empty(), "PositionDrift folded nothing under hybrid");
    assert!(r.alerts.is_empty(), "PositionDrift was held under hybrid: {:?}", r.alerts);
    assert!(mode_applies(&ReconcilePolicy::hybrid(), ReconcileDivergenceKind::PositionDrift));
}

/// ⚠ The preset and the bare-`Hybrid` fallback DISAGREE about this one kind, pinned rather than
/// papered over: `ReconcilePolicy::hybrid()` auto-applies `PositionDrift` (a named `per_kind` row),
/// while `resolve::is_local_origin`, the classifier a bare `ReconcileMode::Hybrid` default falls back
/// to, does NOT list it, so a bare-Hybrid policy quarantines it. `VIKE_RECONCILE_POLICY` never
/// builds a bare `Hybrid` (`parse_policy` builds `ReconcilePolicy::hybrid()`), so this is a latent trap
/// for a hand-written `ReconcilePolicy { default: Hybrid, .. }`, not a live divergence.
/// `PositionDrift` is venue-reported, so listing it in `is_local_origin` would make that name lie;
/// the preset is where the auto-apply decision belongs.
#[test]
fn bare_hybrid_and_the_preset_disagree_about_position_drift() {
    assert!(mode_applies(&ReconcilePolicy::hybrid(), ReconcileDivergenceKind::PositionDrift));
    assert!(!mode_applies(&bare_hybrid(), ReconcileDivergenceKind::PositionDrift));

    // The TRUE half of `is_local_origin` (a predicate returning `false` everywhere must fail
    // here): the only place its agreement with the preset is asserted rather than described.
    assert!(mode_applies(&bare_hybrid(), ReconcileDivergenceKind::MissingFill));
    assert!(mode_applies(&bare_hybrid(), ReconcileDivergenceKind::MissingTerminal));

    let held = resolve(vec![drift(0.0, 100.0)], &bare_hybrid(), None, None);
    assert_eq!(held.alerts.len(), 1, "bare Hybrid should hold PositionDrift");
    assert_eq!(held.alerts[0].kind, ReconcileDivergenceKind::PositionDrift);
    // The held alert carries the SAME legs the preset would have folded — an operator confirm
    // applies exactly what `hybrid` does automatically.
    let auto = resolve(vec![drift(0.0, 100.0)], &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(held.alerts[0].proposed_events, auto.events);
}

// --- The roster gate ---

/// The expected `hybrid` mode of every kind. EXHAUSTIVE match, no `_` arm: adding a
/// `ReconcileDivergenceKind` fails to COMPILE here until somebody states whether the default policy folds it
/// without an operator.
fn expected_hybrid_mode(kind: ReconcileDivergenceKind) -> ReconcileMode {
    match kind {
        // Local-origin: our own activity, confirmed by the venue → fold.
        ReconcileDivergenceKind::MissingFill => ReconcileMode::Synthesize,
        // Auto-applies and resolves to nothing: the mode is inert (a known residual, left for its
        // own change).
        ReconcileDivergenceKind::MissingTerminal => ReconcileMode::Synthesize,
        // Venue-reported position row: positive evidence, and the only re-convergence path for a
        // fill older than the lookback.
        ReconcileDivergenceKind::PositionDrift => ReconcileMode::Synthesize,
        // No local origin → hold for an operator. (`OrphanLocalOrder`: the ORDER is ours; the
        // DIVERGENCE is the venue's silence about it.)
        ReconcileDivergenceKind::OrphanLocalOrder => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::UnknownOrder => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::PositionOnlyExternal => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::OrphanLocalPosition => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::BalanceDrift => ReconcileMode::Quarantine,
        // Never auto-applied regardless of mode — `resolve` alerts on it before the policy is
        // consulted ("no auto-apply of a persistence bug").
        ReconcileDivergenceKind::JournalDivergence => ReconcileMode::Synthesize,
        // No local origin (the venue's terms cannot be shown to be an amend we sent) → hold. Its row
        // rides beside the map (`ReconcilePolicy::order_terms`).
        ReconcileDivergenceKind::OrderTermsDrift => ReconcileMode::Quarantine,
    }
}

/// Whether `p` NAMES `k`'s row: a `per_kind` entry, or for `OrderTermsDrift` the row kept beside the
/// map (`ReconcilePolicy::order_terms`, whose doc says why it is not a map key).
fn names_row(p: &ReconcilePolicy, k: ReconcileDivergenceKind) -> bool {
    match k {
        ReconcileDivergenceKind::OrderTermsDrift => p.order_terms.is_some(),
        _ => p.per_kind.contains_key(&k),
    }
}

/// `expected_hybrid_mode`'s exhaustive match forces a new kind to be classified;
/// `ReconcileDivergenceKind::ALL` is what makes the assertion iterate, and
/// `crates/vike-execution/src/recon/types.rs`'s `all_is_every_divergence_kind_in_declaration_order`
/// holds it complete.
#[test]
fn every_divergence_kind_has_a_pinned_hybrid_mode() {
    // Bump together with the exhaustive match above.
    assert_eq!(
        ReconcileDivergenceKind::ALL.len(),
        10,
        "a ReconcileDivergenceKind was added or removed"
    );
    let p = ReconcilePolicy::hybrid();
    for &k in ReconcileDivergenceKind::ALL {
        assert_eq!(p.mode_for(k), expected_hybrid_mode(k), "hybrid mode changed for {k:?}");
    }
}

/// Every kind is a NAMED row in the preset; none may reach its mode through `ReconcilePolicy`'s
/// `default`, because a fall-through is indistinguishable from an unclassified kind.
#[test]
fn the_hybrid_preset_classifies_every_kind_explicitly() {
    let p = ReconcilePolicy::hybrid();
    for &k in ReconcileDivergenceKind::ALL {
        assert!(
            names_row(&p, k),
            "{k:?} has no named row in ReconcilePolicy::hybrid() — it would inherit `default` silently"
        );
    }
}

/// `JournalDivergence` is the one kind whose mode is decorative: `resolve` short-circuits it into
/// an investigative alert before `mode_applies` is ever consulted.
#[test]
fn journal_divergence_is_held_under_every_policy_regardless_of_mode() {
    for (name, policy) in [
        ("hybrid", ReconcilePolicy::hybrid()),
        ("synthesize", ReconcilePolicy::default()),
        ("quarantine", quarantine()),
    ] {
        let d = vec![ReconcileDivergence::JournalDivergence {
            detail: "lost fill".into(),
            recover_order: None,
        }];
        let r = resolve(d, &policy, None, None);
        assert_eq!(r.alerts.len(), 1, "{name} did not hold JournalDivergence");
        assert!(r.events.is_empty(), "{name} folded a JournalDivergence");
    }
}

// --- The EXTERNAL origin dimension + `external-quarantine` (split-plane Pattern A) ---

/// The expected [`ReconcileDivergenceOrigin`] of every kind, forced like `expected_hybrid_mode`: an
/// EXHAUSTIVE match, so a new `ReconcileDivergenceKind` fails to COMPILE here until somebody states whose
/// activity its evidence describes.
fn expected_origin(kind: ReconcileDivergenceKind) -> ReconcileDivergenceOrigin {
    match kind {
        // Our own activity, venue-settled; the recon pass merely materializes it locally.
        ReconcileDivergenceKind::MissingFill => {
            ReconcileDivergenceOrigin::ReconciliationMaterialized
        }
        ReconcileDivergenceKind::MissingTerminal => {
            ReconcileDivergenceOrigin::ReconciliationMaterialized
        }
        // Positive venue evidence nothing local explains. (`PositionDrift`: the evidence cannot
        // be SHOWN to be ours; the variant's doc carries why that reads External.)
        ReconcileDivergenceKind::UnknownOrder => ReconcileDivergenceOrigin::External,
        ReconcileDivergenceKind::PositionDrift => ReconcileDivergenceOrigin::External,
        ReconcileDivergenceKind::PositionOnlyExternal => ReconcileDivergenceOrigin::External,
        ReconcileDivergenceKind::BalanceDrift => ReconcileDivergenceOrigin::External,
        // The evidence is something missing; these fold nothing under any policy, so the origin
        // is inert for them (classified for exhaustiveness).
        ReconcileDivergenceKind::OrphanLocalOrder => ReconcileDivergenceOrigin::Absence,
        ReconcileDivergenceKind::OrphanLocalPosition => ReconcileDivergenceOrigin::Absence,
        ReconcileDivergenceKind::JournalDivergence => ReconcileDivergenceOrigin::Absence,
        // A venue row with other terms: positive evidence that cannot be SHOWN to be our amend.
        ReconcileDivergenceKind::OrderTermsDrift => ReconcileDivergenceOrigin::External,
    }
}

#[test]
fn every_divergence_kind_has_a_pinned_origin() {
    for &k in ReconcileDivergenceKind::ALL {
        assert_eq!(k.origin(), expected_origin(k), "origin changed for {k:?}");
    }
}

/// `ReconcileDivergenceKind::origin` and `resolve`'s private `is_local_origin` (the bare-`Hybrid` fallback,
/// reachable only through `mode_applies`) are two spellings of one classification and MUST agree:
/// a bare `ReconcileMode::Hybrid` applies exactly the `ReconciliationMaterialized` kinds.
#[test]
fn origin_agrees_with_the_bare_hybrid_classifier() {
    for &k in ReconcileDivergenceKind::ALL {
        assert_eq!(
            mode_applies(&bare_hybrid(), k),
            k.origin() == ReconcileDivergenceOrigin::ReconciliationMaterialized,
            "bare Hybrid and ReconcileDivergenceKind::origin disagree about {k:?}"
        );
    }
}

/// The expected `external-quarantine` mode of every kind, exhaustive like `expected_hybrid_mode`.
fn expected_external_quarantine_mode(kind: ReconcileDivergenceKind) -> ReconcileMode {
    match kind {
        // hybrid's local-origin folds, untouched.
        ReconcileDivergenceKind::MissingFill => ReconcileMode::Synthesize,
        // Inert, exactly as under hybrid.
        ReconcileDivergenceKind::MissingTerminal => ReconcileMode::Synthesize,
        // THE delta: the one External kind hybrid folds blind is HELD for an operator claim.
        ReconcileDivergenceKind::PositionDrift => ReconcileMode::Quarantine,
        // Already held under hybrid; External keeps them held.
        ReconcileDivergenceKind::UnknownOrder => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::PositionOnlyExternal => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::BalanceDrift => ReconcileMode::Quarantine,
        // Absence-origin kinds inherit their hybrid rows verbatim.
        ReconcileDivergenceKind::OrphanLocalOrder => ReconcileMode::Quarantine,
        ReconcileDivergenceKind::OrphanLocalPosition => ReconcileMode::Quarantine,
        // Decorative, inherited from hybrid: `resolve` holds it before the mode is consulted.
        ReconcileDivergenceKind::JournalDivergence => ReconcileMode::Synthesize,
        // External, and held under hybrid already.
        ReconcileDivergenceKind::OrderTermsDrift => ReconcileMode::Quarantine,
    }
}

#[test]
fn every_divergence_kind_has_a_pinned_external_quarantine_mode() {
    let p = ReconcilePolicy::external_quarantine();
    for &k in ReconcileDivergenceKind::ALL {
        assert_eq!(
            p.mode_for(k),
            expected_external_quarantine_mode(k),
            "external-quarantine mode changed for {k:?}"
        );
    }
}

/// The hybrid preset's no-fall-through gate, for this preset: every kind is a NAMED row (inherited
/// from `hybrid()`, which `external_quarantine()` is computed over).
#[test]
fn the_external_quarantine_preset_classifies_every_kind_explicitly() {
    let p = ReconcilePolicy::external_quarantine();
    for &k in ReconcileDivergenceKind::ALL {
        assert!(
            names_row(&p, k),
            "{k:?} has no named row in ReconcilePolicy::external_quarantine()"
        );
    }
}

/// The preset is COMPUTED (`hybrid()` + `origin()`), so the only rows that may differ from hybrid
/// are the External ones — and they may differ only toward `Quarantine`. Every non-External kind
/// keeps its hybrid row verbatim, and so does the policy's `default` mode.
#[test]
fn external_quarantine_differs_from_hybrid_only_where_origin_is_external() {
    let eq = ReconcilePolicy::external_quarantine();
    for &k in ReconcileDivergenceKind::ALL {
        let expected = if k.origin() == ReconcileDivergenceOrigin::External {
            ReconcileMode::Quarantine
        } else {
            expected_hybrid_mode(k)
        };
        assert_eq!(eq.mode_for(k), expected, "{k:?}");
    }
    assert_eq!(eq.default, ReconcilePolicy::hybrid().default, "the default mode is hybrid's");
}

/// The Pattern-A pin: under `external-quarantine` a `PositionDrift` is HELD (no events, one alert
/// in the generic quarantine shape) and its `proposed_events` equal hybrid's blind fold, which
/// `confirm_reconcile` (the CLAIM path, `crates/vike-core/src/runtime/reconcile.rs`) folds verbatim on
/// approval: claiming an EXTERNAL divergence yields hybrid's outcome, with an operator in front.
#[test]
fn external_quarantine_holds_position_drift_and_a_claim_folds_exactly_hybrids_events() {
    let held =
        resolve(vec![drift(0.0, 100.0)], &ReconcilePolicy::external_quarantine(), None, None);
    assert!(held.events.is_empty(), "external-quarantine folded PositionDrift: {:?}", held.events);
    assert_eq!(held.alerts.len(), 1, "expected exactly one held alert: {:?}", held.alerts);
    let a = &held.alerts[0];
    assert_eq!(a.kind, ReconcileDivergenceKind::PositionDrift);
    assert!(a.recover_orders.is_empty());
    // The generic held path is un-keyed, as under `quarantine`: a persisting drift re-raises one
    // alert per pass (a residual named on `external_quarantine`'s doc).
    assert_eq!(a.dedup_key, None);
    assert!(!a.proposed_events.is_empty(), "the claim must have something to fold");
    let auto = resolve(vec![drift(0.0, 100.0)], &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(a.proposed_events, auto.events, "claiming folds exactly what hybrid would have");
    assert!(!mode_applies(
        &ReconcilePolicy::external_quarantine(),
        ReconcileDivergenceKind::PositionDrift
    ));
}

/// The three PRE-EXISTING policies are unchanged by the origin dimension. Policy level: the two
/// flat policies carry NO per-kind rows and one uniform mode each, and hybrid's rows equal its
/// pinned table. Resolve level: `hybrid` and `synthesize` fold the drift identically, `quarantine`
/// holds it with hybrid's events proposed.
#[test]
fn the_three_existing_policies_are_byte_identical_under_the_origin_dimension() {
    let synth = ReconcilePolicy::default();
    let quar = quarantine();
    assert!(synth.per_kind.is_empty(), "synthesize gained a per-kind row");
    assert!(quar.per_kind.is_empty(), "quarantine gained a per-kind row");
    // The refinement flag is external-quarantine's alone: anywhere else it would refine folds
    // nobody opted in to.
    assert!(!synth.hold_external_instances, "synthesize opted in to the refinement");
    assert!(!quar.hold_external_instances, "quarantine opted in to the refinement");
    let hy = ReconcilePolicy::hybrid();
    assert!(!hy.hold_external_instances, "hybrid opted in to the refinement");
    for &k in ReconcileDivergenceKind::ALL {
        assert_eq!(synth.mode_for(k), ReconcileMode::Synthesize);
        assert_eq!(quar.mode_for(k), ReconcileMode::Quarantine);
        assert_eq!(hy.mode_for(k), expected_hybrid_mode(k));
    }
    let auto_h = resolve(vec![drift(0.0, 100.0)], &hy, None, None);
    let auto_s = resolve(vec![drift(0.0, 100.0)], &synth, None, None);
    assert!(!auto_h.events.is_empty() && auto_h.alerts.is_empty());
    assert_eq!(auto_h.events, auto_s.events, "hybrid and synthesize fold the same drift legs");
    let held_q = resolve(vec![drift(0.0, 100.0)], &quar, None, None);
    assert!(held_q.events.is_empty());
    assert_eq!(held_q.alerts.len(), 1);
    assert_eq!(held_q.alerts[0].proposed_events, auto_h.events);
}

// --- The per-DIVERGENCE refinement: an order-client-id-less MissingFill under external-quarantine ---

/// One instance of every divergence shape but `MissingFill` (which `missing_fill` builds in both
/// linkage shapes): the roster for asserting the refinement is exactly one sub-case wide.
fn every_other_shape() -> Vec<ReconcileDivergence> {
    let order = OrderStatusReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        venue_order_id: "V-9".into(),
        order_client_id: Some("c-1".into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 5.0,
        filled_qty: 5.0,
        average_price: 0.55,
        status: vike_model::OrderStatus::Filled,
        timestamp: 1_000,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    let report = PositionStatusReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        position_side: PositionSide::Both,
        qty: 100.0,
        average_price: 0.55,
        timestamp: 1_000,
        margin_mode: MarginMode::Cross,
        isolated_margin: None,
        delta: None,
    };
    vec![
        ReconcileDivergence::MissingTerminal { order: order.clone() },
        ReconcileDivergence::OrphanLocalOrder { order_client_id: "c-1".into() },
        ReconcileDivergence::UnknownOrder(order),
        ReconcileDivergence::PositionDrift { report: report.clone(), local_quantity: 0.0 },
        ReconcileDivergence::PositionOnlyExternal(report),
        ReconcileDivergence::OrphanLocalPosition {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            position_side: PositionSide::Both,
            local_quantity: 5.0,
        },
        ReconcileDivergence::BalanceDrift {
            venue: "polymarket".into(),
            asset: "USDC".into(),
            local: 100.0,
            venue_balance: 50.0,
            timestamp: 1_000,
        },
        ReconcileDivergence::JournalDivergence { detail: "lost fill".into(), recover_order: None },
        ReconcileDivergence::OrderTermsDrift {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            order_client_id: "c-1".into(),
            price: Some((0.55, 0.56)),
            trigger_price: None,
            qty: None,
            timestamp: 1_000,
        },
    ]
}

/// `ReconcileDivergence::origin` refines its kind's origin in EXACTLY one sub-case: a `MissingFill` whose
/// `FillReport` echoes no `order_client_id` names no local order, so it reads External although
/// the KIND is `ReconciliationMaterialized` (its dominant case is our own missed fill). Every other
/// shape, the order-client-id-LINKED `MissingFill` included, answers with its kind's origin.
#[test]
fn an_order_client_id_less_missing_fill_reads_external_and_every_other_shape_keeps_its_kind_origin()
{
    assert_eq!(missing_fill(None).origin(), ReconcileDivergenceOrigin::External);
    assert_eq!(
        missing_fill(None).kind().origin(),
        ReconcileDivergenceOrigin::ReconciliationMaterialized,
        "the KIND tag itself must not move — the refinement is per-instance"
    );
    assert_eq!(
        missing_fill(Some("c-1")).origin(),
        ReconcileDivergenceOrigin::ReconciliationMaterialized
    );
    for d in every_other_shape() {
        assert_eq!(d.origin(), d.kind().origin(), "{:?} refined unexpectedly", d.kind());
    }
}

/// Under `external-quarantine` an order-client-id-less `MissingFill` (a foreign order's fill inside the
/// lookback) is HELD: no events fold, one alert in the generic quarantine shape, and its
/// `proposed_events` equal `hybrid`'s blind fold (the synthesized `EXT-*` accept plus the fill),
/// which `confirm_reconcile` (`crates/vike-core/src/runtime/reconcile.rs`) folds verbatim on approval.
#[test]
fn external_quarantine_holds_an_order_client_id_less_missing_fill_and_a_claim_folds_exactly_hybrids_events()
 {
    let eq = ReconcilePolicy::external_quarantine();
    let held = resolve(vec![missing_fill(None)], &eq, None, None);
    assert!(
        held.events.is_empty(),
        "external-quarantine folded an order-client-id-less MissingFill: {:?}",
        held.events
    );
    assert_eq!(held.alerts.len(), 1, "expected exactly one held alert: {:?}", held.alerts);
    let a = &held.alerts[0];
    assert_eq!(a.kind, ReconcileDivergenceKind::MissingFill);
    assert!(a.recover_orders.is_empty());
    // Un-keyed, the held PositionDrift's residual: a fill that stays inside the lookback
    // unconfirmed re-raises one alert per pass.
    assert_eq!(a.dedup_key, None);
    assert!(!a.proposed_events.is_empty(), "the claim must have something to fold");
    let auto = resolve(vec![missing_fill(None)], &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(a.proposed_events, auto.events, "claiming folds exactly what hybrid would have");
    // Both granularities: the KIND still auto-applies (its row stays `Synthesize`); the INSTANCE
    // is what the refinement holds.
    assert!(mode_applies(&eq, ReconcileDivergenceKind::MissingFill));
    assert!(!mode_applies_divergence(&eq, &missing_fill(None)));
}

/// The other half: an order-client-id-LINKED `MissingFill` (our own order's venue-settled fill) still
/// auto-applies under `external-quarantine`, folding exactly as under `hybrid`, whose local-origin
/// folds the policy promises to leave untouched.
#[test]
fn external_quarantine_still_auto_applies_an_order_client_id_linked_missing_fill() {
    let eq = ReconcilePolicy::external_quarantine();
    let r = resolve(vec![missing_fill(Some("c-1"))], &eq, None, None);
    assert!(r.alerts.is_empty(), "an order-client-id-linked MissingFill was held: {:?}", r.alerts);
    assert!(!r.events.is_empty(), "our own order's fill must keep folding");
    let auto = resolve(vec![missing_fill(Some("c-1"))], &ReconcilePolicy::hybrid(), None, None);
    assert_eq!(r.events, auto.events, "external-quarantine folds it exactly as hybrid does");
    assert!(mode_applies_divergence(&eq, &missing_fill(Some("c-1"))));
}

/// The three pre-existing policies (and the bare-Hybrid fallback) treat BOTH `MissingFill` linkage
/// shapes alike: at the decision level (`mode_applies_divergence` answers as `mode_applies`, since
/// none sets `hold_external_instances`) and at the resolve level (folds fold hybrid's events; holds
/// propose them). Only `external-quarantine` refines, and only through the flag.
#[test]
fn the_three_existing_policies_are_byte_identical_on_both_missing_fill_shapes() {
    for (name, policy) in [
        ("hybrid", ReconcilePolicy::hybrid()),
        ("synthesize", ReconcilePolicy::default()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
    ] {
        assert!(!policy.hold_external_instances, "{name} must not opt in to the refinement");
        for fill in [missing_fill(Some("c-1")), missing_fill(None)] {
            assert_eq!(
                mode_applies_divergence(&policy, &fill),
                mode_applies(&policy, ReconcileDivergenceKind::MissingFill),
                "{name} refined a MissingFill without opting in"
            );
            let r = resolve(vec![fill.clone()], &policy, None, None);
            let auto = resolve(vec![fill.clone()], &ReconcilePolicy::hybrid(), None, None);
            if mode_applies(&policy, ReconcileDivergenceKind::MissingFill) {
                assert!(r.alerts.is_empty(), "{name} held a MissingFill");
                assert_eq!(r.events, auto.events, "{name} folds what hybrid folds");
            } else {
                assert!(r.events.is_empty(), "{name} folded a held MissingFill");
                assert_eq!(r.alerts.len(), 1);
                assert_eq!(r.alerts[0].proposed_events, auto.events);
            }
        }
    }
    assert!(
        ReconcilePolicy::external_quarantine().hold_external_instances,
        "external-quarantine is the ONE policy that opts in"
    );
}

/// The refinement's whole footprint: over every policy and every divergence shape (both
/// `MissingFill` linkages included), `mode_applies_divergence` disagrees with `mode_applies` for
/// EXACTLY the (external-quarantine, order-client-id-less MissingFill) pair.
#[test]
fn the_refinement_is_exactly_one_sub_case_wide() {
    let mut shapes = vec![missing_fill(Some("c-1")), missing_fill(None)];
    shapes.extend(every_other_shape());
    for (name, policy) in [
        ("hybrid", ReconcilePolicy::hybrid()),
        ("synthesize", ReconcilePolicy::default()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
        ("external-quarantine", ReconcilePolicy::external_quarantine()),
    ] {
        for d in &shapes {
            let refined = name == "external-quarantine"
                && matches!(d, ReconcileDivergence::MissingFill(f) if f.order_client_id.is_none());
            let expected = mode_applies(&policy, d.kind()) && !refined;
            assert_eq!(
                mode_applies_divergence(&policy, d),
                expected,
                "{name} / {:?}: unexpected fold-vs-hold refinement",
                d.kind()
            );
        }
    }
    // ...and the safety invariant the fill_window netting rests on (`mode_applies_divergence`'s
    // doc): the one opting-in constructor holds PositionDrift, so a held order-client-id-less fill's qty
    // only ever nets against HELD drift proposals, never against an auto-applied drift fold.
    let eq = ReconcilePolicy::external_quarantine();
    assert!(eq.hold_external_instances);
    assert_eq!(eq.mode_for(ReconcileDivergenceKind::PositionDrift), ReconcileMode::Quarantine);
}

/// The journal seam: `ReconcilePolicy` rides the journaled `Command::ReconcileReports` payload as
/// serde_json, so a payload without `hold_external_instances` must deserialize with the flag
/// `false` (old journals replay), and a flag-false policy must SERIALIZE without the key (every
/// pre-existing policy's journaled form stays byte-identical). Only the opted-in policy's payload
/// carries it.
#[test]
fn the_refinement_flag_is_invisible_in_serde_until_a_policy_opts_in() {
    let hybrid_json = serde_json::to_string(&ReconcilePolicy::hybrid()).expect("serializes");
    assert!(
        !hybrid_json.contains("hold_external_instances"),
        "a flag-false policy must serialize byte-identically to the pre-refinement shape: \
         {hybrid_json}"
    );
    let replayed: ReconcilePolicy = serde_json::from_str(&hybrid_json).expect("old shape replays");
    assert!(!replayed.hold_external_instances);
    let eq_json =
        serde_json::to_string(&ReconcilePolicy::external_quarantine()).expect("serializes");
    assert!(eq_json.contains("\"hold_external_instances\":true"), "{eq_json}");
    let eq: ReconcilePolicy = serde_json::from_str(&eq_json).expect("round-trips");
    assert!(eq.hold_external_instances);
}

/// `ReconcileDivergenceKind::resolves_to_events` (the exhaustive match `vike-docs`' `rosters.json` and
/// `vike_tradehub::reconcile_config::auto_applied_kinds` both read) says what `resolve` really
/// does: under `synthesize`, the policy that folds every kind, one divergence of each kind
/// produces events if and only if the function says so. `JournalDivergence` and the two orphan
/// kinds never do; `MissingTerminal` folds an EMPTY list; `UnknownOrder` resolves only under an
/// adoption context, which this test supplies (the case the function models).
#[test]
fn resolves_to_events_agrees_with_what_resolve_folds() {
    let no_fills = HashSet::new();
    let nothing_seen = HashSet::new();
    let adopt =
        ReconcileAdoptContext { pass_fill_order_ids: &no_fills, seen_trade_ids: &nothing_seen };
    let mut shapes = vec![missing_fill(Some("c-1"))];
    shapes.extend(every_other_shape());
    for &kind in ReconcileDivergenceKind::ALL {
        let d = shapes
            .iter()
            .find(|d| d.kind() == kind)
            .unwrap_or_else(|| panic!("no divergence of kind {kind:?} in the roster"))
            .clone();
        let r = resolve(vec![d], &ReconcilePolicy::default(), None, Some(adopt));
        assert_eq!(
            !r.events.is_empty(),
            kind.resolves_to_events(),
            "{kind:?}: resolve folded {:?} under synthesize, but resolves_to_events says {}",
            r.events,
            kind.resolves_to_events()
        );
    }
}

// --- A deposit needs no hand: `adopt_balance_increase` (decision record 0179, Option B) ---

/// ONLY the `quarantine` word opts in to adopting a pure cash increase. The other three presets,
/// the `Default` and every hand-built policy leave it `false`, so `hybrid`, `synthesize` and
/// `external-quarantine` behave exactly as they did.
#[test]
fn only_the_quarantine_word_adopts_a_balance_increase() {
    for &name in vike_execution::recon::POLICY_NAMES {
        let p = ReconcilePolicy::from_policy_name(name).expect("a listed name resolves");
        assert_eq!(p.adopt_balance_increase, name == "quarantine", "{name}");
    }
    assert!(!ReconcilePolicy::default().adopt_balance_increase);
    assert!(!ReconcilePolicy::hybrid().adopt_balance_increase);
    assert!(!ReconcilePolicy::external_quarantine().adopt_balance_increase);
    assert!(!quarantine().adopt_balance_increase, "a hand-built quarantine is not the word");
}

/// The rule is a per-divergence refinement ABOVE `resolve`: the pure layer still HOLDS a
/// `BalanceDrift` under the named `quarantine` (`mode_applies` says hold, and `resolve` raises the
/// alert with the correcting `AccountState` proposed), an increase and a decrease alike. The
/// adoption moves that alert in `vike-core` (`adopt_a_pure_balance_increase`), where the account's
/// held store is visible.
#[test]
fn resolve_still_holds_a_balance_increase_under_the_named_quarantine() {
    let q = ReconcilePolicy::from_policy_name("quarantine").expect("a policy name");
    assert!(!mode_applies(&q, ReconcileDivergenceKind::BalanceDrift));
    for venue_balance in [250.0, 50.0] {
        let d = ReconcileDivergence::BalanceDrift {
            venue: "polymarket".into(),
            asset: "USDC".into(),
            local: 100.0,
            venue_balance,
            timestamp: 1_000,
        };
        let r = resolve(vec![d], &q, None, None);
        assert!(r.events.is_empty(), "{venue_balance}: nothing folds in the pure layer");
        assert_eq!(r.alerts.len(), 1, "{venue_balance}");
        assert_eq!(r.alerts[0].kind, ReconcileDivergenceKind::BalanceDrift);
        assert_eq!(
            r.alerts[0].proposed_events.len(),
            1,
            "{venue_balance}: the AccountState rides it"
        );
    }
}

/// Which held kinds block the adoption: every kind that could carry the same money as an unexplained
/// increase (the externally-realized-profit double count), and neither the drift itself nor the
/// absence of a resting order. An explicit table, so a changed row is a visible edit here.
#[test]
fn the_kinds_that_block_a_balance_adoption_are_pinned() {
    let blocks: Vec<ReconcileDivergenceKind> = ReconcileDivergenceKind::ALL
        .iter()
        .copied()
        .filter(|k| k.blocks_balance_adoption())
        .collect();
    assert_eq!(
        blocks,
        vec![
            ReconcileDivergenceKind::MissingFill,
            ReconcileDivergenceKind::MissingTerminal,
            ReconcileDivergenceKind::UnknownOrder,
            ReconcileDivergenceKind::PositionDrift,
            ReconcileDivergenceKind::PositionOnlyExternal,
            ReconcileDivergenceKind::OrphanLocalPosition,
            ReconcileDivergenceKind::JournalDivergence,
        ]
    );
}

/// The journal seam, as `hold_external_instances` has it: the flag rides the journaled
/// `Command::ReconcileReports` payload, so a replay re-derives the decision from the policy the
/// original run had. A payload WITHOUT the key (every journal written before the rule) replays with
/// the flag `false` (the pass held the deposit then and holds it now), and a flag-false policy
/// serializes without the key (byte-identical to its old form).
#[test]
fn the_adoption_flag_is_journaled_with_the_policy_and_old_payloads_replay_as_held() {
    let q = ReconcilePolicy::from_policy_name("quarantine").expect("a policy name");
    let json = serde_json::to_string(&q).expect("serializes");
    assert!(json.contains("\"adopt_balance_increase\":true"), "{json}");
    let back: ReconcilePolicy = serde_json::from_str(&json).expect("round-trips");
    assert!(back.adopt_balance_increase);

    for name in ["hybrid", "synthesize", "external-quarantine"] {
        let p = ReconcilePolicy::from_policy_name(name).expect("a policy name");
        let json = serde_json::to_string(&p).expect("serializes");
        assert!(!json.contains("adopt_balance_increase"), "{name}: {json}");
    }

    // An old journaled quarantine payload: no key, so the flag is off and the pass holds.
    let old = r#"{"default":"Quarantine","per_kind":{}}"#;
    let replayed: ReconcilePolicy = serde_json::from_str(old).expect("an old payload replays");
    assert!(!replayed.adopt_balance_increase);
    assert_eq!(replayed.default, ReconcileMode::Quarantine);
}
