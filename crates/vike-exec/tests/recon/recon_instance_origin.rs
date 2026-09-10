//! The CROSS-MACHINE half of the duplicate-instance interlock, at the reconcile seam.
//!
//! `crates/vike-core/src/journal_lock.rs` refuses a second writer on one journal directory and its
//! module doc lists what two writers break. A file lock is a fact about ONE filesystem, so three
//! of those four survive a move to two machines — two containers with separate volumes each take
//! their own lock, each believes it is alone, and if they carry the same venue API key they trade
//! one account. `vike_model::InstanceOrigin` makes the other one RECOGNISABLE by putting an origin
//! claim inside the client order id; these tests are what hold reconcile to reading it.
//!
//! The properties, and why each is here rather than implied:
//!
//! - a fill carrying ANOTHER instance's origin does not fold, under EVERY policy — including
//!   `hybrid`, where a coid-linked `MissingFill` is one of the only two kinds that folds anything
//!   at all. That is the dangerous case: without the claim, a sibling's fill is indistinguishable
//!   from our own late-materialized one, and `hybrid` books it into local position and realized
//!   PnL at the sibling's price;
//! - an instance with NO origin configured behaves exactly as it did before the feature, asserted
//!   against the real `resolve` output rather than by inspecting a flag;
//! - the classification is one-way (it can only HOLD, never fold more), so a mis-read costs an
//!   operator an alert and never a suppressed correction;
//! - and the held alert SAYS which instance placed it, because "UnknownOrder" and "another
//!   deployment of yours is on this account" call for different operator actions.

use vike_exec::recon::{
    Divergence, DivergenceKind, DivergenceOrigin, ReconMode, ReconPolicy, mode_applies,
    mode_applies_divergence, resolve,
};
use vike_model::InstanceOrigin;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

/// This instance.
fn ours() -> InstanceOrigin {
    InstanceOrigin::parse("ap1").unwrap()
}

/// A coid minted by THIS instance, and one minted by a sibling deployment sharing the account.
const OUR_COID: &str = "ap1Vdeadbeef7";
const THEIR_COID: &str = "bx2Vdeadbeef7";
/// ...and an id from before any origin was configured — the shape every id in the tree has today.
const UNTAGGED_COID: &str = "deadbeef7";

/// Every policy an operator can name, each carrying THIS instance's identity. The set matters:
/// the hold must not be a property of one preset.
fn all_policies_knowing_us() -> Vec<(&'static str, ReconPolicy)> {
    let mut out = vec![
        ("hybrid", ReconPolicy::hybrid()),
        ("synthesize", ReconPolicy::default()),
        ("quarantine", ReconPolicy { default: ReconMode::Quarantine, ..Default::default() }),
        ("external-quarantine", ReconPolicy::external_quarantine()),
    ];
    for (_, p) in out.iter_mut() {
        p.local_instance_origin = Some(ours());
    }
    out
}

fn missing_fill(coid: &str) -> Divergence {
    Divergence::MissingFill(FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        client_order_id: Some(coid.to_string()),
        venue_order_id: "v-1".into(),
        trade_id: TradeId::new("t-1").unwrap(),
        side: 1,
        last_qty: 2.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 1_000,
    })
}

fn unknown_order(coid: &str) -> Divergence {
    Divergence::UnknownOrder(OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        client_order_id: Some(coid.to_string()),
        venue_order_id: "v-9".into(),
        status: "NEW".into(),
        side: 1,
        order_type: "LIMIT".into(),
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        ts: 1_000,
    })
}

fn drift() -> Divergence {
    Divergence::PositionDrift {
        report: PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            avg_px: 100.0,
            ts: 1_000,
            margin_mode: MarginMode::Cross,
            isolated_margin: None,
            delta: None,
        },
        local_qty: 0.0,
    }
}

/// THE property. A sibling instance's fill folds NOTHING under any policy — most importantly
/// under `hybrid`, which is the default rollout target and the one policy that auto-applies a
/// coid-linked `MissingFill`.
///
/// The contrast is the whole test: the SAME fill, same qty, same trade id, differing only in whose
/// origin the client order id claims, folds under `hybrid` when it is ours.
#[test]
fn another_instances_fill_never_folds_while_our_own_still_does() {
    for (name, policy) in all_policies_knowing_us() {
        let theirs = resolve(vec![missing_fill(THEIR_COID)], &policy, None, None);
        assert!(
            theirs.events.is_empty(),
            "{name} folded another instance's fill: {:?}",
            theirs.events
        );
        assert_eq!(theirs.alerts.len(), 1, "{name} must SURFACE it: {:?}", theirs.alerts);
        assert_eq!(theirs.alerts[0].kind, DivergenceKind::MissingFill);
    }

    // ...and the same fill from THIS instance still folds where it always did.
    let mut hybrid = ReconPolicy::hybrid();
    hybrid.local_instance_origin = Some(ours());
    let mine = resolve(vec![missing_fill(OUR_COID)], &hybrid, None, None);
    assert!(!mine.events.is_empty(), "hybrid must still fold our own order's fill");
    assert!(mine.alerts.is_empty(), "our own fill is not an operator question");
}

