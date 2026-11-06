//! The FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c): the toxic side widens away from mid and
//! loses size, a full size-cut routes through the per-side PULL path, and the clean side is untouched.

use super::*;

// ---- flow-toxicity guard (RTDS wallet-toxicity, 5c) ----

// The guard OFF (or configured-but-never-fed) quotes BYTE-IDENTICAL to a plain maker; ON with a
// BID-side toxic reading it widens the bid AWAY from mid (lower) and cuts its size, while the ask
// (no toxic pressure) stays byte-identical — the per-side reaction.
#[test]
fn toxicity_widens_and_cuts_only_the_toxic_side() {
    let q = quote(1, 100.0, 100.2); // Mid, half_spread 0.5 ⇒ bid ≈ 99.6 / ask ≈ 100.6, size 1.0
    let tp = ToxicityParams { widen: 0.5, size_cut: 0.5 };

    // baseline: a plain Mid maker, no toxicity guard.
    let mut base = SpreadMaker::new(1.0, 0.5);
    let mut bb = broker(0.0, 1);
    base.on_quote_tick(&mut bb, &q);
    let (base_bid_px, base_bid_qty) = submit_at(&bb, "bid");
    let (base_ask_px, base_ask_qty) = submit_at(&bb, "ask");

    // guard CONFIGURED but never fed a reading (`last_flow` None) ⇒ byte-identical to baseline.
    let mut unfed = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
    let mut bu = broker(0.0, 1);
    unfed.on_quote_tick(&mut bu, &q);
    let (u_bid_px, u_bid_qty) = submit_at(&bu, "bid");
    assert_eq!(u_bid_px.to_bits(), base_bid_px.to_bits(), "no reading ⇒ bid px unchanged");
    assert_eq!(u_bid_qty.to_bits(), base_bid_qty.to_bits(), "no reading ⇒ bid qty unchanged");

    // guard ON + a BID-side toxic reading (ask side clean).
    let mut tox = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
    tox.on_flow(&mut broker(0.0, 1), FlowToxicity { bid: 1.0, ask: 0.0, ts: 1 });
    let mut bt = broker(0.0, 1);
    tox.on_quote_tick(&mut bt, &q);
    let (tox_bid_px, tox_bid_qty) = submit_at(&bt, "bid");
    let (tox_ask_px, tox_ask_qty) = submit_at(&bt, "ask");

    // BID (toxic): widened LOWER by flow·widen·half = 1.0·0.5·(½·(ask−bid)=0.5) = 0.25; size cut
    // to (1 − flow·size_cut)× = (1 − 0.5)× = 0.5×.
    assert!(tox_bid_px < base_bid_px, "toxic bid widened away from mid (lower)");
    assert!(
        (base_bid_px - tox_bid_px - 0.25).abs() < 1e-9,
        "bid widened by flow·widen·half = 0.25 (base {base_bid_px} → {tox_bid_px})"
    );
    assert!(tox_bid_qty < base_bid_qty, "toxic bid size cut below baseline");
    assert_eq!(tox_bid_qty.to_bits(), 0.5_f64.to_bits(), "bid size cut to exactly 0.5");

    // ASK (clean): byte-identical to baseline — flow.ask = 0 ⇒ no widen, no cut.
    assert_eq!(tox_ask_px.to_bits(), base_ask_px.to_bits(), "clean ask px byte-identical");
    assert_eq!(tox_ask_qty.to_bits(), base_ask_qty.to_bits(), "clean ask qty byte-identical");
}

// A size the cut drives to 0 WITHHOLDS that side through the SAME per-side suppression path the
// fill-rate breaker uses (never a zero-size order): a first tick with a full bid-side cut places
// the ask but NOT the bid, while the clean ask is placed normally.
#[test]
fn toxicity_full_size_cut_withholds_the_side() {
    let tp = ToxicityParams { widen: 0.0, size_cut: 1.0 };
    let mut tox = SpreadMaker::new(1.0, 0.5).with_flow_toxicity(tp);
    // full bid toxicity ⇒ bid size × (1 − 1·1) = 0 ⇒ that side is suppressed; ask clean.
    tox.on_flow(&mut broker(0.0, 1), FlowToxicity { bid: 1.0, ask: 0.0, ts: 1 });
    let mut b = broker(0.0, 1);
    tox.on_quote_tick(&mut b, &quote(1, 100.0, 100.2));
    assert!(!submitted(&b, "bid"), "a fully size-cut bid is withheld (no zero-size order)");
    assert!(submitted(&b, "ask"), "the clean ask is placed normally");
}
