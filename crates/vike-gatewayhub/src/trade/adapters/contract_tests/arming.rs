//! Arming a contract row: paper causes, its resolve, a single-account venue, a feature-absent row.
use super::*;

/// Every `PaperCause` names its own arming block and leaves the account NOT RUNNING, and a DEMO
/// arming under a `live` tier does not run either (the no-downgrade rule) whatever the bridge's
/// held-below reason — the whole of `resolution_to_arming`. Nothing here ever answers `Paper` for
/// a `demo` or `live` account (decision 0156 item 1).
#[test]
fn paper_causes_map_to_their_blocks_and_a_live_tier_never_arms_demo() {
    use crate::trade::adapters::contract::resolution_to_arming as fold;
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
        assert_eq!(fold(Resolution::Paper(cause), AccountTier::Live), (None, block), "{cause:?}");
        assert_eq!(fold(Resolution::Paper(cause), AccountTier::Demo), (None, block), "{cause:?}");
        assert!(!block.runs(), "{cause:?}: {block:?} must not run");
        seen[position(cause)] += 1;
    }
    assert_eq!(seen, [1; 8], "every PaperCause has exactly one row in the table above");
    // A DEMO arming, for every held-below reason a bridge can give (none included): under a `demo`
    // tier it arms demo; under a `live` tier it does NOT RUN, `LiveCredentialsAbsent` — never demo.
    for held in [None, Some(HeldBelowLive::DemoOnlyArm), Some(HeldBelowLive::LiveCredentialsAbsent)]
    {
        let demo = Resolution::Armed { tier: AccountTier::Demo, held_below_live: held };
        assert_eq!(
            fold(demo, AccountTier::Live),
            (None, ArmingBlock::LiveCredentialsAbsent),
            "a live account never trades demo ({held:?})"
        );
        assert_eq!(
            fold(demo, AccountTier::Demo),
            (Some(AccountTier::Demo), ArmingBlock::None),
            "demo at demo"
        );
    }
    assert_eq!(
        fold(
            Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
            AccountTier::Live
        ),
        (Some(AccountTier::Live), ArmingBlock::None)
    );
}

static DEMO_ROW: Planted = Planted::new(
    "binance",
    Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    },
    true,
);
static DEMO_REG: [VenueRow; 1] = [VenueRow::Adapter(&DEMO_ROW)];

/// A contract row arms through its `resolve` at its tier — and a demo-only arm under a `live` tier
/// does NOT RUN, the projection agreeing with the start (`contract_parts`' resolve half).
#[test]
fn a_contract_row_arms_through_its_resolve() {
    let vars = HashMap::new();
    assert_eq!(
        crate::trade::adapters::arming::venue_arming_under(
            &DEMO_REG,
            "binance",
            &vars,
            no_creds(),
            AccountTier::Live
        ),
        (None, ArmingBlock::LiveCredentialsAbsent)
    );
    assert_eq!(
        crate::trade::adapters::arming::venue_arming_under(
            &DEMO_REG,
            "binance",
            &vars,
            no_creds(),
            AccountTier::Demo
        ),
        (Some(AccountTier::Demo), ArmingBlock::None)
    );
}

static SINGLE: Planted = Planted {
    addresses_accounts: false,
    ..Planted::new(
        "okx",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        false,
    )
};
static SINGLE_REG: [VenueRow; 1] = [VenueRow::Adapter(&SINGLE)];

/// Review Focus 5 — and its positive half: the SAME labelled account on a row that DOES address
/// accounts reaches `resolve`, so the refusal is the declaration's, not a blanket one.
#[test]
fn a_labelled_account_is_refused_before_resolve_on_a_single_account_venue() {
    let label =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let before = SINGLE.resolves.load(Ordering::SeqCst);
    let row = crate::trade::adapters::arming::account_arming_under(
        &SINGLE_REG,
        "okx",
        &label,
        &HashMap::new(),
        AccountTier::Live,
        None,
    );
    assert_eq!(row, (None, ArmingBlock::NoAccountSupport));
    assert_eq!(SINGLE.resolves.load(Ordering::SeqCst), before, "resolve is never asked");

    let asked_before = DEMO_ROW.resolves.load(Ordering::SeqCst);
    let addressable = crate::trade::adapters::arming::account_arming_under(
        &DEMO_REG,
        "binance",
        &label,
        &HashMap::new(),
        AccountTier::Demo,
        None,
    );
    assert_eq!(addressable, (Some(AccountTier::Demo), ArmingBlock::None));
    assert!(
        DEMO_ROW.resolves.load(Ordering::SeqCst) > asked_before,
        "an addressing row asks its resolve for the labelled account"
    );
}

