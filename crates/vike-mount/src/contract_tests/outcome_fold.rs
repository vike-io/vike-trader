//! A live outcome folded into the engine (grid, legs, multiplier, margin mode) and its engine mode.
use super::*;

static LIVE_ROW: Planted =
    Planted::new("bybit", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true);
static LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_ROW)];

/// The fold of a live outcome: grid, legs, contract size (multiplier) and margin mode — the last
/// is the mount's half of hyperliquid's isolated-only wiring, proved here over a planted venue.
#[test]
fn a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let legs = vec!["ETHUSDT".to_string()];
    let armed = policy("bybit", VenueMode::Demo);
    let (engine, _recon) = budgeted_mount_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &legs,
        &tx,
        &mut live,
        Some(&armed),
    )
    .expect("a budgeted live mount starts");
    assert!(live.contains("bybit"), "a live outcome is recorded live");
    assert!(engine.gate.limits.grid_by_symbol.contains_key("ETHUSDT"), "the leg grid is folded");
    assert_eq!(engine.gate.limits.tick_size, Some(0.5), "the mounted grid is the venue's");
    assert_eq!(engine.account.multiplier_of("BTCUSDT"), 2.0);
    assert_eq!(engine.account.default_margin_mode_of("BTCUSDT"), vike_model::MarginMode::Isolated);
    assert!(LIVE_ROW.mounts.load(Ordering::SeqCst) >= 1);
}

/// A live outcome on the LIVE (real-money) tier, which the demo-wired `LIVE_ROW` never produces.
static LIVE_TIER_ROW: Planted =
    Planted::new("bybit", Resolution::Armed { tier: Tier::Live, held_below_live: None }, true);
static LIVE_TIER_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_TIER_ROW)];

/// An OUTCOME that built no venue client (no credentials): `ExecOutcome::Paper`, `live == false`.
static PAPER_OUTCOME_ROW: Planted =
    Planted::new("bybit", Resolution::Paper(PaperCause::NoCredentials), false);
static PAPER_OUTCOME_REG: [VenueRow; 1] = [VenueRow::Mount(&PAPER_OUTCOME_ROW)];

/// Every engine the mount builds publishes what stands behind its orders: a live outcome states
/// its tier, and a mount capped to paper says PAPER (the Trade window design, §4.3).
#[test]
fn a_mount_says_what_stands_behind_its_orders() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let demo = policy("bybit", VenueMode::Demo);
    let (engine, _recon) =
        budgeted_mount_with_legs(&LIVE_REG, "bybit", "BTCUSDT", &[], &tx, &mut live, Some(&demo))
            .expect("a budgeted demo mount starts");
    assert_eq!(engine.mode, vike_exec::EngineMode::Demo);

    let capped = policy("bybit", VenueMode::Paper);
    let (engine, _recon) =
        budgeted_mount_with_legs(&LIVE_REG, "bybit", "BTCUSDT", &[], &tx, &mut live, Some(&capped))
            .expect("a mount capped to paper starts");
    assert_eq!(engine.mode, vike_exec::EngineMode::Paper);
}

/// The mode of the engine `crate::make_engine_with_legs` builds for `bybit` on `registry`.
fn engine_mode_of(
    registry: &'static [VenueRow],
    mount_policy: crate::MountPolicy,
) -> vike_exec::EngineMode {
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
    .expect("a budgeted mount starts");
    engine.mode
}

/// A LIVE-tier outcome publishes `Live`. Non-vacuous: `LIVE_ROW` only ever sees `Demo`, so a
/// mutation writing `EngineMode::Demo` into `assemble_engine` passed the test above.
#[test]
fn a_live_outcome_on_the_live_tier_says_live() {
    assert_eq!(
        engine_mode_of(&LIVE_TIER_REG, policy("bybit", VenueMode::Live)),
        vike_exec::EngineMode::Live
    );
}

/// No venue client, at a `demo` tier: PAPER through the shared tail — a different
/// road from the capped half above, which returns before the tail.
#[test]
fn an_outcome_with_no_venue_client_says_paper_through_the_shared_tail() {
    assert_eq!(
        engine_mode_of(&PAPER_OUTCOME_REG, policy("bybit", VenueMode::Demo)),
        vike_exec::EngineMode::Paper
    );
}

/// All eight `(live, tier)` cells of `engine_mode`: no client is PAPER whatever the tier; a built
/// one is `Demo` only for the demo tier, else `Live` (unrecorded or paper-recorded too): wrong, if
/// at all, in the direction that costs least.
#[test]
fn engine_mode_reads_the_verdict_then_the_tier() {
    use vike_exec::EngineMode;
    let cells = [
        ((false, None), EngineMode::Paper),
        ((false, Some(VenueMode::Paper)), EngineMode::Paper),
        ((false, Some(VenueMode::Demo)), EngineMode::Paper),
        ((false, Some(VenueMode::Live)), EngineMode::Paper),
        ((true, None), EngineMode::Live),
        ((true, Some(VenueMode::Paper)), EngineMode::Live),
        ((true, Some(VenueMode::Demo)), EngineMode::Demo),
        ((true, Some(VenueMode::Live)), EngineMode::Live),
    ];
    assert_eq!(cells.len(), 8, "two verdicts times four tiers: none, paper, demo and live");
    for ((live, tier), expected) in cells {
        assert_eq!(crate::engine_mode(live, tier), expected, "engine_mode({live}, {tier:?})");
    }
}
