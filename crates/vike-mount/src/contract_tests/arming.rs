//! Arming a contract row: paper causes, its resolve, a single-account venue, a feature-absent row.
use super::*;

/// Every `PaperCause` names its own arming block, and a DEMO arming under a `live` tier is PAPER
/// (the no-downgrade rule) whatever the bridge's held-below reason — the whole of
/// `resolution_to_arming`.
#[test]
fn paper_causes_map_to_their_blocks_and_a_live_tier_never_arms_demo() {
    use crate::contract::resolution_to_arming as fold;
    let table = [
        (PaperCause::NoCredentials, ArmingBlock::NoCredentials),
        (PaperCause::LiveCredentialsAbsent, ArmingBlock::LiveCredentialsAbsent),
        (PaperCause::LiveTierNotWired, ArmingBlock::LiveTierNotWired),
        (PaperCause::ExecFlagUnset, ArmingBlock::ExecFlagUnset),
        (PaperCause::LiveOnlyArm, ArmingBlock::LiveOnlyArm),
        (PaperCause::SdkAbsent, ArmingBlock::SdkAbsent),
        (PaperCause::AccountNotInStore, ArmingBlock::AccountNotInStore),
        (PaperCause::NoLiveArm, ArmingBlock::NoLiveArm),
    ];
    // The table must be the WHOLE of `PaperCause`: `position` is an exhaustive match (a new cause
    // fails to compile without a slot) and `seen` is sized to the slots (a slot with no row fails
    // the count). A new cause cannot exist without a block, a badge and a sentence.
    fn position(cause: PaperCause) -> usize {
        match cause {
            PaperCause::NoCredentials => 0,
            PaperCause::LiveCredentialsAbsent => 1,
            PaperCause::LiveTierNotWired => 2,
            PaperCause::ExecFlagUnset => 3,
            PaperCause::LiveOnlyArm => 4,
            PaperCause::SdkAbsent => 5,
            PaperCause::AccountNotInStore => 6,
            PaperCause::NoLiveArm => 7,
        }
    }
    let mut seen = [0usize; 8];
    for (cause, block) in table {
        assert_eq!(fold(Resolution::Paper(cause), true), (VenueMode::Paper, block), "{cause:?}");
        assert_eq!(fold(Resolution::Paper(cause), false), (VenueMode::Paper, block), "{cause:?}");
        seen[position(cause)] += 1;
    }
    assert_eq!(seen, [1; 8], "every PaperCause has exactly one row in the table above");
    // A DEMO arming, for every held-below reason a bridge can give (none included): under a `demo`
    // tier it arms demo; under a `live` tier it is PAPER, `LiveCredentialsAbsent` — never demo.
    for held in [None, Some(HeldBelowLive::DemoOnlyArm), Some(HeldBelowLive::LiveCredentialsAbsent)]
    {
        let demo = Resolution::Armed { tier: Tier::Demo, held_below_live: held };
        assert_eq!(
            fold(demo, true),
            (VenueMode::Paper, ArmingBlock::LiveCredentialsAbsent),
            "a live account never trades demo ({held:?})"
        );
        assert_eq!(fold(demo, false), (VenueMode::Demo, ArmingBlock::None), "demo at demo");
    }
    assert_eq!(
        fold(Resolution::Armed { tier: Tier::Live, held_below_live: None }, true),
        (VenueMode::Live, ArmingBlock::None)
    );
}

static DEMO_ROW: Planted = Planted::new(
    "binance",
    Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) },
    true,
);
static DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&DEMO_ROW)];

/// A contract row arms through its `resolve` at its tier — and a demo-only arm under a `live` tier
/// is PAPER, the projection agreeing with the mount (`contract_parts`' resolve half).
#[test]
fn a_contract_row_arms_through_its_resolve() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&DEMO_REG, "binance", &vars, VenueMode::Live),
        (VenueMode::Paper, ArmingBlock::LiveCredentialsAbsent)
    );
    assert_eq!(
        crate::venue_arming_under(&DEMO_REG, "binance", &vars, VenueMode::Demo),
        (VenueMode::Demo, ArmingBlock::None)
    );
}

static SINGLE: Planted = Planted {
    addresses_accounts: false,
    ..Planted::new("okx", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, false)
};
static SINGLE_REG: [VenueRow; 1] = [VenueRow::Mount(&SINGLE)];

/// Review Focus 5 — and its positive half: the SAME labelled account on a row that DOES address
/// accounts reaches `resolve`, so the refusal is the declaration's, not a blanket one.
#[test]
fn a_labelled_account_is_refused_before_resolve_on_a_single_account_venue() {
    let label =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let before = SINGLE.resolves.load(Ordering::SeqCst);
    let row = crate::arming::account_arming_under(
        &SINGLE_REG,
        "okx",
        &label,
        &HashMap::new(),
        VenueMode::Live,
        None,
    );
    assert_eq!(row, (VenueMode::Paper, ArmingBlock::NoAccountSupport));
    assert_eq!(SINGLE.resolves.load(Ordering::SeqCst), before, "resolve is never asked");

    let asked_before = DEMO_ROW.resolves.load(Ordering::SeqCst);
    let addressable = crate::arming::account_arming_under(
        &DEMO_REG,
        "binance",
        &label,
        &HashMap::new(),
        VenueMode::Demo,
        None,
    );
    assert_eq!(addressable, (VenueMode::Demo, ArmingBlock::None));
    assert!(
        DEMO_ROW.resolves.load(Ordering::SeqCst) > asked_before,
        "an addressing row asks its resolve for the labelled account"
    );
}

