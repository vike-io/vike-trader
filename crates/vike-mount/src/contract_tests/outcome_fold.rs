//! A live outcome folded into the engine (grid, legs, multiplier, margin mode) and the tier the
//! engine carries — or, for an account that does not arm at its row's tier, no engine at all.
use super::*;

static LIVE_ROW: Planted = Planted::new(
    "bybit",
    Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
    true,
);
static LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_ROW)];

/// The fold of a live outcome: grid, legs, contract size (multiplier) and margin mode — the last
/// is the mount's half of hyperliquid's isolated-only wiring, proved here over a planted venue.
#[test]
fn a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let legs = vec!["ETHUSDT".to_string()];
    let armed = policy("bybit", AccountTier::Demo);
    let (engine, _recon) = budgeted_mount_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &legs,
        &tx,
        &mut live,
        Some(&armed),
    )
    .unwrap_or_else(|e| panic!("a budgeted live mount starts: {e}"));
    assert!(live.contains("bybit"), "a live outcome is recorded live");
    assert!(engine.gate.limits.grid_by_symbol.contains_key("ETHUSDT"), "the leg grid is folded");
    assert_eq!(engine.gate.limits.tick_size, Some(0.5), "the mounted grid is the venue's");
    assert_eq!(engine.account.multiplier_of("BTCUSDT"), 2.0);
    assert_eq!(engine.account.default_margin_mode_of("BTCUSDT"), vike_model::MarginMode::Isolated);
    assert!(LIVE_ROW.mounts.load(Ordering::SeqCst) >= 1);
}

/// A live outcome on the LIVE (real-money) tier, which the demo-wired `LIVE_ROW` never produces.
static LIVE_TIER_ROW: Planted = Planted::new(
    "bybit",
    Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
    true,
);
static LIVE_TIER_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_TIER_ROW)];

/// A bridge whose `resolve` builds no venue client (no credentials): `Resolution::Paper`.
static PAPER_OUTCOME_ROW: Planted =
    Planted::new("bybit", Resolution::Paper(PaperCause::NoCredentials), false);
static PAPER_OUTCOME_REG: [VenueRow; 1] = [VenueRow::Mount(&PAPER_OUTCOME_ROW)];

/// A bridge that ARMS at `demo` but whose mount then builds no client (a connect that failed, a
/// key the venue declined): `ExecOutcome::Paper` after an armed resolve.
static DECLINED_ROW: Planted = Planted::new(
    "bybit",
    Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
    false,
);
static DECLINED_REG: [VenueRow; 1] = [VenueRow::Mount(&DECLINED_ROW)];

/// The tier of the engine `crate::make_engine_with_legs` builds for `bybit` on `registry`.
fn tier_of_engine(registry: &'static [VenueRow], mount_policy: crate::MountPolicy) -> AccountTier {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let (engine, _recon) = budgeted_mount_with_legs(
        registry,
        "bybit",
        "BTCUSDT",
        &[],
        &tx,
        &mut live,
        Some(&mount_policy),
    )
    .unwrap_or_else(|e| panic!("a budgeted mount starts: {e}"));
    engine.tier
}

/// Every engine the mount builds carries its account's tier, which by decision 0156 is what its
/// orders go to (the Trade window design, §4.3): a `demo` row armed at demo is `Demo`, a `paper`
/// row runs the simulator and is `Paper`.
#[test]
fn an_engine_carries_its_accounts_tier() {
    assert_eq!(tier_of_engine(&LIVE_REG, policy("bybit", AccountTier::Demo)), AccountTier::Demo);
    assert_eq!(tier_of_engine(&LIVE_REG, policy("bybit", AccountTier::Paper)), AccountTier::Paper);
}

/// A LIVE-tier outcome carries `Live`. Non-vacuous: `LIVE_ROW` only ever sees `Demo`, so a
/// mutation writing `AccountTier::Demo` into `assemble_engine` passed the test above.
#[test]
fn a_live_outcome_on_the_live_tier_says_live() {
    assert_eq!(
        tier_of_engine(&LIVE_TIER_REG, policy("bybit", AccountTier::Live)),
        AccountTier::Live
    );
}

/// **A `demo` account whose bridge builds no client does NOT RUN** (decision 0156 item 1): the
/// bridge's own cause is the reason, the account states its tier, the bridge is never asked to
/// mount, and nothing goes live. Before the decision this mounted a PAPER engine under the demo
/// account's name.
#[test]
fn an_account_whose_bridge_builds_no_client_does_not_run() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let demo = policy("bybit", AccountTier::Demo);
    let stopped = not_running(budgeted_mount_with_legs(
        &PAPER_OUTCOME_REG,
        "bybit",
        "BTCUSDT",
        &[],
        &tx,
        &mut live,
        Some(&demo),
    ));
    assert_eq!(
        (stopped.venue.as_str(), &stopped.account, stopped.tier, stopped.block),
        ("bybit", &AccountLabel::Default, Some(AccountTier::Demo), ArmingBlock::NoCredentials)
    );
    assert!(live.is_empty(), "nothing goes live");
    assert_eq!(PAPER_OUTCOME_ROW.mounts.load(Ordering::SeqCst), 0, "the mount is never asked");
}

/// **An account that ARMED and whose mount then built no client does NOT RUN either**, with
/// `MountFailed` — the connect that failed and the key the venue declined (decision 0156 item 1).
/// The bridge WAS asked to mount; its paper outcome is not handed an engine.
#[test]
fn an_armed_account_whose_mount_builds_no_client_does_not_run() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let demo = policy("bybit", AccountTier::Demo);
    let before = DECLINED_ROW.mounts.load(Ordering::SeqCst);
    let stopped = not_running(budgeted_mount_with_legs(
        &DECLINED_REG,
        "bybit",
        "BTCUSDT",
        &[],
        &tx,
        &mut live,
        Some(&demo),
    ));
    assert_eq!((stopped.tier, stopped.block), (Some(AccountTier::Demo), ArmingBlock::MountFailed));
    assert!(DECLINED_ROW.mounts.load(Ordering::SeqCst) > before, "the mount was asked");
    assert!(live.is_empty(), "nothing goes live");
}

/// **A `live` account whose bridge arms only DEMO does NOT RUN** — never demo in its place (the
/// no-downgrade rule) and never paper: `LiveCredentialsAbsent`, and the bridge is never mounted.
#[test]
fn a_live_account_whose_bridge_arms_only_demo_does_not_run() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let live_row = policy("bybit", AccountTier::Live);
    let stopped = not_running(budgeted_mount_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &[],
        &tx,
        &mut live,
        Some(&live_row),
    ));
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Live), ArmingBlock::LiveCredentialsAbsent)
    );
    assert!(live.is_empty(), "nothing goes live");
}

/// **No account row: the account does NOT RUN** — the DEFAULT account of a roster venue included,
/// which used to mount a paper engine by itself (decision 0156 item 1). `None` policy is the same
/// answer: widening has to be typed.
#[test]
fn an_account_no_row_names_does_not_run() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let empty = crate::MountPolicy::default();
    for policy in [Some(&empty), None] {
        let stopped = not_running(budgeted_mount_with_legs(
            &LIVE_REG,
            "bybit",
            "BTCUSDT",
            &[],
            &tx,
            &mut live,
            policy,
        ));
        assert_eq!((stopped.tier, stopped.block), (None, ArmingBlock::NoAccountRow));
    }
    assert!(live.is_empty(), "nothing goes live");
}
