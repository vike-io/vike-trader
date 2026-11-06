//! Liquidity-rewards-aware quoting (steal/mm-rewards-quoting): the band clamp at `min_size`, the
//! min-order-age hold, and the two safety authorities (A-S blackout / breaker pull) that outrank it.

use super::*;

// ---- liquidity-rewards-aware quoting (steal/mm-rewards-quoting) ----
//
// The reward fold pulls the quote into a venue's reward band at min_size on both sides and holds
// an in-band quote to the moas floor. These pin: OFF-by-default is byte-identical; ON pulls a
// too-wide quote into the band at min_size; the A-S blackout is never reward-clamped; and the moas
// holds a young in-band quote then releases it.

/// A rewardable bag (weight, band-cents, min_size, moas) for the integration tests.
fn reward_bag(weight: f64, max_spread_cents: f64, min_size: f64, moas: i64) -> RewardParams {
    RewardParams { weight, max_spread_cents, min_size, min_order_age_ms: moas }
}

// OFF BY DEFAULT (the neutral-reduction gate): an unconfigured maker — and one with an explicit
// weight-0 bag — quotes the unchanged mid formula at the unchanged size, bit-for-bit.
#[test]
fn reward_off_by_default_quotes_unchanged() {
    let mut mm = SpreadMaker::new(1.0, 0.05); // half_spread 0.05 ⇒ candidate 0.45/0.55 on a 0.50 mid
    assert!(mm.cfg.reward.is_none(), "reward chasing is OFF by default");
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 0.48, 0.52)); // book mid 0.50
    let mid = 0.5 * (0.48_f64 + 0.52);
    assert_eq!(
        submit_at(&b0, "bid").0.to_bits(),
        (mid - 0.05).to_bits(),
        "bid unchanged (mid − hs)"
    );
    assert_eq!(
        submit_at(&b0, "ask").0.to_bits(),
        (mid + 0.05).to_bits(),
        "ask unchanged (mid + hs)"
    );
    assert_eq!(submit_at(&b0, "bid").1.to_bits(), 1.0_f64.to_bits(), "bid size unchanged");

    // an explicitly weight-0 bag is likewise inert — same price AND size
    let mut inert =
        SpreadMaker::new(1.0, 0.05).with_liquidity_rewards(reward_bag(0.0, 3.0, 100.0, 30_000));
    let mut b1 = broker(0.0, 1);
    inert.on_quote_tick(&mut b1, &quote(1, 0.48, 0.52));
    assert_eq!(
        submit_at(&b1, "bid").0.to_bits(),
        (mid - 0.05).to_bits(),
        "weight 0 ⇒ price unchanged"
    );
    assert_eq!(
        submit_at(&b1, "bid").1.to_bits(),
        1.0_f64.to_bits(),
        "weight 0 ⇒ size unchanged (no min_size floor)"
    );
}

// ON with a rewardable market: a too-wide candidate is PULLED INTO the band, and both sides are
// floored UP to min_size — the core reward-eligibility behavior.
#[test]
fn reward_on_pulls_quote_into_band_at_min_size() {
    // weight 0.5, band 3¢ (0.03), min_size 100; the half_spread-0.05 candidate is OUT of band.
    let mut mm = SpreadMaker::new(1.0, 0.05).with_liquidity_rewards(reward_bag(0.5, 3.0, 100.0, 0));
    let mut b = broker(0.0, 1);
    mm.on_quote_tick(&mut b, &quote(1, 0.48, 0.52)); // book mid 0.50, candidate 0.45 / 0.55
    let (bid_px, bid_sz) = submit_at(&b, "bid");
    let (ask_px, ask_sz) = submit_at(&b, "ask");
    // pulled INTO the band: each side within 3¢ (0.03) of the mid 0.50 …
    assert!((0.50 - bid_px) <= 0.03 + 1e-12, "bid pulled in-band: {bid_px}");
    assert!((ask_px - 0.50) <= 0.03 + 1e-12, "ask pulled in-band: {ask_px}");
    // … specifically to s_target = band·(1−weight) = 0.03·0.5 = 0.015 (the candidate was wider)
    assert!((bid_px - 0.485).abs() < 1e-12, "bid at mid − 0.015: {bid_px}");
    assert!((ask_px - 0.515).abs() < 1e-12, "ask at mid + 0.015: {ask_px}");
    // sizes floored UP to min_size on BOTH sides (eligibility)
    assert_eq!(bid_sz.to_bits(), 100.0_f64.to_bits(), "bid size floored to min_size");
    assert_eq!(ask_sz.to_bits(), 100.0_f64.to_bits(), "ask size floored to min_size");
}