static ABSENT_REG: [VenueRow; 1] = [VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" }];

#[test]
fn a_feature_absent_row_is_paper_with_its_block_and_mounts_the_paper_client() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&ABSENT_REG, "ibkr", &vars, VenueMode::Live),
        (VenueMode::Paper, ArmingBlock::FeatureAbsent)
    );
    // …and the tier is still read first: a `paper` tier is `PaperTier`.
    assert_eq!(
        crate::venue_arming_under(&ABSENT_REG, "ibkr", &vars, VenueMode::Paper),
        (VenueMode::Paper, ArmingBlock::PaperTier)
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("ibkr", VenueMode::Live);
    let mut env = crate::MountEnv::new(&ABSENT_REG, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&armed);
    let (_engine, recon) =
        crate::make_engine(&mut env, "ibkr", "AAPL").expect("paper never refuses");
    assert!(live.is_empty() && recon.is_none());
}

// ---- the account table reaches the mount ----

static CONFLICTED: Planted =
    Planted::new("okx", Resolution::Armed { tier: Tier::Live, held_below_live: None }, true);
static CONFLICTED_REG: [VenueRow; 1] = [VenueRow::Mount(&CONFLICTED)];

/// **Two ACTIVE non-paper tiers for one account mount PAPER, `TierConflict`** — no pick between a
/// demo and a mainnet key set: the bridge is never asked to resolve or mount, nothing goes live,
/// and the projection names the conflict (with the rows to choose between). Control: the same
/// account with one of the two rows deactivated mounts live.
#[test]
fn two_active_tiers_of_one_account_mount_paper_with_tier_conflict() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let vars = HashMap::new();
    let conflicted = crate::MountPolicy::default()
        .with_account("okx", &AccountLabel::Default, VenueMode::Demo)
        .with_account("okx", &AccountLabel::Default, VenueMode::Live);
    let rows = crate::venue_account_arming(&CONFLICTED_REG, "okx", &vars, Some(&conflicted));
    assert_eq!(
        (rows[0].tier, rows[0].effective, rows[0].block),
        (VenueMode::Paper, VenueMode::Paper, ArmingBlock::TierConflict)
    );
    assert_eq!(rows[0].account_ids, vec![1, 2], "the remedy names both rows");

    let mut live = HashSet::new();
    mount_one(
        &CONFLICTED_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&conflicted),
    )
    .expect("a conflict is paper, not an error");
    assert!(live.is_empty(), "no tier is picked");
    assert_eq!(CONFLICTED.resolves.load(Ordering::SeqCst), 0, "the bridge is never asked");
    assert_eq!(CONFLICTED.mounts.load(Ordering::SeqCst), 0);

    let one_active = crate::MountPolicy::default()
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: "okx".to_string(),
            tier: VenueMode::Demo.as_str().to_string(),
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
        })
        .with_account("okx", &AccountLabel::Default, VenueMode::Live);
    mount_one(
        &CONFLICTED_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&one_active),
    )
    .expect("one active tier mounts");
    assert!(live.contains("okx"), "deactivating one row is the whole remedy");
}

static EXPOSED: Planted =
    Planted::new("bybit", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true);
static EXPOSED_REG: [VenueRow; 1] = [VenueRow::Mount(&EXPOSED)];

/// A policy with the box-wide `max_account_exposure` at `box_wide` and ONE active DEFAULT row of
/// `bybit` at `tier` whose own `max_exposure` is `account`.
fn exposure_policy(box_wide: f64, tier: VenueMode, account: f64) -> crate::MountPolicy {
    crate::MountPolicy { max_account_exposure: Some(box_wide), ..Default::default() }
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: "bybit".to_string(),
            tier: tier.as_str().to_string(),
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: vike_secrets::MaxExposure::new(account),
        })
}

/// **The account row's `max_exposure` NARROWS the box-wide ceiling and never widens it** — on a
/// live-armed engine (the shared tail) and on a paper-tier engine (`paper_engine`) alike, so a
/// paper rehearsal is never the permissive side of the mount it rehearses.
#[test]
fn an_accounts_max_exposure_narrows_the_box_wide_ceiling_and_never_widens_it() {
    let (tx, _rx) = vike_exec::event_channel(8);
    for tier in [VenueMode::Demo, VenueMode::Paper] {
        for (account, expected) in [(4_000.0, 4_000.0), (50_000.0, 10_000.0)] {
            let mut live = HashSet::new();
            let policy = exposure_policy(10_000.0, tier, account);
            let (engine, _recon) = budgeted_mount_with_legs(
                &EXPOSED_REG,
                "bybit",
                "BTCUSDT",
                &[],
                &tx,
                &mut live,
                Some(&policy),
            )
            .expect("a budgeted mount starts");
            assert_eq!(live.contains("bybit"), tier == VenueMode::Demo, "{tier}: armed as planted");
            assert_eq!(
                engine.gate.limits.max_account_exposure,
                Some(expected),
                "{tier}: min(box 10000, account {account})"
            );
        }
    }
}