static ABSENT_REG: [VenueRow; 1] = [VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" }];

/// A venue whose bridge this build does not compile runs no `demo` or `live` account — NOT
/// RUNNING, `FeatureAbsent`, and no paper client in its place (decision 0156 item 1: this was the
/// `absent_parts` substitute) — while a `paper` account of it still runs the simulator.
#[test]
fn a_feature_absent_row_does_not_run_a_demo_or_live_account() {
    let vars = HashMap::new();
    assert_eq!(
        crate::trade::adapters::arming::venue_arming_under(
            &ABSENT_REG,
            "ibkr",
            &vars,
            no_creds(),
            AccountTier::Live
        ),
        (None, ArmingBlock::FeatureAbsent)
    );
    // …and the tier is still read first: a `paper` tier runs, `PaperTier`.
    assert_eq!(
        crate::trade::adapters::arming::venue_arming_under(
            &ABSENT_REG,
            "ibkr",
            &vars,
            no_creds(),
            AccountTier::Paper
        ),
        (Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("ibkr", AccountTier::Live);
    let rig = Rig::new(&ABSENT_REG, &vars, Some(&armed));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.policy = Some(&armed);
    let stopped = not_running(vike_tradehub_core::make_engine(&mut env, "ibkr", "AAPL"));
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Live), ArmingBlock::FeatureAbsent),
        "the account states its tier and why it does not run"
    );
    assert!(live.is_empty(), "nothing goes live");
    // CONTROL: the same venue's `paper` account runs the simulator.
    let paper = policy("ibkr", AccountTier::Paper);
    let mut live = HashSet::new();
    let rig = Rig::new(&ABSENT_REG, &vars, Some(&paper));
    let mut env = rig.env(&tx, &mut live);
    env.policy = Some(&paper);
    let (engine, recon) = vike_tradehub_core::make_engine(&mut env, "ibkr", "AAPL")
        .unwrap_or_else(|e| panic!("a paper row runs: {e}"));
    assert_eq!(engine.tier, AccountTier::Paper);
    assert!(live.is_empty() && recon.is_none());
}

// ---- the account table reaches the start ----

static CONFLICTED: Planted =
    Planted::new("okx", Resolution::Armed { tier: AccountTier::Live, held_below_live: None }, true);
static CONFLICTED_REG: [VenueRow; 1] = [VenueRow::Adapter(&CONFLICTED)];

/// **Two ACTIVE non-paper tiers for one account do NOT RUN, `TierConflict`** — no pick between a
/// demo and a mainnet key set, and no paper engine in its place: the bridge is never asked to
/// resolve or start, nothing goes live, and the projection names the conflict (with the rows to
/// choose between). Control: the same account with one of the two rows deactivated starts live.
#[test]
fn two_active_tiers_of_one_account_do_not_run_with_tier_conflict() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let vars = HashMap::new();
    let conflicted = vike_tradehub_core::AdapterPolicy::default()
        .with_account("okx", &AccountLabel::Default, AccountTier::Demo)
        .with_account("okx", &AccountLabel::Default, AccountTier::Live);
    let rows =
        crate::trade::adapters::testutil::rows_of(&CONFLICTED_REG, "okx", &vars, Some(&conflicted));
    assert_eq!(
        (rows[0].tier, rows[0].effective, rows[0].block),
        (None, None, ArmingBlock::TierConflict)
    );
    assert!(!rows[0].runs(), "a conflicted account does not run");
    assert_eq!(rows[0].account_ids, vec![1, 2], "the remedy names both rows");

    let mut live = HashSet::new();
    let stopped = not_running(start_one(
        &CONFLICTED_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&conflicted),
    ));
    assert_eq!((stopped.tier, stopped.block), (None, ArmingBlock::TierConflict));
    assert!(live.is_empty(), "no tier is picked");
    assert_eq!(CONFLICTED.resolves.load(Ordering::SeqCst), 0, "the bridge is never asked");
    assert_eq!(CONFLICTED.mounts.load(Ordering::SeqCst), 0);

    let one_active = vike_tradehub_core::AdapterPolicy::default()
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: "okx".to_string(),
            tier: AccountTier::Demo,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
        })
        .with_account("okx", &AccountLabel::Default, AccountTier::Live);
    let (engine, _recon) = start_one(
        &CONFLICTED_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&one_active),
    )
    .unwrap_or_else(|e| panic!("one active tier mounts: {e}"));
    assert!(live.contains("okx"), "deactivating one row is the whole remedy");
    assert_eq!(engine.tier, AccountTier::Live, "at the tier its one active row states");
}

static EXPOSED: Planted = Planted::new(
    "bybit",
    Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
    true,
);
static EXPOSED_REG: [VenueRow; 1] = [VenueRow::Adapter(&EXPOSED)];

/// A policy with the box-wide `max_account_exposure` at `box_wide` and ONE active DEFAULT row of
/// `bybit` at `tier` whose own `max_exposure` is `account`.
fn exposure_policy(
    box_wide: f64,
    tier: AccountTier,
    account: f64,
) -> vike_tradehub_core::AdapterPolicy {
    vike_tradehub_core::AdapterPolicy { max_account_exposure: Some(box_wide), ..Default::default() }
        .with_account_row(vike_secrets::Account {
            id: 1,
            venue: "bybit".to_string(),
            tier,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: vike_secrets::MaxExposure::new(account),
            leverage: None,
        })
}

