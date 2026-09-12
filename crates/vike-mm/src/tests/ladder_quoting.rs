//! LADDER quoting (steal/mm-ladder-quoting): N rungs per side diffed to the minimal verb set, the
//! single-quote reduction at `levels <= 1`, per-rung tolerance, whole-side pulls, and live toggling.

use super::*;

// ---- ladder quoting (steal/mm-ladder-quoting) ----
//
// A LADDER rests N rungs per side (tags bid0..bidN / ask0..askN) stepping out from the SAME
// reservation the single quote uses. These pin: the OFF/1-level path is byte-identical to today's
// two-tag maker; an active ladder places the right rungs at the right prices/sizes and diffs
// minimally; a suppressed side pulls ALL its rungs (never strands one); the size profile shapes
// rung sizes; and the whole thing is live-tunable across the single↔ladder boundary.

/// A Mid ladder with `levels` rungs per side: one half-spread between rungs, flat sizes — the
/// simplest shape (the geometry/size tests below vary this).
fn mid_ladder(levels: usize) -> LadderParams {
    LadderParams {
        levels,
        offset_step: 1.0,
        offset_unit: LadderOffsetUnit::HalfSpread,
        size_profile: LadderSizeProfile::Linear,
        size_ratio: 1.0,
    }
}

// THE OFF-PATH PROOF: neither a maker with NO ladder NOR one with an inert (empty/1-level) ladder
// changes the pre-feature verbs — a SINGLE order per side, tagged EXACTLY "bid"/"ask" (never
// "bid0"), priced by the unchanged mid formula, re-priced in place next tick. This is the
// byte-identical-when-off gate.
#[test]
fn off_and_single_level_ladder_reproduce_todays_two_tag_maker() {
    for ladder in [None, Some(mid_ladder(0)), Some(mid_ladder(1))] {
        let mut mm = SpreadMaker::new(1.0, 0.5);
        if let Some(lp) = ladder {
            mm = mm.with_ladder(lp);
        }
        let mut b = broker(0.0, 1);
        mm.on_quote_tick(&mut b, &quote(1, 100.0, 100.2));
        // exactly the two single-quote submits, with the pre-feature tags (no rung tags)
        assert_eq!(b.submissions.len(), 2, "one order per side ({ladder:?})");
        assert!(submitted(&b, "bid") && submitted(&b, "ask"), "tags bid/ask ({ladder:?})");
        assert!(!submitted(&b, "bid0"), "never a bid0 rung on the off path ({ladder:?})");
        assert!(!submitted(&b, "ask0"), "never an ask0 rung on the off path ({ladder:?})");
        assert!(b.modifications.is_empty() && b.cancels.is_empty(), "nothing else ({ladder:?})");
        // and the price is today's mid formula, bit-for-bit
        let mid = 0.5 * (100.0_f64 + 100.2);
        assert_eq!(submit_at(&b, "bid").0.to_bits(), (mid - 0.5).to_bits(), "bid unchanged");
        assert_eq!(submit_at(&b, "ask").0.to_bits(), (mid + 0.5).to_bits(), "ask unchanged");
        // second tick modifies the SAME two tags in place — no rung tag ever appears
        let mut b2 = broker(0.0, 2);
        mm.on_quote_tick(&mut b2, &quote(2, 100.0, 100.4));
        assert_eq!(b2.modifications.len(), 2, "two in-place modifies ({ladder:?})");
        assert!(modified(&b2, "bid") && modified(&b2, "ask"), "same tags ({ladder:?})");
        assert!(b2.submissions.is_empty() && b2.cancels.is_empty(), "nothing else ({ladder:?})");
    }
}

