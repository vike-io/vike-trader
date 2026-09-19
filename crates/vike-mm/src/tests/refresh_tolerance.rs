//! The order-refresh TOLERANCE (the anti-churn gate): sub-tolerance drift skips the modify,
//! supra-tolerance re-prices in place, a fill always forces the size top-up, and a PULL is never
//! tolerance-gated.

use super::*;

// ---- order-refresh tolerance (the anti-churn gate) ----
//
// The maker used to emit TWO venue modifies per quote/book tick forever (nothing below it
// dedupes a no-change modify). These pin the gate that stops that churn — and, just as
// importantly, that it is OFF by default and never swallows a PULL.

// OFF BY DEFAULT (the neutral-reduction gate): an unconfigured maker re-issues both modifies on
// every tick — including ticks whose target barely moved, or did not move at all — exactly as
// before this feature. An explicitly all-zero (inert) bag behaves identically.
#[test]
fn refresh_tolerance_off_by_default_re_quotes_every_tick() {
    let mut mm = SpreadMaker::new(1.0, 0.5);
    assert!(mm.cfg.refresh_tolerance.is_none(), "the gate is OFF by default");

    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

    // a run of ticks whose target is unchanged / microscopically moved: every single one still
    // issues EXACTLY two modifies (today's per-tick behavior, reproduced)
    let ticks = [(100.0, 100.2), (100.000_001, 100.200_001), (100.0, 100.2)];
    for (i, &(bid, ask)) in ticks.iter().enumerate() {
        let ts = 2 + i as i64;
        let mut b = broker(0.0, ts);
        mm.on_quote_tick(&mut b, &quote(ts, bid, ask));
        assert!(modified(&b, "bid") && modified(&b, "ask"), "tick {i}: both sides re-priced");
        assert_eq!(b.modifications.len(), 2, "tick {i}: exactly two modifies, as today");
        assert!(b.submissions.is_empty() && b.cancels.is_empty(), "tick {i}: nothing else");
    }

    // an explicitly INERT bag (both axes 0) is likewise never a skip
    let mut inert = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(0.0, 0.0);
    inert.on_quote_tick(&mut broker(0.0, 10), &quote(10, 100.0, 100.2));
    let mut c1 = broker(0.0, 11);
    inert.on_quote_tick(&mut c1, &quote(11, 100.0, 100.2));
    assert_eq!(c1.modifications.len(), 2, "an all-zero tolerance still re-quotes every tick");
}

// (a) SUB-tolerance drift sends NOTHING: the resting orders (and their queue position) are left
// alone — no modify, no submit, no cancel.
#[test]
fn sub_tolerance_drift_skips_the_modify() {
    // 25 bp of price; sizes are constant here, so the (0 bp) size axis passes on exact equality
    let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
    // tick 1 rests both sides off mid 100.1 → bid 99.6 / ask 100.6
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

    // tick 2: the mid moves 100.1 → 100.101, so each target drifts 0.001 ≈ 0.1 bp — far inside
    // the 25 bp gate. NOTHING may go on the wire.
    let mut b1 = broker(0.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 100.001, 100.201));
    assert!(b1.modifications.is_empty(), "sub-tolerance drift must not re-price");
    assert!(b1.submissions.is_empty(), "and must not re-submit either");
    assert!(b1.cancels.is_empty(), "and must not pull anything");
}

// (b) SUPRA-tolerance drift still re-prices IN PLACE (modify, never cancel + resubmit) — the
// gate suppresses churn, never a genuine re-quote.
#[test]
fn supra_tolerance_drift_still_re_prices_in_place() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));

    // the mid jumps 100.1 → 101.1: each target moves 1.0 ≈ 100 bp, well past the 25 bp gate
    let mut b1 = broker(0.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 101.0, 101.2));
    assert!(modified(&b1, "bid") && modified(&b1, "ask"), "a real move re-prices both sides");
    assert!(b1.submissions.is_empty(), "re-priced in place — never cancel + resubmit");
    assert!(b1.cancels.is_empty(), "nothing pulled by a plain re-quote");
    let mid = 0.5 * (101.0_f64 + 101.2);
    assert_eq!(modify_px(&b1, "bid").to_bits(), (mid - 0.5).to_bits(), "at the new mid − hs");
    assert_eq!(modify_px(&b1, "ask").to_bits(), (mid + 0.5).to_bits(), "at the new mid + hs");
}