/// **The account row's `max_exposure` NARROWS the box-wide ceiling and never widens it** — on a
/// live-armed engine (the shared tail) and on a paper-tier engine (`paper_engine`) alike, so a
/// paper rehearsal is never the permissive side of the start it rehearses.
#[test]
fn an_accounts_max_exposure_narrows_the_box_wide_ceiling_and_never_widens_it() {
    let (tx, _rx) = vike_exec::event_channel(8);
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        for (account, expected) in [(4_000.0, 4_000.0), (50_000.0, 10_000.0)] {
            let mut live = HashSet::new();
            let policy = exposure_policy(10_000.0, tier, account);
            let (engine, _recon) = budgeted_start_with_legs(
                &EXPOSED_REG,
                "bybit",
                "BTCUSDT",
                &[],
                &tx,
                &mut live,
                Some(&policy),
            )
            .unwrap_or_else(|e| panic!("{tier}: a budgeted mount starts: {e}"));
            assert_eq!(
                live.contains("bybit"),
                tier == AccountTier::Demo,
                "{tier}: armed as planted"
            );
            assert_eq!(engine.tier, tier, "{tier}: the engine carries its account's tier");
            assert_eq!(
                engine.gate.limits.max_account_exposure,
                Some(expected),
                "{tier}: min(box 10000, account {account})"
            );
        }
    }
}

/// A policy with ONE active DEFAULT row of `bybit` at `tier` and `ceiling` as the box-wide leverage
/// ceiling (`None` = no `policy.max_leverage` row).
fn leverage_policy(ceiling: Option<f64>, tier: AccountTier) -> vike_tradehub_core::AdapterPolicy {
    vike_tradehub_core::AdapterPolicy { max_leverage: ceiling, ..Default::default() }.with_account(
        "bybit",
        &AccountLabel::Default,
        tier,
    )
}

/// **`policy.max_leverage` NARROWS the run profile's leverage at the one start seam, and a box
/// without the row is byte-identical to before** — on a live-armed engine (the shared tail) and on
/// a paper-tier engine (`paper_engine`) alike, so a paper rehearsal is never the permissive side of
/// the start it rehearses. Each row is `(profile's max_leverage, ceiling, declared max_leverage,
/// initial-margin fraction the buying-power lane reads)`:
///
/// * above the ceiling: lowered to it, and `im = 1/ceiling`;
/// * at or under it: untouched — a ceiling never RAISES a leverage;
/// * no ceiling (no row): exactly the profile's own number — the box that has not written the key;
/// * a profile that names no leverage: the start's own 1× floor (`im = 1.0`) and no declared cap,
///   whatever the ceiling — a ceiling must not SET a leverage.
#[test]
fn the_leverage_ceiling_narrows_the_profile_for_the_gate_and_never_sets_or_raises() {
    type Row = (Option<f64>, Option<f64>, Option<f64>, f64);
    let rows: [Row; 6] = [
        (Some(10.0), Some(4.0), Some(4.0), 0.25),
        (Some(2.0), Some(4.0), Some(2.0), 0.5),
        (Some(4.0), Some(4.0), Some(4.0), 0.25),
        (Some(10.0), None, Some(10.0), 0.1),
        (None, Some(4.0), None, 1.0),
        (None, None, None, 1.0),
    ];
    let (tx, _rx) = vike_exec::event_channel(8);
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        for (held, ceiling, want_declared, want_im) in rows {
            let risk = vike_model::ProfileRisk { max_leverage: held, ..budget() };
            let policy = leverage_policy(ceiling, tier);
            let mut live = HashSet::new();
            let (engine, _recon) = start_one(
                &EXPOSED_REG,
                "bybit",
                "BTCUSDT",
                &tx,
                &mut live,
                Some(&risk),
                Some(&policy),
            )
            .unwrap_or_else(|e| panic!("{tier}: a budgeted mount starts: {e}"));
            assert_eq!(engine.tier, tier, "{tier}: the engine carries its account's tier");
            let limits = &engine.gate.limits;
            assert_eq!(
                limits.max_leverage, want_declared,
                "{tier}: profile {held:?} under ceiling {ceiling:?}"
            );
            assert_eq!(
                limits.im_requirement,
                Some(want_im),
                "{tier}: profile {held:?} under ceiling {ceiling:?}: the buying-power lane's margin"
            );
        }
    }
}

/// The default account with nothing stored — the planted venues here read no credential.
fn no_creds() -> vike_secrets::account_credentials::AccountCredentials<'static> {
    vike_secrets::account_credentials::AccountCredentials::none()
}