// An ACTIVE 3-rung ladder rests three orders per side at the reservation ± k half-spreads, tagged
// bid0/bid1/bid2 & ask0/ask1/ask2, then re-prices them in place next tick (no submit/cancel churn).
#[test]
fn active_ladder_rests_rungs_at_stepped_prices() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
    let mut b = broker(0.0, 1);
    // mid 100.5, half_spread 0.5 → bid0 100.0 / ask0 101.0, half 0.5 between rungs
    mm.on_quote_tick(&mut b, &quote(1, 100.0, 101.0));
    assert_eq!(b.submissions.len(), 6, "three rungs per side");
    // bid rungs step DOWN one half-spread (0.5) each; ask rungs step UP; flat size 1.0
    for (tag, px) in [
        ("bid0", 100.0f64),
        ("bid1", 99.5),
        ("bid2", 99.0),
        ("ask0", 101.0),
        ("ask1", 101.5),
        ("ask2", 102.0),
    ] {
        assert_eq!(submit_at(&b, tag).0.to_bits(), px.to_bits(), "{tag} price");
        assert_eq!(submit_at(&b, tag).1.to_bits(), 1.0_f64.to_bits(), "{tag} flat size");
    }
    assert!(b.cancels.is_empty(), "nothing pulled on the first ladder tick");
    // next tick, same book → six in-place modifies, no new submits, no cancels (minimal diff)
    let mut b2 = broker(0.0, 2);
    mm.on_quote_tick(&mut b2, &quote(2, 100.0, 101.0));
    assert_eq!(b2.modifications.len(), 6, "all six rungs re-priced in place");
    assert!(b2.submissions.is_empty() && b2.cancels.is_empty(), "no churn: modify-only");
}

// The per-rung refresh tolerance gates each rung's re-price: an unchanged/sub-tolerance tick
// re-quotes NOTHING (all rungs inside tolerance); without the gate the same tick re-quotes all six.
#[test]
fn ladder_honors_refresh_tolerance_per_rung() {
    // gate OFF: an identical second tick still re-prices every rung
    let mut plain = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
    plain.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));
    let mut p2 = broker(0.0, 2);
    plain.on_quote_tick(&mut p2, &quote(2, 100.0, 101.0));
    assert_eq!(p2.modifications.len(), 6, "gate off ⇒ every rung re-quotes");

    // gate ON (wide, price-only): a microscopic move leaves ALL rungs alone
    let mut gated =
        SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3)).with_refresh_tolerance(25.0, 0.0);
    gated.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));
    let mut g2 = broker(0.0, 2);
    gated.on_quote_tick(&mut g2, &quote(2, 100.001, 101.001));
    assert!(g2.modifications.is_empty(), "a sub-tolerance move skips every rung");
    assert!(g2.submissions.is_empty() && g2.cancels.is_empty(), "nothing sent at all");
    // a real move re-quotes every rung again
    let mut g3 = broker(0.0, 3);
    gated.on_quote_tick(&mut g3, &quote(3, 100.5, 101.5));
    assert_eq!(g3.modifications.len(), 6, "a real move re-prices all rungs");
}

// Under the fill-rate breaker, a suppressed side pulls EVERY rung (never strands one), while the
// un-hit side keeps laddering in place.
#[test]
fn ladder_suppression_pulls_all_rungs_of_the_hit_side() {
    let mut mm =
        SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3)).with_fill_breaker(1000, 2.5, 5000);
    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
    assert_eq!(b0.submissions.len(), 6, "both sides laddered first");

    // three bid fills net +3 >= 2.5 → suppress the bid side
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    let mut b1 = broker(3.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
    // EVERY bid rung is pulled — none stranded, none re-quoted while suppressed
    for tag in ["bid0", "bid1", "bid2"] {
        assert!(canceled(&b1, tag), "{tag} pulled under suppression");
        assert!(!submitted(&b1, tag) && !modified(&b1, tag), "{tag} not re-quoted");
    }
    // and the un-hit ask side keeps quoting all its rungs in place
    for tag in ["ask0", "ask1", "ask2"] {
        assert!(modified(&b1, tag), "{tag} keeps quoting");
        assert!(!canceled(&b1, tag), "{tag} never pulled");
    }
}