// (c) A SIZE change beyond tolerance re-quotes even when the PRICE is pinned — the two axes are
// ANDed, so an inventory-skew re-size is never swallowed by a wide price tolerance.
#[test]
fn size_drift_beyond_tolerance_re_quotes_with_the_price_pinned() {
    // a very wide 500 bp price gate + a tight 10 bp size gate; the skew moves the SIZES
    let mut mm =
        SpreadMaker::new(2.0, 0.5).with_skew(0.0, 4.0, 0.5).with_refresh_tolerance(500.0, 10.0);
    // flat inventory → both sides rest at the base size 2.0
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    let (rested_bid_px, rested_bid_qty) = submit_at(&b0, "bid");
    assert_eq!(rested_bid_qty.to_bits(), 2.0_f64.to_bits(), "flat rests the base size");

    // the SAME book (price target identical — zero price drift) but now LONG 2.0: the skew
    // shrinks the bid to 1.5 and grows the ask to 2.5 (25% ≫ 10 bp), so BOTH sides re-quote.
    let mut b1 = broker(2.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.2));
    let bid = b1.modifications.iter().find(|m| m.tag == "bid").expect("bid re-sized");
    let ask = b1.modifications.iter().find(|m| m.tag == "ask").expect("ask re-sized");
    assert!(bid.new_qty.unwrap() < 2.0, "long → the bid shrank: {:?}", bid.new_qty);
    assert!(ask.new_qty.unwrap() > 2.0, "long → the ask grew: {:?}", ask.new_qty);
    assert_eq!(
        bid.new_price.unwrap().to_bits(),
        rested_bid_px.to_bits(),
        "the price never moved — the SIZE axis alone forced the re-quote"
    );

    // and the mirror: holding that same inventory (identical price AND size targets) is a skip
    let mut b2 = broker(2.0, 3);
    mm.on_quote_tick(&mut b2, &quote(3, 100.0, 100.2));
    assert!(b2.modifications.is_empty(), "an unchanged price AND size sends nothing");
}

// (d) SAFETY: a FILL invalidates the FILLED side's resting snapshot, so the partial-fill size
// top-up can never be swallowed by the gate. `SideState::own` holds the maker's INTENDED
// quote, not the venue-side remainder: after a partial fill the remainder is smaller, yet the
// intended-vs-target comparison still reads "no change". The next tick must therefore go
// through the modify arm — re-issuing the FULL configured size — even with the price unmoved.
#[test]
fn a_fill_forces_the_filled_side_through_the_modify_arm() {
    // NO breaker (so `on_fill`'s early return is exercised) + an armed 25 bp price gate
    let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "first tick rests both sides");

    // precondition: with NO fill, an identical tick sends nothing (the gate really is armed)
    let mut b1 = broker(0.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 100.0, 100.2));
    assert!(b1.modifications.is_empty(), "precondition: an unchanged tick is skipped");

    // a PARTIAL fill on the bid: 0.4 of the resting 1.0 traded, only 0.6 remains at the venue
    mm.on_fill(&mut broker(0.0, 3), &a_fill(1, 0.4, 3));

    // the very next tick — same book, so the computed price AND size are bit-identical to what
    // the bid's `own` snapshot holds — MUST still re-issue the bid, topping it back up to the full size
    let mut b2 = broker(0.0, 4);
    mm.on_quote_tick(&mut b2, &quote(4, 100.0, 100.2));
    let bid = b2.modifications.iter().find(|m| m.tag == "bid").expect("the filled side re-quotes");
    assert_eq!(
        bid.new_qty.unwrap().to_bits(),
        1.0_f64.to_bits(),
        "topped back up to the full configured size"
    );
    assert!(!modified(&b2, "ask"), "the UNFILLED side is still gated — no churn there");

    // the invalidation is ONE-SHOT: the following unchanged tick is skipped again
    let mut b3 = broker(0.0, 5);
    mm.on_quote_tick(&mut b3, &quote(5, 100.0, 100.2));
    assert!(b3.modifications.is_empty(), "the stale mark clears on the re-quote it forced");

    // the mirror: a SELL fill invalidates the ASK side only
    mm.on_fill(&mut broker(0.0, 6), &a_fill(-1, 0.4, 6));
    let mut b4 = broker(0.0, 7);
    mm.on_quote_tick(&mut b4, &quote(7, 100.0, 100.2));
    assert!(modified(&b4, "ask"), "a sell fill re-quotes the ask side");
    assert!(!modified(&b4, "bid"), "…and leaves the untouched bid gated");
}

// (e) SAFETY: a PULL is never tolerance-gated. Even with a tolerance wide enough to swallow any
// re-price, the fill-rate breaker's suppression still cancels the over-hit side — tolerance may
// only ever skip a re-price, never strand a quote that must come off the book.
#[test]
fn suppression_pull_ignores_the_refresh_tolerance() {
    // 10_000 bp = 100%: every conceivable re-price is inside this gate
    let mut mm = SpreadMaker::new(1.0, 0.5)
        .with_fill_breaker(1_000, 2.5, 5_000)
        .with_refresh_tolerance(10_000.0, 10_000.0);
    let mut b0 = broker(0.0, 100);
    mm.on_quote_tick(&mut b0, &quote(100, 100.0, 101.0));
    assert!(submitted(&b0, "bid") && submitted(&b0, "ask"), "both sides rest");

    // a one-sided bid run trips the breaker's bid suppression
    for t in [110, 120, 130] {
        mm.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
    }
    let mut b1 = broker(0.0, 140);
    mm.on_quote_tick(&mut b1, &quote(140, 100.0, 101.0));
    assert!(canceled(&b1, "bid"), "the breaker's PULL is never tolerance-gated");
    assert!(!modified(&b1, "bid") && !submitted(&b1, "bid"), "and it is not re-quoted");
    // the un-hit ask, whose target did not move at all, is legitimately left resting — but it
    // is NOT pulled: the gate only ever skips a re-price.
    assert!(!modified(&b1, "ask"), "the unchanged ask side is left alone (no churn)");
    assert!(!canceled(&b1, "ask"), "…and is NOT pulled by the tolerance");

    // once the cooldown expires the bid re-enters the book with a fresh SUBMIT — the place path
    // is likewise never tolerance-gated.
    let mut b2 = broker(0.0, 5_200);
    mm.on_quote_tick(&mut b2, &quote(5_200, 100.0, 101.0));
    assert!(submitted(&b2, "bid"), "a PLACE is never tolerance-gated either");
}

