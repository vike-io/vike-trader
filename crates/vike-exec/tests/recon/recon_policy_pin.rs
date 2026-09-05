//! What each `ReconPolicy` ACTUALLY does with each `DivergenceKind` — pinned, because prose about
//! it went wrong everywhere at once and stayed wrong long enough to drive a rollout policy.
//!
//! ## The claim this file exists to kill
//!
//! Every in-tree statement of it — `CLAUDE.md`, `crates/bridges/polymarket/src/recon_client.rs`,
//! `crates/vike-mount/src/lib.rs`, `crates/vike-run/src/node.rs`,
//! `crates/vike-tradehub/src/tradehub_cli.rs`, `crates/vike-config/src/flags.rs`,
//! `crates/bridges/binance/src/family/recon.rs`, `docs/ops/tradehub-the CI box.md` and
//! `docs/decisions/0013-degrade-vs-refuse.md` — said that under `hybrid` an `OrphanLocalOrder`
//! "auto-cancels every pass", and `poly_reconcile_enabled` called it "the one genuinely destructive
//! outcome" of reconciling a live account against a paper engine. It is false, and the trace is
//! short enough to restate here:
//!
//! 1. `vike_exec::recon::diff` raises `Divergence::OrphanLocalOrder { client_order_id }` — a coid
//!    and nothing else. There is no venue evidence attached because the evidence IS an absence.
//! 2. `resolve`'s `events_for` has arms for `MissingFill` / `PositionDrift` /
//!    `PositionOnlyExternal` only. `OrphanLocalOrder` falls into the `_ => (Vec::new(), false)`
//!    catch-all: **zero events**.
//! 3. `ReconPolicy::hybrid` mapped the kind to `ReconMode::Synthesize`, so `resolve`'s
//!    `mode_applies` was `true` and the empty vec was "folded" — which also meant **no alert was
//!    raised**. Under `hybrid` the divergence was detected and then produced nothing whatsoever.
//! 4. `resolve` constructs no `Event::OrderCanceled` anywhere, under any policy, for any kind.
//!
//! ## What CHANGED, and what did not
//!
//! Step 3 above described a second defect hiding behind the first: **detection with no outcome.**
//! A kind that is diffed, counted and then silently discarded under the production-default policy
//! reads as coverage in every enumeration of the reconcile engine and tells nobody anything. That
//! is now fixed, and the fix is deliberately the SMALLEST one that removes the silence:
//! `OrphanLocalOrder` is classified no-local-origin (`ReconPolicy::hybrid`'s preset and
//! `resolve::is_local_origin` both moved, together), so `hybrid` and `quarantine` surface ONE
//! aggregated, dedup-keyed, EVENT-FREE operator alert per pass instead of folding an empty list.
//!
//! **Steps 1, 2 and 4 are untouched, and the tests below still assert them verbatim.** No policy
//! folds an event for this kind, no alert proposes one, and nothing anywhere synthesizes a cancel —
//! so an operator confirm on the new alert is a pure acknowledgement. The `hybrid` MODE moved; the
//! economics did not. `vike_exec::recon::resolve`'s module doc carries the argument for why an
//! alert (and never an auto-cancel) is the right outcome, and for why the alert is aggregated
//! rather than keyed per coid.
//!
//! The one place that DOES terminalize an order the venue stopped reporting is
//! `ExecutionEngine::apply_snapshot`'s reap (`crates/vike-exec/tests/recon/reconcile_reap.rs`) — and even
//! that synthesizes a **local** `OrderCanceled` folded through the FSM; it never calls the venue.
//! The live reconcile path cannot reach it: `vike_core::recon_manager`'s only side effect is
//! enqueuing `Command::ReconcileReports`, and `CoreThread::reconcile_reports` documents that it
//! deliberately does NOT route through `Command::ApplySnapshot` *because* that arm's reap "would
//! cancel EVERY live order on this venue as a side effect". The only production driver of
//! `Command::ApplySnapshot` is `CoreHandle::spawn_periodic_reconcile`, which nothing outside
//! `crates/vike-core/tests/runtime_smoke.rs` calls.
//!
//! ## The kind that DOES auto-apply under `hybrid`
//!
//! `PositionDrift` — see `hybrid_auto_applies_position_drift`. That is deliberate (the venue's own
//! position row is positive evidence, and it is the only thing that re-converges qty after a fill
//! older than the lookback), but it was reaching `Synthesize` through `ReconPolicy`'s `default`
//! rather than through a named `per_kind` row, and no test asserted it —
//! `hybrid_quarantines_only_no_local_origin_kinds` in `types.rs` names six kinds and omits it.
//! `every_divergence_kind_has_a_pinned_hybrid_mode` below closes that with an exhaustive match, so
//! a newly-added kind is a COMPILE error here rather than a silent auto-fold.