/// An UNTAGGED id is never foreign. An instance that turns the feature on while a sibling has not
/// is in exactly the position it was in before — not a worse one — and this is what says so.
#[test]
fn an_untagged_id_is_not_treated_as_another_instance() {
    let mut hybrid = ReconPolicy::hybrid();
    hybrid.local_instance_origin = Some(ours());
    let r = resolve(vec![missing_fill(UNTAGGED_COID)], &hybrid, None, None);
    assert!(!r.events.is_empty(), "an untagged fill must fold exactly as it always has");
    assert!(!missing_fill(UNTAGGED_COID).is_foreign_instance(Some(&ours())));
}

/// An instance with NO origin configured is byte-identical to before the feature existed —
/// asserted on the real `resolve` OUTPUT for every policy and for every id shape, so it cannot
/// pass by virtue of a flag being unset somewhere the fold never reads.
#[test]
fn an_unconfigured_instance_resolves_exactly_as_it_did_before() {
    for (name, configured) in all_policies_knowing_us() {
        let mut blind = configured.clone();
        blind.local_instance_origin = None;
        for coid in [OUR_COID, THEIR_COID, UNTAGGED_COID] {
            let with = resolve(vec![missing_fill(coid)], &blind, None, None);
            // The pre-feature answer for a coid-linked MissingFill: `hybrid`/`synthesize` fold it,
            // the two held policies surface it. Compared against the SAME policy resolving the
            // untagged id, which is the only id shape that existed before this feature.
            let baseline = resolve(vec![missing_fill(UNTAGGED_COID)], &blind, None, None);
            assert_eq!(
                with.events.len(),
                baseline.events.len(),
                "{name}: an unconfigured instance must not classify {coid} at all"
            );
            assert_eq!(with.alerts.len(), baseline.alerts.len(), "{name}/{coid}");
        }
    }
}

/// The refinement is ONE-WAY: it can turn a fold into a hold and never the reverse. Asserted over
/// every policy and every id shape by comparing the origin-aware decision with the origin-blind
/// one — a boolean implication, so a future refinement that started folding something extra fails
/// here rather than in production.
#[test]
fn the_instance_refinement_can_only_hold_never_fold() {
    for (name, policy) in all_policies_knowing_us() {
        for coid in [OUR_COID, THEIR_COID, UNTAGGED_COID] {
            for d in [missing_fill(coid), unknown_order(coid)] {
                let mut blind = policy.clone();
                blind.local_instance_origin = None;
                let aware = mode_applies_divergence(&policy, &d);
                let unaware = mode_applies_divergence(&blind, &d);
                assert!(
                    !aware || unaware,
                    "{name}/{coid}/{:?}: the origin refinement started FOLDING something",
                    d.kind()
                );
            }
        }
        // A divergence carrying no client order id at all cannot be refined either way.
        let mut blind = policy.clone();
        blind.local_instance_origin = None;
        assert_eq!(
            mode_applies_divergence(&policy, &drift()),
            mode_applies_divergence(&blind, &drift()),
            "{name}: a position drift names no order and must be untouched"
        );
    }
}

/// The KIND is untouched — only the INSTANCE reads External. Same shape as the coid-less
/// `MissingFill` refinement that came before it: the journaled per-kind mode map stays stable, so
/// an old journal's policy replays byte-identically.
#[test]
fn a_foreign_instance_reads_external_without_moving_its_kinds_tag() {
    let d = missing_fill(THEIR_COID);
    assert_eq!(d.origin_for(Some(&ours())), DivergenceOrigin::External);
    assert_eq!(
        d.kind().origin(),
        DivergenceOrigin::ReconciliationMaterialized,
        "the KIND tag must not move — the refinement is per-instance"
    );
    assert_eq!(
        d.origin(),
        DivergenceOrigin::ReconciliationMaterialized,
        "the origin-BLIND answer is unchanged, which is what keeps old callers exact"
    );
    // The mode ROW is likewise untouched: `hybrid` still auto-applies the kind.
    let mut hybrid = ReconPolicy::hybrid();
    hybrid.local_instance_origin = Some(ours());
    assert!(mode_applies(&hybrid, DivergenceKind::MissingFill));
    assert!(!mode_applies_divergence(&hybrid, &d));
}