// Drift is measured against what is actually RESTING, not against the previous target — so a
// slow one-way crawl of small steps still re-prices once it has genuinely moved, and can never
// ratchet away from the book one sub-tolerance step at a time.
#[test]
fn drift_is_measured_against_the_resting_order_not_the_last_target() {
    let mut mm = SpreadMaker::new(1.0, 0.5).with_refresh_tolerance(25.0, 0.0);
    // rest at mid 100.1 → bid 99.6 / ask 100.6; 25 bp of 99.6 ≈ 0.249
    let mut b0 = broker(0.0, 1);
    mm.on_quote_tick(&mut b0, &quote(1, 100.0, 100.2));
    assert!(submitted(&b0, "bid"), "first tick rests the bid");

    // four successive +0.05 steps of the mid: each is a NEW target, but every one is still
    // within 0.249 of the RESTING 99.6, so nothing goes on the wire
    for step in 1..=4_i32 {
        let shift = 0.05 * f64::from(step);
        let ts = 1 + i64::from(step);
        let mut b = broker(0.0, ts);
        mm.on_quote_tick(&mut b, &quote(ts, 100.0 + shift, 100.2 + shift));
        assert!(b.modifications.is_empty(), "step {step}: still inside the resting tolerance");
    }
    // the fifth step (mid +0.25 → bid target 99.85, i.e. 0.25 off the resting 99.6 ≈ 25.1 bp)
    // finally trips the gate — the accumulated drift is NOT lost
    let mut b5 = broker(0.0, 6);
    mm.on_quote_tick(&mut b5, &quote(6, 100.25, 100.45));
    assert!(modified(&b5, "bid"), "accumulated drift past the tolerance DOES re-price");
    // the ask's own 0.25 drift is a smaller RELATIVE move off its higher resting price
    // (0.25/100.6 ≈ 24.9 bp), so it is still inside — the two sides gate independently
    assert!(!modified(&b5, "ask"), "each side is gated against its own resting price");
}

// The gate rides the live-parameter plane: it can be turned on, and back off, mid-session
// WITHOUT unmounting — and the update itself never touches an order (queue position preserved).
#[test]
fn refresh_tolerance_is_live_tunable_without_a_remount() {
    let mut mm = SpreadMaker::new(1.0, 0.5); // gate OFF
    mm.on_quote_tick(&mut broker(0.0, 1), &quote(1, 100.0, 100.2));
    let mut b1 = broker(0.0, 2);
    mm.on_quote_tick(&mut b1, &quote(2, 100.001, 100.201));
    assert_eq!(b1.modifications.len(), 2, "gate off ⇒ even a 0.1 bp move re-quotes both sides");

    // turn the gate ON over the plane
    let tuned = SpreadMakerParams {
        refresh_tolerance: Some(RefreshTolerance { price_bps: 25.0, size_bps: 0.0 }),
        ..mm.params()
    };
    let mut bp = broker(0.0, 3);
    mm.on_params_updated(&mut bp, &StrategyParams::SpreadMaker(tuned));
    assert_eq!(mm.params().refresh_tolerance, tuned.refresh_tolerance, "the bag round-trips");
    assert!(
        bp.submissions.is_empty() && bp.modifications.is_empty() && bp.cancels.is_empty(),
        "the update itself must not disturb the resting orders"
    );

    // the same microscopic move is now skipped
    let mut b2 = broker(0.0, 4);
    mm.on_quote_tick(&mut b2, &quote(4, 100.002, 100.202));
    assert!(b2.modifications.is_empty(), "the live-tuned gate takes effect on the next tick");

    // and a `None` bag turns it straight back off
    let off = SpreadMakerParams { refresh_tolerance: None, ..mm.params() };
    mm.on_params_updated(&mut broker(0.0, 5), &StrategyParams::SpreadMaker(off));
    assert!(mm.cfg.refresh_tolerance.is_none(), "a None bag turns the gate off");
    let mut b3 = broker(0.0, 6);
    mm.on_quote_tick(&mut b3, &quote(6, 100.002, 100.202));
    assert_eq!(b3.modifications.len(), 2, "gate off again ⇒ back to re-quoting every tick");
}