use vike_exec::recon::{
    Divergence, DivergenceKind, DivergenceOrigin, ORPHAN_LOCAL_ORDER_KEY, ReconMode, ReconPolicy,
    mode_applies, mode_applies_divergence, resolve,
};
use vike_model::events::{Event, LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

/// Every `DivergenceKind`. Kept beside `expected_hybrid_mode`'s exhaustive match, which is what
/// actually forces a new kind to be classified; this array is what makes the assertion iterate.
const ALL_KINDS: &[DivergenceKind] = &[
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

fn orphan(coid: &str) -> Divergence {
    Divergence::OrphanLocalOrder { client_order_id: coid.to_string() }
}

fn drift(local_qty: f64, venue_qty: f64) -> Divergence {
    Divergence::PositionDrift {
        report: PositionStatusReport {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            position_side: PositionSide::Both,
            qty: venue_qty,
            avg_px: 0.55,
            ts: 1_000,
            margin_mode: MarginMode::Cross,
            isolated_margin: None,
            delta: None,
        },
        local_qty,
    }
}

fn quarantine() -> ReconPolicy {
    ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }
}

/// A bare `ReconMode::Hybrid` default — NOT what `VIKE_RECONCILE_POLICY=hybrid` builds (that is
/// `ReconPolicy::hybrid()`), but the fallback `resolve::is_local_origin` serves.
fn bare_hybrid() -> ReconPolicy {
    ReconPolicy { default: ReconMode::Hybrid, ..Default::default() }
}

/// A `MissingFill` in either linkage shape: `Some` coid = the venue echoed OUR client id (our own
/// order's fill, materialized late); `None` = the fill names no local order (a foreign order's
/// fill inside the lookback) — the one divergence instance whose EVIDENCE refines its kind's
/// origin (`Divergence::origin`).
fn missing_fill(client_order_id: Option<&str>) -> Divergence {
    Divergence::MissingFill(FillReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        trade_id: TradeId::new("T-77").expect("non-empty"),
        venue_order_id: "V-9".into(),
        client_order_id: client_order_id.map(str::to_string),
        side: 1,
        last_qty: 5.0,
        last_px: 0.55,
        commission: 0.01,
        commission_asset: "USDC".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 1_000,
    })
}

// ---------------------------------------------------------------------------------------------
// OrphanLocalOrder: the falsified claim, and the silence that hid behind it
// ---------------------------------------------------------------------------------------------