/// The operator-facing half: the held row NAMES both tags, because an operator with three tagged
/// deployments needs to know WHICH box placed the order, not merely that it was not this one.
#[test]
fn the_held_alert_names_the_instance_that_placed_the_order() {
    let mut hybrid = ReconPolicy::hybrid();
    hybrid.local_instance_origin = Some(ours());
    let r = resolve(vec![unknown_order(THEIR_COID)], &hybrid, None, None);
    assert_eq!(r.alerts.len(), 1, "{:?}", r.alerts);
    let detail = &r.alerts[0].detail;
    // The COMPOSITION, not merely the presence of the note: the divergence's own description comes
    // first and is unchanged, so an operator reads the same row they always did with the
    // attribution appended. `docs/ops/double-live-instances.md` shows this exact shape.
    assert!(
        detail.starts_with(&unknown_order(THEIR_COID).describe()),
        "the note replaced the description instead of following it: {detail}"
    );
    assert!(detail.contains("ANOTHER INSTANCE"), "{detail}");
    assert!(detail.contains("bx2"), "the detail must name the SIBLING's tag: {detail}");
    assert!(detail.contains("ap1"), "...and ours, so the operator can tell them apart: {detail}");

    // ⚠ The alert IDENTITY must NOT pick up the note: `vike_core`'s `HeldId::new` keys an
    // un-keyed alert off `identity_detail`, so annotating it would make an unchanged divergence a
    // NEW held row every pass — the measured production bug `identity_detail` exists to prevent.
    let identity = r.alerts[0].identity_detail.as_deref().unwrap_or_default();
    assert!(!identity.contains("ANOTHER INSTANCE"), "identity picked up the note: {identity}");

    // ...and our own order gets no note at all.
    let mine = resolve(vec![unknown_order(OUR_COID)], &hybrid, None, None);
    assert_eq!(mine.alerts.len(), 1);
    assert!(!mine.alerts[0].detail.contains("ANOTHER INSTANCE"), "{:?}", mine.alerts[0]);
}

/// The JOURNAL seam. `ReconPolicy` rides the journaled `Command::ReconcileReports` payload as
/// serde_json, so an unconfigured instance's policy must serialize WITHOUT the identity key at all
/// — a payload byte-identical to what it wrote before the field existed — and a payload written
/// before it existed must replay with no identity rather than failing to parse.
///
/// The twin of `the_refinement_flag_is_invisible_in_serde_until_a_policy_opts_in`
/// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`), for the same reason: a journal is a
/// durable format and an old one has to keep replaying.
#[test]
fn the_identity_is_invisible_in_serde_until_an_instance_carries_one() {
    let plain = serde_json::to_string(&ReconPolicy::hybrid()).expect("serializes");
    assert!(
        !plain.contains("local_instance_origin"),
        "an unconfigured policy must serialize to the pre-origin shape: {plain}"
    );
    let replayed: ReconPolicy = serde_json::from_str(&plain).expect("the old shape replays");
    assert_eq!(replayed.local_instance_origin, None);

    let mut tagged = ReconPolicy::hybrid();
    tagged.local_instance_origin = Some(ours());
    let json = serde_json::to_string(&tagged).expect("serializes");
    assert!(json.contains("\"local_instance_origin\":\"ap1\""), "{json}");
    let back: ReconPolicy = serde_json::from_str(&json).expect("round-trips");
    assert_eq!(back.local_instance_origin, Some(ours()));

    // ...and a payload carrying a tag no `InstanceOrigin` could ever be is REFUSED rather than
    // silently adopted: the identity that decides a fold must go through the same validation the
    // operator's configuration does.
    let bad = json.replace("\"ap1\"", "\"NOT-A-TAG\"");
    assert!(serde_json::from_str::<ReconPolicy>(&bad).is_err(), "{bad}");
}

/// A foreign fill is excluded from the pass's fill WINDOW, not merely held.
///
/// The window exists so that a fill folding THIS pass is not counted twice — once as the fill,
/// once inside the venue's net position. "Held fills still count" rests on every constructible
/// policy that holds a fill also holding `PositionDrift`, and a foreign-instance fill breaks that
/// pairing: it is held under `hybrid`, which still auto-applies the drift. Counting it would net a
/// sibling's qty out of a correction that WILL fold, silently suppressing the re-convergence.
///
/// Asserted as an EQUALITY against the same pass with the fill absent — the drift must fold the
/// full venue-vs-local difference, exactly as it did before an origin was configured.
#[test]
fn a_foreign_fill_does_not_net_against_a_drift_that_still_folds() {
    let mut hybrid = ReconPolicy::hybrid();
    hybrid.local_instance_origin = Some(ours());
    let with_foreign = resolve(vec![missing_fill(THEIR_COID), drift()], &hybrid, None, None).events;
    let drift_alone = resolve(vec![drift()], &hybrid, None, None).events;
    assert_eq!(
        with_foreign, drift_alone,
        "a sibling's fill changed what the drift folded — the netting hazard is back"
    );
    assert!(!drift_alone.is_empty(), "the drift must actually fold something for this to bite");
}