// SAFETY: inside the A-S near-resolution blackout the maker quotes at the WALLS and is NEVER
// reward-clamped back into the band (that would farm rewards into the resolution vol spike).
#[test]
fn reward_never_clamps_through_an_as_blackout() {
    // A-S in time-to-resolution with a blackout; reward chasing ON at full weight.
    let as_params = AsParams {
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(2_000),
        resolution_blackout_ms: 200, // ts >= 1800 is blacked out
        variance_mode: VarianceMode::PureBernoulli,
        kappa_mode: KappaMode::Fixed,
        gamma: 0.5,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        ..AsParams::default()
    };
    let mut mm = SpreadMaker::new(1.0, 0.5)
        .with_quote_style(QuoteStyle::Mid, 1, 0.01)
        .with_avellaneda_stoikov(as_params)
        .with_liquidity_rewards(reward_bag(1.0, 3.0, 0.0, 0));
    // INSIDE the blackout (ts 1900): pushed to the walls [tick, 1−tick] = [0.01, 0.99], NOT
    // reward-clamped back into the ±3¢ band around 0.50.
    let mut b = broker(0.0, 1_900);
    mm.on_quote_tick(&mut b, &quote(1_900, 0.49, 0.51)); // mid 0.50
    assert_eq!(
        submit_at(&b, "bid").0.to_bits(),
        0.01_f64.to_bits(),
        "blackout bid stays at the wall (not reward-clamped in)"
    );
    assert_eq!(
        submit_at(&b, "ask").0.to_bits(),
        0.99_f64.to_bits(),
        "blackout ask stays at the wall (not reward-clamped in)"
    );
}

// The moas gate HOLDS a young in-band quote (so it does not churn below the reward floor) then
// RELEASES it once it has rested >= min_order_age_ms.
#[test]
fn reward_moas_holds_a_young_in_band_quote_then_releases() {
    // reward ON with a 30s moas; NO refresh tolerance, so absent the moas every tick would
    // re-price — the moas is what holds the young in-band quote.
    let mut mm =
        SpreadMaker::new(1.0, 0.05).with_liquidity_rewards(reward_bag(0.5, 3.0, 0.0, 30_000));
    // tick 1 (ts 1_000): rests both sides in-band
    let mut b0 = broker(0.0, 1_000);
    mm.on_quote_tick(&mut b0, &quote(1_000, 0.48, 0.52));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

    // tick 2 (ts 11_000, age 10s < 30s), same in-band mid: the young quote is HELD — nothing sent
    let mut b1 = broker(0.0, 11_000);
    mm.on_quote_tick(&mut b1, &quote(11_000, 0.48, 0.52));
    assert!(b1.modifications.is_empty(), "young in-band quote is held for the reward floor");
    assert!(b1.submissions.is_empty() && b1.cancels.is_empty(), "nothing sent while holding");

    // tick 3 (ts 31_000, age 30s >= 30s): released — re-prices in place (no cancel/resubmit)
    let mut b2 = broker(0.0, 31_000);
    mm.on_quote_tick(&mut b2, &quote(31_000, 0.48, 0.52));
    assert!(modified(&b2, "bid") && modified(&b2, "ask"), "re-prices once the moas elapses");
    assert!(b2.submissions.is_empty() && b2.cancels.is_empty(), "released via modify, not churn");
}

// SAFETY: a suppression PULL (fill-rate breaker) is never held by the moas — a quote that must
// come off the book always does, even mid-moas.
#[test]
fn reward_moas_never_holds_a_breaker_pull() {
    let mut mm = SpreadMaker::new(1.0, 0.02)
        .with_fill_breaker(1_000, 2.5, 5_000)
        .with_liquidity_rewards(reward_bag(0.5, 3.0, 0.0, 30_000));
    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 0.49, 0.51));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides rest");
    // a one-sided bid run trips the breaker's bid suppression
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    // next tick is well inside BOTH the cooldown and the moas — the pull must still fire
    let mut b1 = broker(3.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 0.49, 0.51));
    assert!(canceled(&b1, "bid"), "the breaker PULL is never held by the moas");
    assert!(!modified(&b1, "bid") && !submitted(&b1, "bid"), "and it is not re-quoted");
}

// The reward plane is LIVE-TUNABLE without a remount: turn it on, then off, mid-session — and the
// update itself never touches an order (queue position preserved).
#[test]
fn reward_is_live_tunable_without_a_remount() {
    let mut mm = SpreadMaker::new(1.0, 0.05); // reward OFF
    mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 0.48, 0.52));
    // gate OFF ⇒ the candidate is the plain mid ∓ hs (0.45 / 0.55), out of any band
    let mut b1 = broker(0.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 0.48, 0.52));
    assert!(
        (0.5 - modify_px(&b1, "bid") - 0.05).abs() < 1e-12,
        "reward off ⇒ bid at mid − hs (out of band)"
    );

    // turn reward ON over the plane (weight 0.5, band 3¢) — the update itself sends nothing
    let tuned = SpreadMakerParams { reward: Some(reward_bag(0.5, 3.0, 0.0, 0)), ..mm.params() };
    let mut bp = broker(0.0, 3);
    mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(tuned));
    assert_eq!(mm.params().reward, tuned.reward, "the reward bag round-trips");
    assert!(
        bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
        "the update itself must not disturb the resting orders"
    );

    // the NEXT tick re-prices IN PLACE into the band (bid pulled from 0.45 up to 0.485)
    let mut b2 = broker(0.0, 4);
    mm.on_quote_tick(&mut b2, &quote(4, 0.48, 0.52));
    assert!((modify_px(&b2, "bid") - 0.485).abs() < 1e-12, "live-tuned reward pulls into the band");
    assert!(b2.submissions.is_empty() && b2.cancels.is_empty(), "re-priced in place, no churn");

    // a `None` bag turns it straight back off
    let off = SpreadMakerParams { reward: None, ..mm.params() };
    mm.on_params_updated(&mut broker(0.0, 5), &StrategyParams::SpreadMaker(off));
    assert!(mm.cfg.reward.is_none(), "a None reward bag turns the fold off");
}