// A live re-tune to FEWER levels cancels exactly the now-extra rungs and re-prices the kept ones —
// the minimal diff, never a full cancel/replace of the side.
#[test]
fn ladder_shrink_cancels_only_the_extra_rungs() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(mid_ladder(3));
    mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0)); // rest 6 rungs

    // re-tune to 2 levels per side; the update itself touches no order (queue preserved)
    let two = SpreadMakerParams { ladder: Some(mid_ladder(2)), ..mm.params() };
    let mut bp = broker(0.0, 2);
    mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(two));
    assert!(
        bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
        "a re-tune never disturbs an order itself"
    );

    let mut b = broker(0.0, 3);
    mm.on_quote_tick(&mut b, &quote(3, 100.0, 101.0));
    // the deepest rung each side is canceled; the two kept rungs re-price in place
    assert!(canceled(&b, "bid2") && canceled(&b, "ask2"), "the extra rungs are pulled");
    for tag in ["bid0", "bid1", "ask0", "ask1"] {
        assert!(modified(&b, tag), "{tag} kept + re-priced");
        assert!(!canceled(&b, tag), "{tag} never pulled");
    }
    assert!(b.submissions.is_empty(), "no rung is re-submitted (queue preserved)");
}

// The size profile shapes rung SIZES off the base quote: geometric ×2 gives 1, 2, 4 per rung.
#[test]
fn ladder_size_profile_shapes_rung_sizes() {
    let lp = LadderParams {
        levels: 3,
        offset_step: 1.0,
        offset_unit: LadderOffsetUnit::HalfSpread,
        size_profile: LadderSizeProfile::Geometric,
        size_ratio: 2.0,
    };
    let mut mm = SpreadMaker::new(1.0, 0.5).with_ladder(lp);
    let mut b = broker(0.0, 1);
    mm.on_quote_tick(&mut b, &quote(1, 100.0, 101.0));
    for (tag, sz) in [
        ("bid0", 1.0f64),
        ("bid1", 2.0),
        ("bid2", 4.0),
        ("ask0", 1.0),
        ("ask1", 2.0),
        ("ask2", 4.0),
    ] {
        assert_eq!(submit_at(&b, tag).1.to_bits(), sz.to_bits(), "{tag} geometric size");
    }
}

// Live-tunable across the single↔ladder boundary WITHOUT a remount: turning the ladder ON retires
// the single "bid"/"ask" and rests the rungs; turning it OFF retires the rungs and restores the
// single quote — no order is stranded either way.
#[test]
fn ladder_toggles_live_without_stranding_orders() {
    let mut mm = SpreadMaker::new(1.0, 0.5); // single-quote maker
    mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 101.0));

    // turn the ladder ON over the plane
    let on = SpreadMakerParams { ladder: Some(mid_ladder(3)), ..mm.params() };
    mm.on_params_updated(&mut broker(0.0, 2), &StrategyParams::SpreadMaker(on));
    let mut b = broker(0.0, 3);
    mm.on_quote_tick(&mut b, &quote(3, 100.0, 101.0));
    assert!(canceled(&b, "bid") && canceled(&b, "ask"), "the single quote is retired");
    for tag in ["bid0", "bid1", "bid2", "ask0", "ask1", "ask2"] {
        assert!(submitted(&b, tag), "{tag} rung placed");
    }

    // turn it back OFF
    let off = SpreadMakerParams { ladder: None, ..mm.params() };
    mm.on_params_updated(&mut broker(0.0, 4), &StrategyParams::SpreadMaker(off));
    let mut b2 = broker(0.0, 5);
    mm.on_quote_tick(&mut b2, &quote(5, 100.0, 101.0));
    for tag in ["bid0", "bid1", "bid2", "ask0", "ask1", "ask2"] {
        assert!(canceled(&b2, tag), "{tag} rung retired");
    }
    assert!(submitted(&b2, "bid") && submitted(&b2, "ask"), "the single quote is restored");
}