/// THE pin, updated. Under the DEFAULT policy an operator gets from `VIKE_RECONCILE_POLICY` unset,
/// `OrphanLocalOrder` still folds NOTHING — but it is no longer invisible: it surfaces exactly one
/// event-free operator alert, dedup-keyed on `ORPHAN_LOCAL_ORDER_KEY` so a recurring pass refreshes
/// that one row rather than appending. (This test used to assert `r.alerts.is_empty()`, which was
/// the true reading of a defect: detected, then discarded.)
#[test]
fn hybrid_surfaces_an_orphan_local_order_and_still_folds_nothing() {
    let r = resolve(vec![orphan("paper-1")], &ReconPolicy::hybrid(), None, None);
    assert!(r.events.is_empty(), "hybrid folded events for OrphanLocalOrder: {:?}", r.events);
    assert_eq!(r.alerts.len(), 1, "hybrid must SURFACE the divergence: {:?}", r.alerts);
    assert_eq!(r.alerts[0].kind, DivergenceKind::OrphanLocalOrder);
    assert_eq!(r.alerts[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(r.alerts[0].detail.contains("paper-1"), "names the order: {}", r.alerts[0].detail);
}

/// The aggregation pin — the property that makes surfacing this kind affordable at all. The held
/// alert store (`crates/vike-core/src/runtime/mod.rs`'s `confirm_recon` is its only remover) never
/// self-clears and `crates/vike-core/src/runtime/publish.rs`'s `recon_block` clones every held row
/// into every published snapshot, while an orphan heals routinely and its natural key (the coid) is
/// unbounded. So a pass in which EVERY live local order orphans at once — a venue report that
/// echoes no client ids, the exact scenario
/// `crates/bridges/binance/src/family/recon.rs`'s `parse_order_row` strips a broker prefix to
/// avoid — must still cost ONE row.
#[test]
fn a_whole_book_of_orphans_costs_exactly_one_alert_row() {
    let d: Vec<Divergence> = (0..50).map(|i| orphan(&format!("mm-{i:03}"))).collect();
    for (name, policy) in [("hybrid", ReconPolicy::hybrid()), ("quarantine", quarantine())] {
        let r = resolve(d.clone(), &policy, None, None);
        assert_eq!(r.alerts.len(), 1, "{name} raised {} rows", r.alerts.len());
        assert!(r.alerts[0].detail.starts_with("50 live LOCAL order(s)"), "{name}: exact count");
    }
}

/// The generalization: NO policy turns an `OrphanLocalOrder` into a cancel — or into any event.
/// This is the assertion every one of those documents contradicted.
#[test]
fn no_policy_ever_synthesizes_a_cancel_for_an_orphan_local_order() {
    for (name, policy) in [
        ("hybrid", ReconPolicy::hybrid()),
        ("synthesize", ReconPolicy::default()),
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

/// `synthesize` is now the ONLY policy under which the divergence is invisible — the same NAMED
/// residual `OrphanLocalPosition` carries: an operator who chose "fold everything, no operator in
/// front of it" opted out of alerts. `hybrid`, `quarantine` and the bare-`Hybrid` fallback all
/// surface it, all with EMPTY `proposed_events`. So quarantine-first still buys this kind nothing
/// but visibility — and now `hybrid` buys the same visibility, which is the change.
#[test]
fn every_held_policy_surfaces_an_orphan_local_order_and_synthesize_alone_stays_silent() {
    for (name, policy) in [
        ("hybrid", ReconPolicy::hybrid()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
    ] {
        let held = resolve(vec![orphan("paper-1")], &policy, None, None);
        assert_eq!(held.alerts.len(), 1, "{name} surfaced nothing");
        assert_eq!(held.alerts[0].kind, DivergenceKind::OrphanLocalOrder);
        assert!(held.alerts[0].proposed_events.is_empty(), "{name} proposed an action");
    }
    let silent = resolve(vec![orphan("p")], &ReconPolicy::default(), None, None);
    assert!(silent.alerts.is_empty(), "synthesize: the named residual, a true no-op");
    assert!(silent.events.is_empty());
}

// ---------------------------------------------------------------------------------------------
// PositionDrift: the kind that really does auto-apply
// ---------------------------------------------------------------------------------------------

/// `PositionDrift` auto-applies under `hybrid` — it folds legs that move local qty onto the
/// venue's number and books realized PnL at the venue's price. Deliberate (see this file's doc and
/// `ReconPolicy::hybrid`'s comment), and now asserted rather than inherited from `default`.
#[test]
fn hybrid_auto_applies_position_drift() {
    let r = resolve(vec![drift(0.0, 100.0)], &ReconPolicy::hybrid(), None, None);
    assert!(!r.events.is_empty(), "PositionDrift folded nothing under hybrid");
    assert!(r.alerts.is_empty(), "PositionDrift was held under hybrid: {:?}", r.alerts);
    assert!(mode_applies(&ReconPolicy::hybrid(), DivergenceKind::PositionDrift));
}

/// ⚠ The preset and the bare-`Hybrid` fallback DISAGREE about this one kind, and the disagreement
/// is pinned rather than papered over: `ReconPolicy::hybrid()` auto-applies `PositionDrift` (a
/// named `per_kind` row), while `resolve::is_local_origin` — the classifier a bare
/// `ReconMode::Hybrid` default falls back to — does NOT list it, so a bare-Hybrid policy
/// quarantines it. Both are defensible and neither is reachable from `VIKE_RECONCILE_POLICY`
/// (`parse_policy` builds `ReconPolicy::hybrid()` for "hybrid" and never a bare `Hybrid`), so this
/// is a latent trap for the next author who writes `ReconPolicy { default: Hybrid, .. }` by hand —
/// not a live divergence. `is_local_origin` is named for LOCAL ORIGIN and `PositionDrift` is
/// venue-reported, so adding it there would make the name lie; the preset is where the
/// auto-apply decision belongs.
#[test]
fn bare_hybrid_and_the_preset_disagree_about_position_drift() {
    assert!(mode_applies(&ReconPolicy::hybrid(), DivergenceKind::PositionDrift));
    assert!(!mode_applies(&bare_hybrid(), DivergenceKind::PositionDrift));

    // The TRUE half of `is_local_origin`, which is what bare Hybrid consults and which nothing
    // pinned: a sweep replaced the whole predicate with `false` and every test stayed green. Its
    // own doc says the preset "is the other half; the two MUST agree" — this is the only place
    // that agreement is asserted rather than described. Drift here is fail-safe (bare Hybrid would
    // over-quarantine rather than over-apply), which is why these two lines are the whole fix.
    assert!(mode_applies(&bare_hybrid(), DivergenceKind::MissingFill));
    assert!(mode_applies(&bare_hybrid(), DivergenceKind::MissingTerminal));

    let held = resolve(vec![drift(0.0, 100.0)], &bare_hybrid(), None, None);
    assert_eq!(held.alerts.len(), 1, "bare Hybrid should hold PositionDrift");
    assert_eq!(held.alerts[0].kind, DivergenceKind::PositionDrift);
    // The held alert carries the SAME legs the preset would have folded — an operator confirm
    // applies exactly what `hybrid` does automatically.
    let auto = resolve(vec![drift(0.0, 100.0)], &ReconPolicy::hybrid(), None, None);
    assert_eq!(held.alerts[0].proposed_events, auto.events);
}

// ---------------------------------------------------------------------------------------------
// The roster gate
// ---------------------------------------------------------------------------------------------

/// The expected `hybrid` mode of every kind. EXHAUSTIVE match, no `_` arm: adding a
/// `DivergenceKind` fails to COMPILE here until somebody states whether the default policy folds it
/// without an operator. `PositionDrift` is the reason this exists — it reached `Synthesize` through
/// `ReconPolicy`'s `default` with no named row and no assertion anywhere.
fn expected_hybrid_mode(kind: DivergenceKind) -> ReconMode {
    match kind {
        // Local-origin: our own activity, confirmed by the venue → fold.
        DivergenceKind::MissingFill => ReconMode::Synthesize,
        // Auto-applies and resolves to nothing (this file's doc) — the mode is inert. The LAST kind
        // in that state, and a sibling defect deliberately left for its own change.
        DivergenceKind::MissingTerminal => ReconMode::Synthesize,
        // Venue-reported position row: positive evidence, and the only re-convergence path for a
        // fill older than the lookback.
        DivergenceKind::PositionDrift => ReconMode::Synthesize,
        // No local origin → hold for an operator. The ORDER is ours; the DIVERGENCE is the venue's
        // silence about it, and folding an empty list in silence was the defect.
        DivergenceKind::OrphanLocalOrder => ReconMode::Quarantine,
        DivergenceKind::UnknownOrder => ReconMode::Quarantine,
        DivergenceKind::PositionOnlyExternal => ReconMode::Quarantine,
        DivergenceKind::OrphanLocalPosition => ReconMode::Quarantine,
        DivergenceKind::BalanceDrift => ReconMode::Quarantine,
        // Never auto-applied regardless of mode — `resolve` alerts on it before the policy is
        // consulted ("no auto-apply of a persistence bug").
        DivergenceKind::JournalDivergence => ReconMode::Synthesize,
    }
}

#[test]
fn every_divergence_kind_has_a_pinned_hybrid_mode() {
    // Bump together with the exhaustive match above AND `ALL_KINDS`.
    assert_eq!(ALL_KINDS.len(), 9, "a DivergenceKind was added or removed — update ALL_KINDS");
    let p = ReconPolicy::hybrid();
    for &k in ALL_KINDS {
        assert_eq!(p.mode_for(k), expected_hybrid_mode(k), "hybrid mode changed for {k:?}");
    }
}

/// Every kind is a NAMED row in the preset — none may reach its mode through `ReconPolicy`'s
/// `default`. A fall-through is indistinguishable from an unclassified kind, which is exactly how
/// `PositionDrift` came to auto-apply on ten venues with nothing asserting it.
#[test]
fn the_hybrid_preset_classifies_every_kind_explicitly() {
    let p = ReconPolicy::hybrid();
    for &k in ALL_KINDS {
        assert!(
            p.per_kind.contains_key(&k),
            "{k:?} has no named row in ReconPolicy::hybrid() — it would inherit `default` silently"
        );
    }
}

/// `JournalDivergence` is the one kind whose mode is decorative: `resolve` short-circuits it into
/// an investigative alert before `mode_applies` is ever consulted.
#[test]
fn journal_divergence_is_held_under_every_policy_regardless_of_mode() {
    for (name, policy) in [
        ("hybrid", ReconPolicy::hybrid()),
        ("synthesize", ReconPolicy::default()),
        ("quarantine", quarantine()),
    ] {
        let d =
            vec![Divergence::JournalDivergence { detail: "lost fill".into(), recover_order: None }];
        let r = resolve(d, &policy, None, None);
        assert_eq!(r.alerts.len(), 1, "{name} did not hold JournalDivergence");
        assert!(r.events.is_empty(), "{name} folded a JournalDivergence");
    }
}

// ---------------------------------------------------------------------------------------------
// The EXTERNAL origin dimension + `external-quarantine` (split-plane Pattern A)
// ---------------------------------------------------------------------------------------------

/// The expected [`DivergenceOrigin`] of every kind — the same forcing shape as
/// `expected_hybrid_mode`: an EXHAUSTIVE match with no `_` arm, so adding a `DivergenceKind`
/// fails to COMPILE here until somebody states whose activity its evidence describes.
fn expected_origin(kind: DivergenceKind) -> DivergenceOrigin {
    match kind {
        // Our own activity, venue-settled; the recon pass merely materializes it locally.
        DivergenceKind::MissingFill => DivergenceOrigin::ReconciliationMaterialized,
        DivergenceKind::MissingTerminal => DivergenceOrigin::ReconciliationMaterialized,
        // Positive venue evidence nothing local explains. (`PositionDrift`: the evidence cannot
        // be SHOWN to be ours — the variant's own doc carries why that ambiguity reads External.)
        DivergenceKind::UnknownOrder => DivergenceOrigin::External,
        DivergenceKind::PositionDrift => DivergenceOrigin::External,
        DivergenceKind::PositionOnlyExternal => DivergenceOrigin::External,
        DivergenceKind::BalanceDrift => DivergenceOrigin::External,
        // The evidence is something missing; these fold nothing under any policy, so the origin
        // dimension is inert for them (classified for exhaustiveness, never able to change a fold).
        DivergenceKind::OrphanLocalOrder => DivergenceOrigin::Absence,
        DivergenceKind::OrphanLocalPosition => DivergenceOrigin::Absence,
        DivergenceKind::JournalDivergence => DivergenceOrigin::Absence,
    }
}

#[test]
fn every_divergence_kind_has_a_pinned_origin() {
    for &k in ALL_KINDS {
        assert_eq!(k.origin(), expected_origin(k), "origin changed for {k:?}");
    }
}

/// `DivergenceKind::origin` and `resolve`'s private `is_local_origin` (the bare-`Hybrid` fallback
/// classifier, reachable only through `mode_applies`) are two spellings of one classification and
/// MUST agree: a bare `ReconMode::Hybrid` applies exactly the `ReconciliationMaterialized` kinds.
#[test]
fn origin_agrees_with_the_bare_hybrid_classifier() {
    for &k in ALL_KINDS {
        assert_eq!(
            mode_applies(&bare_hybrid(), k),
            k.origin() == DivergenceOrigin::ReconciliationMaterialized,
            "bare Hybrid and DivergenceKind::origin disagree about {k:?}"
        );
    }
}

/// The expected `external-quarantine` mode of every kind — exhaustive like `expected_hybrid_mode`,
/// so a new kind must be classified under this policy too before this file compiles.
fn expected_external_quarantine_mode(kind: DivergenceKind) -> ReconMode {
    match kind {
        // hybrid's local-origin folds, untouched.
        DivergenceKind::MissingFill => ReconMode::Synthesize,
        // Auto-applies and resolves to nothing, exactly as under hybrid — the mode stays inert.
        DivergenceKind::MissingTerminal => ReconMode::Synthesize,
        // THE delta: the one External kind hybrid folds blind is HELD for an operator claim.
        DivergenceKind::PositionDrift => ReconMode::Quarantine,
        // Already held under hybrid; External keeps them held.
        DivergenceKind::UnknownOrder => ReconMode::Quarantine,
        DivergenceKind::PositionOnlyExternal => ReconMode::Quarantine,
        DivergenceKind::BalanceDrift => ReconMode::Quarantine,
        // Absence-origin kinds inherit their hybrid rows verbatim.
        DivergenceKind::OrphanLocalOrder => ReconMode::Quarantine,
        DivergenceKind::OrphanLocalPosition => ReconMode::Quarantine,
        // Decorative, inherited from hybrid: `resolve` holds it before the mode is consulted.
        DivergenceKind::JournalDivergence => ReconMode::Synthesize,
    }
}

#[test]
fn every_divergence_kind_has_a_pinned_external_quarantine_mode() {
    let p = ReconPolicy::external_quarantine();
    for &k in ALL_KINDS {
        assert_eq!(
            p.mode_for(k),
            expected_external_quarantine_mode(k),
            "external-quarantine mode changed for {k:?}"
        );
    }
}

/// Same no-fall-through gate the hybrid preset carries: every kind is a NAMED row (inherited from
/// `hybrid()`, which `external_quarantine()` is computed over), so none can reach its mode through
/// `ReconPolicy`'s `default` unclassified.
#[test]
fn the_external_quarantine_preset_classifies_every_kind_explicitly() {
    let p = ReconPolicy::external_quarantine();
    for &k in ALL_KINDS {
        assert!(
            p.per_kind.contains_key(&k),
            "{k:?} has no named row in ReconPolicy::external_quarantine()"
        );
    }
}

/// The preset is COMPUTED (`hybrid()` + `origin()`), so the only rows that may differ from hybrid
/// are the External ones — and they may differ only toward `Quarantine`. Every non-External kind
/// keeps its hybrid row verbatim, and so does the policy's `default` mode.
#[test]
fn external_quarantine_differs_from_hybrid_only_where_origin_is_external() {
    let eq = ReconPolicy::external_quarantine();
    for &k in ALL_KINDS {
        let expected = if k.origin() == DivergenceOrigin::External {
            ReconMode::Quarantine
        } else {
            expected_hybrid_mode(k)
        };
        assert_eq!(eq.mode_for(k), expected, "{k:?}");
    }
    assert_eq!(eq.default, ReconPolicy::hybrid().default, "the default mode is hybrid's");
}

/// The Pattern-A pin: under `external-quarantine` a `PositionDrift` is HELD — no events, exactly
/// one alert in the generic quarantine shape — and its `proposed_events` are byte-identical to
/// hybrid's blind fold, which is exactly what `confirm_recon` (the CLAIM path,
/// `crates/vike-core/src/runtime/mod.rs`) folds verbatim on operator approval. So claiming an
/// EXTERNAL divergence yields precisely hybrid's outcome, with an operator in front of it.
#[test]
fn external_quarantine_holds_position_drift_and_a_claim_folds_exactly_hybrids_events() {
    let held = resolve(vec![drift(0.0, 100.0)], &ReconPolicy::external_quarantine(), None, None);
    assert!(held.events.is_empty(), "external-quarantine folded PositionDrift: {:?}", held.events);
    assert_eq!(held.alerts.len(), 1, "expected exactly one held alert: {:?}", held.alerts);
    let a = &held.alerts[0];
    assert_eq!(a.kind, DivergenceKind::PositionDrift);
    assert!(a.recover_orders.is_empty());
    // The generic held path is un-keyed — inherited from `quarantine`, where a persisting drift
    // has always re-raised one alert per pass (named as a residual on `external_quarantine`'s doc).
    assert_eq!(a.dedup_key, None);
    assert!(!a.proposed_events.is_empty(), "the claim must have something to fold");
    let auto = resolve(vec![drift(0.0, 100.0)], &ReconPolicy::hybrid(), None, None);
    assert_eq!(a.proposed_events, auto.events, "claiming folds exactly what hybrid would have");
    assert!(!mode_applies(&ReconPolicy::external_quarantine(), DivergenceKind::PositionDrift));
}

/// The three PRE-EXISTING policies are byte-identical under the new dimension. Policy level: the
/// two flat policies still carry NO per-kind rows and one uniform mode each, and hybrid's every
/// row still equals its pinned table. Resolve level, on the same fixture the new policy is tested
/// on: `hybrid` and `synthesize` still fold the drift (identically), `quarantine` still holds it
/// with hybrid's events proposed.
#[test]
fn the_three_existing_policies_are_byte_identical_under_the_origin_dimension() {
    let synth = ReconPolicy::default();
    let quar = quarantine();
    assert!(synth.per_kind.is_empty(), "synthesize gained a per-kind row");
    assert!(quar.per_kind.is_empty(), "quarantine gained a per-kind row");
    // The per-divergence refinement flag is external-quarantine's alone — a pre-existing policy
    // carrying it would refine folds nobody opted in to.
    assert!(!synth.hold_external_instances, "synthesize opted in to the refinement");
    assert!(!quar.hold_external_instances, "quarantine opted in to the refinement");
    let hy = ReconPolicy::hybrid();
    assert!(!hy.hold_external_instances, "hybrid opted in to the refinement");
    for &k in ALL_KINDS {
        assert_eq!(synth.mode_for(k), ReconMode::Synthesize);
        assert_eq!(quar.mode_for(k), ReconMode::Quarantine);
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

// ---------------------------------------------------------------------------------------------
// The per-DIVERGENCE refinement: a coid-less MissingFill under external-quarantine
// (#1380's named residual, closed)
// ---------------------------------------------------------------------------------------------

/// One instance of every OTHER divergence shape (everything but `MissingFill`, which
/// `missing_fill` builds in both linkage shapes) — the roster for asserting the instance-level
/// origin refinement is exactly one sub-case wide.
fn every_other_shape() -> Vec<Divergence> {
    let order = OrderStatusReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        venue_order_id: "V-9".into(),
        client_order_id: Some("c-1".into()),
        side: 1,
        order_type: "LIMIT".into(),
        qty: 5.0,
        filled_qty: 5.0,
        avg_px: 0.55,
        status: "FILLED".into(),
        ts: 1_000,
    };
    let report = PositionStatusReport {
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        position_side: PositionSide::Both,
        qty: 100.0,
        avg_px: 0.55,
        ts: 1_000,
        margin_mode: MarginMode::Cross,
        isolated_margin: None,
        delta: None,
    };
    vec![
        Divergence::MissingTerminal { order: order.clone() },
        Divergence::OrphanLocalOrder { client_order_id: "c-1".into() },
        Divergence::UnknownOrder(order),
        Divergence::PositionDrift { report: report.clone(), local_qty: 0.0 },
        Divergence::PositionOnlyExternal(report),
        Divergence::OrphanLocalPosition {
            venue: "polymarket".into(),
            symbol: "TOKEN".into(),
            position_side: "BOTH".into(),
            local_qty: 5.0,
        },
        Divergence::BalanceDrift {
            venue: "polymarket".into(),
            asset: "USDC".into(),
            local: 100.0,
            venue_bal: 50.0,
            ts: 1_000,
        },
        Divergence::JournalDivergence { detail: "lost fill".into(), recover_order: None },
    ]
}

/// The instance-level origin classifier (`Divergence::origin`) refines its kind's origin in
/// EXACTLY one sub-case: a `MissingFill` whose `FillReport` echoes no `client_order_id` — the
/// fill names no local order, so the evidence reads External even though the KIND is classified
/// `ReconciliationMaterialized` by its dominant case (our own missed fill). Every other shape —
/// including the coid-LINKED `MissingFill` — answers with its kind's origin verbatim.
#[test]
fn a_coidless_missing_fill_reads_external_and_every_other_shape_keeps_its_kind_origin() {
    assert_eq!(missing_fill(None).origin(), DivergenceOrigin::External);
    assert_eq!(
        missing_fill(None).kind().origin(),
        DivergenceOrigin::ReconciliationMaterialized,
        "the KIND tag itself must not move — the refinement is per-instance"
    );
    assert_eq!(missing_fill(Some("c-1")).origin(), DivergenceOrigin::ReconciliationMaterialized);
    for d in every_other_shape() {
        assert_eq!(d.origin(), d.kind().origin(), "{:?} refined unexpectedly", d.kind());
    }
}

/// THE pin this change exists for. Under `external-quarantine` a coid-less `MissingFill` — a
/// foreign order's fill inside the lookback, the sub-case #1380 named as still auto-applying —
/// is HELD: no events fold, exactly one alert in the generic quarantine shape, and its
/// `proposed_events` are byte-identical to `hybrid`'s blind fold (the synthesized `EXT-*` accept
/// plus the fill), which is exactly what `confirm_recon` (the CLAIM path,
/// `crates/vike-core/src/runtime/mod.rs`) folds verbatim on operator approval.
#[test]
fn external_quarantine_holds_a_coidless_missing_fill_and_a_claim_folds_exactly_hybrids_events() {
    let eq = ReconPolicy::external_quarantine();
    let held = resolve(vec![missing_fill(None)], &eq, None, None);
    assert!(
        held.events.is_empty(),
        "external-quarantine folded a coid-less MissingFill: {:?}",
        held.events
    );
    assert_eq!(held.alerts.len(), 1, "expected exactly one held alert: {:?}", held.alerts);
    let a = &held.alerts[0];
    assert_eq!(a.kind, DivergenceKind::MissingFill);
    assert!(a.recover_orders.is_empty());
    // The generic held path is un-keyed — the same inherited residual the held PositionDrift
    // carries: a fill that stays inside the lookback unconfirmed re-raises one alert per pass.
    assert_eq!(a.dedup_key, None);
    assert!(!a.proposed_events.is_empty(), "the claim must have something to fold");
    let auto = resolve(vec![missing_fill(None)], &ReconPolicy::hybrid(), None, None);
    assert_eq!(a.proposed_events, auto.events, "claiming folds exactly what hybrid would have");
    // The decision seam, stated at both granularities: the KIND still auto-applies (the mode row
    // is untouched — `Synthesize`); the INSTANCE is what the refinement holds.
    assert!(mode_applies(&eq, DivergenceKind::MissingFill));
    assert!(!mode_applies_divergence(&eq, &missing_fill(None)));
}

/// The other half of the refinement's precision: a coid-LINKED `MissingFill` — our own order's
/// venue-settled fill, the kind's dominant case — still auto-applies under `external-quarantine`,
/// folding byte-identically to `hybrid`. Holding it would break the policy's contract that
/// `hybrid`'s local-origin folds are untouched.
#[test]
fn external_quarantine_still_auto_applies_a_coid_linked_missing_fill() {
    let eq = ReconPolicy::external_quarantine();
    let r = resolve(vec![missing_fill(Some("c-1"))], &eq, None, None);
    assert!(r.alerts.is_empty(), "a coid-linked MissingFill was held: {:?}", r.alerts);
    assert!(!r.events.is_empty(), "our own order's fill must keep folding");
    let auto = resolve(vec![missing_fill(Some("c-1"))], &ReconPolicy::hybrid(), None, None);
    assert_eq!(r.events, auto.events, "external-quarantine folds it exactly as hybrid does");
    assert!(mode_applies_divergence(&eq, &missing_fill(Some("c-1"))));
}

/// #1380's byte-identity pin, extended to BOTH `MissingFill` linkage shapes: the three
/// pre-existing policies (and the bare-Hybrid fallback) treat a coid-less and a coid-linked
/// `MissingFill` identically to each other and to their pre-refinement behavior — at the decision
/// level (`mode_applies_divergence` answers exactly as `mode_applies`, because none of them sets
/// `hold_external_instances`) and at the resolve level (folds fold hybrid's events; holds hold
/// them as proposals). Only `external-quarantine` refines, and only through the flag.
#[test]
fn the_three_existing_policies_are_byte_identical_on_both_missing_fill_shapes() {
    for (name, policy) in [
        ("hybrid", ReconPolicy::hybrid()),
        ("synthesize", ReconPolicy::default()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
    ] {
        assert!(!policy.hold_external_instances, "{name} must not opt in to the refinement");
        for fill in [missing_fill(Some("c-1")), missing_fill(None)] {
            assert_eq!(
                mode_applies_divergence(&policy, &fill),
                mode_applies(&policy, DivergenceKind::MissingFill),
                "{name} refined a MissingFill without opting in"
            );
            let r = resolve(vec![fill.clone()], &policy, None, None);
            let auto = resolve(vec![fill.clone()], &ReconPolicy::hybrid(), None, None);
            if mode_applies(&policy, DivergenceKind::MissingFill) {
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
        ReconPolicy::external_quarantine().hold_external_instances,
        "external-quarantine is the ONE policy that opts in"
    );
}

/// The refinement's whole footprint, enumerated: over every policy and one instance of every
/// divergence shape (both `MissingFill` linkages included), `mode_applies_divergence` disagrees
/// with the per-kind `mode_applies` for EXACTLY the (external-quarantine, coid-less MissingFill)
/// pair. A second divergent cell appearing here means the refinement grew without this file
/// noticing.
#[test]
fn the_refinement_is_exactly_one_sub_case_wide() {
    let mut shapes = vec![missing_fill(Some("c-1")), missing_fill(None)];
    shapes.extend(every_other_shape());
    for (name, policy) in [
        ("hybrid", ReconPolicy::hybrid()),
        ("synthesize", ReconPolicy::default()),
        ("quarantine", quarantine()),
        ("bare-hybrid", bare_hybrid()),
        ("external-quarantine", ReconPolicy::external_quarantine()),
    ] {
        for d in &shapes {
            let refined = name == "external-quarantine"
                && matches!(d, Divergence::MissingFill(f) if f.client_order_id.is_none());
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
    // doc): the one opting-in constructor holds PositionDrift, so a held coid-less fill's qty
    // only ever nets against HELD drift proposals, never against an auto-applied drift fold.
    let eq = ReconPolicy::external_quarantine();
    assert!(eq.hold_external_instances);
    assert_eq!(eq.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
}

/// The journal seam: `ReconPolicy` rides the journaled `Command::ReconcileReports` payload as
/// serde_json, so (a) a pre-refinement payload — no `hold_external_instances` key — must
/// deserialize with the flag `false` (old journals replay byte-identically), and (b) a flag-false
/// policy must SERIALIZE without the key at all, keeping every pre-existing policy's journaled
/// form byte-identical to what it wrote before the field existed. The opted-in policy is the only
/// one whose payload carries the key.
#[test]
fn the_refinement_flag_is_invisible_in_serde_until_a_policy_opts_in() {
    let hybrid_json = serde_json::to_string(&ReconPolicy::hybrid()).expect("serializes");
    assert!(
        !hybrid_json.contains("hold_external_instances"),
        "a flag-false policy must serialize byte-identically to the pre-refinement shape: \
         {hybrid_json}"
    );
    let replayed: ReconPolicy = serde_json::from_str(&hybrid_json).expect("old shape replays");
    assert!(!replayed.hold_external_instances);
    let eq_json = serde_json::to_string(&ReconPolicy::external_quarantine()).expect("serializes");
    assert!(eq_json.contains("\"hold_external_instances\":true"), "{eq_json}");
    let eq: ReconPolicy = serde_json::from_str(&eq_json).expect("round-trips");
    assert!(eq.hold_external_instances);
}
