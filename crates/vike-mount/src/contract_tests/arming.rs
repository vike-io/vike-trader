//! Arming a contract row: paper causes, its resolve, a single-account venue, a feature-absent row.
use super::*;

/// Every `PaperCause` names its own arming block, and a held-below-live reason is reported only
/// under a `live` ceiling — the whole of `resolution_to_arming`.
#[test]
fn paper_causes_and_held_reasons_map_to_their_blocks() {
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
    let demo =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    assert_eq!(fold(demo, true), (VenueMode::Demo, ArmingBlock::DemoOnlyArm), "held under live");
    assert_eq!(fold(demo, false), (VenueMode::Demo, ArmingBlock::None), "nothing refused at demo");
    let testnet = Resolution::Armed {
        tier: Tier::Demo,
        held_below_live: Some(HeldBelowLive::LiveCredentialsAbsent),
    };
    assert_eq!(fold(testnet, true), (VenueMode::Demo, ArmingBlock::LiveCredentialsAbsent));
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

#[test]
fn a_contract_row_arms_through_its_resolve() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&DEMO_REG, "binance", &vars, VenueMode::Live),
        (VenueMode::Demo, ArmingBlock::DemoOnlyArm)
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
        VenueMode::Live,
        None,
    );
    assert_eq!(addressable, (VenueMode::Demo, ArmingBlock::DemoOnlyArm));
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
    // …and the ceiling is still read first: a `paper` line is `Disarmed`, as the legacy row said.
    assert_eq!(
        crate::venue_arming_under(&ABSENT_REG, "ibkr", &vars, VenueMode::Paper),
        (VenueMode::Paper, ArmingBlock::Disarmed)
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
